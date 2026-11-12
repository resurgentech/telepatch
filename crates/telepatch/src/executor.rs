//! The real executor: runs an optimized window on a host's root shell.
//!
//! One [`HostExec`] per host, one engine future per host, all joined on a
//! current-thread runtime with a `LocalSet` ([`run`]). Per host:
//!
//! 1. open the root shell (`want` 1) and read `os` before the playbook
//!    future starts ([`crate::transport::read_os`]);
//! 2. the engine drives the playbook; [`Executor::read_facts`] sends the
//!    fact probes in one batch, each to its principal's shell;
//!    [`Executor::apply`] runs prepare ([`crate::prepare`]), then pipelines
//!    the window's ops and collects results in order, streaming output
//!    live;
//! 3. at the end of the host's run (also when it is blocked or failed) the
//!    user shells close, then the root shell, then the transport's
//!    connections (SSH).
//!
//! # Principals
//!
//! The host keeps the root shell plus one shell per user, opened the first
//! time a window's ops or a fact read address that user (after the
//! `getent` check; on SSH `sudo -n -iu u bash`, a refusal blocks the host).
//! An op goes to its `run_as.principal`'s shell: `Root` (including
//! `as_user("root")`) to the root shell, `User(u)` to `u`'s. Local and
//! docker open no user shells: a window that needs one is blocked with the
//! transport's limit. Consecutive ops of one principal form a run; a run
//! is pipelined and collected in full before the next run is sent, so a
//! change of principal waits for the previous shell (every switch is
//! treated as a dependency edge).
//!
//! # Failure gating
//!
//! Ops are pipelined, so by the time op N fails, N+1.. are already written
//! to the shell. Each op is therefore sent as
//! `if [ -z "${__tp_halt-}" ]; then <op>; [ $? -eq 0 ] || __tp_halt=1;
//! else <skip trailer>; fi`. `__tp_halt` lives in the persistent shell and
//! is never reset: after a failure no later op on that shell runs, and the
//! executor sends nothing more on any of the host's shells. An
//! `allow_fail` op never sets it. After a failure the executor stops
//! sending windows but lets the engine finish tracing, so the report can
//! list every skipped role; fact reads still run (they only read).
//!
//! # Guards
//!
//! A guarded op (`.when(g)`) is sent inside the halt gate as `if <test>;
//! then <op>; else <guard-skip trailer>; fi`; the shell decides, the
//! executor never waits for it. In `<test>` each `changed(#n)` / `ok(#n)`
//! of the guard is either
//!
//! - a shell variable, `__tp_c_<n>` / `__tp_ok_<n>` (`1` or `0`), when the
//!   source op is earlier in the same run (same principal's shell, not yet
//!   collected). Such a source is sent with `__tp_f=$(mktemp)` set: its
//!   trailer also writes `changed` and the `installed` names to that file,
//!   and the parent shell, after the op, reads it back, removes it and sets
//!   the variables beside `__tp_halt` (for every member of a merged op;
//!   for a merged `pkg` op a member's `changed` is whether one of its own
//!   names is in `installed`). `ok` is the op's exit status being 0.
//! - a constant, `true` / `false`, when the source was already reported
//!   (an earlier run, another principal, an earlier window): runs are
//!   collected in full before the next is sent, so the outcome is known.
//!
//! A failed source (an `allow_fail` op) is neither `ok` nor `changed`; a
//! source skipped by its own guard is `ok`, not `changed`; one never run is
//! neither. The guard-skip trailer is rc 0 with `"when":false`, reported as
//! [`Outcome::Skipped`]: never changed, never failed.
//!
//! # Restart and reload
//!
//! A `svc` `.restart()` / `.reload()` of a unit that an op earlier in the
//! run wants `.started()` (any window, merged or not) is sent with "that
//! op started the unit" and-not-ed into its guard; the report prints it as
//! `g & !started(#start)`. A start op started the unit when the unit was
//! not active and its `systemctl start` succeeded (the trailer's
//! `started`, [`crate::trailer::SvcResult`]); the start then already runs
//! the new configuration. A start op that only enabled or unmasked a unit
//! that was running is `changed` but did not start it, so the restart
//! still runs. The start is a guard source like any other: its trailer
//! writes `started` as a second line, the parent shell sets
//! `__tp_st_<n>` beside `__tp_c_<n>` (a variable in the same run), and a
//! reported start is a constant. Prepare prints one line per action, from
//! its own guard with the ops named: `svc docker.service: will restart if pkg
//! nvidia-container-toolkit changes`; `will restart, every run` for an
//! allowed unguarded one; `will restart` when every source is reported and
//! the guard holds; nothing when it is already false.
//!
//! # Units and daemon-reload
//!
//! A `unit` op (`h.unit`) is always sent as a guard source, so its
//! `changed` reaches the parent shell; when it changed, the root shell's
//! window flag `__tp_dr` is set to `1` (beside `__tp_halt`). Every `svc` op
//! after a unit op in the same window is rendered with
//! [`crate::ops::svc::SvcSpec::render_exec_reload`]: with the flag set and
//! systemd running it runs `systemctl daemon-reload` first, and after the
//! op ran (not when its guard skipped it) the parent shell clears the flag.
//! So any number of changed units cost one reload, before the next `svc`
//! op in the window (unit, unit, svc: one reload, before the svc op).
//!
//! The flag never outlives its window. When a unit changed and no `svc` op
//! ran after it in the window (none follows, or its guard skipped it), the
//! executor ends the window with the flush: one more op on the root shell,
//! in the halt gate, that runs `systemctl daemon-reload` when the flag is
//! set and systemd runs, then clears the flag. A reload that ran, wherever
//! it ran, is one stream line (`   systemd daemon-reload: <reason>`), one
//! `daemon_reload` event (`reason`, `op`) and one
//! [`crate::report::DaemonReload`]. It is not an op, so it is not counted
//! in OK or CHANGED. A skipped one prints nothing. Prepare still announces
//! a flush it can see (`systemd daemon-reload: at the end of the window,
//! after unit x.service`). A failed flush fails the host like a failed op:
//! its rc, systemctl's message as the tail, the unit's role, and the stream
//! keeps `-> FAILED`.
//! A unit changed in one window with its `svc` op in a later one is
//! reloaded by the first window's flush. No unit changed, or no running
//! systemd, no reload. `svc` ops before any unit op in their window get the
//! scripts unchanged; there the per-op `NeedDaemonReload` check is all
//! there is.
//!
//! # Stream
//!
//! Each op prints `== host [role] #id desc ==`, its output lines prefixed
//! `host | `, then `-> result`. No timing is printed. With
//! [`RunOpts::log`] a JSONL file gets the events `host_open`, `window`,
//! `op_start`, `output` (lossy UTF-8 text per chunk), `op_result`,
//! `facts`, `host_status`.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write as _;
use std::path::PathBuf;
use std::rc::Rc;

use serde_json::json;

use crate::engine::{
    Applied, Engine, EngineError, Executor, HostInfo, Split, Window, WindowKind, WindowOp,
};
use crate::facts::query::FactQuery;
use crate::host::Host;
use crate::key::Value;
use crate::ops::pkg::{PkgMode, Wanted};
use crate::pool::{Event, Shell};
use crate::prepare;
use crate::report::{HostReport, HostStatus, OpRecord, Outcome, Report};
use crate::role::Playbook;
use crate::trace::{Guard, NodeId, OpNode, OpSpec, Principal};
use crate::trailer::{OpKind, Trailer, wrap};
use crate::transport::docker::DockerTransport;
use crate::transport::local::LocalTransport;
use crate::transport::ssh::SshTransport;
use crate::transport::{Limit, Shells, Transport, read_os};

/// Where to reach a host.
pub enum Target {
    Local(LocalTransport),
    Docker(DockerTransport),
    Ssh(SshTransport),
}

impl Transport for Target {
    async fn open(&self, host: &str, principal: &Principal, want: usize) -> Shells {
        match self {
            Target::Local(t) => t.open(host, principal, want).await,
            Target::Docker(t) => t.open(host, principal, want).await,
            Target::Ssh(t) => t.open(host, principal, want).await,
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

/// A host's open shells: the root shell, one per user opened so far, and
/// the target that opens more.
struct HostShells {
    host: String,
    target: Rc<Target>,
    root: Shell,
    users: BTreeMap<String, Shell>,
}

impl HostShells {
    /// Close the user shells, then the root shell, then the transport's
    /// connections (SSH).
    async fn close(self) {
        for (_, sh) in self.users {
            sh.close().await;
        }
        self.root.close().await;
        close_target(&self.target).await;
    }
}

async fn close_target(t: &Target) {
    if let Target::Ssh(t) = t {
        t.close().await;
    }
}

/// Why `target` could not open `who`'s shell on `host`, from the `Limit`
/// and (SSH) the ssh-side text.
fn refusal(target: &Target, host: &str, who: &Principal, limit: Option<Limit>) -> String {
    let msg = match target {
        Target::Ssh(t) => t.message(host),
        _ => None,
    };
    let with = |s: String| match &msg {
        Some(m) if !m.is_empty() => format!("{s}: {m}"),
        _ => s,
    };
    match (who, target, limit) {
        (Principal::User(u), Target::Ssh(_), Some(Limit::SudoRefused)) => {
            with(format!("sudo -n -iu {u} refused"))
        }
        (Principal::User(_), _, Some(Limit::Other(m))) => m,
        (Principal::User(u), _, l) => with(format!("could not open a shell for user {u}: {l:?}")),
        (Principal::Root, Target::Ssh(_), Some(Limit::ConnectRefused)) => {
            with("ssh could not connect".to_string())
        }
        (Principal::Root, Target::Ssh(_), Some(Limit::AuthFailed)) => {
            with("ssh authentication failed (BatchMode: key or agent only)".to_string())
        }
        (Principal::Root, Target::Ssh(_), Some(Limit::SudoRefused)) => with(
            "sudo refused: `sudo -n` needs passwordless sudo for the ssh user (or log in as root)"
                .to_string(),
        ),
        (Principal::Root, Target::Ssh(_), Some(Limit::Other(m))) => {
            format!("could not open a root shell: {m}")
        }
        (Principal::Root, Target::Ssh(_), Some(l)) => {
            with(format!("could not open a root shell: {l:?}"))
        }
        (Principal::Root, _, Some(Limit::SudoRefused)) => {
            "sudo refused: `sudo -n` needs passwordless sudo (or run as root)".to_string()
        }
        (Principal::Root, _, Some(l)) => format!("could not open a root shell: {l:?}"),
        (Principal::Root, _, None) => "could not open a root shell".to_string(),
    }
}

impl prepare::ShellSet for HostShells {
    fn shell(&mut self, who: &Principal) -> Option<&mut Shell> {
        match who {
            Principal::Root => Some(&mut self.root),
            Principal::User(u) => self.users.get_mut(u),
        }
    }

    async fn open_user(&mut self, u: &str) -> Result<(), String> {
        if self.users.contains_key(u) {
            return Ok(());
        }
        let who = Principal::User(u.to_string());
        let mut s = self.target.open(&self.host, &who, 1).await;
        if s.got.is_empty() {
            return Err(refusal(&self.target, &self.host, &who, s.limit));
        }
        let mut sh = s.got.remove(0);
        // More than asked for is not expected; close the rest.
        for extra in s.got {
            extra.close().await;
        }
        if let Err(e) = init_shell(&mut sh).await {
            sh.close().await;
            return Err(format!("could not set up the shell for user {u}: {e}"));
        }
        self.users.insert(u.to_string(), sh);
        Ok(())
    }
}

pub struct HostExec {
    name: String,
    sink: Rc<Sink>,
    shells: HostShells,
    report: HostReport,
    /// Failed or blocked: nothing more runs on this host.
    halted: bool,
    verdict: Option<HostStatus>,
    users_ok: BTreeSet<String>,
    seq: usize,
    /// The reported outcome of every trace op so far, by member id: the
    /// constants guards in later runs and windows are evaluated with.
    known: HashMap<NodeId, Known>,
    /// What every op seen so far is, by member id (`pkg docker-ce`, `file
    /// /etc/x`), for the prepare line of a restart.
    subjects: HashMap<NodeId, String>,
    /// The `svc` ops seen so far that want `.started()`: unit key, op id.
    starts: Vec<(String, NodeId)>,
    /// Per restart or reload (by op id): the earlier start ops of its unit.
    /// It is dropped when one of them started the unit ("Restart and
    /// reload").
    suppress: HashMap<NodeId, Vec<NodeId>>,
    /// A window had a fetch, so the run's staging directory may exist on
    /// the host: it is removed when the host is done.
    fetch_staged: bool,
}

/// What a guard can know about a reported op.
#[derive(Debug, Clone, Copy, Default)]
struct Known {
    ok: bool,
    changed: bool,
    /// A `svc` op that started its unit (the trailer's `started`).
    started: bool,
}

/// Does a member of `w` change only when one of its own names was
/// installed? For a merged `pkg` op whose `installed` list is known (not
/// `build-dep`).
fn member_rule(w: &WindowOp) -> bool {
    matches!(&w.node.spec, OpSpec::Pkg(p) if p.mode != PkgMode::BuildDep) && w.members.len() > 1
}

/// Was `name` (a member's own) installed, per an `installed` entry `got`?
/// pip pins are reported as `name==version`.
fn installed_as(got: &str, name: &str) -> bool {
    got == name
        || got
            .strip_prefix(name)
            .is_some_and(|rest| rest.starts_with("=="))
}

/// The shell test for `g`: constants for reported sources, variables for
/// sources earlier in this run (`in_run`), `false` for anything else (a
/// trace error prevents that).
fn guard_test(g: &Guard, known: &HashMap<NodeId, Known>, in_run: &BTreeSet<NodeId>) -> String {
    let atom = |n: &NodeId, var: &str, pick: fn(&Known) -> bool| match known.get(n) {
        Some(k) if pick(k) => "true".to_string(),
        Some(_) => "false".to_string(),
        None if in_run.contains(n) => format!("[ \"${{__tp_{var}_{}-0}}\" = 1 ]", n.0),
        None => "false".to_string(),
    };
    match g {
        Guard::Changed(n) => atom(n, "c", |k| k.changed),
        Guard::ExitOk(n) => atom(n, "ok", |k| k.ok),
        Guard::Not(g) => format!("! {{ {}; }}", guard_test(g, known, in_run)),
        Guard::Or(a, b) => format!(
            "{{ {} || {}; }}",
            guard_test(a, known, in_run),
            guard_test(b, known, in_run)
        ),
        Guard::And(a, b) => format!(
            "{{ {} && {}; }}",
            guard_test(a, known, in_run),
            guard_test(b, known, in_run)
        ),
    }
}

/// The shell test "one of `starts` started its unit": a constant for a
/// reported start, `__tp_st_<n>` for one earlier in this run, `false`
/// otherwise.
fn started_test(
    starts: &[NodeId],
    known: &HashMap<NodeId, Known>,
    in_run: &BTreeSet<NodeId>,
) -> String {
    let atoms: Vec<String> = starts
        .iter()
        .map(|n| match known.get(n) {
            Some(k) if k.started => "true".to_string(),
            Some(_) => "false".to_string(),
            None if in_run.contains(n) => format!("[ \"${{__tp_st_{}-0}}\" = 1 ]", n.0),
            None => "false".to_string(),
        })
        .collect();
    format!("{{ {}; }}", atoms.join(" || "))
}

/// The test `w` is sent with: its own guard, and-not "an earlier start
/// started the unit" for a restart or reload with `starts`; `None` when
/// it has neither.
fn sent_test(
    w: &WindowOp,
    starts: &[NodeId],
    known: &HashMap<NodeId, Known>,
    in_run: &BTreeSet<NodeId>,
) -> Option<String> {
    let own = w.node.guard.as_ref().map(|g| guard_test(g, known, in_run));
    if starts.is_empty() {
        return own;
    }
    let st = started_test(starts, known, in_run);
    Some(match own {
        Some(g) => format!("{{ {g}; }} && ! {st}"),
        None => format!("! {st}"),
    })
}

/// `w`'s guard as the report prints it: its own, then `& !started(#n)`
/// for each earlier start of its unit (`!(started(#a) | started(#b))`).
fn when_text(w: &WindowOp, starts: &[NodeId]) -> Option<String> {
    let st = match starts {
        [] => None,
        [n] => Some(format!("!started({n})")),
        many => Some(format!(
            "!({})",
            many.iter()
                .map(|n| format!("started({n})"))
                .collect::<Vec<_>>()
                .join(" | ")
        )),
    };
    let own = w.node.guard.as_ref().map(|g| match g {
        Guard::Or(..) if st.is_some() => format!("({g})"),
        _ => g.to_string(),
    });
    match (own, st) {
        (Some(g), Some(s)) => Some(format!("{g} & {s}")),
        (g, s) => g.or(s),
    }
}

/// `__tp_c_<m>=<c>; __tp_ok_<m>=<ok>` for every member of `w` (and
/// `__tp_st_<m>=0` for a `svc` op).
fn assign(w: &WindowOp, c: u8, ok: u8) -> String {
    let svc = matches!(w.node.spec, OpSpec::Svc(_));
    w.members
        .iter()
        .map(|m| {
            let st = if svc {
                format!("; __tp_st_{}=0", m.id.0)
            } else {
                String::new()
            };
            format!("__tp_c_{0}={c}; __tp_ok_{0}={ok}{st}\n", m.id.0)
        })
        .collect()
}

/// After a guard source ran (its exit status in `__tp_rc`): read what its
/// trailer wrote to `$__tp_f`, remove the file, set every member's
/// variables.
fn source_vars(w: &WindowOp) -> String {
    let mut o = String::from(
        "__tp_x=; __tp_inst=()\n\
         if [ -n \"$__tp_f\" ]; then { IFS= read -r __tp_x; mapfile -t __tp_inst; } < \"$__tp_f\"; rm -f \"$__tp_f\"; fi\n\
         __tp_f=\n",
    );
    o.push_str(&assign(w, 0, 0));
    o.push_str("if [ \"$__tp_rc\" -eq 0 ]; then\n");
    for m in &w.members {
        let id = m.id.0;
        let _ = std::fmt::Write::write_fmt(&mut o, format_args!("  __tp_ok_{id}=1\n"));
        if matches!(w.node.spec, OpSpec::Svc(_)) {
            // a svc op's second line is `started`
            let _ = std::fmt::Write::write_fmt(
                &mut o,
                format_args!("  [ \"${{__tp_inst[0]-}}\" != true ] || __tp_st_{id}=1\n"),
            );
        }
        if !member_rule(w) {
            let _ = std::fmt::Write::write_fmt(
                &mut o,
                format_args!("  [ \"$__tp_x\" != true ] || __tp_c_{id}=1\n"),
            );
        } else if !m.pkg_names.is_empty() {
            let pats: Vec<String> = m
                .pkg_names
                .iter()
                .map(|n| {
                    let q = crate::sh::quote(n);
                    format!("{q}|{q}==*")
                })
                .collect();
            let _ = std::fmt::Write::write_fmt(
                &mut o,
                format_args!(
                    "  for __tp_y in \"${{__tp_inst[@]}}\"; do case \"$__tp_y\" in {}) __tp_c_{id}=1;; esac; done\n",
                    pats.join("|")
                ),
            );
        }
    }
    o.push_str("fi\n");
    o
}

/// What the executor sets once in every shell it opens (root and login):
/// `__tp_run`, this process's staging subdirectory name, so scripts name
/// the variable and the plan carries no per-process value. A login op in
/// a root shell gets it through its `env -i` ([`crate::trailer`]).
async fn init_shell(shell: &mut Shell) -> Result<(), String> {
    // An op's wrapper runs its body in a subshell, so the assignment goes
    // in front of it, in the shell itself; the wrapped no-op is the
    // handshake that it ran.
    let ack = crate::trailer::wrap(
        crate::trailer::OpKind::Sh,
        &crate::ops::shell::raw_body("true"),
        0,
        shell.nonce(),
    );
    let script = format!("{}{ack}", crate::ops::fetch::shell_init());
    shell.send(0, &script).await.map_err(|e| e.to_string())?;
    let (rc, out) = prepare::collect(shell, 0).await?;
    if rc == 0 {
        Ok(())
    } else {
        Err(format!("exit {rc}: {out}"))
    }
}

/// After a `unit` op sent as a source: a change sets the window flag.
const UNIT_FLAG: &str = "if [ \"$__tp_rc\" -eq 0 ] && [ \"$__tp_x\" = true ]; then __tp_dr=1; fi\n";

/// After a `svc` op that carries the daemon-reload: the flag is spent.
const UNIT_FLAG_CLEAR: &str = "__tp_dr=0\n";

/// "Booted": the test the svc scripts use for a running systemd, and a
/// `systemctl` to run.
const BOOTED: &str =
    "[ -d \"${TP_SVC_RUNDIR:-/run/systemd/system}\" ] && command -v systemctl >/dev/null 2>&1";

/// The flush at a window's end: with the flag still set and systemd
/// running, one `systemctl daemon-reload` (rc and output are the op's);
/// otherwise skipped.
fn flush_body() -> String {
    format!(
        "if [ \"${{__tp_dr-0}}\" = 1 ] && {BOOTED}; then\n  systemctl daemon-reload\n  rc=$?\n  if [ \"$rc\" -eq 0 ]; then changed=1; fi\nelse\n  skipped=1\nfi\n"
    )
}

/// What the stream, the failure and the event call the flush.
fn flush_desc(window: usize) -> String {
    format!("systemd daemon-reload, end of window {window}")
}

/// The window's `unit` ops after its last `svc` op (all of them when it
/// has none): a change of one of these leaves the flag set at the end.
fn units_after_last_svc(window: &Window) -> Vec<&WindowOp> {
    let last_svc = window
        .ops
        .iter()
        .rposition(|w| matches!(w.node.spec, OpSpec::Svc(_)));
    window
        .ops
        .iter()
        .skip(last_svc.map_or(0, |i| i + 1))
        .filter(|w| matches!(w.node.spec, OpSpec::Unit(_)))
        .collect()
}

/// `w`'s script as sent: inside the halt gate; inside its guard's test
/// when it has one; as a guard source (`source`: a guard later in the run
/// reads it) with the result variables set after it; then `after`, in the
/// parent shell, only when the op ran.
fn gate(
    w: &WindowOp,
    script: &str,
    nonce: &str,
    test: Option<&str>,
    source: bool,
    after: &str,
) -> String {
    let id = w.id.0;
    let kind = w.node.spec.op_kind();
    let run = if source {
        let halt = if w.node.allow_fail {
            ""
        } else {
            "[ \"$__tp_rc\" -eq 0 ] || __tp_halt=1\n"
        };
        format!(
            "__tp_f=$(mktemp 2>/dev/null) || __tp_f=\n{script}__tp_rc=$?\n{halt}{}",
            source_vars(w)
        )
    } else {
        let set = if w.node.allow_fail {
            ""
        } else {
            "[ $? -eq 0 ] || __tp_halt=1\n"
        };
        format!("{script}{set}")
    };
    let run = format!("{run}{after}");
    let not_run = if source {
        assign(w, 0, 0)
    } else {
        String::new()
    };
    let run = match test {
        None => run,
        Some(t) => {
            let skipped = if source {
                assign(w, 0, 1)
            } else {
                String::new()
            };
            format!(
                "if {t}; then\n{run}else\n{}{skipped}fi\n",
                crate::trailer::guard_skip(kind, id, nonce)
            )
        }
    };
    format!(
        "if [ -z \"${{__tp_halt-}}\" ]; then\n{run}else\n{}{not_run}fi\n",
        skip_script(&w.node, id, nonce)
    )
}

/// What member `m` of `w` is, for a guard's phrase: the op's kind and its
/// name (`pkg nvidia-container-toolkit`, the member's own names for a
/// merged `pkg` op; `file /etc/docker/daemon.json`; `svc docker.service`).
fn subject(w: &WindowOp, m: &crate::engine::Member) -> String {
    match &w.node.spec {
        OpSpec::Pkg(p) => {
            // A rustup member also lists its targets and components after
            // the toolchain: the subject is the toolchain.
            let names = if m.pkg_names.is_empty()
                || matches!(p.source, crate::ops::pkg::Source::Rustup(_))
            {
                &p.names
            } else {
                &m.pkg_names
            };
            format!("{} {}", p.kind(), names.join(" "))
        }
        OpSpec::File(f) if f.is_secret() => format!("file {} (secret)", f.path),
        OpSpec::File(f) => format!("file {}", f.path),
        OpSpec::Svc(s) => format!("svc {}", s.key_name()),
        OpSpec::Unit(u) if u.content.as_ref().is_some_and(|c| c.is_secret()) => {
            format!("unit {} (secret)", u.rel())
        }
        OpSpec::Unit(u) => format!("unit {}", u.rel()),
        _ => describe(&w.node),
    }
}

/// `g` in words, each op named by its subject: `pkg x changes`, `file /y
/// succeeds`, `... does not change`, `... fails`, joined with `or` and
/// `and`, parenthesised where binding needs it, `not (..)` around a
/// compound.
fn guard_phrase(g: &Guard, subjects: &HashMap<NodeId, String>) -> String {
    let name = |n: &NodeId| subjects.get(n).cloned().unwrap_or_else(|| n.to_string());
    let rank = |g: &Guard| match g {
        Guard::Or(..) => 0,
        Guard::And(..) => 1,
        _ => 2,
    };
    let part = |g: &Guard, min: u8| {
        let p = guard_phrase(g, subjects);
        if rank(g) < min { format!("({p})") } else { p }
    };
    match g {
        Guard::Changed(n) => format!("{} changes", name(n)),
        Guard::ExitOk(n) => format!("{} succeeds", name(n)),
        Guard::Not(inner) => match inner.as_ref() {
            Guard::Changed(n) => format!("{} does not change", name(n)),
            Guard::ExitOk(n) => format!("{} fails", name(n)),
            other => format!("not ({})", guard_phrase(other, subjects)),
        },
        Guard::Or(a, b) => format!("{} or {}", part(a, 0), part(b, 0)),
        Guard::And(a, b) => format!("{} and {}", part(a, 1), part(b, 1)),
    }
}

/// `g`'s value when the reported outcomes settle it, else `None`.
fn decide(g: &Guard, known: &HashMap<NodeId, Known>) -> Option<bool> {
    match g {
        Guard::Changed(n) => known.get(n).map(|k| k.changed),
        Guard::ExitOk(n) => known.get(n).map(|k| k.ok),
        Guard::Not(g) => decide(g, known).map(|v| !v),
        Guard::Or(a, b) => match (decide(a, known), decide(b, known)) {
            (Some(true), _) | (_, Some(true)) => Some(true),
            (Some(false), Some(false)) => Some(false),
            _ => None,
        },
        Guard::And(a, b) => match (decide(a, known), decide(b, known)) {
            (Some(false), _) | (_, Some(false)) => Some(false),
            (Some(true), Some(true)) => Some(true),
            _ => None,
        },
    }
}

/// The prepare detail of a `svc` restart or reload `w` (`w.node.guard` is
/// the guard the role wrote; `starts` the earlier start ops of its unit),
/// with its allowances; `None` for an op that is no action or that is
/// already decided not to run (its guard is false, or a start already
/// started the unit).
fn action_line(
    w: &WindowOp,
    starts: &[NodeId],
    known: &HashMap<NodeId, Known>,
    subjects: &HashMap<NodeId, String>,
) -> Option<String> {
    let OpSpec::Svc(s) = &w.node.spec else {
        return None;
    };
    let verb = s.action()?.name();
    let base = w.node.guard.as_ref();
    if base.map(|g| decide(g, known)) == Some(Some(false))
        || starts
            .iter()
            .any(|n| known.get(n).is_some_and(|k| k.started))
    {
        return None;
    }
    let mut line = match base {
        None => format!("will {verb}, every run"),
        Some(g) if decide(g, known) == Some(true) => format!("will {verb}"),
        Some(g) => format!("will {verb} if {}", guard_phrase(g, subjects)),
    };
    for a in &w.node.allowed {
        line.push_str(&format!("; {a}"));
    }
    Some(line)
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
                crate::ops::pkg::Source::Script(_)
                    | crate::ops::pkg::Source::BinaryRelease(_)
                    | crate::ops::pkg::Source::Cargo { .. }
                    | crate::ops::pkg::Source::Mise
                    | crate::ops::pkg::Source::Rustup(_)
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
        OpSpec::Group(g) => format!("group {}", g.name),
        OpSpec::User(u) if u.shows_secret() => format!("user {} <secret>", u.name),
        OpSpec::User(u) => format!("user {}", u.name),
        OpSpec::Repo(r) => format!("repo {}", r.name),
        OpSpec::Svc(s) => match s.action() {
            Some(a) => format!("svc {} {}", s.unit, a.name()),
            None => format!("svc {}", s.unit),
        },
        OpSpec::Unit(u) if u.content.as_ref().is_some_and(|c| c.is_secret()) => {
            format!("unit {} (secret)", u.rel())
        }
        OpSpec::Unit(u) => {
            let what = match (&u.drop_in, u.absent) {
                (_, true) => "absent",
                (Some(_), false) => "drop-in",
                (None, false) => "content",
            };
            format!("unit {} {what}", u.rel())
        }
        OpSpec::File(f) if f.is_secret() => format!("file {} (secret)", f.path),
        OpSpec::File(f) => {
            use crate::ops::file::FileKind;
            let what = match f.kind {
                FileKind::Dir => "dir".to_string(),
                FileKind::Absent => "absent".to_string(),
                FileKind::File if f.src.is_some() => {
                    let src = f.src.as_ref().unwrap();
                    let verb = if src.unpack.is_some() {
                        "unpack"
                    } else {
                        "from"
                    };
                    format!("{verb} {}", src.fetched.url)
                }
                FileKind::File if f.content.is_some() => "content".to_string(),
                FileKind::File if !f.lines.is_empty() => "lines".to_string(),
                FileKind::File => "attrs".to_string(),
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

/// A trailer's `(rc, changed, skipped, installed, started, reloads)`;
/// `installed` for a `pkg` result only, `started` true for a `svc` start
/// that ran, `reloads` the svc trailer's daemon-reload tokens (empty
/// otherwise).
type Parsed = (i32, bool, bool, Option<Vec<String>>, bool, String);

fn parse_result(kind: OpKind, t: &Trailer) -> Result<Parsed, String> {
    use crate::trailer::{
        FileResult, GroupResult, PkgResult, RepoResult, ShellResult, SvcResult, UserResult,
    };
    let r = match kind {
        OpKind::Pkg => t.typed::<PkgResult>().map(|r| {
            (
                r.rc,
                r.changed,
                false,
                Some(r.installed),
                false,
                String::new(),
            )
        }),
        OpKind::File => t
            .typed::<FileResult>()
            .map(|r| (r.rc, r.changed, false, None, false, String::new())),
        OpKind::Group => t
            .typed::<GroupResult>()
            .map(|r| (r.rc, r.changed, false, None, false, String::new())),
        OpKind::User => t
            .typed::<UserResult>()
            .map(|r| (r.rc, r.changed, false, None, false, String::new())),
        OpKind::Repo => t
            .typed::<RepoResult>()
            .map(|r| (r.rc, r.changed, false, None, false, String::new())),
        OpKind::Svc => t
            .typed::<SvcResult>()
            .map(|r| (r.rc, r.changed, r.skipped, None, r.started, r.reloads)),
        OpKind::Sh => t
            .typed::<ShellResult>()
            .map(|r| (r.rc, r.changed, r.skipped, None, false, String::new())),
    };
    r.map_err(|e| format!("unreadable result {:?}: {e}", t.json))
}

fn last_lines(bytes: &[u8], n: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

impl HostExec {
    fn new(name: &str, shells: HostShells, sink: Rc<Sink>) -> HostExec {
        HostExec {
            name: name.to_string(),
            sink,
            shells,
            report: HostReport::new(name),
            halted: false,
            verdict: None,
            users_ok: BTreeSet::new(),
            seq: PROBE_BASE,
            known: HashMap::new(),
            subjects: HashMap::new(),
            starts: Vec::new(),
            suppress: HashMap::new(),
            fetch_staged: false,
        }
    }

    /// Each `svc` restart or reload of a unit that an earlier op of the
    /// run wants `.started()` is sent with "that op started the unit"
    /// and-not-ed into its guard (`self.suppress`, read by [`sent_test`]
    /// and [`when_text`]): not that op's `changed`, which is also true when
    /// it only enabled or unmasked a running unit. Also the prepare lines
    /// of the window's actions, from the role's own guard. Records the
    /// window's ops (their subjects; the starts) for later windows.
    fn and_not_starts(&mut self, window: &Window) -> Vec<crate::report::WillCreate> {
        let mut lines = Vec::new();
        for w in &window.ops {
            for m in &w.members {
                self.subjects.insert(m.id, subject(w, m));
            }
            let OpSpec::Svc(s) = &w.node.spec else {
                continue;
            };
            let key = s.key_name();
            if s.action().is_some() {
                let starts: Vec<NodeId> = self
                    .starts
                    .iter()
                    .filter(|(k, _)| *k == key)
                    .map(|(_, id)| *id)
                    .collect();
                if let Some(detail) = action_line(w, &starts, &self.known, &self.subjects) {
                    lines.push(crate::report::WillCreate {
                        window: window.number,
                        kind: "svc".to_string(),
                        name: key.clone(),
                        detail: Some(detail),
                    });
                }
                if !starts.is_empty() {
                    self.suppress.insert(w.id, starts);
                }
            }
            if s.started {
                self.starts.push((key, w.id));
            }
        }
        lines
    }

    /// The earlier start ops a restart or reload `w` is and-not-ed with.
    fn starts_of(&self, w: &WindowOp) -> &[NodeId] {
        self.suppress.get(&w.id).map_or(&[], Vec::as_slice)
    }

    /// The prepare line of the window's flush: when prepare says a unit
    /// after the window's last `svc` op will change and systemd runs (one
    /// probe, only then). Gathered, never asserted: a guard-skipped svc op
    /// after a changed unit also leaves a flush, with no line here.
    async fn flush_line(
        &mut self,
        window: &Window,
        found: &[crate::report::WillCreate],
    ) -> Option<crate::report::WillCreate> {
        let names: Vec<String> = units_after_last_svc(window)
            .into_iter()
            .filter_map(|w| match &w.node.spec {
                OpSpec::Unit(u) => Some(u.rel()),
                _ => None,
            })
            .filter(|n| found.iter().any(|c| c.kind == "unit" && c.name == *n))
            .collect();
        if names.is_empty() {
            return None;
        }
        self.seq += 1;
        let probe = format!("if {BOOTED}; then echo booted; fi");
        let (_, out) = prepare::probe(&mut self.shells.root, self.seq, &probe)
            .await
            .ok()?;
        if out.trim() != "booted" {
            return None;
        }
        Some(crate::report::WillCreate {
            window: window.number,
            kind: "systemd".to_string(),
            name: "daemon-reload".to_string(),
            detail: Some(format!(
                "at the end of the window, after unit {}",
                names.join(", ")
            )),
        })
    }

    /// End the window's unit flag: when a unit changed and no `svc` op ran
    /// after it (it would have reloaded and cleared the flag), send one
    /// `systemctl daemon-reload` to the root shell, in the halt gate; the
    /// shell runs it only with the flag set and systemd running, then
    /// clears the flag. A reload that ran is streamed and reported
    /// ([`crate::report::DaemonReload`]); a failed one fails the host with
    /// its rc and output, like a failed op.
    async fn flush(&mut self, window: &Window) {
        let outcome = |id: NodeId, rep: &HostReport| {
            rep.ops
                .iter()
                .rev()
                .find(|o| o.id == id.0)
                .map(|o| o.outcome)
        };
        let mut pending: Vec<&WindowOp> = Vec::new();
        for w in &window.ops {
            match (&w.node.spec, outcome(w.id, &self.report)) {
                (OpSpec::Unit(_), Some(Outcome::Changed)) => pending.push(w),
                (
                    OpSpec::Svc(_),
                    Some(Outcome::Changed | Outcome::Unchanged | Outcome::AllowedFail),
                ) => pending.clear(),
                _ => {}
            }
        }
        if pending.is_empty() {
            return;
        }
        self.seq += 1;
        let id = self.seq;
        let nonce = self.shells.root.nonce().to_string();
        let script = format!(
            "if [ -z \"${{__tp_halt-}}\" ]; then\n{}[ $? -eq 0 ] || __tp_halt=1\n__tp_dr=0\nelse\n{}fi\n",
            wrap(OpKind::Sh, &flush_body(), id, &nonce),
            wrap(OpKind::Sh, "rc=0\nskipped=1\n", id, &nonce)
        );
        let desc = flush_desc(window.number);
        let mut roles: Vec<String> = Vec::new();
        for w in &pending {
            for r in roles_of(w) {
                if !roles.contains(&r) {
                    roles.push(r);
                }
            }
        }
        let roles = roles.join(", ");
        let got = match self.shells.root.send(id, &script).await {
            Ok(()) => self.shells.root.wait(id).await.map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        let (rc, output) = match got {
            Ok(o) => match o.trailer.typed::<crate::trailer::ShellResult>() {
                Ok(r) if r.rc == 0 && r.skipped => return,
                Ok(r) => (r.rc, String::from_utf8_lossy(&o.output).into_owned()),
                Err(e) => (-1, format!("unreadable result {:?}: {e}", o.trailer.json)),
            },
            Err(e) => (-1, e),
        };
        let reason = "end of window";
        if rc == 0 {
            // One line, not a fake op: `-> changed` would count as work.
            self.record_daemon_reload(window.number, rc, reason, None);
        } else {
            self.sink
                .line(&format!("== {} [{roles}] {desc} ==", self.name));
            for l in output.lines() {
                self.sink.line(&format!("{} | {l}", self.name));
            }
            self.sink.line(&format!("-> FAILED rc={rc}"));
            self.record_daemon_reload(window.number, rc, reason, None);
        }
        if rc != 0 {
            self.halted = true;
            if self.verdict.is_none() {
                let last = pending[pending.len() - 1];
                self.verdict = Some(HostStatus::Failed {
                    role: roles,
                    op_id: last.id.0,
                    op: desc,
                    rc,
                    tail: last_lines(output.as_bytes(), TAIL_LINES),
                });
            }
        }
    }

    /// The prepare line, event and report entry of a window split at `s`.
    fn split(&mut self, window: &Window, s: &Split) {
        let sp = crate::report::WindowSplit {
            window: window.number,
            after: s.installer.0,
            tool: s.tool.clone(),
            consumer: s.consumer.0,
        };
        self.sink
            .line(&format!("== {} [prepare] {} ==", self.name, sp.line()));
        self.sink.event(json!({"ev":"window_split","host":self.name,
            "window":sp.window,"after":sp.after,"tool":sp.tool,"consumer":sp.consumer}));
        self.report.splits.push(sp);
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

    /// One daemon-reload: the stream line, the event, the report entry.
    /// Not an op, so it is not counted in OK or CHANGED. `op` is the svc
    /// op it ran inside, or none for the end-of-window flush.
    fn record_daemon_reload(&mut self, window: usize, rc: i32, reason: &str, op: Option<usize>) {
        self.sink
            .line(&format!("   systemd daemon-reload: {reason}"));
        self.sink.event(json!({
            "ev": "daemon_reload",
            "host": self.name,
            "window": window,
            "rc": rc,
            "reason": reason,
            "op": op,
        }));
        self.report
            .daemon_reloads
            .push(crate::report::DaemonReload {
                window,
                rc,
                reason: reason.to_string(),
            });
    }

    /// Report each token in the svc trailer's `reloads` (`unit`, `need`).
    /// The line is printed before `-> result`. A failed reload is the last
    /// token: the script stops on it, so earlier tokens in the same op
    /// succeeded.
    fn note_reloads(&mut self, window: &Window, w: &WindowOp, desc: &str, reloads: &str, rc: i32) {
        let tokens: Vec<&str> = reloads.split(',').filter(|t| !t.is_empty()).collect();
        if tokens.is_empty() {
            return;
        }
        let units = self.changed_units_since_svc(window, w.id.0);
        let n = tokens.len();
        for (i, tok) in tokens.iter().enumerate() {
            let this_rc = if rc != 0 && i + 1 == n { rc } else { 0 };
            let reason = match *tok {
                "unit" => format!("before #{} {desc} (unit {units} changed)", w.id.0),
                "need" => format!("before #{} {desc} (systemd NeedDaemonReload)", w.id.0),
                other => format!("before #{} {desc} ({other})", w.id.0),
            };
            self.record_daemon_reload(window.number, this_rc, &reason, Some(w.id.0));
        }
    }

    /// Unit files changed since the previous svc op that actually ran
    /// (a skipped svc does not clear the window flag).
    fn changed_units_since_svc(&self, window: &Window, before: usize) -> String {
        let mut names = Vec::new();
        for op in &window.ops {
            if op.id.0 == before {
                break;
            }
            match &op.node.spec {
                OpSpec::Svc(_) => {
                    let ran = self.report.ops.iter().any(|o| {
                        o.id == op.id.0
                            && matches!(
                                o.outcome,
                                Outcome::Changed | Outcome::Unchanged | Outcome::AllowedFail
                            )
                    });
                    if ran {
                        names.clear();
                    }
                }
                OpSpec::Unit(u) => {
                    let changed = self
                        .report
                        .ops
                        .iter()
                        .any(|o| o.id == op.id.0 && o.outcome == Outcome::Changed);
                    if changed {
                        names.push(u.rel());
                    }
                }
                _ => {}
            }
        }
        names.join(", ")
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
        let when = when_text(w, self.starts_of(w));
        let label = match outcome {
            Outcome::Changed => "changed".to_string(),
            Outcome::Unchanged if note.is_empty() => "ok".to_string(),
            Outcome::Unchanged => format!("ok ({note})"),
            Outcome::Failed => format!("FAILED rc={rc}"),
            Outcome::AllowedFail => format!("failed rc={rc} (allowed)"),
            Outcome::NotRun => "not run".to_string(),
            Outcome::Skipped => format!("skipped (when {})", when.as_deref().unwrap_or("")),
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
            allowed: w.node.allowed.clone(),
            when,
        });
        self.remember(w, outcome, changed, None);
    }

    /// Record `w`'s outcome for later guards, per member. `installed` is
    /// the `pkg` result's list (the member rule); `None` uses `changed`.
    fn remember(
        &mut self,
        w: &WindowOp,
        outcome: Outcome,
        changed: bool,
        installed: Option<&[String]>,
    ) {
        let ok = matches!(
            outcome,
            Outcome::Unchanged | Outcome::Changed | Outcome::Skipped
        );
        let ran = ok && outcome != Outcome::Skipped;
        for m in &w.members {
            let c = match installed {
                Some(got) if member_rule(w) => got
                    .iter()
                    .any(|g| m.pkg_names.iter().any(|n| installed_as(g, n))),
                _ => changed,
            };
            self.known.insert(
                m.id,
                Known {
                    ok,
                    changed: ran && c,
                    started: false,
                },
            );
        }
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

    /// Verify the users `users` exist (on the root shell), then open the
    /// shell of each that is not open yet; `Err` blocks the host.
    async fn ensure_users(&mut self, users: &BTreeSet<String>) -> Result<(), String> {
        use prepare::ShellSet as _;
        let new: Vec<&String> = users
            .iter()
            .filter(|u| !self.users_ok.contains(*u))
            .collect();
        if !new.is_empty() {
            self.seq += 1;
            let script = prepare::users_script(new.iter().copied());
            let (_, out) = prepare::probe(&mut self.shells.root, self.seq, &script).await?;
            let missing = prepare::missing_users(&out);
            if !missing.is_empty() {
                return Err(format!(
                    "user {} does not exist on the host",
                    missing.join(", ")
                ));
            }
            let ok: Vec<String> = new.into_iter().cloned().collect();
            self.users_ok.extend(ok);
        }
        for u in users {
            self.shells.open_user(u).await?;
        }
        Ok(())
    }

    /// The shell `who`'s ops run in (opened by prepare or `ensure_users`).
    fn shell_for(&mut self, who: &Principal) -> Option<&mut Shell> {
        use prepare::ShellSet as _;
        self.shells.shell(who)
    }

    /// Remove the run's staging directory on the host (what a fetch with no
    /// checksum left there). Best effort: a failure is not the run's.
    async fn cleanup_stage(&mut self) {
        if !self.fetch_staged {
            return;
        }
        self.seq += 1;
        let _ = prepare::probe(
            &mut self.shells.root,
            self.seq,
            &crate::ops::fetch::cleanup_script(),
        )
        .await;
    }

    fn finish(mut self, res: Result<(), EngineError>) -> (HostReport, HostShells) {
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
        (rep, self.shells)
    }
}

/// Does prepare's missing-downloader block of `window` (the ops in
/// `no_downloader`) lift by splitting it at `s`: every op that needed a
/// downloader comes after the one that installs it?
fn splits_at(window: &Window, s: &Split, no_downloader: &[NodeId]) -> bool {
    !no_downloader.is_empty()
        && no_downloader.iter().all(|id| {
            window
                .ops
                .iter()
                .position(|o| o.id == *id)
                .is_some_and(|i| i > s.at)
        })
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
                site: None,
                allowed: Vec::new(),
                at_end: false,
            };
            let id = self.seq;
            let who = &q.run_as.principal;
            let sent = match self.shell_for(who) {
                Some(sh) => sh
                    .send_op(&node, id, &host.os)
                    .await
                    .map_err(|e| e.to_string()),
                None => Err(format!("no shell for {who:?}")),
            };
            match sent {
                Ok(()) => ids.push(id),
                Err(e) => {
                    sent_err = Some(e);
                    break;
                }
            }
        }
        let mut values = Vec::new();
        for (q, id) in queries.iter().zip(&ids) {
            let got = match self.shell_for(&q.run_as.principal) {
                Some(sh) => prepare::collect(sh, *id).await,
                None => Err("no shell".to_string()),
            };
            match got {
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

    async fn apply(&mut self, host: &HostInfo, window: &Window) -> Result<Applied, String> {
        let mut window = window.clone();
        let resolved = prepare::resolve_secrets(&mut window);
        for w in crate::secret::backend_warnings() {
            self.sink
                .line(&format!("== {} [prepare] {w} ==", self.name));
        }
        resolved?;
        let window = &window;
        let kind = match window.kind {
            WindowKind::NonMutating => "non-mutating",
            WindowKind::ImplicitBarrier => "implicit barrier",
            WindowKind::Final => "final",
            WindowKind::AtEnd => "at end",
        };
        // Prepare first: a window declined for its head (no downloader on
        // the host, and an op of the window installs one) is sent,
        // reported and recorded nowhere, so nothing below runs for it.
        let prepared = if window.ops.is_empty() || self.halted {
            None
        } else {
            // A fetch or a binary release stages under the run's
            // directory; removed when the host is done, even if its op
            // never ran.
            let binary_release = window.ops.iter().any(|o| {
                matches!(&o.node.spec, OpSpec::Pkg(p)
                    if matches!(p.source, crate::ops::pkg::Source::BinaryRelease(_)))
            });
            if binary_release || !crate::ops::fetch::items(window).is_empty() {
                self.fetch_staged = true;
            }
            // Prepare: nothing is sent to be applied until all of it passes.
            let prepared = prepare::prepare_window(
                &mut self.shells,
                window,
                &host.os,
                &mut self.users_ok,
                &mut self.seq,
            )
            .await;
            if let (Err(r), Some(s)) = (&prepared, &window.split)
                && splits_at(window, s, &r.no_downloader)
            {
                self.split(window, s);
                return Ok(Applied::Head(s.at + 1));
            }
            Some(prepared)
        };
        self.sink.event(
            json!({"ev":"window","host":self.name,"n":window.number,"kind":kind,
            "ops": window.ops.iter().map(|o| o.id.0).collect::<Vec<_>>()}),
        );
        for site in crate::trace::sh_sites(window.ops.iter().map(|o| &o.node)) {
            if !self.report.sh_sites.contains(&site) {
                self.report.sh_sites.push(site);
            }
        }
        let Some(prepared) = prepared else {
            // Empty, or the host halted: its ops do not run.
            for w in &window.ops {
                self.not_run(w);
            }
            return Ok(Applied::All);
        };
        let actions = self.and_not_starts(window);
        match prepared {
            Ok(mut will) => {
                will.will_create.extend(actions);
                if let Some(l) = self.flush_line(window, &will.will_create).await {
                    will.will_create.push(l);
                }
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
                for fl in will.fetches {
                    self.sink
                        .line(&format!("== {} [prepare] {} ==", self.name, fl.line()));
                    self.sink.event(json!({"ev":"fetch","host":self.name,
                        "window":fl.window,"url":fl.url,"sha256":fl.sha256,
                        "state":fl.state.words()}));
                    self.report.fetches.push(fl);
                }
                for l in crate::report::WillChange::lines(&will.will_change) {
                    self.sink
                        .line(&format!("== {} [prepare] {l} ==", self.name));
                }
                for c in &will.will_create {
                    self.sink
                        .line(&format!("== {} [prepare] {} ==", self.name, c.line()));
                }
                for c in will.will_create {
                    self.sink.event(json!({"ev":"will_create","host":self.name,
                        "window":c.window,"kind":c.kind,"name":c.name,"detail":c.detail}));
                    self.report.will_create.push(c);
                }
                for w in will.will_change {
                    self.sink.event(json!({"ev":"will_change","host":self.name,
                        "window":w.window,"source":w.source,"user":w.user,"name":w.name,
                        "installed":w.installed,"wanted":w.wanted}));
                    self.report.will_change.push(w);
                }
            }
            Err(r) => {
                self.block(r.why.clone());
                return Err(r.why);
            }
        }

        // Consecutive ops on the same principal's shell form a run.
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut s = 0;
        while s < window.ops.len() {
            let who = &window.ops[s].node.run_as.principal;
            let e = window.ops[s..]
                .iter()
                .position(|w| w.node.run_as.principal != *who)
                .map_or(window.ops.len(), |n| s + n);
            runs.push((s, e));
            s = e;
        }
        // The ops a guard later in the same run reads: sent as sources.
        let mut source = vec![false; window.ops.len()];
        for &(s, e) in &runs {
            let read: BTreeSet<NodeId> = window.ops[s..e]
                .iter()
                .flat_map(|w| {
                    let own = w
                        .node
                        .guard
                        .as_ref()
                        .map(|g| g.sources())
                        .unwrap_or_default();
                    own.into_iter().chain(self.starts_of(w).iter().copied())
                })
                .collect();
            for (i, w) in window.ops.iter().enumerate().take(e).skip(s) {
                // A unit op's `changed` sets the window flag.
                source[i] = w.members.iter().any(|m| read.contains(&m.id))
                    || matches!(w.node.spec, OpSpec::Unit(_));
            }
        }

        // Render each op for its principal's shell (open now).
        let mut scripts = Vec::new();
        let mut afters = Vec::new();
        // A `unit` op came earlier in this window: later `svc` ops carry
        // the daemon-reload of a changed unit. The flag never outlives the
        // window (the flush at its end), so earlier windows do not count.
        let mut units = false;
        for (i, w) in window.ops.iter().enumerate() {
            let daemon_reload = units && matches!(w.node.spec, OpSpec::Svc(_));
            afters.push(match &w.node.spec {
                OpSpec::Unit(_) => UNIT_FLAG,
                OpSpec::Svc(_) if daemon_reload => UNIT_FLAG_CLEAR,
                _ => "",
            });
            if matches!(w.node.spec, OpSpec::Unit(_)) {
                units = true;
            }
            let who = &w.node.run_as.principal;
            let Some(sh) = self.shell_for(who) else {
                let why = format!("op #{}: no shell open for {who:?}", w.id.0);
                self.block(why.clone());
                return Err(why);
            };
            let opts = crate::trailer::RenderOpts {
                source: source[i],
                daemon_reload,
            };
            match crate::trailer::render_with(
                &w.node,
                w.id.0,
                sh.nonce(),
                &host.os,
                sh.user(),
                opts,
            ) {
                Ok(s) => scripts.push(s),
                Err(e) => {
                    let why = format!("op #{}: {e}", w.id.0);
                    self.block(why.clone());
                    return Err(why);
                }
            }
        }

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
        let mut send_err = None;

        // Each run is pipelined into its shell and collected in full
        // before the next run is sent (Assumption A3); after a failure
        // nothing more is sent on any shell.
        let mut r = 0;
        while r < runs.len() && !failed_seen && send_err.is_none() {
            let (start, end) = runs[r];
            let who = window.ops[start].node.run_as.principal.clone();

            // Send the run, gated on the shell's halt flag and each op's
            // guard (constants for what is reported, variables for sources
            // earlier in this run).
            let Some(nonce) = self.shell_for(&who).map(|sh| sh.nonce().to_string()) else {
                break;
            };
            let mut in_run: BTreeSet<NodeId> = BTreeSet::new();
            let mut gated = Vec::new();
            for i in start..end {
                let w = &window.ops[i];
                let test = sent_test(w, self.starts_of(w), &self.known, &in_run);
                gated.push(gate(
                    w,
                    &scripts[i],
                    &nonce,
                    test.as_deref(),
                    source[i],
                    afters[i],
                ));
                in_run.extend(w.members.iter().map(|m| m.id));
            }
            let Some(sh) = self.shell_for(&who) else {
                break;
            };
            for (w, g) in window.ops[start..end].iter().zip(&gated) {
                if let Err(e) = sh.send(w.id.0, g).await {
                    send_err = Some(e.to_string());
                    break;
                }
            }

            // Collect the run in order.
            while let Some(ev) = match self.shell_for(&who) {
                Some(sh) => sh.next_event().await,
                None => None,
            } {
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
                            Ok((rc, changed, skipped, installed, started, reloads)) => {
                                let skip = rc == 0 && crate::trailer::is_guard_skip(&trailer);
                                if !skip {
                                    self.note_reloads(window, w, &descs[i], &reloads, rc);
                                }
                                if skip {
                                    self.finish_op(w, &descs[i], Outcome::Skipped, 0, false, "");
                                } else if rc == 0 {
                                    let o = if changed {
                                        Outcome::Changed
                                    } else {
                                        Outcome::Unchanged
                                    };
                                    let note = if skipped { "skipped" } else { "" };
                                    self.finish_op(w, &descs[i], o, 0, changed, note);
                                    if installed.is_some() {
                                        self.remember(w, o, changed, installed.as_deref());
                                    }
                                    if started {
                                        for m in &w.members {
                                            if let Some(k) = self.known.get_mut(&m.id) {
                                                k.started = true;
                                            }
                                        }
                                    }
                                } else if w.node.allow_fail {
                                    self.finish_op(
                                        w,
                                        &descs[i],
                                        Outcome::AllowedFail,
                                        rc,
                                        false,
                                        "",
                                    );
                                } else {
                                    failed_seen = true;
                                    let tail = last_lines(&live[i].tail, TAIL_LINES);
                                    self.finish_op(w, &descs[i], Outcome::Failed, rc, false, "");
                                    self.fail(w, &descs[i], rc, tail);
                                }
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
            // An op of this run with no result was lost with the shell:
            // stop here, the loop below reports it.
            if done[start..end].iter().any(|d| !d) {
                break;
            }
            r += 1;
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
        if !failed_seen && !self.halted {
            self.flush(window).await;
        }
        Ok(Applied::All)
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
    let target = Rc::new(spec.target);
    let mut shells = target.open(&name, &Principal::Root, 1).await;
    if shells.got.is_empty() {
        let why = refusal(&target, &name, &Principal::Root, shells.limit);
        close_target(&target).await;
        return blocked(why);
    }
    let mut shell = shells.got.remove(0);
    for extra in shells.got {
        extra.close().await;
    }
    let os = match init_shell(&mut shell).await {
        Ok(()) => read_os(&mut shell).await,
        Err(e) => Err(e),
    };
    let os = match os {
        Ok(os) => os,
        Err(e) => {
            shell.close().await;
            close_target(&target).await;
            return blocked(format!("reading os: {e}"));
        }
    };
    let host = Host::new(&name, os, Rc::new(spec.vars));
    let pb = Playbook::new(pb_name, pb_run);
    let shells = HostShells {
        host: name.clone(),
        target,
        root: shell,
        users: BTreeMap::new(),
    };
    let mut engine = Engine::new(HostExec::new(&name, shells, sink.clone()));
    let res = engine.run_host(&pb, host).await;
    engine.executor.cleanup_stage().await;
    let (rep, shells) = engine.executor.finish(res);
    shells.close().await;
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
    crate::secret::begin_run();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn subjects(v: &[(usize, &str)]) -> HashMap<NodeId, String> {
        v.iter().map(|(i, s)| (NodeId(*i), s.to_string())).collect()
    }

    #[test]
    fn guard_phrases_name_the_ops() {
        let s = subjects(&[
            (0, "pkg nvidia-container-toolkit"),
            (1, "file /etc/docker/daemon.json"),
            (2, "sh make"),
        ]);
        let (a, b, c) = (NodeId(0), NodeId(1), NodeId(2));
        assert_eq!(
            guard_phrase(&Guard::Changed(a), &s),
            "pkg nvidia-container-toolkit changes"
        );
        assert_eq!(
            guard_phrase(&(Guard::Changed(a) | Guard::Changed(b)), &s),
            "pkg nvidia-container-toolkit changes or file /etc/docker/daemon.json changes"
        );
        assert_eq!(
            guard_phrase(
                &((Guard::Changed(a) | Guard::Changed(b)) & Guard::ExitOk(c)),
                &s
            ),
            "(pkg nvidia-container-toolkit changes or file /etc/docker/daemon.json changes) and sh make succeeds"
        );
        assert_eq!(
            guard_phrase(&!Guard::Changed(b), &s),
            "file /etc/docker/daemon.json does not change"
        );
        assert_eq!(guard_phrase(&!Guard::ExitOk(c), &s), "sh make fails");
        assert_eq!(
            guard_phrase(&!(Guard::Changed(a) | Guard::Changed(b)), &s),
            "not (pkg nvidia-container-toolkit changes or file /etc/docker/daemon.json changes)"
        );
        assert_eq!(guard_phrase(&Guard::Changed(NodeId(9)), &s), "#9 changes");
    }

    #[test]
    fn a_guard_is_decided_only_when_its_sources_are_reported() {
        let mut known = HashMap::new();
        known.insert(
            NodeId(0),
            Known {
                ok: true,
                changed: false,
                started: false,
            },
        );
        known.insert(
            NodeId(1),
            Known {
                ok: true,
                changed: true,
                started: false,
            },
        );
        let (a, b, c) = (NodeId(0), NodeId(1), NodeId(2));
        assert_eq!(decide(&Guard::Changed(a), &known), Some(false));
        assert_eq!(
            decide(&(Guard::Changed(a) | Guard::Changed(b)), &known),
            Some(true)
        );
        assert_eq!(decide(&Guard::Changed(c), &known), None);
        assert_eq!(
            decide(&(Guard::Changed(b) | Guard::Changed(c)), &known),
            Some(true)
        );
        assert_eq!(
            decide(&(Guard::Changed(a) & Guard::Changed(c)), &known),
            Some(false)
        );
        assert_eq!(decide(&!Guard::Changed(c), &known), None);
    }

    fn restart_op(id: usize, guard: Option<Guard>) -> WindowOp {
        let mut spec = crate::ops::svc::SvcSpec::new("docker");
        spec.restart = true;
        let spec = OpSpec::Svc(spec);
        WindowOp {
            id: NodeId(id),
            members: vec![crate::engine::Member {
                id: NodeId(id),
                role: String::new(),
                pkg_names: Vec::new(),
            }],
            node: OpNode {
                writes: spec.default_writes(),
                post: spec.default_post(),
                spec,
                run_as: crate::trace::RunAs::root_shell(),
                reads: Default::default(),
                guard,
                allow_fail: false,
                site: None,
                allowed: Vec::new(),
                at_end: false,
            },
        }
    }

    #[test]
    fn a_restart_is_dropped_only_by_a_start_that_started_the_unit() {
        let s = subjects(&[(0, "file /etc/x"), (1, "svc docker.service")]);
        let g = Some(Guard::Changed(NodeId(0)));
        let w = restart_op(2, g);
        let starts = [NodeId(1)];
        let mut known = HashMap::new();
        known.insert(
            NodeId(1),
            Known {
                ok: true,
                changed: true,
                started: false,
            },
        );
        assert_eq!(
            action_line(&w, &starts, &known, &s).as_deref(),
            Some("will restart if file /etc/x changes"),
            "enabled a running unit: changed, not started"
        );
        known.get_mut(&NodeId(1)).unwrap().started = true;
        assert_eq!(action_line(&w, &starts, &known, &s), None);
        assert_eq!(
            when_text(&w, &starts).as_deref(),
            Some("changed(#0) & !started(#1)")
        );
        let either = restart_op(
            3,
            Some(Guard::Changed(NodeId(0)) | Guard::ExitOk(NodeId(1))),
        );
        assert_eq!(
            when_text(&either, &[NodeId(1), NodeId(2)]).as_deref(),
            Some("(changed(#0) | exit_ok(#1)) & !(started(#1) | started(#2))")
        );
        assert_eq!(
            when_text(&restart_op(4, None), &[NodeId(1)]).as_deref(),
            Some("!started(#1)")
        );
        let in_run = BTreeSet::from([NodeId(1)]);
        assert_eq!(
            sent_test(&w, &starts, &HashMap::new(), &in_run).as_deref(),
            Some("{ false; } && ! { [ \"${__tp_st_1-0}\" = 1 ]; }")
        );
    }

    #[test]
    fn the_prepare_line_of_a_restart() {
        let s = subjects(&[(0, "pkg nvidia-container-toolkit")]);
        let mut known = HashMap::new();
        let g = Some(Guard::Changed(NodeId(0)));
        let w = restart_op(1, g.clone());
        assert_eq!(
            action_line(&w, &[], &known, &s).as_deref(),
            Some("will restart if pkg nvidia-container-toolkit changes")
        );
        known.insert(
            NodeId(0),
            Known {
                ok: true,
                changed: false,
                started: false,
            },
        );
        assert_eq!(action_line(&w, &[], &known, &s), None, "decided: settled");
        known.insert(
            NodeId(0),
            Known {
                ok: true,
                changed: true,
                started: false,
            },
        );
        assert_eq!(
            action_line(&w, &[], &known, &s).as_deref(),
            Some("will restart")
        );
        let mut w = restart_op(1, None);
        w.node.allowed.push(crate::policy::Allowance::new(
            crate::policy::Policy::UnguardedRestart,
            "leaks memory",
        ));
        assert_eq!(
            action_line(&w, &[], &known, &s).as_deref(),
            Some("will restart, every run; allowed unguarded-restart: leaks memory")
        );
    }
}
