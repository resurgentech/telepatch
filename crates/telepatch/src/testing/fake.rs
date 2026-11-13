//! A fake executor: records what it would run and answers facts from a
//! fixture map, then renders the record as deterministic text.

use std::collections::BTreeMap;
use std::fmt::Write;

use crate::engine::{
    Applied, Crossed, Engine, Executor, HostInfo, Observation, RebootAt, Window, WindowKind,
};
use crate::facts::query::{Fact, FactQuery};
use crate::host::Host;
use crate::key::Value;
use crate::resolver::Resolved;
use crate::role::Playbook;
use crate::testing::{FixtureHost, block_on};
use crate::trace::{NodeId, RunAs, Source};

#[derive(Debug, Clone)]
pub struct AppliedOp {
    pub id: NodeId,
    pub kind: &'static str,
    pub run_as: RunAs,
    pub script: String,
}

#[derive(Debug, Clone)]
pub enum Call {
    Resolved {
        window: usize,
        read: Resolved,
    },
    ImplicitBarrier {
        window: usize,
        reads: Vec<String>,
    },
    Apply {
        window: usize,
        kind: WindowKind,
        ops: Vec<AppliedOp>,
    },
    Read {
        window: usize,
        answers: Vec<(FactQuery, Value)>,
    },
    /// A reboot barrier the engine asked the executor to cross.
    Reboot {
        window: usize,
    },
}

/// Records every call and answers reads from `(RunAs, Fact) -> Value`;
/// unlisted facts are missing (`Exists(false)` / `Absent`).
#[derive(Default)]
pub struct FakeExecutor {
    fixtures: BTreeMap<(RunAs, Fact), Value>,
    /// Calls per host, in order.
    pub calls: Vec<(String, Call)>,
    /// Every window applied, with its host, as the engine prepared it.
    pub windows: Vec<(String, Window)>,
    /// Every reboot barrier crossed, with its host and the number of the
    /// last window applied before it (0: none).
    pub reboots: Vec<(String, usize)>,
    /// A host with neither curl nor wget, where every download needs one:
    /// a window with a split candidate is declined for its head, unrecorded.
    no_downloader: bool,
}

impl FakeExecutor {
    pub fn new() -> FakeExecutor {
        FakeExecutor::default()
    }

    /// Fixture: what `fact` answers when asked in `run_as`.
    pub fn answer(mut self, run_as: RunAs, fact: Fact, value: Value) -> FakeExecutor {
        self.fixtures.insert((run_as, fact), value);
        self
    }

    /// A host with neither `curl` nor `wget`: every window with a split
    /// candidate is declined for its head ([`Applied::Head`]), as the
    /// executor does when prepare finds no downloader.
    pub fn without_downloader(mut self) -> FakeExecutor {
        self.no_downloader = true;
        self
    }

    fn lookup(&self, q: &FactQuery) -> Value {
        self.fixtures
            .get(&(q.run_as.clone(), q.fact.clone()))
            .cloned()
            .unwrap_or_else(|| q.fact.missing())
    }

    /// Number of reads that needed a round trip.
    pub fn blocking_reads(&self) -> usize {
        self.calls
            .iter()
            .map(|(_, c)| match c {
                Call::Read { answers, .. } => answers.len(),
                _ => 0,
            })
            .sum()
    }

    /// Deterministic text: per host, numbered windows; each lists reads
    /// (with source), applied ops and where implicit barriers fell.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let mut host: Option<&str> = None;
        let mut window = 0;
        for (h, call) in &self.calls {
            if host != Some(h) {
                if host.is_some() {
                    out.push('\n');
                }
                let _ = writeln!(out, "host {h}");
                host = Some(h);
                window = 0;
            }
            let w = match call {
                Call::Resolved { window, .. }
                | Call::ImplicitBarrier { window, .. }
                | Call::Apply { window, .. }
                | Call::Reboot { window }
                | Call::Read { window, .. } => *window,
            };
            if w != window {
                window = w;
                let _ = writeln!(out, "window {w}");
            }
            match call {
                Call::Resolved { read, .. } => {
                    let _ = writeln!(
                        out,
                        "  read {} = {} [{}] (no round trip)",
                        read.query,
                        read.value,
                        source(&read.source)
                    );
                }
                Call::ImplicitBarrier { reads, .. } => {
                    let _ = writeln!(
                        out,
                        "  -- implicit barrier: read of {} needs pending writes --",
                        reads.join(", ")
                    );
                }
                Call::Apply { kind, ops, .. } => {
                    let _ = writeln!(out, "  apply ({}):", kind_name(*kind));
                    for op in ops {
                        let _ = writeln!(
                            out,
                            "    {} {} as {}: {}",
                            op.id,
                            op.kind,
                            op.run_as,
                            op.script.replace('\n', " ; ")
                        );
                    }
                }
                Call::Reboot { .. } => {
                    let _ = writeln!(out, "  -- reboot if pending --");
                }
                Call::Read { answers, .. } => {
                    let _ = writeln!(out, "  round trip:");
                    for (q, v) in answers {
                        let _ = writeln!(out, "    read {q} = {v} [Block]");
                    }
                }
            }
        }
        let _ = writeln!(out, "\nblocking reads: {}", self.blocking_reads());
        out
    }
}

fn source(s: &Source) -> String {
    match s {
        Source::Confirmed => "Confirmed".into(),
        Source::Postcondition(id) => format!("Postcondition {id}"),
        Source::Block => "Block".into(),
    }
}

fn kind_name(k: WindowKind) -> &'static str {
    match k {
        WindowKind::NonMutating => "non-mutating",
        WindowKind::ImplicitBarrier => "implicit barrier",
        WindowKind::Final => "final",
        WindowKind::AtEnd => "at end",
    }
}

impl Executor for FakeExecutor {
    async fn read_facts(
        &mut self,
        host: &HostInfo,
        window: usize,
        queries: &[FactQuery],
    ) -> Vec<Value> {
        let answers: Vec<(FactQuery, Value)> = queries
            .iter()
            .map(|q| (q.clone(), self.lookup(q)))
            .collect();
        let values = answers.iter().map(|(_, v)| v.clone()).collect();
        self.calls
            .push((host.name.clone(), Call::Read { window, answers }));
        values
    }

    async fn apply(&mut self, host: &HostInfo, window: &Window) -> Result<Applied, String> {
        if !window.assertions.is_empty() {
            return Err(window.assertions.join("\n"));
        }
        if self.no_downloader
            && let Some(s) = &window.split
        {
            return Ok(Applied::Head(s.at + 1));
        }
        self.windows.push((host.name.clone(), window.clone()));
        let ops = window
            .ops
            .iter()
            .map(|o| AppliedOp {
                id: o.id,
                kind: o.node.spec.kind(),
                run_as: o.node.run_as.clone(),
                script: o.node.spec.script(&host.os),
            })
            .collect();
        self.calls.push((
            host.name.clone(),
            Call::Apply {
                window: window.number,
                kind: window.kind,
                ops,
            },
        ));
        Ok(Applied::All)
    }

    async fn reboot(
        &mut self,
        host: &HostInfo,
        window: usize,
        _at: &RebootAt,
    ) -> Result<Crossed, String> {
        let last = self
            .windows
            .iter()
            .rev()
            .find(|(h, _)| *h == host.name)
            .map_or(0, |(_, w)| w.number);
        self.reboots.push((host.name.clone(), last));
        self.calls
            .push((host.name.clone(), Call::Reboot { window }));
        Ok(Crossed::Continue)
    }

    fn observe(&mut self, host: &HostInfo, obs: &Observation) {
        let call = match obs {
            Observation::Resolved { window, read } => Call::Resolved {
                window: *window,
                read: read.clone(),
            },
            Observation::ImplicitBarrier { window, reads } => Call::ImplicitBarrier {
                window: *window,
                reads: reads.iter().map(|k| k.to_string()).collect(),
            },
        };
        self.calls.push((host.name.clone(), call));
    }
}

/// Run `playbook` on `host` through the engine with `fake`; return the
/// rendered record. Panics if the playbook or engine fails, or if the
/// fake's blocking-read count disagrees with the trace's.
pub fn run_engine<V>(playbook: &Playbook<V>, host: &FixtureHost<V>, fake: FakeExecutor) -> String {
    let h = Host::new(host.name.clone(), host.os.clone(), host.vars.clone());
    let mut engine = Engine::new(fake);
    block_on(engine.run_host(playbook, h.clone()))
        .unwrap_or_else(|e| panic!("playbook {} on {}: {e}", playbook.name, host.name));
    assert_eq!(
        engine.executor.blocking_reads(),
        h.snapshot().blocking_reads(),
        "fake and trace disagree on blocking reads"
    );
    engine.executor.render()
}
