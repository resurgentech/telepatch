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
//! The host keeps the root shell plus one persistent login shell per user
//! (`as_user(u)`, root included), opened once, the first time a window's
//! ops or a fact read address that user. Shells are keyed by [`RunAs`]
//! (principal and login), not by principal: a plain op (`login: false`)
//! goes to the root shell, an `as_user(u)` op to `u`'s login shell. An op
//! sent to a login shell is a plain `( ... )` subshell like any other:
//! there is no per-op `bash`, `env -i` or here-doc.
//!
//! - `User(u)`: after the `getent` check, over SSH the channel `sudo -n
//!   -iu u bash` (a refusal blocks the host). Local and docker open no
//!   such shell: a window that needs one is blocked with the transport's
//!   limit.
//! - `as_user("root")` (root, the shell's own user; under a local
//!   transport with `uid(0)` also the test user): one more root shell of
//!   the transport, into which `exec env -i HOME=.. USER=.. LOGNAME=..
//!   SHELL=.. PATH=.. bash -l` is typed (the values from `getent passwd`
//!   read in that shell: short arguments, no script) and a hello, as
//!   `Shell::exec` does for `sudo -n -H bash`. It has root's login
//!   environment and nothing of the plain root shell's but the test switch
//!   and the path seams that shell has set (`sandbox::CARRY`, T128; none
//!   in production). A missing passwd
//!   entry refuses to open it, and blocks the host.
//!
//! Consecutive ops with one [`RunAs`] form a run; a run is pipelined and
//! collected in full before the next run is sent, so a change of shell
//! waits for the previous one (every switch is treated as a dependency
//! edge). The window flag `__tp_dr` and a guard's source outcome are
//! variables of one shell; across runs they are carried as constants the
//! executor knows from the collected trailers (see below).
//!
//! # Failure gating
//!
//! Ops are pipelined, so by the time op N fails, N+1.. are already written
//! to the shell. Each op is therefore sent as
//! `if [ -z "${__tp_halt-}" ]; then <op>; [ $? -eq 0 ] || __tp_halt=1;
//! else <skip trailer>; fi`. `__tp_halt` lives in the persistent shell and
//! is never reset: after a failure no later op on that shell runs. A signal
//! that ends an op reaches `$?` as 143 or 137, so a killed op trips the halt
//! too. The shell runs under `set -m` ([`init_shell`]): each op is its own
//! process group, and the skip trailer prints no start line
//! ([`crate::trailer`]). After a failure the
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
//!   (an earlier run, another shell, an earlier window): runs are
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
//! `changed` reaches the parent shell; when it changed, the shell's
//! window flag `__tp_dr` is set to `1` (beside `__tp_halt`). Every `svc` op
//! after a unit op in the same window is rendered with
//! [`crate::ops::svc::SvcSpec::render_exec_reload`]: with the flag set and
//! systemd running it runs `systemctl daemon-reload` first, and after the
//! op ran (not when its guard skipped it) the parent shell clears the flag.
//! So any number of changed units cost one reload, before the next `svc`
//! op in the window (unit, unit, svc: one reload, before the svc op).
//!
//! The flag is a variable of one shell, and the window's ops may run in
//! several (the root shell, the login shells). It is carried across them
//! the way a guard's outcome is: a run is collected before the next is
//! sent, so the executor knows from the reported outcomes whether a unit
//! changed and no `svc` op that ran came after it, and sets `__tp_dr` to
//! `1` or `0` in the next run's shell before the run's first op, when
//! the run has an `svc` op that carries the reload. Within a run the
//! shell's own variable does the work.
//!
//! The flag never outlives its window. When a unit changed and no `svc` op
//! ran after it in the window (none follows, or its guard skipped it), the
//! executor ends the window with the flush: one more op on the root shell,
//! in the halt gate, that sets the flag (the executor knows it is due),
//! runs `systemctl daemon-reload` when systemd runs, then clears the flag. A reload that ran, wherever
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
//! # Check
//!
//! [`run_check`] runs the same trace, windows and prepare, and applies
//! nothing: [`Executor::apply`] ends where the ops would be sent. On top
//! of prepare it probes every file op (read-only: a line edit is tried on
//! a temp copy) and prints `file <path>: will create|change|remove`, with
//! a unified diff under `--diff` (none for a secret file). A window runs
//! against the host as it is, so what an earlier window would change is
//! not there: ops for a user an earlier window creates are not probed and
//! a `svc` or `unit` probe says it is decided at run time. A file line
//! says `(as the host is now; ...)` when its probe may differ from what
//! apply sees: after any earlier window found a change (`the earlier
//! windows' changes may differ`), or after an earlier op of its window
//! whose declared writes name the same path or a parent directory
//! (`this window's earlier ops may differ`; a `*` write does not count).
//! At an implicit barrier the engine stops ([`Executor::check_only`]).
//! Each host ends with `[prepare] N will change, K ok, B blocked` and a
//! `[coverage]` line per role. N and K count trace ops, as `[coverage]`
//! does (a merged pkg op once per member): N the ops with a pending
//! finding, K the ops prepare probed and found settled. Ops decided at
//! run time (an `sh` with its own test, a guarded op not settled by the
//! check, an op whose line says it is decided at run time, an annotated
//! file that is settled) are in neither. The exit status follows the
//! findings, not N.
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
    Applied, Crossed, Engine, EngineError, Executor, HostInfo, RebootAt, Split, Window, WindowKind,
    WindowOp,
};
use crate::facts::query::{Fact, FactQuery, check_run_probe, parse_check_run};
use crate::host::Host;
use crate::key::Value;
use crate::ops::pkg::{PkgMode, Wanted};
use crate::pool::{Event, Shell};
use crate::prepare;
use crate::report::{
    CheckInfo, CheckStop, Coverage, FileDiff, HostReport, HostStatus, OpRecord, Outcome, Report,
};
use crate::role::Playbook;
use crate::trace::{Guard, NodeId, OpNode, OpSpec, Principal, RunAs};
use crate::trailer::{OpKind, Trailer, wrap};
use crate::transport::docker::DockerTransport;
use crate::transport::local::LocalTransport;
use crate::transport::ssh::SshTransport;
use crate::transport::{Limit, Shells, Transport, read_os};

#[path = "cache_cmd.rs"]
mod cache_cmd;
pub use cache_cmd::{CacheAction, run_cache};
#[path = "reboot.rs"]
mod reboot;
pub use reboot::{RebootPolicy, RebootRecord, RebootWait};

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

/// A host to run on: its name, how to reach it, the inventory groups it is
/// in (`h.in_group(..)`; none for `--local` and `--host`), the playbook
/// variables.
pub struct HostSpec<V> {
    pub name: String,
    pub target: Target,
    pub groups: Vec<String>,
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

/// The run controls of `--serial`, `--max-fail` and `--only` ([`run_with`]).
/// The default is no control: every host at once, every role.
#[derive(Debug, Clone, Default)]
pub struct RunControls {
    /// At most this many hosts at a time (`None`: all at once). Hosts start
    /// in the order given, each as a running one ends.
    pub serial: Option<usize>,
    /// Stop starting hosts once more than this many have ended not
    /// successfully (failed, blocked or errored). Hosts already started
    /// finish; each one never started is reported `NotStarted`. It can only
    /// hold a host back that `serial` has queued, so without `serial` every
    /// host has started already.
    pub max_fail: Option<usize>,
    /// Run only these roles (see [`Host::with_only`]); empty runs them all.
    pub only: Vec<String>,
    /// `--reboot` (see [`RebootPolicy`]); the default reboots at a pending
    /// barrier and only reports at the end.
    pub reboot: RebootPolicy,
    /// How long a rebooted host is waited for (tests inject short ones).
    pub reboot_wait: RebootWait,
}

/// The host limiter: `serial` permits, and the count of hosts that failed.
struct Limiter {
    permits: Option<tokio::sync::Semaphore>,
    max_fail: Option<usize>,
    failed: std::cell::Cell<usize>,
}

impl Limiter {
    fn new(serial: Option<usize>, max_fail: Option<usize>) -> Limiter {
        Limiter {
            permits: serial.map(tokio::sync::Semaphore::new),
            max_fail,
            failed: std::cell::Cell::new(0),
        }
    }

    /// Has `--max-fail` tripped: more than `n` hosts have failed.
    fn tripped(&self) -> Option<usize> {
        self.max_fail.filter(|n| self.failed.get() > *n)
    }
}

/// Wait for a `--serial` slot (the semaphore is fair, so hosts start in
/// the order given), then run the host unless `--max-fail` has tripped; a
/// host that ended without success counts toward it.
async fn gated(
    limiter: Rc<Limiter>,
    sink: Rc<Sink>,
    name: String,
    check: Option<CheckOpts>,
    run: impl std::future::Future<Output = HostReport>,
) -> HostReport {
    let _permit = match &limiter.permits {
        Some(s) => Some(s.acquire().await.expect("the limiter is never closed")),
        None => None,
    };
    if let Some(n) = limiter.tripped() {
        let mut r = HostReport::new(&name);
        r.check = check.map(|_| CheckInfo::default());
        r.status = HostStatus::NotStarted(format!("max-fail {n} reached"));
        sink.event(
            json!({"ev":"host_status","host":name,"status":r.status.label(),"detail":r.status_detail()}),
        );
        return r;
    }
    let rep = run.await;
    if !rep.status.is_success() {
        limiter.failed.set(limiter.failed.get() + 1);
    }
    rep
}

/// `--check`: trace and prepare, apply nothing ([`run_check`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct CheckOpts {
    /// `--diff`: a unified diff for every file that will change.
    pub diff: bool,
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

/// A host's open shells: the root shell, one login shell per user opened
/// so far (keyed by user name, `"root"` for `as_user("root")`), and the
/// target that opens more.
struct HostShells {
    host: String,
    target: Rc<Target>,
    root: Shell,
    logins: BTreeMap<String, Shell>,
    /// Open login shells marked stale (T114): user name -> the rule that
    /// staled it and the op it came after. A stale shell is closed and
    /// reopened, lazily, by the next [`HostShells::open_login`] for it.
    stale: BTreeMap<String, (&'static str, usize)>,
    /// Homes learned from `getent passwd` (root's, when its login shell
    /// opens), for matching an absolute profile path to its user.
    homes: BTreeMap<String, String>,
    sink: Rc<Sink>,
    /// The second shell that stops an op, opened when the first timeout
    /// fires ([`Killer`]).
    killer: Killer,
    /// `(sum, dir)` of every cache entry this run's prepare found or put in a
    /// cache directory, for the end-of-run probe (T135).
    cache_uses: BTreeSet<(String, String)>,
    /// The sums every prepared window's fetch ops name (T122).
    fetched_sums: BTreeSet<String>,
}

impl HostShells {
    /// Close the login shells, then the root shell, then the second shell,
    /// then the transport's connections (SSH).
    async fn close(self) {
        for (_, sh) in self.logins {
            sh.close().await;
        }
        self.root.close().await;
        if let Some(k) = self.killer.shell {
            k.close().await;
        }
        close_target(&self.target).await;
    }

    /// Drop the host's connection now: every shell is ended without
    /// waiting, then the transport's connections are closed. The host
    /// cannot run anything more. (`close` still works afterwards.)
    async fn drop_connections(&mut self) {
        self.root.abort();
        for sh in self.logins.values_mut() {
            sh.abort();
        }
        if let Some(k) = self.killer.shell.as_mut() {
            k.abort();
        }
        close_target(&self.target).await;
    }

    /// Mark the open login shells `scope` reaches stale, after op `after`
    /// (a shell not open reads the new profile when it opens; one already
    /// stale keeps its first reason).
    fn mark_stale(&mut self, scope: &crate::trace::StaleScope, rule: &'static str, after: usize) {
        for name in self.logins.keys() {
            if scope.hits(name) {
                self.stale.entry(name.clone()).or_insert((rule, after));
            }
        }
    }

    /// The shell `who`'s ops run in: the root shell for a plain op, else
    /// the login shell of `who`'s user.
    fn shell_of(&mut self, who: &RunAs) -> Option<&mut Shell> {
        match (&who.principal, who.login) {
            (_, false) => Some(&mut self.root),
            (Principal::Root, true) => self.logins.get_mut(ROOT),
            (Principal::User(u), true) => self.logins.get_mut(u),
        }
    }

    /// `who`'s shell and the second shell, for the watchdog. The second
    /// shell is always a root shell: it signals the op's group from
    /// outside, whichever login shell holds the op.
    fn watched(&mut self, who: &RunAs) -> Option<(&mut Shell, &mut Killer)> {
        let sh = match (&who.principal, who.login) {
            (_, false) => &mut self.root,
            (Principal::Root, true) => self.logins.get_mut(ROOT)?,
            (Principal::User(u), true) => self.logins.get_mut(u)?,
        };
        Some((sh, &mut self.killer))
    }
}

async fn close_target(t: &Target) {
    if let Target::Ssh(t) = t {
        t.close().await;
    }
}

// ---- the watchdog: the controller's clock and the second shell ----------

use std::collections::VecDeque;
use std::time::Duration;
use tokio::time::Instant;

/// The timing of the kill sequence. Policy: TERM to the op's process group
/// once a second for five seconds, then KILL. Tests pass shorter bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KillBounds {
    /// Between two TERMs.
    pub term_every: Duration,
    /// How long TERM is repeated before KILL.
    pub grace: Duration,
    /// How long to wait for the op's own trailer once the killer reports
    /// the group gone, before the result is made up.
    pub settle: Duration,
}

impl KillBounds {
    pub const POLICY: KillBounds = KillBounds {
        term_every: Duration::from_secs(1),
        grace: Duration::from_secs(5),
        settle: Duration::from_secs(1),
    };
}

/// The second shell: opened as the root login shell is (a root shell of
/// the same target, so the same PID namespace), once, when the first
/// timeout fires, then reused.
pub struct Killer {
    target: Rc<Target>,
    host: String,
    shell: Option<Shell>,
    next_id: usize,
}

/// Why the watchdog took an op out of the stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stopped {
    /// The clock expired and the controller killed the op's group. `trailer`
    /// is the op's own trailer (rc 143) when it arrived.
    TimedOut { trailer: Option<Trailer> },
    /// The clock expired and the op's process was already gone without a
    /// trailer (it ended before its EXIT trap ran).
    EndedWithoutResult,
    /// No start line came within the limit: the shell is wedged before the
    /// op began. The shell was aborted.
    NeverStarted,
    /// The second shell could not be had (or failed). The shell was
    /// aborted; the host's connection is the caller's to drop.
    NoSecondShell(String),
}

/// What [`Watch::next`] hands back.
#[derive(Debug)]
pub enum Tick {
    Event(Event),
    Stopped {
        id: usize,
        why: Stopped,
    },
    /// No op is in flight.
    Idle,
}

/// How long the second shell gets for one round trip.
const KILLER_ROUND: Duration = Duration::from_secs(15);

/// The kill script for the op whose subshell is `pid` (its process group
/// under `set -m`) and whose parent is the persistent shell `shell_pid`
/// (the start line's two numbers), run in the second shell like any op:
/// through its stdin, never on a command line.
///
/// It prints `TPK_GONE` (the group is not ours or has no live member: the
/// leader is gone, or `pid` now belongs to a process that is not the op,
/// whose parent is not `shell_pid`), `TPK_SENT` (a TERM went to the
/// group) or, for KILL, `TPK_GONE` or `TPK_STUCK` after waiting up to
/// four seconds for the group to empty.
fn kill_script(pid: i32, shell_pid: i32, kill: bool) -> String {
    let (sig, after) = if kill {
        (
            "KILL",
            "  i=0; res=TPK_STUCK\n\
             \x20 while [ \"$i\" -lt 40 ]; do\n\
             \x20   if [ \"$(__tpk_n)\" -eq 0 ]; then res=TPK_GONE; break; fi\n\
             \x20   sleep 0.1; i=$((i+1))\n\
             \x20 done\n\
             \x20 echo $res\n",
        )
    } else {
        ("TERM", "  echo TPK_SENT\n")
    };
    format!(
        "pid={pid}; sp={shell_pid}\n\
         __tpk_n() {{\n\
         \x20 local n=0 f l r\n\
         \x20 for f in /proc/[0-9]*/stat; do\n\
         \x20   read -r l < \"$f\" 2>/dev/null || continue\n\
         \x20   r=${{l##*) }}; set -- $r\n\
         \x20   if [ \"$3\" = \"$pid\" ] && [ \"$1\" != Z ]; then n=$((n+1)); fi\n\
         \x20 done\n\
         \x20 echo $n\n\
         }}\n\
         lead=0; foreign=0\n\
         if read -r l < /proc/$pid/stat 2>/dev/null; then\n\
         \x20 r=${{l##*) }}; set -- $r\n\
         \x20 if [ \"$3\" = \"$pid\" ] && [ \"$1\" != Z ]; then\n\
         \x20   if [ \"$2\" = \"$sp\" ]; then lead=1; else foreign=1; fi\n\
         \x20 fi\n\
         fi\n\
         ours=0\n\
         if [ \"$lead\" = 1 ]; then ours=1\n\
         elif [ \"$foreign\" != 1 ] && [ \"$(__tpk_n)\" -gt 0 ]; then ours=1; fi\n\
         if [ \"$ours\" = 0 ]; then\n\
         \x20 echo TPK_GONE\n\
         else\n\
         \x20 kill -{sig} -- \"-$pid\" 2>/dev/null\n\
         {after}\
         fi\n"
    )
}

/// What the second shell said about one round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Round {
    Gone,
    Sent,
    Stuck,
}

impl Killer {
    pub fn new(target: Rc<Target>, host: String) -> Killer {
        Killer {
            target,
            host,
            shell: None,
            next_id: PROBE_BASE << 1,
        }
    }

    /// The second shell, opened on first use as a root shell
    /// (a root shell of the same target, so the same PID namespace, where
    /// root can signal any principal's op).
    async fn shell(&mut self) -> Result<&mut Shell, String> {
        if self.shell.is_none() {
            let who = Principal::Root;
            let mut s = self.target.open(&self.host, &who, 1).await;
            if s.got.is_empty() {
                return Err(refusal(&self.target, &self.host, &who, s.limit));
            }
            let mut sh = s.got.remove(0);
            for extra in s.got {
                extra.close().await;
            }
            if let Err(e) = init_shell(&mut sh, None).await {
                sh.close().await;
                return Err(format!("could not set up the second shell: {e}"));
            }
            self.shell = Some(sh);
        }
        Ok(self.shell.as_mut().expect("just opened"))
    }

    /// One round: the kill script for `pid` (`kill`: KILL, else TERM).
    async fn round(&mut self, pid: i32, shell_pid: i32, kill: bool) -> Result<Round, String> {
        let id = self.next_id;
        self.next_id += 1;
        let script = kill_script(pid, shell_pid, kill);
        let sh = self.shell().await?;
        let (_, out) = tokio::time::timeout(KILLER_ROUND, prepare::probe(sh, id, &script))
            .await
            .map_err(|_| "the second shell did not answer".to_string())??;
        if out.contains("TPK_GONE") {
            Ok(Round::Gone)
        } else if out.contains("TPK_SENT") {
            Ok(Round::Sent)
        } else if out.contains("TPK_STUCK") {
            Ok(Round::Stuck)
        } else {
            Err(format!("the second shell said {:?}", out.trim()))
        }
    }
}

/// The current op's clock and, once it has expired, the kill.
struct Cur {
    id: usize,
    limit: Duration,
    /// Expiry: the limit after the start line, or, until one comes, after
    /// the op became the shell's current op.
    deadline: Instant,
    /// `(pid, shell pid)` of the start line.
    start: Option<(i32, i32)>,
    kill: Option<Kill>,
    /// The op's own trailer came after a signal, while members of its group
    /// may remain (a background child that forked late or ignores TERM):
    /// it is kept, and the kill goes on (TERM through the grace, then
    /// KILL) until the killer reports the group gone.
    held: Option<Held>,
}

enum Held {
    Trailer(Trailer),
    Lost,
}

impl From<Held> for Stopped {
    fn from(h: Held) -> Stopped {
        Stopped::TimedOut {
            trailer: match h {
                Held::Trailer(t) => Some(t),
                Held::Lost => None,
            },
        }
    }
}

struct Kill {
    t0: Instant,
    /// When the next round runs.
    next: Instant,
    /// A TERM or KILL went out: whatever ends the op now is our doing.
    signalled: bool,
    /// The killer reported the group gone: wait until `.0` for the op's
    /// own trailer, then make the result up. `.1`: the controller killed
    /// it (else it ended by itself without a trailer).
    verdict: Option<(Instant, bool)>,
}

/// The clock of one shell's run of ops: ops go in in send order, and the
/// first one not yet finished is the current op. One clock per shell, as
/// only one op of a shell runs at a time.
///
/// The clock starts at the op's start line ([`Event::Started`]). If none
/// comes it starts when the op becomes the current op (at the previous
/// op's end, or at [`Watch::new`] for the first), and its expiry then
/// drops the shell ([`Stopped::NeverStarted`]). On expiry with a start
/// line, in the second shell ([`Killer`]): check the PID is still the
/// op's; send TERM to its process group every `term_every` for `grace`,
/// then KILL (one TERM can be lost to a child that forked but has not yet
/// exec'd and still carries the op's TERM trap). The op's own rc-143
/// trailer is taken if it comes, but a signalled op ends only when its
/// group is empty: the trailer is held while the TERM rounds and the KILL
/// go on (a background child may outlive it), and the events of later ops
/// are replayed after the result; if no trailer comes, the result is made up once
/// the killer reports the group gone. A trailer that comes later is
/// ignored ([`Shell::abandon`]).
pub struct Watch {
    bounds: KillBounds,
    queue: VecDeque<(usize, Duration)>,
    cur: Option<Cur>,
    /// Events of later ops that came while the current op's group was
    /// still being finished after its trailer ([`Cur::held`]); replayed in
    /// order once the current op's result has gone out.
    replay: VecDeque<Event>,
}

impl Watch {
    pub fn new(bounds: KillBounds, ops: impl IntoIterator<Item = (usize, Duration)>) -> Watch {
        let mut w = Watch {
            bounds,
            queue: ops.into_iter().collect(),
            cur: None,
            replay: VecDeque::new(),
        };
        w.arm();
        w
    }

    /// The next op becomes the current op; its clock starts now.
    fn arm(&mut self) {
        self.cur = self.queue.pop_front().map(|(id, limit)| Cur {
            id,
            limit,
            deadline: Instant::now() + limit,
            start: None,
            kill: None,
            held: None,
        });
    }

    /// The next event of `sh`, or the end of the current op by the
    /// controller.
    pub async fn next(&mut self, sh: &mut Shell, killer: &mut Killer) -> Tick {
        loop {
            // Events that came out of order are replayed first, unless the
            // current op is again waiting for its group.
            if self.cur.as_ref().is_none_or(|c| c.held.is_none())
                && let Some(ev) = self.replay.pop_front()
            {
                if let Some(t) = self.seen(ev) {
                    return t;
                }
                continue;
            }
            let Some(cur) = self.cur.as_ref() else {
                return match sh.next_event().await {
                    Some(e) => Tick::Event(e),
                    None => Tick::Idle,
                };
            };
            let wake = match &cur.kill {
                Some(k) => k.verdict.map_or(k.next, |v| v.0),
                None => cur.deadline,
            };
            // The clock is checked before the stream, so an op that never
            // stops printing is still stopped.
            if Instant::now() >= wake {
                if let Some(t) = self.expired(sh, killer).await {
                    return t;
                }
                continue;
            }
            tokio::select! {
                ev = sh.next_event() => {
                    match ev {
                        Some(ev) => {
                            if let Some(t) = self.seen(ev) {
                                return t;
                            }
                        }
                        // Nothing in flight in the shell. An op waiting for
                        // its group still has the killer to run.
                        None if self.cur.as_ref().is_some_and(|c| c.held.is_some()) => {
                            tokio::time::sleep_until(wake).await;
                        }
                        None => return Tick::Idle,
                    }
                }
                _ = tokio::time::sleep_until(wake) => {}
            }
        }
    }

    /// An event passes through; it may start the clock or end the op.
    /// `None`: held back until the current op's result has gone out.
    fn seen(&mut self, ev: Event) -> Option<Tick> {
        let Some(cur) = self.cur.as_mut() else {
            return Some(Tick::Event(ev));
        };
        if cur.held.is_some() {
            // The op's own trailer is in, and its group is being finished.
            // Its further output passes; whatever belongs to a later op
            // waits its turn.
            return match &ev {
                Event::Output(id, _) if *id == cur.id => Some(Tick::Event(ev)),
                _ => {
                    self.replay.push_back(ev);
                    None
                }
            };
        }
        let signalled = cur.kill.as_ref().is_some_and(|k| k.signalled);
        // The group is not yet known to be gone: the trailer is kept and
        // the kill goes on until it is.
        let hold = signalled && cur.kill.as_ref().is_some_and(|k| k.verdict.is_none());
        match &ev {
            Event::Started { id, pid, shell_pid } if *id == cur.id && cur.kill.is_none() => {
                cur.start = Some((*pid, *shell_pid));
                cur.deadline = Instant::now() + cur.limit;
            }
            Event::Done(id, t) if *id == cur.id => {
                let (id, t) = (*id, t.clone());
                if hold {
                    cur.held = Some(Held::Trailer(t));
                    if let Some(k) = cur.kill.as_mut() {
                        k.next = Instant::now();
                    }
                    return None;
                }
                self.arm();
                if signalled {
                    return Some(Tick::Stopped {
                        id,
                        why: Stopped::TimedOut { trailer: Some(t) },
                    });
                }
            }
            Event::Failed(id, e) if *id == cur.id => {
                let id = *id;
                let lost = matches!(e, crate::pool::ShellError::TrailerLost);
                if hold && lost {
                    cur.held = Some(Held::Lost);
                    if let Some(k) = cur.kill.as_mut() {
                        k.next = Instant::now();
                    }
                    return None;
                }
                self.arm();
                if signalled && lost {
                    return Some(Tick::Stopped {
                        id,
                        why: Stopped::TimedOut { trailer: None },
                    });
                }
            }
            Event::Done(id, _) | Event::Failed(id, _) => {
                // A later op of the run ended: the current one is over.
                if let Some(pos) = self.queue.iter().position(|(i, _)| i == id) {
                    self.queue.drain(..=pos);
                    self.arm();
                }
            }
            _ => {}
        }
        Some(Tick::Event(ev))
    }

    /// The clock of the current op ran out (or the settle time after the
    /// killer's report). `None`: carry on.
    async fn expired(&mut self, sh: &mut Shell, killer: &mut Killer) -> Option<Tick> {
        let now = Instant::now();
        let b = self.bounds;
        let cur = self.cur.as_mut()?;
        let id = cur.id;
        // The group is gone and the op's own trailer did not come.
        if let Some(k) = &cur.kill
            && let Some((until, timed)) = k.verdict
            && now >= until
        {
            sh.abandon(id);
            self.arm();
            let why = if timed {
                Stopped::TimedOut { trailer: None }
            } else {
                Stopped::EndedWithoutResult
            };
            return Some(Tick::Stopped { id, why });
        }
        let Some((pid, shell_pid)) = cur.start else {
            // No start line: the shell is wedged before the op began and
            // there is no PID to kill.
            sh.abandon(id);
            sh.abort();
            self.cur = None;
            self.queue.clear();
            self.replay.clear();
            return Some(Tick::Stopped {
                id,
                why: Stopped::NeverStarted,
            });
        };
        let k = cur.kill.get_or_insert(Kill {
            t0: now,
            next: now,
            signalled: false,
            verdict: None,
        });
        let kill = now.duration_since(k.t0) >= b.grace;
        match killer.round(pid, shell_pid, kill).await {
            Err(reason) => {
                sh.abandon(id);
                sh.abort();
                self.cur = None;
                self.queue.clear();
                self.replay.clear();
                Some(Tick::Stopped {
                    id,
                    why: Stopped::NoSecondShell(reason),
                })
            }
            Ok(r) => {
                let now = Instant::now();
                match r {
                    Round::Sent => {
                        k.signalled = true;
                        k.next = now + b.term_every;
                    }
                    Round::Gone | Round::Stuck if cur.held.is_some() => {
                        // The trailer is in hand and the group is empty (or
                        // beyond a KILL): nothing left to wait for.
                        let h = cur.held.take().expect("checked");
                        self.arm();
                        return Some(Tick::Stopped { id, why: h.into() });
                    }
                    Round::Gone | Round::Stuck => {
                        // After a KILL the op is ours even if the group had
                        // emptied already.
                        k.signalled |= kill;
                        k.verdict = Some((now + b.settle, k.signalled));
                    }
                }
                None
            }
        }
    }
}

/// How a probe under the controller's clock ended ([`probe_bounded`]).
#[derive(Debug)]
pub enum Bounded {
    /// The probe's own trailer came: its rc and output.
    Done(i32, String),
    /// The controller stopped it, with the output it had printed.
    Stopped(Stopped, String),
}

/// Run `script` in `sh` as a plain probe (as [`prepare::probe`] does) under
/// the same watchdog as an op ([`Watch`]): its clock is `limit` from its
/// start line; on expiry the second shell kills its process group. The
/// probe is the only op in flight, so the helper's queue is one op long.
/// `Err` is a shell that died, not a stop.
pub async fn probe_bounded(
    sh: &mut Shell,
    killer: &mut Killer,
    bounds: KillBounds,
    id: usize,
    script: &str,
    limit: Duration,
) -> Result<Bounded, String> {
    let wrapped = wrap(
        OpKind::Sh,
        &crate::ops::shell::raw_body(script),
        id,
        sh.nonce(),
    );
    sh.send(id, &wrapped).await.map_err(|e| e.to_string())?;
    let mut watch = Watch::new(bounds, [(id, limit)]);
    let mut out: Vec<u8> = Vec::new();
    loop {
        match watch.next(sh, killer).await {
            Tick::Event(Event::Output(i, b)) if i == id => out.extend_from_slice(&b),
            Tick::Event(Event::Done(i, t)) if i == id => {
                let rc = t
                    .typed::<crate::trailer::ShellResult>()
                    .map_err(|e| e.to_string())?
                    .rc;
                return Ok(Bounded::Done(
                    rc,
                    String::from_utf8_lossy(&out).into_owned(),
                ));
            }
            Tick::Event(Event::Failed(i, e)) if i == id => return Err(e.to_string()),
            Tick::Event(_) => {}
            Tick::Stopped { why, .. } => {
                return Ok(Bounded::Stopped(
                    why,
                    String::from_utf8_lossy(&out).into_owned(),
                ));
            }
            Tick::Idle => return Err("shell ended without a result".to_string()),
        }
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
        // The local transport's text is complete (T120).
        (Principal::User(u), Target::Local(t), Some(Limit::SudoRefused)) => t
            .message()
            .unwrap_or_else(|| format!("sudo -n -iu {u} bash refused on this machine")),
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

/// The path a login shell started under `env -i` begins with: a profile
/// may add to it, as at a terminal.
const LOGIN_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// The login name `as_user("root")` is.
const ROOT: &str = "root";

/// Turn `sh`, a fresh shell of the caller that has run nothing yet, into
/// `name`'s login shell with no `sudo`: read `getent passwd name` in it,
/// then `exec env -i HOME=.. USER=.. LOGNAME=.. SHELL=.. PATH=.. <carried>
/// bash -l` typed into it. `<carried>` is [`crate::sandbox::carry_words`]:
/// the test switch and the path seams that shell has set, none in
/// production (T128). The arguments are short and no script is on a command
/// line. `Ok` is the shell and the home directory; `Err` says why, and `sh` is closed (a
/// missing passwd entry is a refusal, said so). Root's login shell
/// ([`open_root_login`]) and the local transport's shell for the calling
/// user (T120) are both this.
pub(crate) async fn start_login(mut sh: Shell, name: &str) -> Result<(Shell, String), String> {
    // One id, on a shell that has run nothing yet.
    let got = prepare::probe(
        &mut sh,
        1,
        &format!("getent passwd {}", crate::sh::quote(name)),
    )
    .await;
    let fields = match got {
        Ok((0, out)) => out
            .lines()
            .map(|l| l.split(':').collect::<Vec<_>>())
            .find(|f| f.len() >= 7 && f[0] == name)
            .map(|f| (f[5].to_string(), f[6].to_string())),
        Ok(_) => None,
        Err(e) => {
            sh.close().await;
            return Err(format!("could not open the login shell for {name}: {e}"));
        }
    };
    let Some((home, login_shell)) = fields.filter(|(h, _)| !h.is_empty()) else {
        sh.close().await;
        return Err(format!(
            "no passwd entry for {name}: cannot open its login shell (as_user(\"{name}\"))"
        ));
    };
    let q = crate::sh::quote;
    let command = format!(
        "env -i HOME={} USER={} LOGNAME={} SHELL={} PATH={LOGIN_PATH} {} bash -l",
        q(&home),
        q(name),
        q(name),
        q(&login_shell),
        crate::sandbox::carry_words(),
    );
    if let Err(e) = sh.exec(&command, name).await {
        sh.close().await;
        return Err(format!("could not start the login shell for {name}: {e}"));
    }
    Ok((sh, home))
}

/// Open root's login shell: one more root shell of the transport, turned
/// into root's login shell by [`start_login`]. `Err` says why.
async fn open_root_login(target: &Target, host: &str) -> Result<(Shell, String), String> {
    let who = Principal::Root;
    let mut s = target.open(host, &who, 1).await;
    if s.got.is_empty() {
        return Err(refusal(target, host, &who, s.limit));
    }
    let sh = s.got.remove(0);
    for extra in s.got {
        extra.close().await;
    }
    start_login(sh, ROOT).await
}

impl prepare::ShellSet for HostShells {
    fn shell(&mut self, who: &RunAs) -> Option<&mut Shell> {
        self.shell_of(who)
    }

    fn note_cache_uses(&mut self, out: &str) {
        self.cache_uses.extend(crate::ops::fetch::parse_uses(out));
    }

    fn note_fetch_sums(&mut self, sums: Vec<String>) {
        self.fetched_sums.extend(sums);
    }

    fn watched_shell(&mut self, who: &RunAs) -> Option<(&mut Shell, &mut Killer)> {
        self.watched(who)
    }

    async fn open_login(&mut self, who: &RunAs) -> Result<(), String> {
        if !who.login {
            return Ok(());
        }
        let name = match &who.principal {
            Principal::Root => ROOT.to_string(),
            Principal::User(u) => u.clone(),
        };
        // A stale shell is closed and opened again: the profile is read
        // once more. A shell never used again never gets here.
        let reopened = match self.stale.remove(&name) {
            Some(why) => {
                if let Some(old) = self.logins.remove(&name) {
                    old.close().await;
                }
                Some(why)
            }
            None => None,
        };
        if self.logins.contains_key(&name) {
            return Ok(());
        }
        let mut sh = match &who.principal {
            Principal::Root => {
                let (sh, home) = open_root_login(&self.target, &self.host).await?;
                self.homes.insert(name.clone(), home);
                sh
            }
            Principal::User(u) => {
                let p = Principal::User(u.clone());
                let mut s = self.target.open(&self.host, &p, 1).await;
                if s.got.is_empty() {
                    return Err(refusal(&self.target, &self.host, &p, s.limit));
                }
                let sh = s.got.remove(0);
                // More than asked for is not expected; close the rest.
                for extra in s.got {
                    extra.close().await;
                }
                sh
            }
        };
        if let Err(e) = init_shell(&mut sh, None).await {
            sh.close().await;
            return Err(format!("could not set up the shell for user {name}: {e}"));
        }
        self.logins.insert(name.clone(), sh);
        if let Some((rule, after)) = reopened {
            self.sink.line(&format!(
                "   login shell {name} reopened: {rule} after #{after}"
            ));
            self.sink
                .event(json!({"ev":"login_reopened","host":self.host,
                "user":name,"rule":rule,"after":after}));
        }
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
    /// A file or unit op was sent to be applied: it may have saved undo
    /// data under the run's directories ([`crate::ops::undo`]).
    undo_used: bool,
    /// `--check`: what the run has found so far.
    check: Option<CheckState>,
    /// Reboot policy and state ([`reboot`]).
    reboot: reboot::RebootState,
}

/// What a `--check` carries from window to window of one host.
struct CheckState {
    opts: CheckOpts,
    info: CheckInfo,
    /// Trace ops already counted (a window declined for its head comes
    /// back as two).
    counted: BTreeSet<NodeId>,
    /// Users an earlier window's `user` op creates, and which window.
    future_users: BTreeMap<String, usize>,
    /// The first window that found a change: what later windows probe
    /// does not have it yet.
    first_change: Option<usize>,
    /// A window was split for a missing downloader: its head installs one.
    downloader_pending: bool,
    /// Trace ops (members) already tallied into `will_ops` / `ok_ops`.
    tallied: BTreeSet<NodeId>,
    /// What prepare says per member of the window being checked, for the
    /// kinds it probes itself (not files and `sh`, which
    /// [`Executor::check_window_end`] adds). `None`: decided at run time,
    /// or not probed.
    states: HashMap<NodeId, Option<OpState>>,
}

/// What a check found for one trace op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpState {
    /// A finding says it will change.
    Will,
    /// Prepare probed it and found it settled.
    Ok,
}

/// The note on a file line whose probe may not match what apply sees.
const AS_NOW_EARLIER_WINDOWS: &str =
    "(as the host is now; the earlier windows' changes may differ)";
const AS_NOW_THIS_WINDOW: &str = "(as the host is now; this window's earlier ops may differ)";

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
/// the variable and the plan carries no per-process value. A login shell
/// has it as a variable of its own, like the root shell. Under
/// `TP_SANDBOX_REQUIRED=1` (the tests) the handshake also checks, once,
/// that the stage, cache, run directory and `TP_FS_ROOTS` lie under
/// `$TP_SANDBOX`, and the open fails with the refusal when they do not.
///
/// `owner`, for the root shell of a run outside `--check` only
/// ([`cache_owner`]): the encoded owner directory prepare marks under when it
/// touches a cache item (T135).
async fn init_shell(shell: &mut Shell, owner: Option<&str>) -> Result<(), String> {
    // An op's wrapper runs its body in a subshell, so the assignment goes
    // in front of it, in the shell itself; the wrapped no-op is the
    // handshake that it ran.
    let ack = crate::trailer::wrap(
        crate::trailer::OpKind::Sh,
        &crate::ops::shell::raw_body(&format!(
            "{}{}",
            crate::sandbox::SBX,
            crate::sandbox::INIT_CHECK
        )),
        0,
        shell.nonce(),
    );
    // `set -m`: job control, so each op's subshell is its own process
    // group (pgid = its pid), which a controller can signal from a second
    // shell. It also makes bash report a signalled job; the op wrapper
    // sends that notice to /dev/null ([`crate::trailer`]).
    // The undo helpers and their version (T126): defined once here, so an op
    // script carries a guard line and no helper text.
    let script = format!(
        "set -m\n{}{}{ack}",
        crate::ops::fetch::shell_init(owner),
        crate::ops::undo::init_text(),
    );
    shell.send(0, &script).await.map_err(|e| e.to_string())?;
    let (rc, out) = prepare::collect(shell, 0).await?;
    if rc == 0 {
        Ok(())
    } else {
        Err(format!("exit {rc}: {out}"))
    }
}

/// The owner directory a run's root shell marks cache items under (T135),
/// already encoded ([`crate::ops::fetch::owner_dir`]): none under
/// `--check`, which changes nothing on the host.
fn cache_owner(playbook: &str, check: bool) -> Option<String> {
    (!check && crate::ops::fetch::valid_playbook_name(playbook))
        .then(|| crate::ops::fetch::owner_dir(playbook))
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

/// The `unit` ops of `ops` that changed and that no `svc` op that ran
/// came after, by the outcomes reported so far: what the window flag
/// `__tp_dr` stands for after `ops`, whichever shell each ran in.
fn pending_units<'a>(report: &HostReport, ops: &'a [WindowOp]) -> Vec<&'a WindowOp> {
    let outcome = |id: NodeId| {
        report
            .ops
            .iter()
            .rev()
            .find(|o| o.id == id.0)
            .map(|o| o.outcome)
    };
    let mut pending: Vec<&WindowOp> = Vec::new();
    for w in ops {
        match (&w.node.spec, outcome(w.id)) {
            (OpSpec::Unit(_), Some(Outcome::Changed)) => pending.push(w),
            (
                OpSpec::Svc(_),
                Some(Outcome::Changed | Outcome::Unchanged | Outcome::AllowedFail),
            ) => pending.clear(),
            _ => {}
        }
    }
    pending
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
                Wanted::Absent => "absent".to_string(),
            };
            format!("{} {mode} {}", p.kind(), p.names.join(" "))
        }
        OpSpec::Pkg(p) => {
            let mode = match (p.mode, &p.wanted) {
                (PkgMode::BuildDep, _) => "build-dep",
                (_, Wanted::Latest) => "latest",
                (_, Wanted::Pinned(_)) => "pinned",
                (_, Wanted::Absent) if p.purge => "purge",
                (_, Wanted::Absent) => "absent",
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
        OpSpec::Git(g) if g.path.is_empty() => format!("git {}", g.url),
        OpSpec::Git(g) => format!("git {}", g.path),
        OpSpec::Resource(r) => format!("{} {}", r.kind, r.name),
        OpSpec::Wait(w) => format!("wait_for {}", w.describe()),
        OpSpec::Relogin(u) => format!("relogin {u}"),
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
        OpKind::Git => t
            .typed::<crate::trailer::GitResult>()
            .map(|r| (r.rc, r.changed, false, None, false, String::new())),
        OpKind::Resource => t
            .typed::<crate::trailer::ResourceResult>()
            .map(|r| (r.rc, r.changed, false, None, false, String::new())),
    };
    r.map_err(|e| format!("unreadable result {:?}: {e}", t.json))
}

/// `timed out after 2s` when a `wait_for` failed with rc 124, what its
/// body exits with at its own deadline; empty for everything else. (The
/// controller's timeouts are never keyed on an rc: they come as
/// [`Stopped`].)
fn timed_out(w: &WindowOp, rc: i32) -> String {
    if rc == 124 && matches!(w.node.spec, OpSpec::Wait(_)) {
        format!(
            "timed out after {}",
            crate::trace::fmt_duration(w.node.effective_timeout())
        )
    } else {
        String::new()
    }
}

/// How long the controller waits before attempt `k + 1` of a retried op,
/// `k` being the attempt that just failed (1-based): 1, 2, 4 .. 30
/// seconds.
pub fn retry_wait(k: u32) -> Duration {
    Duration::from_secs((1u64 << k.saturating_sub(1).min(5)).min(30))
}

fn last_lines(bytes: &[u8], n: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

impl HostExec {
    fn new(name: &str, shells: HostShells, sink: Rc<Sink>, check: Option<CheckOpts>) -> HostExec {
        let check = check.map(|opts| CheckState {
            opts,
            info: CheckInfo::default(),
            counted: BTreeSet::new(),
            future_users: BTreeMap::new(),
            first_change: None,
            downloader_pending: false,
            tallied: BTreeSet::new(),
            states: HashMap::new(),
        });
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
            undo_used: false,
            check,
            reboot: reboot::RebootState::default(),
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
        let pending = pending_units(&self.report, &window.ops);
        if pending.is_empty() {
            return;
        }
        self.seq += 1;
        let id = self.seq;
        let nonce = self.shells.root.nonce().to_string();
        let script = format!(
            "if [ -z \"${{__tp_halt-}}\" ]; then\n__tp_dr=1\n{}[ $? -eq 0 ] || __tp_halt=1\n__tp_dr=0\nelse\n{}fi\n",
            wrap(OpKind::Sh, &flush_body(), id, &nonce),
            crate::trailer::skip(OpKind::Sh, id, &nonce)
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

    /// One `== host [prepare] <text> ==` line.
    fn say(&self, text: String) {
        self.sink
            .line(&format!("== {} [prepare] {text} ==", self.name));
    }

    /// A check counts each trace op once, for the summary and per role for
    /// `[coverage]`.
    fn check_count(&mut self, window: &Window) {
        let Some(ck) = &mut self.check else {
            return;
        };
        for w in &window.ops {
            for m in &w.members {
                if !ck.counted.insert(m.id) {
                    continue;
                }
                ck.info.ops += 1;
                let role = if m.role.is_empty() {
                    "(no role)"
                } else {
                    m.role.as_str()
                };
                let i = match ck.info.coverage.iter().position(|c| c.role == role) {
                    Some(i) => i,
                    None => {
                        ck.info.coverage.push(Coverage {
                            role: role.to_string(),
                            ops: 0,
                            reversible: 0,
                            sh_without_undo: 0,
                        });
                        ck.info.coverage.len() - 1
                    }
                };
                let c = &mut ck.info.coverage[i];
                c.ops += 1;
                if w.node.spec.has_inverse() {
                    c.reversible += 1;
                } else if w.node.spec.kind() == "sh" {
                    c.sh_without_undo += 1;
                }
            }
        }
    }

    /// A check cannot open the shell of a user an earlier window creates:
    /// that window's ops for the user are not probed, and the line says so.
    fn check_filter(&mut self, window: &mut Window) {
        let Some(ck) = &self.check else {
            return;
        };
        let future = ck.future_users.clone();
        let mut noted: Vec<String> = Vec::new();
        window.ops.retain(|o| match &o.node.run_as.principal {
            Principal::User(u) if future.contains_key(u) => {
                if !noted.contains(u) {
                    noted.push(u.clone());
                }
                false
            }
            _ => true,
        });
        for u in noted {
            self.say(format!(
                "window {}: ops as {u} not checked: the user does not exist yet (window {} creates it)",
                window.number, future[&u]
            ));
        }
    }

    /// Has a `--check` found a change in a window before `number`?
    fn check_earlier(&self, number: usize) -> bool {
        self.check
            .as_ref()
            .is_some_and(|c| c.first_change.is_some_and(|w| w < number))
    }

    /// What prepare found for a window, for the check's summary, and the
    /// users its `user` ops will create.
    fn check_note_prepared(&mut self, window: &Window, will: &crate::prepare::Prepared) {
        self.check_states(window, will);
        let Some(ck) = &mut self.check else {
            return;
        };
        let n = will_pending(will);
        ck.info.pending += n;
        if n > 0 && ck.first_change.is_none() {
            ck.first_change = Some(window.number);
        }
        for w in &window.ops {
            let OpSpec::User(u) = &w.node.spec else {
                continue;
            };
            let secret = format!("{} <secret>", u.name);
            if will.will_create.iter().any(|c| {
                c.kind == "user" && c.detail.is_none() && (c.name == u.name || c.name == secret)
            }) {
                ck.future_users
                    .entry(u.name.clone())
                    .or_insert(window.number);
            }
        }
    }

    /// The note a file op's line carries when what it probed may differ
    /// from what apply sees: an earlier window found a change, or an
    /// earlier op of this window declares a write to the path or to a
    /// parent directory of it (a `*` write does not count; nor does a
    /// package's files, which no key names).
    fn file_annotation(&self, window: &Window, idx: usize) -> Option<&'static str> {
        let OpSpec::File(f) = &window.ops[idx].node.spec else {
            return None;
        };
        if self.check_earlier(window.number) {
            return Some(AS_NOW_EARLIER_WINDOWS);
        }
        let covers = |k: &str| {
            k == f.path
                || f.path
                    .strip_prefix(k.trim_end_matches('/'))
                    .is_some_and(|rest| rest.starts_with('/'))
        };
        window.ops[..idx]
            .iter()
            .any(|e| {
                e.node
                    .writes
                    .iter()
                    .any(|k| matches!(k, crate::key::Key::File(p) if covers(p)))
            })
            .then_some(AS_NOW_THIS_WINDOW)
    }

    /// `will` for the ops prepare probes itself, per member.
    fn check_states(&mut self, window: &Window, will: &crate::prepare::Prepared) {
        let mut states: HashMap<NodeId, Option<OpState>> = HashMap::new();
        let counting = |kind: &str, name: &str| {
            will.will_create
                .iter()
                .any(|c| c.kind == kind && c.name == name && create_counts(c))
        };
        let runtime = |kind: &str, name: &str| {
            will.will_create.iter().any(|c| {
                c.kind == kind
                    && c.name == name
                    && c.detail
                        .as_deref()
                        .is_some_and(|d| d.contains("decided at run time"))
            })
        };
        for (i, w) in window.ops.iter().enumerate() {
            let guarded = w.node.guard.is_some();
            for m in &w.members {
                let hit_names = |names: &[String]| {
                    names.iter().any(|n| {
                        will.will_install.iter().any(|i| i.names.contains(n))
                            || will.will_remove.iter().any(|r| r.names.contains(n))
                            || will.will_change.iter().any(|c| c.name == *n)
                    })
                };
                // `Some(true)`: a finding; `Some(false)`: probed, settled;
                // `None`: not decided here.
                let found: Option<bool> = match &w.node.spec {
                    OpSpec::Pkg(p) => {
                        let names = if m.pkg_names.is_empty() {
                            &p.names
                        } else {
                            &m.pkg_names
                        };
                        if hit_names(names) {
                            Some(true)
                        } else if p.mode == PkgMode::BuildDep
                            || names.iter().any(|n| n.starts_with('@'))
                        {
                            None
                        } else {
                            Some(false)
                        }
                    }
                    OpSpec::Group(g) => Some(counting("group", &g.name)),
                    OpSpec::User(u) => Some(
                        counting("user", &u.name)
                            || counting("user", &format!("{} <secret>", u.name)),
                    ),
                    OpSpec::Repo(r) => Some(counting("repo", &r.name)),
                    OpSpec::Unit(u) => {
                        let n = u.rel();
                        if counting("unit", &n) {
                            Some(true)
                        } else if runtime("unit", &n) {
                            None
                        } else {
                            Some(false)
                        }
                    }
                    OpSpec::Svc(s) => {
                        let n = s.key_name();
                        if counting("svc", &n) {
                            Some(true)
                        } else if runtime("svc", &n) || s.action().is_some() {
                            None
                        } else {
                            Some(false)
                        }
                    }
                    OpSpec::Git(g) => {
                        let pre = format!("git {}:", g.path);
                        Some(will.git_notes.iter().any(|n| n.starts_with(&pre)))
                    }
                    OpSpec::Resource(_) => Some(!will.resource_skip.contains(&w.id)),
                    OpSpec::File(f) if f.local.is_some() => {
                        let pre = format!("file {}:", f.path);
                        if will.local_notes.iter().any(|n| n.starts_with(&pre)) {
                            Some(true)
                        } else if self.file_annotation(window, i).is_some() {
                            None
                        } else {
                            Some(false)
                        }
                    }
                    _ => None,
                };
                let state = match found {
                    Some(true) => Some(OpState::Will),
                    Some(false) if !guarded => Some(OpState::Ok),
                    _ => None,
                };
                states.insert(m.id, state);
            }
        }
        if let Some(ck) = &mut self.check {
            ck.states = states;
        }
    }

    /// Put a local file's note on its prepare line when its probe may
    /// differ from what apply sees.
    fn annotate_local(&self, window: &Window, note: &str) -> String {
        for (i, w) in window.ops.iter().enumerate() {
            if let OpSpec::File(f) = &w.node.spec
                && f.local.is_some()
                && note.starts_with(&format!("file {}:", f.path))
                && let Some(a) = self.file_annotation(window, i)
            {
                return format!("{note} {a}");
            }
        }
        note.to_string()
    }

    /// The end of a window of a check: the file lines (and diffs), then
    /// the `sh` ops prepare cannot decide and the guarded ops.
    fn check_window_end(&mut self, window: &Window, files: Vec<prepare::FileFinding>) {
        let name = self.name.clone();
        let mut pending = 0;
        let mut states: HashMap<NodeId, Option<OpState>> = HashMap::new();
        for f in files {
            let idx = window.ops.iter().position(|o| o.id == f.id);
            let note = idx.and_then(|i| self.file_annotation(window, i));
            let state = match (&f.detail, note) {
                (Some(_), _) => Some(OpState::Will),
                (None, None) => Some(OpState::Ok),
                (None, Some(_)) => None,
            };
            states.insert(f.id, state);
            let Some(mut detail) = f.detail.clone() else {
                continue;
            };
            if let Some(n) = note {
                detail = format!("{detail} {n}");
            }
            pending += 1;
            let c = crate::report::WillCreate {
                window: window.number,
                kind: "file".to_string(),
                name: f.name(),
                detail: Some(detail),
            };
            self.say(c.line());
            self.sink.event(json!({"ev":"will_create","host":name,
                "window":c.window,"kind":c.kind,"name":c.name,"detail":c.detail}));
            if let Some(d) = &f.diff {
                for l in d.lines() {
                    self.sink.line(&format!("{name} | {l}"));
                }
                self.sink.event(
                    json!({"ev":"file_diff","host":name,"window":window.number,"path":f.path,"diff":d}),
                );
                self.report.diffs.push(FileDiff {
                    window: window.number,
                    path: f.path.clone(),
                    diff: d.clone(),
                });
            }
            self.report.will_create.push(c);
        }
        let mut unchecked = 0;
        for w in &window.ops {
            let svc_action = matches!(&w.node.spec, OpSpec::Svc(s) if s.action().is_some());
            match (&w.node.guard, &w.node.spec) {
                (Some(g), _) if !svc_action => {
                    // Settled by what the check found before: nothing to say.
                    if decide(g, &self.known).is_none() {
                        let m = w.members.first();
                        let subject = m.map_or_else(|| describe(&w.node), |m| subject(w, m));
                        self.say(format!(
                            "{subject}: runs only if {}",
                            guard_phrase(g, &self.subjects)
                        ));
                    }
                }
                (None, OpSpec::Shell(s)) => {
                    let always =
                        s.unless.is_none() && s.creates.is_none() && s.changed_if.is_none();
                    let d = describe(&w.node);
                    let cmd = d.strip_prefix("sh ").unwrap_or(&d).to_string();
                    states.insert(w.id, always.then_some(OpState::Will));
                    let (verb, c) = if always {
                        pending += 1;
                        ("will run", "will run")
                    } else {
                        unchecked += 1;
                        (
                            "not checked",
                            "not checked: its own test decides at run time",
                        )
                    };
                    let line = crate::report::WillCreate {
                        window: window.number,
                        kind: "sh".to_string(),
                        name: cmd,
                        detail: Some(c.to_string()),
                    };
                    self.say(line.line());
                    self.sink.event(json!({"ev":"will_create","host":name,
                        "window":line.window,"kind":line.kind,"name":line.name,"detail":verb}));
                    self.report.will_create.push(line);
                }
                _ => {}
            }
        }
        if let Some(ck) = &mut self.check {
            for w in &window.ops {
                for m in &w.members {
                    if !ck.tallied.insert(m.id) {
                        continue;
                    }
                    let st = states
                        .get(&w.id)
                        .copied()
                        .unwrap_or_else(|| ck.states.get(&m.id).copied().flatten());
                    match st {
                        Some(OpState::Will) => ck.info.will_ops += 1,
                        Some(OpState::Ok) => ck.info.ok_ops += 1,
                        None => {}
                    }
                }
            }
            ck.info.pending += pending;
            ck.info.unchecked += unchecked;
            if pending > 0 && ck.first_change.is_none() {
                ck.first_change = Some(window.number);
            }
        }
    }

    fn block(&mut self, why: String) {
        self.halted = true;
        if self.verdict.is_none() {
            self.verdict = Some(HostStatus::Blocked(why));
        }
    }

    /// After attempt `k` of `w` failed (`why`) with retries left: the
    /// stream line and event, the wait, then the same gated `script` as a
    /// new send, with the halt flag cleared first (the failed attempt set
    /// it). The new send gets its own start line and clock.
    async fn resend(
        &mut self,
        who: &RunAs,
        w: &WindowOp,
        script: &str,
        k: u32,
        why: &str,
    ) -> Result<(), String> {
        let wait = retry_wait(k);
        let of = w.node.retry + 1;
        self.sink.line(&format!(
            "{} | attempt {k} of {of} failed ({why}); retrying in {}s",
            self.name,
            wait.as_secs()
        ));
        self.sink
            .event(json!({"ev":"op_retry","host":self.name,"id":w.id.0,
            "attempt":k,"of":of,"why":why,"wait_s":wait.as_secs()}));
        tokio::time::sleep(wait).await;
        let sh = self
            .shell_for(who)
            .ok_or_else(|| "no shell to send the retry to".to_string())?;
        sh.send(w.id.0, &format!("unset __tp_halt\n{script}"))
            .await
            .map_err(|e| e.to_string())
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
            Outcome::Changed if w.node.needs_reboot => "changed; needs reboot".to_string(),
            Outcome::Changed => "changed".to_string(),
            Outcome::Unchanged if note.is_empty() => "ok".to_string(),
            Outcome::Unchanged => format!("ok ({note})"),
            Outcome::Failed if !note.is_empty() => format!("FAILED ({note})"),
            Outcome::Failed => format!("FAILED rc={rc}"),
            Outcome::AllowedFail if !note.is_empty() => format!("failed ({note}, allowed)"),
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

    /// Verify the users of `logins` exist (on the root shell), then open
    /// the login shell of each that is not open yet; `Err` blocks the host.
    async fn ensure_logins(&mut self, logins: &BTreeSet<RunAs>) -> Result<(), String> {
        use prepare::ShellSet as _;
        let users: BTreeSet<String> = logins
            .iter()
            .filter_map(|l| match &l.principal {
                Principal::User(u) => Some(u.clone()),
                Principal::Root => None,
            })
            .collect();
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
        for l in logins {
            self.shells.open_login(l).await?;
        }
        Ok(())
    }

    /// The shell `who`'s ops run in (opened by prepare or `ensure_logins`).
    fn shell_for(&mut self, who: &RunAs) -> Option<&mut Shell> {
        self.shells.shell_of(who)
    }

    /// Stream controller files whose sha256 differs, one chunk per shell
    /// script. A matching file is not sent.
    async fn stream_copies(&mut self, copies: &[crate::local::CopyItem]) -> Result<(), String> {
        for item in copies {
            for f in &item.files {
                if f.same || f.is_dir {
                    continue;
                }
                self.stream_file(&item.who, &item.dest_word, &f.rel, &f.abs)
                    .await?;
                self.fetch_staged = true;
            }
        }
        Ok(())
    }

    async fn stream_file(
        &mut self,
        who: &RunAs,
        dest_word: &str,
        rel: &str,
        src: &std::path::Path,
    ) -> Result<(), String> {
        let mut file =
            std::fs::File::open(src).map_err(|e| format!("local {}: {e}", src.display()))?;
        let mut buf = vec![0u8; crate::local::CHUNK];
        let mut truncate = true;
        loop {
            let n = std::io::Read::read(&mut file, &mut buf)
                .map_err(|e| format!("local {}: {e}", src.display()))?;
            if n == 0 {
                if truncate {
                    let script = crate::local::chunk_script(dest_word, rel, &[], true);
                    self.probe_as(who, script).await?;
                }
                break;
            }
            let script = crate::local::chunk_script(dest_word, rel, &buf[..n], truncate);
            truncate = false;
            self.probe_as(who, script).await?;
        }
        Ok(())
    }

    async fn probe_as(&mut self, who: &RunAs, script: String) -> Result<(), String> {
        self.seq += 1;
        let id = self.seq;
        let sh = self
            .shell_for(who)
            .ok_or_else(|| format!("no shell open for {who:?}"))?;
        let (rc, out) = prepare::probe(sh, id, &script).await?;
        if rc != 0 {
            return Err(format!("copy to the host failed (rc {rc}) {out}"));
        }
        Ok(())
    }

    /// The end-of-run cache probe (T135): one script on the root shell,
    /// before [`HostExec::cleanup_stage`], on every run that reaches the
    /// end, a failed host too ([`crate::ops::fetch::end_script`]). It removes
    /// what went unaccessed for 90 days and, on an entitled run, what
    /// `playbook` no longer fetches. When the root shell is gone nothing is
    /// removed, and no fresh shell is opened for it (unlike the cleanup). A
    /// `--check` run does not run it: it changes nothing on the host.
    ///
    /// `unentitled` is the reason the run may not drop the playbook's
    /// markers ("Cache ownership" in the note): `None` when the run is a
    /// real, unrestricted one that the engine ended `Ok`; the host's own
    /// outcome is judged here.
    async fn end_cache(&mut self, playbook: &str, unentitled: Option<String>) {
        let mut why = unentitled;
        if why.is_none() {
            match &self.verdict {
                None | Some(HostStatus::Ok) | Some(HostStatus::Changed) => {}
                Some(s) => why = Some(format!("the host ended {}", s.label())),
            }
        }
        if let Some(w) = &why {
            self.say(format!("cache: ownership not checked ({w})"));
        }
        if self.check.is_some() {
            return;
        }
        let uses: Vec<(String, String)> = self.shells.cache_uses.iter().cloned().collect();
        let owner = why.is_none().then_some(playbook);
        let script = crate::ops::fetch::end_script(&uses, &self.shells.fetched_sums, owner);
        self.seq += 1;
        match prepare::probe(&mut self.shells.root, self.seq, &script).await {
            Ok((_, out)) => {
                for l in out
                    .lines()
                    .filter(|l| l.starts_with("telepatch: ") || l.starts_with("cache "))
                {
                    self.say(l.to_string());
                }
            }
            Err(e) => self.say(format!("telepatch: cache not checked: no root shell ({e})")),
        }
    }

    /// Remove what the run left on the host: its staging directory and the
    /// undo data it made, on every filesystem the run's `fsroots` lists
    /// ([`crate::ops::undo::cleanup_script`]). Best effort: a failure is not
    /// the run's, but when the root shell is gone a fresh one is opened for
    /// it, and when that cannot be, the data left is said.
    async fn cleanup_stage(&mut self) {
        if !self.fetch_staged && !self.undo_used {
            return;
        }
        let script = crate::ops::undo::cleanup_script();
        self.seq += 1;
        let out = match prepare::probe(&mut self.shells.root, self.seq, &script).await {
            Ok((_, out)) => Ok(out),
            Err(_) => self.cleanup_fresh(&script).await,
        };
        match out {
            Ok(out) => {
                for l in out.lines().filter(|l| l.starts_with("telepatch: ")) {
                    self.say(l.to_string());
                }
            }
            Err(why) => self.say(format!(
                "telepatch: cleanup: no root shell to remove the run's undo data ({why}); left under <stage>/{} on the host",
                crate::ops::fetch::run_id()
            )),
        }
    }

    /// `script` in a fresh root shell, for a host whose own is gone.
    async fn cleanup_fresh(&mut self, script: &str) -> Result<String, String> {
        let who = Principal::Root;
        let mut s = self.shells.target.open(&self.shells.host, &who, 1).await;
        if s.got.is_empty() {
            return Err(refusal(
                &self.shells.target,
                &self.shells.host,
                &who,
                s.limit,
            ));
        }
        let mut sh = s.got.remove(0);
        for extra in s.got {
            extra.close().await;
        }
        if let Err(e) = init_shell(&mut sh, None).await {
            sh.close().await;
            return Err(e);
        }
        self.seq += 1;
        let r = prepare::probe(&mut sh, self.seq, script).await;
        sh.close().await;
        r.map(|(_, out)| out)
    }

    /// The root shell's preparation op for one run of a login shell's ops
    /// (T106): the undo directories those ops need that only root can make.
    /// Nothing is sent when no op of the run captures at a path it can name.
    async fn prepare_undo(&mut self, who: &RunAs, ops: &[WindowOp]) -> Result<(), String> {
        let user = match &who.principal {
            Principal::User(u) => u.clone(),
            Principal::Root => ROOT.to_string(),
        };
        let mut words: Vec<String> = Vec::new();
        for w in ops {
            if let Some(word) = crate::ops::undo::capture_word(&w.node.spec, &user)
                && !words.contains(&word)
            {
                words.push(word);
            }
        }
        if words.is_empty() {
            return Ok(());
        }
        self.seq += 1;
        let script = crate::ops::undo::prepare_script(&user, &words);
        let (_, out) = prepare::probe(&mut self.shells.root, self.seq, &script).await?;
        self.sink.event(json!({"ev":"undo_prepare","host":self.name,
            "user":user,"paths":words.len()}));
        for l in out.lines().filter(|l| l.starts_with("telepatch: ")) {
            self.say(l.to_string());
        }
        Ok(())
    }

    /// The stale sweep, once per host before the first window that holds a
    /// capturing op ([`crate::ops::undo::sweep_script`]): a run of a dead
    /// controller older than 24 hours is removed, a younger one is named.
    async fn sweep_stale(&mut self) {
        self.seq += 1;
        if let Ok((_, out)) = prepare::probe(
            &mut self.shells.root,
            self.seq,
            &crate::ops::undo::sweep_script(),
        )
        .await
        {
            for l in out.lines().filter(|l| l.starts_with("telepatch: ")) {
                self.say(l.to_string());
            }
        }
    }

    fn finish(mut self, res: Result<(), EngineError>) -> (HostReport, HostShells) {
        let mut rep = std::mem::replace(&mut self.report, HostReport::new(&self.name));
        let stopped = match &res {
            Err(EngineError::CheckStopped { site, reads }) => Some(CheckStop {
                site: site.map(|s| s.to_string()),
                reads: reads.iter().map(|k| k.to_string()).collect(),
                reboot: false,
            }),
            Err(EngineError::CheckStoppedAtReboot { site }) => Some(CheckStop {
                site: site.map(|s| s.to_string()),
                reads: Vec::new(),
                reboot: true,
            }),
            _ => None,
        };
        if let Err(e) = res
            && self.verdict.is_none()
            && stopped.is_none()
        {
            self.verdict = Some(HostStatus::Error(e.to_string()));
        }
        let check = self.check.take().map(|mut ck| {
            ck.info.stopped = stopped;
            ck.info
        });
        rep.status = self.verdict.take().unwrap_or_else(|| match &check {
            Some(c) if c.pending > 0 || c.stopped.is_some() => HostStatus::Changed,
            Some(_) => HostStatus::Ok,
            None if rep.ops.iter().any(|o| o.changed) => HostStatus::Changed,
            None => HostStatus::Ok,
        });
        // A stop at a barrier says how many ops it left, and says it all at
        // once, after the ops it did not run.
        let not_run = rep
            .ops
            .iter()
            .filter(|o| o.outcome == Outcome::NotRun)
            .count();
        if let HostStatus::RebootPending(stop) = &mut rep.status {
            stop.not_run = not_run;
            for l in stop.lines(&self.name) {
                self.sink.line(&l);
            }
        }
        if let Some(info) = check {
            let name = self.name.clone();
            self.say(info.summary(!rep.status.is_success()));
            for c in &info.coverage {
                self.sink
                    .line(&format!("== {name} [coverage] {} ==", c.line()));
            }
            if let Some(stop) = &info.stopped {
                if stop.reboot {
                    self.say(
                        "a reboot barrier may be pending and a check never reboots: nothing after it was traced or checked"
                            .to_string(),
                    );
                } else {
                    self.say(format!(
                        "a read of {} needs changes this check cannot apply: nothing after it was traced or checked",
                        stop.reads.join(", ")
                    ));
                }
            }
            self.sink.event(json!({"ev":"check","host":name,
                "pending":info.pending,"ops":info.ops,"unchecked":info.unchecked,
                "will_ops":info.will_ops,"ok_ops":info.ok_ops,
                "coverage":info.coverage.iter().map(|c| c.line()).collect::<Vec<_>>(),
                "stopped":info.stopped.as_ref().map(CheckStop::line)}));
            rep.check = Some(info);
        }
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

/// Does a `will_create` finding say something will change? Not: the
/// daemon-reload that follows a unit, `no systemctl: skipped`, a restart
/// that waits on a change already counted.
fn create_counts(c: &crate::report::WillCreate) -> bool {
    match c.kind.as_str() {
        "systemd" => false,
        "svc" => {
            let d = c.detail.as_deref().unwrap_or("");
            d != "no systemctl: skipped" && !d.contains(" if ")
        }
        _ => true,
    }
}

/// How many findings of a window's prepare say something will change.
/// Not counted: `no systemctl: skipped`, a restart that waits on a change
/// already counted, and the daemon-reload that follows a unit.
fn will_pending(w: &crate::prepare::Prepared) -> usize {
    let creates = w.will_create.iter().filter(|c| create_counts(c)).count();
    w.will_install.len()
        + w.will_remove.len()
        + w.will_change.len()
        + w.local_notes.len()
        + w.git_notes.len()
        + w.resource_notes.len()
        + creates
}

/// The skip trailer an op emits instead of running once the host halted.
fn skip_script(op: &OpNode, id: usize, nonce: &str) -> String {
    crate::trailer::skip(op.spec.op_kind(), id, nonce)
}

impl Executor for HostExec {
    fn check_only(&self) -> bool {
        self.check.is_some()
    }

    async fn read_facts(
        &mut self,
        host: &HostInfo,
        window: usize,
        queries: &[FactQuery],
    ) -> Vec<Value> {
        let default = |q: &FactQuery| q.fact.missing();
        let logins: BTreeSet<RunAs> = queries
            .iter()
            .filter(|q| q.run_as.login)
            .map(|q| q.run_as.clone())
            .collect();
        if let Err(why) = self.ensure_logins(&logins).await {
            self.block(why);
            return queries.iter().map(default).collect();
        }
        // Group the queries into probes: a maximal run of `check` reads with
        // one `RunAs`, adjacent in this round's order, is one probe (one
        // bash); every other read is a probe of its own (T115).
        let mut probes: Vec<std::ops::Range<usize>> = Vec::new();
        for (i, q) in queries.iter().enumerate() {
            let joins = match (probes.last(), &q.fact) {
                (Some(r), Fact::Check(_)) => {
                    let prev = &queries[r.end - 1];
                    matches!(prev.fact, Fact::Check(_)) && prev.run_as == q.run_as
                }
                _ => false,
            };
            if joins {
                probes.last_mut().unwrap().end = i + 1;
            } else {
                probes.push(i..i + 1);
            }
        }
        // Render and send all probes, then collect.
        let mut ids = Vec::new();
        let mut sent_err = None;
        for r in &probes {
            let q = &queries[r.start];
            let script = if matches!(q.fact, Fact::Check(_)) {
                let cmds: Vec<&str> = queries[r.clone()]
                    .iter()
                    .filter_map(|q| match &q.fact {
                        Fact::Check(c) => Some(c.as_str()),
                        _ => None,
                    })
                    .collect();
                check_run_probe(&cmds)
            } else {
                q.fact.probe_for(&host.os)
            };
            self.seq += 1;
            let node = OpNode {
                spec: OpSpec::Raw { script },
                run_as: q.run_as.clone(),
                reads: Default::default(),
                writes: Default::default(),
                guard: None,
                post: Default::default(),
                allow_fail: false,
                site: None,
                allowed: Vec::new(),
                at_end: false,
                timeout: None,
                retry: 0,
                needs_reboot: false,
            };
            let id = self.seq;
            let who = &q.run_as;
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
        for (r, id) in probes.iter().zip(&ids) {
            let q = &queries[r.start];
            let got = match self.shell_for(&q.run_as) {
                Some(sh) => prepare::collect(sh, *id).await,
                None => Err("no shell".to_string()),
            };
            match got {
                Ok((_, out)) if matches!(q.fact, Fact::Check(_)) => {
                    values.extend(parse_check_run(&out, r.len()));
                }
                Ok((_, out)) => values.push(q.fact.parse(&out)),
                Err(e) => {
                    sent_err = sent_err.or(Some(e));
                    values.extend(queries[r.clone()].iter().map(default));
                }
            }
        }
        while values.len() < queries.len() {
            values.push(default(&queries[values.len()]));
        }
        self.sink.event(json!({"ev":"facts","host":self.name,"window":window,
            "probes": probes.len(),
            "answers": queries.iter().zip(&values).map(|(q, v)| format!("{q} = {v}")).collect::<Vec<_>>()}));
        if let Some(e) = sent_err {
            self.block(format!("reading facts: {e}"));
        }
        values
    }

    async fn reboot(
        &mut self,
        _host: &HostInfo,
        window: usize,
        at: &RebootAt,
    ) -> Result<Crossed, String> {
        self.cross_barrier(window, at).await
    }

    async fn end_of_run(&mut self, _host: &HostInfo, window: usize) -> Result<(), String> {
        self.end_pending(window).await
    }

    async fn apply(&mut self, host: &HostInfo, window: &Window) -> Result<Applied, String> {
        let mut window = window.clone();
        let resolved = prepare::resolve_secrets(&mut window);
        for w in crate::secret::backend_warnings() {
            self.sink
                .line(&format!("== {} [prepare] {w} ==", self.name));
        }
        resolved?;
        if let Err(why) = prepare::fill_local(&mut window) {
            self.block(why.clone());
            return Err(why);
        }
        // A check counts every op once, then drops what it cannot probe.
        self.check_count(&window);
        let kind = match window.kind {
            WindowKind::NonMutating => "non-mutating",
            WindowKind::ImplicitBarrier => "implicit barrier",
            WindowKind::Final => "final",
            WindowKind::AtEnd => "at end",
        };
        // Prepare first: a window declined for its head (no downloader on
        // the host, and an op of the window installs one) is sent,
        // reported and recorded nowhere, so nothing below runs for it.
        let prepared = if self.halted || (window.ops.is_empty() && window.assertions.is_empty()) {
            None
        } else {
            // A fetch or a binary release stages under the run's
            // directory; removed when the host is done, even if its op
            // never ran.
            let binary_release = window.ops.iter().any(|o| {
                matches!(&o.node.spec, OpSpec::Pkg(p)
                    if matches!(p.source, crate::ops::pkg::Source::BinaryRelease(_)))
            });
            let fetching = binary_release || !crate::ops::fetch::items(&window).is_empty();
            if fetching {
                self.fetch_staged = true;
            }
            // The stale sweep also covers a fetch-only window (a binary
            // release, a package from a file): a download may have made
            // `<root>/.telepatch/tmp/<run>` on another filesystem (T119).
            if self.check.is_none()
                && (fetching
                    || window.ops.iter().any(|o| {
                        matches!(
                            o.node.spec,
                            OpSpec::File(_) | OpSpec::Unit(_) | OpSpec::Repo(_)
                        )
                    }))
                && !self.undo_used
            {
                self.undo_used = true;
                self.sweep_stale().await;
            }
            // Prepare: nothing is sent to be applied until all of it passes.
            self.check_filter(&mut window);
            let earlier = self.check_earlier(window.number);
            let mut prepared = prepare::prepare_window(
                &mut self.shells,
                &window,
                &host.os,
                &mut self.users_ok,
                &mut self.seq,
                earlier,
            )
            .await;
            if let (Err(r), Some(s)) = (&prepared, &window.split)
                && splits_at(&window, s, &r.no_downloader)
            {
                self.split(&window, s);
                if let Some(ck) = &mut self.check {
                    ck.downloader_pending = true;
                }
                return Ok(Applied::Head(s.at + 1));
            }
            // A check does not install the downloader an earlier window
            // would: the downloads that wait for it are not checked.
            if let Err(r) = &prepared
                && !r.no_downloader.is_empty()
                && self.check.as_ref().is_some_and(|c| c.downloader_pending)
            {
                let held: Vec<NodeId> = r.no_downloader.clone();
                window.ops.retain(|o| !held.contains(&o.id));
                self.say(format!(
                    "window {}: downloads not checked: neither curl nor wget is on the host and an earlier window installs one",
                    window.number
                ));
                prepared = prepare::prepare_window(
                    &mut self.shells,
                    &window,
                    &host.os,
                    &mut self.users_ok,
                    &mut self.seq,
                    earlier,
                )
                .await;
            }
            if let Ok(will) = &prepared {
                crate::resource::mark_skipped(&mut window, &will.resource_skip);
            }
            Some(prepared)
        };
        let window = &window;
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
        // A check probes the files too, read-only, before the restarts are
        // phrased: a file known to stay as it is settles a guard on it.
        let mut file_found: Vec<prepare::FileFinding> = Vec::new();
        if self.check.is_some() && prepared.is_ok() {
            let diff = self.check.as_ref().is_some_and(|c| c.opts.diff);
            match prepare::check_files(&mut self.shells, window, diff, &mut self.seq).await {
                Ok(f) => file_found = f,
                Err(why) => {
                    self.block(why.clone());
                    return Err(why);
                }
            }
            for f in &file_found {
                if let Some(w) = window.ops.iter().find(|o| o.id == f.id) {
                    let (o, c) = if f.detail.is_some() {
                        (Outcome::Changed, true)
                    } else {
                        (Outcome::Unchanged, false)
                    };
                    self.remember(w, o, c, None);
                }
            }
        }
        let actions = self.and_not_starts(window);
        match prepared {
            Ok(mut will) => {
                let earlier = self.check_earlier(window.number);
                if earlier {
                    for c in &mut will.will_create {
                        if let Some(d) = &mut c.detail {
                            *d = d.replace(
                                "this window's earlier ops",
                                "the earlier windows' changes",
                            );
                        }
                    }
                }
                will.will_create.extend(actions);
                if let Some(l) = self.flush_line(window, &will.will_create).await {
                    will.will_create.push(l);
                }
                self.check_note_prepared(window, &will);
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
                for w in will.will_remove {
                    self.sink
                        .line(&format!("== {} [prepare] {} ==", self.name, w.line()));
                    self.sink.event(json!({"ev":"will_remove","host":self.name,
                        "names":w.names}));
                    self.report.will_remove.push(w);
                }
                for n in &will.local_notes {
                    let n = self.annotate_local(window, n);
                    self.sink
                        .line(&format!("== {} [prepare] {n} ==", self.name));
                }
                for n in &will.git_notes {
                    self.sink
                        .line(&format!("== {} [prepare] {n} ==", self.name));
                }
                for n in &will.resource_notes {
                    self.sink
                        .line(&format!("== {} [prepare] {n} ==", self.name));
                }
                // A check does not stream the controller's files; the lines
                // above already say which differ.
                if self.check.is_none()
                    && let Err(why) = self.stream_copies(&will.copies).await
                {
                    self.block(why.clone());
                    return Err(why);
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
        if self.check.is_some() {
            self.note_check_marks(window);
            self.check_window_end(window, file_found);
            return Ok(Applied::All);
        }

        // Consecutive ops with the same RunAs (one shell) form a run.
        let mut runs: Vec<(usize, usize)> = Vec::new();
        let mut s = 0;
        while s < window.ops.len() {
            let who = &window.ops[s].node.run_as;
            let mut e = window.ops[s..]
                .iter()
                .position(|w| w.node.run_as != *who)
                .map_or(window.ops.len(), |n| s + n);
            // An op with retries ends its run: the controller sends it
            // again after a failure, so nothing may be queued behind it.
            if let Some(n) = window.ops[s..e].iter().position(|w| w.node.retry > 0) {
                e = s + n + 1;
            }
            // So does an op that may stale its own login shell (T114,
            // [`OpNode::stales_own_shell`]): nothing queued behind it may
            // run in the old environment.
            let homes = &self.shells.homes;
            if let Some(n) = window.ops[s..e]
                .iter()
                .position(|w| w.node.stales_own_shell(homes))
            {
                e = s + n + 1;
            }
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

        // Render each op for its shell (open now).
        let mut scripts = Vec::new();
        let mut afters = Vec::new();
        // Per op: it carries the daemon-reload of a changed unit.
        let mut reloading = Vec::new();
        // A `unit` op came earlier in this window: later `svc` ops carry
        // the daemon-reload of a changed unit. The flag never outlives the
        // window (the flush at its end), so earlier windows do not count.
        let mut units = false;
        for (i, w) in window.ops.iter().enumerate() {
            let daemon_reload = units && matches!(w.node.spec, OpSpec::Svc(_));
            reloading.push(daemon_reload);
            afters.push(match &w.node.spec {
                OpSpec::Unit(_) => UNIT_FLAG,
                OpSpec::Svc(_) if daemon_reload => UNIT_FLAG_CLEAR,
                _ => "",
            });
            if matches!(w.node.spec, OpSpec::Unit(_)) {
                units = true;
            }
            let who = &w.node.run_as;
            let Some(sh) = self.shell_for(who) else {
                let why = format!("op #{}: no shell open for {who}", w.id.0);
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
        // Attempts that failed and were sent again, per op ([`HostExec::resend`]).
        let mut attempts = vec![0u32; window.ops.len()];
        let mut failed_seen = false;
        let mut send_err = None;
        // What the run's ops marked, written after the run is collected.
        let mut marks: Vec<(usize, crate::report::RebootItem)> = Vec::new();

        // Each run is pipelined into its shell and collected in full
        // before the next run is sent (Assumption A3); after a failure
        // nothing more is sent on any shell.
        let mut r = 0;
        while r < runs.len() && !failed_seen && send_err.is_none() {
            let (start, end) = runs[r];
            let who = window.ops[start].node.run_as.clone();

            // A login shell an earlier op staled is closed and opened
            // again now; the scripts rendered for it (this run's and every
            // later run's) carry its new nonce.
            let was = self.shell_for(&who).map(|sh| sh.nonce().to_string());
            if let Err(why) = prepare::ShellSet::open_login(&mut self.shells, &who).await {
                self.block(why.clone());
                return Err(why);
            }
            if self.shell_for(&who).map(|sh| sh.nonce().to_string()) != was {
                for i in start..window.ops.len() {
                    let w = &window.ops[i];
                    if w.node.run_as != who {
                        continue;
                    }
                    let Some(sh) = self.shells.shell_of(&who) else {
                        break;
                    };
                    let opts = crate::trailer::RenderOpts {
                        source: source[i],
                        daemon_reload: reloading[i],
                    };
                    match crate::trailer::render_with(
                        &w.node,
                        w.id.0,
                        sh.nonce(),
                        &host.os,
                        sh.user(),
                        opts,
                    ) {
                        Ok(r) => scripts[i] = r,
                        Err(e) => {
                            let why = format!("op #{}: {e}", w.id.0);
                            self.block(why.clone());
                            return Err(why);
                        }
                    }
                }
            }

            // A user's capturing ops need directories only root can make:
            // the root shell prepares them first.
            if who.login
                && let Err(why) = self.prepare_undo(&who, &window.ops[start..end]).await
            {
                self.block(why.clone());
                return Err(why);
            }

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
            // The window flag is a variable of one shell: a run with an
            // `svc` op that carries the reload starts with the flag as the
            // reported outcomes have it (a unit changed in another shell,
            // and no `svc` op that ran after it).
            if reloading[start..end].iter().any(|r| *r) {
                let flag = u8::from(!pending_units(&self.report, &window.ops[..start]).is_empty());
                gated[0] = format!("__tp_dr={flag}\n{}", gated[0]);
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

            // Collect the run in order, under the controller's clock: the
            // current op has its limit from its start line ([`Watch`]).
            // (After a failed send the shell is dead: nothing to time.)
            let timed: &[WindowOp] = if send_err.is_some() {
                &[]
            } else {
                &window.ops[start..end]
            };
            let mut watch = Watch::new(
                KillBounds::POLICY,
                timed.iter().map(|w| (w.id.0, w.node.enforced_timeout())),
            );
            loop {
                let tick = match self.shells.watched(&who) {
                    Some((sh, killer)) => watch.next(sh, killer).await,
                    None => Tick::Idle,
                };
                let ev = match tick {
                    Tick::Event(ev) => ev,
                    Tick::Idle => break,
                    Tick::Stopped { id, why } => {
                        let Some(&i) = index.get(&id) else { continue };
                        let w = &window.ops[i];
                        self.header(&mut live[i], w, &descs[i]);
                        self.flush_partial(&mut live[i]);
                        done[i] = true;
                        let lim = crate::trace::fmt_duration(w.node.effective_timeout());
                        // The host cannot go on after these two.
                        let (note, drop_host) = match &why {
                            Stopped::TimedOut { .. } => (format!("timed out after {lim}"), false),
                            Stopped::EndedWithoutResult => {
                                ("ended without a result".to_string(), false)
                            }
                            Stopped::NeverStarted => (
                                format!("op never started within {lim}; shell dropped"),
                                true,
                            ),
                            Stopped::NoSecondShell(_) => (
                                format!("timed out after {lim}; no second shell to stop it"),
                                true,
                            ),
                        };
                        let again = !failed_seen
                            && !drop_host
                            && w.node.retry > attempts[i]
                            && i + 1 == end;
                        if again {
                            attempts[i] += 1;
                            live[i].tail.clear();
                            done[i] = false;
                            match self
                                .resend(&who, w, &gated[i - start], attempts[i], &note)
                                .await
                            {
                                Ok(()) => {
                                    watch = Watch::new(
                                        KillBounds::POLICY,
                                        [(w.id.0, w.node.enforced_timeout())],
                                    );
                                }
                                Err(e) => send_err = Some(e),
                            }
                            continue;
                        }
                        if failed_seen {
                            self.finish_op(w, &descs[i], Outcome::NotRun, 0, false, "");
                        } else if w.node.allow_fail && !drop_host {
                            self.finish_op(w, &descs[i], Outcome::AllowedFail, -1, false, &note);
                        } else {
                            failed_seen = true;
                            let mut tail = last_lines(&live[i].tail, TAIL_LINES);
                            if let Stopped::NoSecondShell(reason) = &why {
                                // Why there was none, for whoever reads the failure.
                                if !tail.is_empty() {
                                    tail.push('\n');
                                }
                                tail.push_str(reason);
                            }
                            self.finish_op(w, &descs[i], Outcome::Failed, -1, false, &note);
                            self.fail(w, &format!("{}, {note}", descs[i]), -1, tail);
                        }
                        if drop_host {
                            self.shells.drop_connections().await;
                        }
                        continue;
                    }
                };
                match ev {
                    Event::Started { .. } => {}
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
                                    // The profile may have changed under the
                                    // login shells (T114, rules 1-6).
                                    if let Some(st) = w.node.stales(&self.shells.homes)
                                        && (changed || st.always)
                                    {
                                        self.shells.mark_stale(&st.scope, st.rule, w.id.0);
                                    }
                                    if let (crate::trace::OpSpec::Pkg(p), Some(names)) =
                                        (&w.node.spec, installed.as_ref())
                                        && matches!(p.wanted, Wanted::Absent)
                                        && !names.is_empty()
                                    {
                                        self.sink
                                            .event(json!({"ev":"pkg_removed","host":self.name,
                                            "id":w.id.0,"names":names}));
                                        self.report.removed.push(crate::report::Removed {
                                            id: w.id.0,
                                            names: names.clone(),
                                        });
                                    }
                                    if installed.is_some() {
                                        self.remember(w, o, changed, installed.as_deref());
                                    }
                                    self.marks_of(w, i, &mut marks);
                                    if let crate::trace::OpSpec::Git(g) = &w.node.spec
                                        && let Ok(gr) = trailer.typed::<crate::trailer::GitResult>()
                                    {
                                        self.report.git.push(crate::report::GitCheckout {
                                            id: w.id.0,
                                            path: g.path.clone(),
                                            created: gr.created,
                                            was_rev: gr.was_rev,
                                        });
                                    }
                                    if let crate::trace::OpSpec::Resource(rs) = &w.node.spec {
                                        self.report.resources.push(crate::report::ResourceRecord {
                                            id: w.id.0,
                                            kind: rs.kind.to_string(),
                                            name: rs.name.clone(),
                                            irreversible: rs.inverse.is_none(),
                                        });
                                    }
                                    if started {
                                        for m in &w.members {
                                            if let Some(k) = self.known.get_mut(&m.id) {
                                                k.started = true;
                                            }
                                        }
                                    }
                                } else if w.node.retry > attempts[i] && i + 1 == end {
                                    attempts[i] += 1;
                                    live[i].tail.clear();
                                    done[i] = false;
                                    let why = match timed_out(w, rc) {
                                        t if t.is_empty() => format!("rc {rc}"),
                                        t => t,
                                    };
                                    match self
                                        .resend(&who, w, &gated[i - start], attempts[i], &why)
                                        .await
                                    {
                                        Ok(()) => {
                                            watch = Watch::new(
                                                KillBounds::POLICY,
                                                [(w.id.0, w.node.enforced_timeout())],
                                            );
                                        }
                                        Err(e) => send_err = Some(e),
                                    }
                                } else if w.node.allow_fail {
                                    let why = timed_out(w, rc);
                                    self.finish_op(
                                        w,
                                        &descs[i],
                                        Outcome::AllowedFail,
                                        rc,
                                        false,
                                        &why,
                                    );
                                } else {
                                    failed_seen = true;
                                    let tail = last_lines(&live[i].tail, TAIL_LINES);
                                    let why = timed_out(w, rc);
                                    self.finish_op(w, &descs[i], Outcome::Failed, rc, false, &why);
                                    let op = if why.is_empty() {
                                        descs[i].clone()
                                    } else {
                                        format!("{}, {why}", descs[i])
                                    };
                                    self.fail(w, &op, rc, tail);
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
            // The marks of this run, before a failure of a later op (or the
            // halted host) is acted on: a change is never left unmarked.
            if !marks.is_empty() {
                let at = marks[marks.len() - 1].0;
                if let Err(why) = self.write_marks(&marks).await {
                    failed_seen = true;
                    self.fail(&window.ops[at], "reboot mark", -1, why);
                }
                marks.clear();
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
    check: Option<CheckOpts>,
    only: Rc<[String]>,
    reboot: (RebootPolicy, RebootWait),
) -> HostReport {
    let name = spec.name.clone();
    sink.event(json!({"ev":"host_open","host":name}));
    let blocked = |why: String| {
        let mut r = HostReport::new(&name);
        r.check = check.map(|_| CheckInfo::default());
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
    let os = match init_shell(&mut shell, cache_owner(pb_name, check.is_some()).as_deref()).await {
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
    let host = Host::new(&name, os, Rc::new(spec.vars))
        .with_groups(spec.groups)
        .with_only(only.iter().cloned());
    let probe = host.clone();
    let pb = Playbook::new(pb_name, pb_run);
    let target_rc = target.clone();
    let shells = HostShells {
        host: name.clone(),
        target,
        root: shell,
        logins: BTreeMap::new(),
        stale: BTreeMap::new(),
        homes: BTreeMap::new(),
        sink: sink.clone(),
        killer: Killer::new(target_rc, name.clone()),
        cache_uses: BTreeSet::new(),
        fetched_sums: BTreeSet::new(),
    };
    let mut exec = HostExec::new(&name, shells, sink.clone(), check);
    exec.set_reboot(reboot.0, reboot.1, pb_name);
    let mut engine = Engine::new(exec);
    let res = engine.run_host(&pb, host).await;
    let unentitled = if check.is_some() {
        Some("--check".to_string())
    } else if !only.is_empty() {
        Some("--only".to_string())
    } else {
        res.as_ref()
            .err()
            .map(|_| "the run did not trace and prepare every window".to_string())
    };
    engine.executor.end_cache(pb_name, unentitled).await;
    engine.executor.cleanup_stage().await;
    let (mut rep, shells) = engine.executor.finish(res);
    rep.not_selected = probe.not_selected_roles();
    rep.only_untraced = probe.untraced_only_roles();
    shells.close().await;
    sink.event(json!({"ev":"host_status","host":name,"status":rep.status.label(),"detail":rep.status_detail()}));
    rep
}

impl HostReport {
    fn status_detail(&self) -> String {
        match &self.status {
            HostStatus::Failed { role, op, rc, .. } => format!("role {role}: {op} (rc {rc})"),
            HostStatus::Blocked(w) | HostStatus::Error(w) | HostStatus::NotStarted(w) => w.clone(),
            HostStatus::RebootPending(_) => "reboot pending at barrier".to_string(),
            _ => String::new(),
        }
    }
}

/// Run `playbook` on every host concurrently and return the report. Builds
/// its own current-thread runtime, so call it from plain (non-async) code.
pub fn run<V: 'static>(playbook: &Playbook<V>, hosts: Vec<HostSpec<V>>, opts: RunOpts) -> Report {
    run_with(playbook, hosts, opts, None, RunControls::default())
}

/// [`run`] as a dry run: trace and prepare every host, apply nothing (see
/// "Check" in the module doc). The report's [`Report::exit_code`] is 0
/// nothing to change, 2 changes pending, 1 a host blocked.
pub fn run_check<V: 'static>(
    playbook: &Playbook<V>,
    hosts: Vec<HostSpec<V>>,
    opts: RunOpts,
    check: CheckOpts,
) -> Report {
    run_with(playbook, hosts, opts, Some(check), RunControls::default())
}

/// [`run`] or [`run_check`] (`check` set) under [`RunControls`]: `--serial`,
/// `--max-fail` and `--only`.
pub fn run_with<V: 'static>(
    playbook: &Playbook<V>,
    hosts: Vec<HostSpec<V>>,
    opts: RunOpts,
    check: Option<CheckOpts>,
    controls: RunControls,
) -> Report {
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
    let only: Rc<[String]> = controls.only.into();
    let reboot = (controls.reboot, controls.reboot_wait);
    let limiter = Rc::new(Limiter::new(controls.serial, controls.max_fail));
    let reports = rt.block_on(local.run_until(async {
        let tasks: Vec<_> = hosts
            .into_iter()
            .map(|h| {
                let name = h.name.clone();
                let run = run_one(
                    pb_name,
                    pb_run,
                    h,
                    sink.clone(),
                    check,
                    only.clone(),
                    reboot,
                );
                tokio::task::spawn_local(gated(limiter.clone(), sink.clone(), name, check, run))
            })
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

    #[test]
    fn the_wait_between_attempts_is_1_2_4_up_to_30_seconds() {
        let got: Vec<u64> = (1..=9).map(|k| retry_wait(k).as_secs()).collect();
        assert_eq!(got, [1, 2, 4, 8, 16, 30, 30, 30, 30]);
    }

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
                needs_reboot: false,
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
                timeout: None,
                retry: 0,
                needs_reboot: false,
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

/// The watchdog over local shells (`LocalTransport::new().uid(0)` is a
/// transport setting, not root), with short bounds. Every process a test
/// starts is killed on drop of its [`Reaper`].
#[cfg(test)]
mod watchdog_tests {
    use super::*;
    use crate::ops::shell::raw_body;
    use crate::trailer::ShellResult;

    fn bounds() -> KillBounds {
        KillBounds {
            term_every: Duration::from_millis(150),
            grace: Duration::from_millis(600),
            settle: Duration::from_millis(300),
        }
    }

    /// The target and the sandbox it lives in (T124): the caller keeps the
    /// sandbox alive for the length of the test.
    fn local_target() -> (Rc<Target>, crate::testing::Sandbox) {
        let sb = crate::testing::sandbox();
        let t = Rc::new(Target::Local(LocalTransport::new().uid(0).sandboxed(&sb)));
        (t, sb)
    }

    async fn shell(t: &Target) -> Shell {
        let mut s = t.open("local", &Principal::Root, 1).await;
        assert!(s.limit.is_none(), "{:?}", s.limit);
        let mut sh = s.got.remove(0);
        init_shell(&mut sh, None).await.unwrap();
        sh
    }

    fn op(sh: &Shell, id: usize, body: &str) -> String {
        wrap(OpKind::Sh, &raw_body(body), id, sh.nonce())
    }

    /// Kills what a test started, by a marker in its command line.
    struct Reaper(Vec<String>);
    impl Drop for Reaper {
        fn drop(&mut self) {
            for m in &self.0 {
                let _ = std::process::Command::new("pkill")
                    .args(["-KILL", "-f", m])
                    .status();
            }
        }
    }

    /// Everything up to the end of the watch: the events seen, and how it
    /// ended (`None`: the stream ran dry). Panics after 20 s.
    async fn drive(
        w: &mut Watch,
        sh: &mut Shell,
        k: &mut Killer,
    ) -> (Vec<Event>, Option<(usize, Stopped)>) {
        let r = tokio::time::timeout(Duration::from_secs(20), async {
            let mut v = Vec::new();
            loop {
                match w.next(sh, k).await {
                    Tick::Event(e) => v.push(e),
                    Tick::Idle => return (v, None),
                    Tick::Stopped { id, why } => return (v, Some((id, why))),
                }
            }
        })
        .await;
        r.expect("the watchdog did not end the op")
    }

    fn pgid(events: &[Event]) -> i32 {
        events
            .iter()
            .find_map(|e| match e {
                Event::Started { pid, .. } => Some(*pid),
                _ => None,
            })
            .expect("no start line")
    }

    fn members(pgid: i32) -> Vec<i32> {
        let mut v = Vec::new();
        for e in std::fs::read_dir("/proc").unwrap().flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<i32>().ok()) else {
                continue;
            };
            let Ok(s) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                continue;
            };
            let Some(i) = s.rfind(')') else { continue };
            let f: Vec<&str> = s[i + 2..].split(' ').collect();
            if f.get(2).and_then(|g| g.parse::<i32>().ok()) == Some(pgid) && f[0] != "Z" {
                v.push(pid);
            }
        }
        v
    }

    async fn next_op_runs(sh: &mut Shell, k: &mut Killer, id: usize) {
        let s = op(sh, id, "echo next");
        sh.send(id, &s).await.unwrap();
        let mut w = Watch::new(bounds(), [(id, Duration::from_secs(10))]);
        let (ev, stop) = drive(&mut w, sh, k).await;
        assert!(stop.is_none(), "{stop:?}");
        let Some(Event::Done(_, t)) = ev.last() else {
            panic!("{ev:?}")
        };
        assert_eq!(t.typed::<ShellResult>().unwrap().rc, 0);
    }

    #[tokio::test]
    async fn a_probe_under_the_watchdog_returns_its_output_and_a_stuck_one_is_stopped() {
        let (t, _sb) = local_target();
        let mut sh = shell(&t).await;
        let mut k = Killer::new(t.clone(), "local".into());
        let (ms, secs) = (Duration::from_millis, Duration::from_secs);
        // Inside its limit: the probe's rc and output.
        match probe_bounded(&mut sh, &mut k, bounds(), 1, "echo hi; exit 3", secs(10)).await {
            Ok(Bounded::Done(3, out)) => assert_eq!(out.trim(), "hi"),
            other => panic!("{other:?}"),
        }
        // Past it: stopped, with what it printed, and no process left.
        let t0 = std::time::Instant::now();
        let r = probe_bounded(
            &mut sh,
            &mut k,
            bounds(),
            2,
            "echo partial; sleep 303 & sleep 304 & wait",
            ms(300),
        )
        .await;
        let Ok(Bounded::Stopped(Stopped::TimedOut { .. }, out)) = r else {
            panic!("{r:?}")
        };
        assert_eq!(out.trim(), "partial");
        assert!(t0.elapsed() < secs(5), "{:?}", t0.elapsed());
        next_op_runs(&mut sh, &mut k, 3).await;
    }

    #[tokio::test]
    async fn expiry_kills_the_group_and_the_op_ends_with_its_own_rc_143_trailer() {
        let (t, _sb) = local_target();
        let mut sh = shell(&t).await;
        let mut k = Killer::new(t.clone(), "local".into());
        let s = op(&sh, 1, "sleep 300 & sleep 301 & wait");
        sh.send(1, &s).await.unwrap();
        let mut w = Watch::new(bounds(), [(1, Duration::from_millis(300))]);
        let t0 = std::time::Instant::now();
        let (ev, stop) = drive(&mut w, &mut sh, &mut k).await;
        let Some((1, Stopped::TimedOut { trailer: Some(tr) })) = stop else {
            panic!("{stop:?} {ev:?}")
        };
        assert_eq!(tr.typed::<ShellResult>().unwrap().rc, 143);
        assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
        assert_eq!(members(pgid(&ev)), Vec::<i32>::new());
        next_op_runs(&mut sh, &mut k, 2).await;
    }

    #[tokio::test]
    async fn an_op_that_ignores_term_is_killed_after_the_grace_and_the_result_is_made_up() {
        let (t, _sb) = local_target();
        let mut sh = shell(&t).await;
        let mut k = Killer::new(t.clone(), "local".into());
        let s = op(&sh, 1, "trap '' TERM; sleep 302");
        sh.send(1, &s).await.unwrap();
        let mut w = Watch::new(bounds(), [(1, Duration::from_millis(300))]);
        let t0 = std::time::Instant::now();
        let (ev, stop) = drive(&mut w, &mut sh, &mut k).await;
        assert!(
            matches!(stop, Some((1, Stopped::TimedOut { trailer: None }))),
            "{stop:?} {ev:?}"
        );
        // After the limit and the grace, not before.
        assert!(
            t0.elapsed() >= Duration::from_millis(850),
            "{:?}",
            t0.elapsed()
        );
        assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
        assert_eq!(members(pgid(&ev)), Vec::<i32>::new());
        // The abandoned op's trailer, if one ever comes, is not reported.
        next_op_runs(&mut sh, &mut k, 2).await;
    }

    #[tokio::test]
    async fn a_lost_term_is_repeated_and_the_op_ends_inside_the_grace_with_its_own_trailer() {
        let (t, _sb) = local_target();
        let mut sh = shell(&t).await;
        let mut k = Killer::new(t.clone(), "local".into());
        // The first TERM is absorbed (and re-arms the trap); the second ends
        // the op. The short foreground sleep dies with each group TERM.
        let body = "trap 'trap \"exit 143\" TERM' TERM\nwhile :; do sleep 0.05; done";
        let s = op(&sh, 1, body);
        sh.send(1, &s).await.unwrap();
        let mut w = Watch::new(bounds(), [(1, Duration::from_millis(300))]);
        let t0 = std::time::Instant::now();
        let (ev, stop) = drive(&mut w, &mut sh, &mut k).await;
        let Some((1, Stopped::TimedOut { trailer: Some(tr) })) = stop else {
            panic!("a single TERM then KILL: {stop:?}")
        };
        assert_eq!(tr.typed::<ShellResult>().unwrap().rc, 143);
        // 300 ms limit + one repeat (150 ms), well inside the 600 ms grace.
        assert!(
            t0.elapsed() < Duration::from_millis(1200),
            "{:?}",
            t0.elapsed()
        );
        assert_eq!(members(pgid(&ev)), Vec::<i32>::new());
    }

    #[tokio::test]
    async fn an_op_with_no_trailer_that_is_gone_is_reported_as_ended_without_a_result() {
        let (t, _sb) = local_target();
        let mut sh = shell(&t).await;
        let mut k = Killer::new(t.clone(), "local".into());
        // `exec` replaces the op's subshell: its EXIT trap never runs.
        let s = op(&sh, 1, "exec true");
        sh.send(1, &s).await.unwrap();
        let mut w = Watch::new(bounds(), [(1, Duration::from_millis(300))]);
        let t0 = std::time::Instant::now();
        let (_, stop) = drive(&mut w, &mut sh, &mut k).await;
        assert_eq!(stop, Some((1, Stopped::EndedWithoutResult)));
        assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
        next_op_runs(&mut sh, &mut k, 2).await;
    }

    #[tokio::test]
    async fn an_op_inside_its_limit_is_untouched() {
        let (t, _sb) = local_target();
        let mut sh = shell(&t).await;
        let mut k = Killer::new(t.clone(), "local".into());
        let s = op(&sh, 1, "sleep 0.2; echo quick");
        sh.send(1, &s).await.unwrap();
        let mut w = Watch::new(bounds(), [(1, Duration::from_secs(10))]);
        let (ev, stop) = drive(&mut w, &mut sh, &mut k).await;
        assert!(stop.is_none(), "{stop:?}");
        assert!(matches!(ev.last(), Some(Event::Done(1, _))), "{ev:?}");
        assert!(k.shell.is_none(), "no second shell without a timeout");
    }

    #[tokio::test]
    async fn no_start_line_drops_the_shell_and_reports_never_started() {
        let (t, _sb) = local_target();
        let mut sh = shell(&t).await;
        let mut k = Killer::new(t.clone(), "local".into());
        let mut reap = Reaper(vec!["sleep 3017".into()]);
        // An unwrapped command wedges the shell; the wrapped op queues
        // behind it and never starts.
        sh.send(1, "sleep 3017\n").await.unwrap();
        let s = op(&sh, 2, "echo never");
        sh.send(2, &s).await.unwrap();
        let mut w = Watch::new(bounds(), [(2, Duration::from_millis(300))]);
        let t0 = std::time::Instant::now();
        let (ev, stop) = drive(&mut w, &mut sh, &mut k).await;
        assert_eq!(stop, Some((2, Stopped::NeverStarted)), "{ev:?}");
        assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
        assert!(
            k.shell.is_none(),
            "there is no PID to kill, no second shell"
        );
        // The shell is dropped: it ends, and ends promptly.
        tokio::time::timeout(Duration::from_secs(5), sh.close())
            .await
            .expect("the dropped shell did not end");
        // The unwrapped `sleep` is no op of ours: no PID is known, so the
        // test stops it.
        reap.0.push("sleep 3017".into());
    }

    #[tokio::test]
    async fn no_second_shell_drops_the_shell_and_says_so() {
        let (t, _sb) = local_target();
        let mut sh = shell(&t).await;
        // A target that cannot open a shell: an ssh that does not exist.
        let dead = Rc::new(Target::Ssh(SshTransport::new().ssh("/nonexistent/tp-ssh")));
        let mut k = Killer::new(dead, "h".into());
        let s = op(&sh, 1, "sleep 303");
        sh.send(1, &s).await.unwrap();
        let mut reap = Reaper(vec![]);
        reap.0.push("sleep 303".into());
        let mut w = Watch::new(bounds(), [(1, Duration::from_millis(300))]);
        let (ev, stop) = drive(&mut w, &mut sh, &mut k).await;
        let Some((1, Stopped::NoSecondShell(why))) = stop else {
            panic!("{stop:?} {ev:?}")
        };
        assert!(!why.is_empty(), "the reason is kept");
        tokio::time::timeout(Duration::from_secs(5), sh.close())
            .await
            .expect("the dropped shell did not end");
    }
}
