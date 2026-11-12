//! Author-defined resources. The shell lives in the resource, quoted by
//! `cmd!`. The probe runs in prepare; the plan turns it into a prepare
//! line; the apply runs only when the plan wants a change. Resources never
//! merge. An inverse is recorded for a later undo and is not run here;
//! without one the op counts as irreversible.

use std::fmt;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use telepatch_macros::cmd;

use crate::engine::Window;
use crate::host::{Host, OpHandle};
use crate::key::Key;
use crate::sh::quote;
use crate::trace::{NodeId, OpSpec, Principal};

/// What a resource's probe found. `stdout` is `None` when the command was
/// missing (`missing_is_none`) or was not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probed {
    stdout: Option<String>,
}

impl Probed {
    pub fn stdout(&self) -> Option<&str> {
        self.stdout.as_deref()
    }
}

/// A probe command. `missing_is_none` turns a non-zero exit into
/// [`Probed`] with no stdout instead of blocking the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    script: Script,
    missing_is_none: bool,
}

impl Probe {
    pub fn stdout(cmd: Script) -> Probe {
        Probe {
            script: cmd,
            missing_is_none: false,
        }
    }

    pub fn missing_is_none(mut self) -> Probe {
        self.missing_is_none = true;
        self
    }
}

/// What prepare should do with a resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The host already matches. The apply is not run.
    None,
    /// The apply will run. The string is the verb in the prepare line
    /// (`will compile`).
    Will(String),
}

impl Change {
    pub fn will(verb: impl Into<String>) -> Change {
        Change::Will(verb.into())
    }
}

/// A shell command, already quoted, with optional stdin. Stdin is embedded
/// in the script: the trailer closes the shell's stdin, which is the op
/// queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    body: String,
    stdin: Option<String>,
}

impl Script {
    pub fn new(body: impl Into<String>) -> Script {
        Script {
            body: body.into(),
            stdin: None,
        }
    }

    pub fn stdin(mut self, data: impl Into<String>) -> Script {
        self.stdin = Some(data.into());
        self
    }

    /// The command text, before stdin is attached.
    pub fn body(&self) -> &str {
        &self.body
    }

    /// The script the shell runs.
    pub fn render(&self) -> String {
        let Some(data) = &self.stdin else {
            return self.body.clone();
        };
        // One pipeline. Without `pipefail` the status is the last command's,
        // which is the apply (`tic`), not `base64`.
        format!(
            "tp_b={}\nprintf %s \"$tp_b\" | base64 -d | {}\n",
            quote(&STANDARD.encode(data.as_bytes())),
            self.body
        )
    }
}

/// An author-defined op. `KIND` is the word in the prepare line. `inverse`
/// defaults to none, which the run records as irreversible.
pub trait Resource {
    const KIND: &'static str;

    fn key(&self) -> Key;
    fn probe(&self) -> Probe;
    fn plan(&self, now: &Probed) -> Change;
    fn apply(&self) -> Script;

    fn inverse(&self) -> Option<Script> {
        None
    }
}

/// Run `r.plan` against a recorded probe. No host.
pub fn plan_against<R: Resource>(r: &R, stdout: Option<&str>) -> Change {
    r.plan(&Probed {
        stdout: stdout.map(str::to_string),
    })
}

impl<V> Host<V> {
    /// Trace `r` as one op. The plan closure is kept for prepare and is
    /// not part of the op's `Eq` or `Debug`.
    #[track_caller]
    pub fn resource<R: Resource + 'static>(&self, r: R) -> OpHandle {
        let key = r.key();
        let name = match &key {
            Key::Custom { name, .. } => name.clone(),
            other => other.to_string(),
        };
        let probe = r.probe();
        let spec = ResourceSpec {
            kind: R::KIND,
            name,
            key,
            probe: probe.script.render(),
            missing_is_none: probe.missing_is_none,
            apply: r.apply().render(),
            inverse: r.inverse().map(|s| s.render()),
            run: true,
            plan: Arc::new(move |now: &Probed| r.plan(now)),
        };
        self.op(OpSpec::Resource(spec))
    }
}

/// A traced resource. `Eq` and `Debug` see the kind, the key and the
/// rendered scripts, not the plan closure and not whether prepare decided
/// to skip the apply (`run`).
pub struct ResourceSpec {
    pub(crate) kind: &'static str,
    pub(crate) name: String,
    pub(crate) key: Key,
    pub(crate) probe: String,
    pub(crate) missing_is_none: bool,
    pub(crate) apply: String,
    pub(crate) inverse: Option<String>,
    pub(crate) run: bool,
    plan: Arc<dyn Fn(&Probed) -> Change>,
}

impl Clone for ResourceSpec {
    fn clone(&self) -> Self {
        ResourceSpec {
            kind: self.kind,
            name: self.name.clone(),
            key: self.key.clone(),
            probe: self.probe.clone(),
            missing_is_none: self.missing_is_none,
            apply: self.apply.clone(),
            inverse: self.inverse.clone(),
            run: self.run,
            plan: Arc::clone(&self.plan),
        }
    }
}

impl PartialEq for ResourceSpec {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.name == other.name
            && self.key == other.key
            && self.probe == other.probe
            && self.missing_is_none == other.missing_is_none
            && self.apply == other.apply
            && self.inverse == other.inverse
    }
}

impl Eq for ResourceSpec {}

impl fmt::Debug for ResourceSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Resource")
            .field("kind", &self.kind)
            .field("key", &self.key)
            .field("probe", &self.probe)
            .field("missing_is_none", &self.missing_is_none)
            .field("apply", &self.apply)
            .field("inverse", &self.inverse)
            .finish()
    }
}

impl ResourceSpec {
    /// The body the trailer wraps. A skipped resource is `:` (rc 0,
    /// unchanged). An apply that exits 0 sets `changed`.
    pub(crate) fn exec_body(&self) -> String {
        if !self.run {
            return ":\n".to_string();
        }
        let mut s = self.apply.clone();
        if !s.ends_with('\n') {
            s.push('\n');
        }
        s.push_str("rc=$?\nif [ \"$rc\" -eq 0 ]; then changed=1; fi\n");
        s
    }
}

/// A terminfo entry compiled into the system database with `tic`.
/// `infocmp` reflows the entry and prefixes a comment, so a match ignores
/// `#` lines and whitespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Terminfo {
    pub name: String,
    pub source: String,
}

impl Resource for Terminfo {
    const KIND: &'static str = "terminfo";

    fn key(&self) -> Key {
        Key::custom(Self::KIND, &self.name)
    }

    fn probe(&self) -> Probe {
        Probe::stdout(cmd!("infocmp -x {}", &self.name)).missing_is_none()
    }

    fn plan(&self, now: &Probed) -> Change {
        match now.stdout() {
            Some(text) if same_entry(text, &self.source) => Change::None,
            Some(_) => Change::will("replace"),
            None => Change::will("compile"),
        }
    }

    fn apply(&self) -> Script {
        cmd!("tic -x -").stdin(&self.source)
    }
}

fn same_entry(probed: &str, source: &str) -> bool {
    if probed.trim() == source.trim() {
        return true;
    }
    let a = entry_norm(probed);
    let b = entry_norm(source);
    !a.is_empty() && a == b
}

/// Drop `#` comment lines and every whitespace character.
fn entry_norm(s: &str) -> String {
    s.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .flat_map(|l| l.chars())
        .filter(|c| !c.is_whitespace())
        .collect()
}

pub(crate) struct ProbeJob {
    pub id: NodeId,
    pub who: Principal,
    pub script: String,
    pub kind: &'static str,
    pub name: String,
}

pub(crate) fn jobs(window: &Window) -> Vec<ProbeJob> {
    window
        .ops
        .iter()
        .filter_map(|op| {
            let OpSpec::Resource(r) = &op.node.spec else {
                return None;
            };
            Some(ProbeJob {
                id: op.id,
                who: op.node.run_as.principal.clone(),
                script: capture_script(&r.probe),
                kind: r.kind,
                name: r.name.clone(),
            })
        })
        .collect()
}

/// Stdout of `cmd`, length-prefixed, with stderr discarded. The wrapper
/// itself exits 0; the command's status is the first line (`fail` when
/// `mktemp` did not run).
fn capture_script(cmd: &str) -> String {
    let mut s = String::from("tp_f=$(mktemp 2>/dev/null) || tp_f=\n");
    s.push_str("if [ -z \"$tp_f\" ]; then\nprintf '%s\\n' fail\nelse\n{ ");
    s.push_str(cmd);
    if !cmd.ends_with('\n') {
        s.push('\n');
    }
    s.push_str("} >\"$tp_f\" 2>/dev/null\n");
    s.push_str("tp_rc=$?\nprintf '%s\\n' \"$tp_rc\"\n");
    s.push_str("wc -c < \"$tp_f\" | tr -d '[:space:]'\nprintf '\\n'\n");
    s.push_str("cat \"$tp_f\"\nrm -f -- \"$tp_f\"\nfi\n");
    s
}

enum Captured {
    Fail,
    Out { rc: i32, stdout: String },
}

fn parse_capture(text: &str) -> Captured {
    let Some((rc_line, rest)) = text.split_once('\n') else {
        return Captured::Fail;
    };
    if rc_line.trim() == "fail" {
        return Captured::Fail;
    }
    let Ok(rc) = rc_line.trim().parse::<i32>() else {
        return Captured::Fail;
    };
    let Some((len_line, rest)) = rest.split_once('\n') else {
        return Captured::Fail;
    };
    let Ok(len) = len_line.trim().parse::<usize>() else {
        return Captured::Fail;
    };
    let bytes = rest.as_bytes();
    let take = len.min(bytes.len());
    Captured::Out {
        rc,
        stdout: String::from_utf8_lossy(&bytes[..take]).into_owned(),
    }
}

pub(crate) enum Decision {
    Skip,
    Will(String),
}

pub(crate) fn decide(spec: &ResourceSpec, out: &str) -> Result<Decision, String> {
    let probed = match parse_capture(out) {
        Captured::Fail => {
            return Err(format!(
                "resource {} {}: probe output was unreadable",
                spec.kind, spec.name
            ));
        }
        Captured::Out { rc, stdout } => {
            if rc != 0 {
                if spec.missing_is_none {
                    Probed { stdout: None }
                } else {
                    return Err(format!(
                        "resource {} {}: probe failed (rc {rc})",
                        spec.kind, spec.name
                    ));
                }
            } else {
                Probed {
                    stdout: Some(stdout),
                }
            }
        }
    };
    Ok(match (spec.plan)(&probed) {
        Change::None => Decision::Skip,
        Change::Will(verb) => Decision::Will(format!("{} {}: will {verb}", spec.kind, spec.name)),
    })
}

/// Prepare decided these resources are already in the wanted state.
pub(crate) fn mark_skipped(window: &mut Window, ids: &[NodeId]) {
    for op in &mut window.ops {
        if !ids.contains(&op.id) {
            continue;
        }
        if let OpSpec::Resource(r) = &mut op.node.spec {
            r.run = false;
        }
    }
}
