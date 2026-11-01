//! The real executor: runs an optimized window on a host's root shell.
//!
//! One [`HostExec`] per host, one engine future per host, all joined on a
//! current-thread runtime with a `LocalSet` ([`run`]). Per host:
//!
//! 1. open the root shell (`want` 1) and read `os` before the playbook
//!    future starts ([`crate::transport::read_os`]);
//! 2. the engine drives the playbook; [`Executor::read_facts`] sends the
//!    fact probes through the same shell in one batch; [`Executor::apply`]
//!    runs prepare ([`crate::prepare`]), then pipelines every op of the
//!    window and collects results in order, streaming output live.
//!
//! # Failure gating
//!
//! Ops are pipelined, so by the time op N fails, N+1.. are already written
//! to the shell. Each op is therefore sent as
//! `if [ -z "${__tp_halt-}" ]; then <op>; [ $? -eq 0 ] || __tp_halt=1;
//! else <skip trailer>; fi`. `__tp_halt` lives in the persistent shell and
//! is never reset: after a failure no later op on the host runs. An
//! `allow_fail` op never sets it. After a failure the executor stops
//! sending windows but lets the engine finish tracing, so the report can
//! list every skipped role; fact reads still run (they only read).
//!
//! # Stream
//!
//! Each op prints `== host [role] #id desc ==`, its output lines prefixed
//! `host | `, then `-> result`. No timing is printed. With
//! [`RunOpts::log`] a JSONL file gets the events `host_open`, `window`,
//! `op_start`, `output` (lossy UTF-8 text per chunk), `op_result`,
//! `facts`, `host_status`.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::io::Write as _;
use std::path::PathBuf;
use std::rc::Rc;

use serde_json::json;

use crate::engine::{Engine, EngineError, Executor, HostInfo, Window, WindowKind, WindowOp};
use crate::facts::query::FactQuery;
use crate::host::Host;
use crate::key::Value;
use crate::ops::pkg::{PkgMode, Wanted};
use crate::pool::{Event, Shell};
use crate::prepare;
use crate::report::{HostReport, HostStatus, OpRecord, Outcome, Report};
use crate::role::Playbook;
use crate::trace::{OpNode, OpSpec, Principal};
use crate::trailer::{OpKind, Trailer, wrap};
use crate::transport::docker::DockerTransport;
use crate::transport::local::LocalTransport;
use crate::transport::{Limit, Shells, Transport, read_os};

/// Where to reach a host.
pub enum Target {
    Local(LocalTransport),
    Docker(DockerTransport),
}

impl Transport for Target {
    async fn open(&self, host: &str, principal: &Principal, want: usize) -> Shells {
        match self {
            Target::Local(t) => t.open(host, principal, want).await,
            Target::Docker(t) => t.open(host, principal, want).await,
        }
    }
}

/// A host to run on: its name, how to reach it, the playbook variables.
pub struct HostSpec<V> {
    pub name: String,
    pub target: Target,
    pub vars: V,
}

/// Where op output goes.
#[derive(Clone)]
pub enum Stdout {
    /// This process's stdout.
    Inherit,
    /// Appended to this string (tests).
    Capture(Rc<RefCell<String>>),
    Quiet,
}

pub struct RunOpts {
    /// Write JSONL events here.
    pub log: Option<PathBuf>,
    pub stdout: Stdout,
}

impl Default for RunOpts {
    fn default() -> Self {
        RunOpts {
            log: None,
            stdout: Stdout::Inherit,
        }
    }
}

/// Stdout and the JSONL log, shared by all hosts.
struct Sink {
    stdout: Stdout,
    log: Option<RefCell<std::io::BufWriter<std::fs::File>>>,
}

impl Sink {
    fn line(&self, s: &str) {
        match &self.stdout {
            Stdout::Inherit => {
                let mut o = std::io::stdout().lock();
                let _ = writeln!(o, "{s}");
                let _ = o.flush();
            }
            Stdout::Capture(buf) => {
                let mut b = buf.borrow_mut();
                b.push_str(s);
                b.push('\n');
            }
            Stdout::Quiet => {}
        }
    }

    fn event(&self, v: serde_json::Value) {
        if let Some(log) = &self.log {
            let mut w = log.borrow_mut();
            let _ = writeln!(w, "{v}");
            let _ = w.flush();
        }
    }
}

const TAIL_LINES: usize = 40;
const TAIL_BYTES: usize = 256 * 1024;
/// Trace ids are small; probes use ids far above them.
const PROBE_BASE: usize = 1 << 40;

/// Per-op state while its window runs.
struct Live {
    header: bool,
    partial: Vec<u8>,
    tail: Vec<u8>,
}

pub struct HostExec {
    name: String,
    sink: Rc<Sink>,
    shell: Shell,
    report: HostReport,
    /// Failed or blocked: nothing more runs on this host.
    halted: bool,
    verdict: Option<HostStatus>,
    users_ok: BTreeSet<String>,
    seq: usize,
}

fn describe(op: &OpNode) -> String {
    fn first_line(s: &str) -> String {
        let l = s.lines().next().unwrap_or("").trim();
        if l.chars().count() > 70 {
            format!("{}...", l.chars().take(67).collect::<String>())
        } else {
            l.to_string()
        }
    }
    match &op.spec {
        OpSpec::Raw { script } => format!("sh {}", first_line(script)),
        OpSpec::Shell(s) => format!("sh {}", first_line(&s.cmd)),
        OpSpec::Pkg(p)
            if matches!(
                p.source,
                crate::ops::pkg::Source::Script(_) | crate::ops::pkg::Source::Cargo
            ) =>
        {
            let mode = match &p.wanted {
                Wanted::Present => "install".to_string(),
                Wanted::Pinned(v) => format!("pin {v}"),
                Wanted::Latest => "latest".to_string(),
            };
            format!("{} {mode} {}", p.kind(), p.names.join(" "))
        }
        OpSpec::Pkg(p) => {
            let mode = match (p.mode, &p.wanted) {
                (PkgMode::BuildDep, _) => "build-dep",
                (_, Wanted::Latest) => "latest",
                (_, Wanted::Pinned(_)) => "pinned",
                (PkgMode::Install, Wanted::Present) => "install",
            };
            format!(
                "pkg {} {mode} {}",
                prepare::manager_name(&p.source),
                p.names.join(" ")
            )
        }
        OpSpec::File(f) => {
            use crate::ops::file::FileKind;
            let what = match f.kind {
                FileKind::Dir => "dir",
                FileKind::Absent => "absent",
                FileKind::File if f.content.is_some() => "content",
                FileKind::File if !f.lines.is_empty() => "lines",
                FileKind::File => "attrs",
            };
            format!("file {} {what}", f.path)
        }
    }
}

fn roles_of(w: &WindowOp) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for m in &w.members {
        let r = if m.role.is_empty() {
            "(no role)".to_string()
        } else {
            m.role.clone()
        };
        if !v.contains(&r) {
            v.push(r);
        }
    }
    v
}

fn parse_result(kind: OpKind, t: &Trailer) -> Result<(i32, bool, bool), String> {
    use crate::trailer::{FileResult, PkgResult, ShellResult};
    let r = match kind {
        OpKind::Pkg => t.typed::<PkgResult>().map(|r| (r.rc, r.changed, false)),
        OpKind::File => t.typed::<FileResult>().map(|r| (r.rc, r.changed, false)),
        OpKind::Sh => t
            .typed::<ShellResult>()
            .map(|r| (r.rc, r.changed, r.skipped)),
    };
    r.map_err(|e| format!("unreadable result {:?}: {e}", t.json))
}

fn last_lines(bytes: &[u8], n: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

impl HostExec {
    fn new(name: &str, shell: Shell, sink: Rc<Sink>) -> HostExec {
        HostExec {
            name: name.to_string(),
            sink,
            shell,
            report: HostReport::new(name),
            halted: false,
            verdict: None,
            users_ok: BTreeSet::new(),
            seq: PROBE_BASE,
        }
    }

    fn block(&mut self, why: String) {
        self.halted = true;
        if self.verdict.is_none() {
            self.verdict = Some(HostStatus::Blocked(why));
        }
    }

    /// Print the header (once) and log `op_start`.
    fn header(&self, live: &mut Live, w: &WindowOp, desc: &str) {
        if live.header {
            return;
        }
        live.header = true;
        let roles = roles_of(w).join(", ");
        self.sink
            .line(&format!("== {} [{roles}] #{} {desc} ==", self.name, w.id.0));
        self.sink
            .event(json!({"ev":"op_start","host":self.name,"id":w.id.0,"role":roles,"desc":desc}));
    }

    fn output(&self, live: &mut Live, w: &WindowOp, bytes: &[u8]) {
        self.sink
            .event(json!({"ev":"output","host":self.name,"id":w.id.0,
            "data":String::from_utf8_lossy(bytes)}));
        live.partial.extend_from_slice(bytes);
        while let Some(i) = live.partial.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = live.partial.drain(..=i).collect();
            self.sink.line(&format!(
                "{} | {}",
                self.name,
                String::from_utf8_lossy(&line[..line.len() - 1])
            ));
        }
        live.tail.extend_from_slice(bytes);
        if live.tail.len() > TAIL_BYTES {
            let cut = live.tail.len() - TAIL_BYTES / 2;
            live.tail.drain(..cut);
        }
    }

    fn flush_partial(&self, live: &mut Live) {
        if !live.partial.is_empty() {
            self.sink.line(&format!(
                "{} | {}",
                self.name,
                String::from_utf8_lossy(&live.partial)
            ));
            live.partial.clear();
        }
    }

    /// Record `w`'s end. `result` is `None` for never-ran.
    fn finish_op(
        &mut self,
        w: &WindowOp,
        desc: &str,
        outcome: Outcome,
        rc: i32,
        changed: bool,
        note: &str,
    ) {
        let label = match outcome {
            Outcome::Changed => "changed".to_string(),
            Outcome::Unchanged if note.is_empty() => "ok".to_string(),
            Outcome::Unchanged => format!("ok ({note})"),
            Outcome::Failed => format!("FAILED rc={rc}"),
            Outcome::AllowedFail => format!("failed rc={rc} (allowed)"),
            Outcome::NotRun => "not run".to_string(),
        };
        self.sink.line(&format!("-> {label}"));
        self.sink
            .event(json!({"ev":"op_result","host":self.name,"id":w.id.0,
            "rc":rc,"changed":changed,"outcome":format!("{outcome:?}")}));
        self.report.ops.push(OpRecord {
            id: w.id.0,
            members: w.members.iter().map(|m| (m.id.0, m.role.clone())).collect(),
            desc: desc.to_string(),
            outcome,
            rc,
            changed,
        });
    }

    fn fail(&mut self, w: &WindowOp, desc: &str, rc: i32, tail: String) {
        self.halted = true;
        if self.verdict.is_none() {
            self.verdict = Some(HostStatus::Failed {
                role: roles_of(w).join(", "),
                op_id: w.id.0,
                op: desc.to_string(),
                rc,
                tail,
            });
        }
    }

    fn not_run(&mut self, w: &WindowOp) {
        let desc = describe(&w.node);
        let mut live = Live {
            header: false,
            partial: vec![],
            tail: vec![],
        };
        self.header(&mut live, w, &desc);
        self.finish_op(w, &desc, Outcome::NotRun, 0, false, "");
    }

    /// Verify the users `users` exist; `Err` blocks the host.
    async fn ensure_users(&mut self, users: &BTreeSet<String>) -> Result<(), String> {
        let new: Vec<&String> = users
            .iter()
            .filter(|u| !self.users_ok.contains(*u))
            .collect();
        if new.is_empty() {
            return Ok(());
        }
        self.seq += 1;
        let script = prepare::users_script(new.iter().copied());
        let (_, out) = prepare::probe(&mut self.shell, self.seq, &script).await?;
        let missing = prepare::missing_users(&out);
        if !missing.is_empty() {
            return Err(format!(
                "user {} does not exist on the host",
                missing.join(", ")
            ));
        }
        let ok: Vec<String> = new.into_iter().cloned().collect();
        self.users_ok.extend(ok);
        Ok(())
    }

    fn finish(mut self, res: Result<(), EngineError>) -> (HostReport, Shell) {
        let mut rep = std::mem::replace(&mut self.report, HostReport::new(&self.name));
        if let Err(e) = res
            && self.verdict.is_none()
        {
            self.verdict = Some(HostStatus::Error(e.to_string()));
        }
        rep.status = self.verdict.take().unwrap_or_else(|| {
            if rep.ops.iter().any(|o| o.changed) {
                HostStatus::Changed
            } else {
                HostStatus::Ok
            }
        });
        let failed: Vec<String> = match &rep.status {
            HostStatus::Failed { role, .. } => role.split(", ").map(str::to_string).collect(),
            _ => vec![],
        };
        for o in rep.ops.iter().filter(|o| o.outcome == Outcome::NotRun) {
            for (_, r) in &o.members {
                let r = if r.is_empty() { "(no role)" } else { r };
                if !failed.iter().any(|f| f == r) && !rep.skipped_roles.iter().any(|s| s == r) {
                    rep.skipped_roles.push(r.to_string());
                }
            }
        }
        (rep, self.shell)
    }
}

/// The skip trailer an op emits instead of running once the host halted.
fn skip_script(op: &OpNode, id: usize, nonce: &str) -> String {
    wrap(op.spec.op_kind(), "rc=0\nskipped=1\n", id, nonce)
}

impl Executor for HostExec {
    async fn read_facts(
        &mut self,
        host: &HostInfo,
        window: usize,
        queries: &[FactQuery],
    ) -> Vec<Value> {
        let default = |q: &FactQuery| match q.fact {
            crate::facts::query::Fact::BinVersion(_) => Value::Absent,
            _ => Value::Exists(false),
        };
        let users: BTreeSet<String> = queries
            .iter()
            .filter_map(|q| match &q.run_as.principal {
                Principal::User(u) => Some(u.clone()),
                Principal::Root => None,
            })
            .collect();
        if let Err(why) = self.ensure_users(&users).await {
            self.block(why);
            return queries.iter().map(default).collect();
        }
        // Render and send all probes, then collect.
        let mut ids = Vec::new();
        let mut sent_err = None;
        for q in queries {
            self.seq += 1;
            let node = OpNode {
                spec: OpSpec::Raw {
                    script: q.fact.probe(),
                },
                run_as: q.run_as.clone(),
                reads: Default::default(),
                writes: Default::default(),
                guard: None,
                post: Default::default(),
                allow_fail: false,
            };
            match self.shell.send_op(&node, self.seq, &host.os).await {
                Ok(()) => ids.push(self.seq),
                Err(e) => {
                    sent_err = Some(e.to_string());
                    break;
                }
            }
        }
        let mut values = Vec::new();
        for (q, id) in queries.iter().zip(&ids) {
            match prepare::collect(&mut self.shell, *id).await {
                Ok((_, out)) => values.push(q.fact.parse(&out)),
                Err(e) => {
                    sent_err = sent_err.or(Some(e));
                    values.push(default(q));
                }
            }
        }
        while values.len() < queries.len() {
            values.push(default(&queries[values.len()]));
        }
        self.sink.event(json!({"ev":"facts","host":self.name,"window":window,
            "answers": queries.iter().zip(&values).map(|(q, v)| format!("{q} = {v}")).collect::<Vec<_>>()}));
        if let Some(e) = sent_err {
            self.block(format!("reading facts: {e}"));
        }
        values
    }

    async fn apply(&mut self, host: &HostInfo, window: &Window) -> Result<(), String> {
        let kind = match window.kind {
            WindowKind::NonMutating => "non-mutating",
            WindowKind::ImplicitBarrier => "implicit barrier",
            WindowKind::Final => "final",
        };
        self.sink.event(
            json!({"ev":"window","host":self.name,"n":window.number,"kind":kind,
            "ops": window.ops.iter().map(|o| o.id.0).collect::<Vec<_>>()}),
        );
        if window.ops.is_empty() {
            return Ok(());
        }
        if self.halted {
            for w in &window.ops {
                self.not_run(w);
            }
            return Ok(());
        }

        // Prepare: nothing is sent to be applied until all of it passes.
        let mut scripts = Vec::new();
        for w in &window.ops {
            if w.node.guard.is_some() {
                let why = format!(
                    "op #{} has a guard (when); guards are not supported by the executor yet",
                    w.id.0
                );
                self.block(why.clone());
                return Err(why);
            }
            match crate::trailer::render_for(
                &w.node,
                w.id.0,
                self.shell.nonce(),
                &host.os,
                self.shell.user(),
            ) {
                Ok(s) => scripts.push(s),
                Err(e) => {
                    let why = format!("op #{}: {e}", w.id.0);
                    self.block(why.clone());
                    return Err(why);
                }
            }
        }
        let prepared = prepare::prepare_window(
            &mut self.shell,
            window,
            &host.os,
            &mut self.users_ok,
            &mut self.seq,
        )
        .await;
        match prepared {
            Ok(will) => {
                for w in will.will_install {
                    self.sink.line(&format!(
                        "== {} [prepare] window {} will install ({}): {} ==",
                        self.name,
                        w.window,
                        w.manager,
                        w.names.join(" ")
                    ));
                    self.sink.event(json!({"ev":"will_install","host":self.name,
                        "window":w.window,"manager":w.manager,"names":w.names}));
                    self.report.will_install.push(w);
                }
                for l in crate::report::WillChange::lines(&will.will_change) {
                    self.sink
                        .line(&format!("== {} [prepare] {l} ==", self.name));
                }
                for w in will.will_change {
                    self.sink.event(json!({"ev":"will_change","host":self.name,
                        "window":w.window,"source":w.source,"user":w.user,"name":w.name,
                        "installed":w.installed,"wanted":w.wanted}));
                    self.report.will_change.push(w);
                }
            }
            Err(why) => {
                self.block(why.clone());
                return Err(why);
            }
        }

        // Send everything, gated on the host's halt flag.
        let nonce = self.shell.nonce().to_string();
        let mut send_err = None;
        for (w, script) in window.ops.iter().zip(&scripts) {
            let set = if w.node.allow_fail {
                ""
            } else {
                "[ $? -eq 0 ] || __tp_halt=1\n"
            };
            let gated = format!(
                "if [ -z \"${{__tp_halt-}}\" ]; then\n{script}{set}else\n{}fi\n",
                skip_script(&w.node, w.id.0, &nonce)
            );
            if let Err(e) = self.shell.send(w.id.0, &gated).await {
                send_err = Some(e.to_string());
                break;
            }
        }

        // Collect in order.
        let index: HashMap<usize, usize> = window
            .ops
            .iter()
            .enumerate()
            .map(|(i, w)| (w.id.0, i))
            .collect();
        let descs: Vec<String> = window.ops.iter().map(|w| describe(&w.node)).collect();
        let mut live: Vec<Live> = window
            .ops
            .iter()
            .map(|_| Live {
                header: false,
                partial: vec![],
                tail: vec![],
            })
            .collect();
        let mut done = vec![false; window.ops.len()];
        let mut failed_seen = false;
        while let Some(ev) = self.shell.next_event().await {
            match ev {
                Event::Output(id, bytes) => {
                    let Some(&i) = index.get(&id) else { continue };
                    let w = &window.ops[i];
                    self.header(&mut live[i], w, &descs[i]);
                    self.output(&mut live[i], w, &bytes);
                }
                Event::Done(id, trailer) => {
                    let Some(&i) = index.get(&id) else { continue };
                    let w = &window.ops[i];
                    self.header(&mut live[i], w, &descs[i]);
                    self.flush_partial(&mut live[i]);
                    done[i] = true;
                    if failed_seen {
                        self.finish_op(w, &descs[i], Outcome::NotRun, 0, false, "");
                        continue;
                    }
                    match parse_result(w.node.spec.op_kind(), &trailer) {
                        Ok((0, changed, skipped)) => {
                            let o = if changed {
                                Outcome::Changed
                            } else {
                                Outcome::Unchanged
                            };
                            let note = if skipped { "skipped" } else { "" };
                            self.finish_op(w, &descs[i], o, 0, changed, note);
                        }
                        Ok((rc, _, _)) if w.node.allow_fail => {
                            self.finish_op(w, &descs[i], Outcome::AllowedFail, rc, false, "");
                        }
                        Ok((rc, _, _)) => {
                            failed_seen = true;
                            let tail = last_lines(&live[i].tail, TAIL_LINES);
                            self.finish_op(w, &descs[i], Outcome::Failed, rc, false, "");
                            self.fail(w, &descs[i], rc, tail);
                        }
                        Err(msg) => {
                            failed_seen = true;
                            self.finish_op(w, &descs[i], Outcome::Failed, -1, false, "");
                            self.fail(w, &descs[i], -1, msg);
                        }
                    }
                }
                Event::Failed(id, err) => {
                    let Some(&i) = index.get(&id) else { continue };
                    let w = &window.ops[i];
                    self.header(&mut live[i], w, &descs[i]);
                    self.flush_partial(&mut live[i]);
                    done[i] = true;
                    if failed_seen {
                        self.finish_op(w, &descs[i], Outcome::NotRun, 0, false, "");
                    } else {
                        failed_seen = true;
                        let mut tail = last_lines(&live[i].tail, TAIL_LINES);
                        if !tail.is_empty() {
                            tail.push('\n');
                        }
                        tail.push_str(&err.to_string());
                        self.finish_op(w, &descs[i], Outcome::Failed, -1, false, "");
                        self.fail(w, &descs[i], -1, tail);
                    }
                }
            }
        }
        // Ops with no result: never sent, or lost with the shell.
        for (i, w) in window.ops.iter().enumerate() {
            if done[i] {
                continue;
            }
            if failed_seen {
                self.not_run(w);
            } else {
                failed_seen = true;
                let why = send_err
                    .clone()
                    .unwrap_or_else(|| "shell ended without a result".to_string());
                let mut l = Live {
                    header: false,
                    partial: vec![],
                    tail: vec![],
                };
                self.header(&mut l, w, &descs[i]);
                self.finish_op(w, &descs[i], Outcome::Failed, -1, false, "");
                self.fail(w, &descs[i], -1, why);
            }
        }
        Ok(())
    }
}

/// Run one host to completion.
async fn run_one<V: 'static>(
    pb_name: &'static str,
    pb_run: fn(Host<V>) -> crate::role::PlaybookFuture,
    spec: HostSpec<V>,
    sink: Rc<Sink>,
) -> HostReport {
    let name = spec.name.clone();
    sink.event(json!({"ev":"host_open","host":name}));
    let blocked = |why: String| {
        let mut r = HostReport::new(&name);
        r.status = HostStatus::Blocked(why);
        sink.event(
            json!({"ev":"host_status","host":name,"status":"blocked","detail":r.status_detail()}),
        );
        r
    };
    let mut shells = spec.target.open(&name, &Principal::Root, 1).await;
    if shells.got.is_empty() {
        let why = match shells.limit {
            Some(Limit::SudoRefused) => {
                "sudo refused: `sudo -n` needs passwordless sudo (or run as root)".to_string()
            }
            Some(l) => format!("could not open a root shell: {l:?}"),
            None => "could not open a root shell".to_string(),
        };
        return blocked(why);
    }
    let mut shell = shells.got.remove(0);
    let os = match read_os(&mut shell).await {
        Ok(os) => os,
        Err(e) => {
            shell.close().await;
            return blocked(format!("reading os: {e}"));
        }
    };
    let host = Host::new(&name, os, Rc::new(spec.vars));
    let pb = Playbook {
        name: pb_name,
        run: pb_run,
    };
    let mut engine = Engine::new(HostExec::new(&name, shell, sink.clone()));
    let res = engine.run_host(&pb, host).await;
    let (rep, shell) = engine.executor.finish(res);
    shell.close().await;
    sink.event(json!({"ev":"host_status","host":name,"status":rep.status.label(),"detail":rep.status_detail()}));
    rep
}

impl HostReport {
    fn status_detail(&self) -> String {
        match &self.status {
            HostStatus::Failed { role, op, rc, .. } => format!("role {role}: {op} (rc {rc})"),
            HostStatus::Blocked(w) | HostStatus::Error(w) => w.clone(),
            _ => String::new(),
        }
    }
}

/// Run `playbook` on every host concurrently and return the report. Builds
/// its own current-thread runtime, so call it from plain (non-async) code.
pub fn run<V: 'static>(playbook: &Playbook<V>, hosts: Vec<HostSpec<V>>, opts: RunOpts) -> Report {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("building the tokio runtime");
    let log = opts
        .log
        .as_ref()
        .and_then(|p| match std::fs::File::create(p) {
            Ok(f) => Some(RefCell::new(std::io::BufWriter::new(f))),
            Err(e) => {
                eprintln!("telepatch: cannot open log {}: {e}", p.display());
                None
            }
        });
    let sink = Rc::new(Sink {
        stdout: opts.stdout,
        log,
    });
    let names: Vec<String> = hosts.iter().map(|h| h.name.clone()).collect();
    let (pb_name, pb_run) = (playbook.name, playbook.run);
    let local = tokio::task::LocalSet::new();
    let reports = rt.block_on(local.run_until(async {
        let tasks: Vec<_> = hosts
            .into_iter()
            .map(|h| tokio::task::spawn_local(run_one(pb_name, pb_run, h, sink.clone())))
            .collect();
        let mut out = Vec::new();
        for (t, name) in tasks.into_iter().zip(&names) {
            out.push(t.await.unwrap_or_else(|e| {
                let mut r = HostReport::new(name);
                r.status = HostStatus::Error(format!("host task panicked: {e}"));
                r
            }));
        }
        out
    }));
    Report { hosts: reports }
}
