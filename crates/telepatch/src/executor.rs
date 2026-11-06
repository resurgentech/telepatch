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
        let sh = s.got.remove(0);
        // More than asked for is not expected; close the rest.
        for extra in s.got {
            extra.close().await;
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
                    | crate::ops::pkg::Source::Cargo
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
        OpSpec::User(u) => format!("user {}", u.name),
        OpSpec::Repo(r) => format!("repo {}", r.name),
        OpSpec::Svc(s) => format!("svc {}", s.unit),
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
    use crate::trailer::{
        FileResult, GroupResult, PkgResult, RepoResult, ShellResult, SvcResult, UserResult,
    };
    let r = match kind {
        OpKind::Pkg => t.typed::<PkgResult>().map(|r| (r.rc, r.changed, false)),
        OpKind::File => t.typed::<FileResult>().map(|r| (r.rc, r.changed, false)),
        OpKind::Group => t.typed::<GroupResult>().map(|r| (r.rc, r.changed, false)),
        OpKind::User => t.typed::<UserResult>().map(|r| (r.rc, r.changed, false)),
        OpKind::Repo => t.typed::<RepoResult>().map(|r| (r.rc, r.changed, false)),
        OpKind::Svc => t.typed::<SvcResult>().map(|r| (r.rc, r.changed, r.skipped)),
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
            allowed: w.node.allowed.clone(),
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
        for site in crate::trace::sh_sites(window.ops.iter().map(|o| &o.node)) {
            if !self.report.sh_sites.contains(&site) {
                self.report.sh_sites.push(site);
            }
        }
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
        for w in &window.ops {
            if w.node.guard.is_some() {
                let why = format!(
                    "op #{} has a guard (when); guards are not supported by the executor yet",
                    w.id.0
                );
                self.block(why.clone());
                return Err(why);
            }
        }
        let prepared = prepare::prepare_window(
            &mut self.shells,
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
            Err(why) => {
                self.block(why.clone());
                return Err(why);
            }
        }

        // Render each op for its principal's shell (open now).
        let mut scripts = Vec::new();
        for w in &window.ops {
            let who = &w.node.run_as.principal;
            let Some(sh) = self.shell_for(who) else {
                let why = format!("op #{}: no shell open for {who:?}", w.id.0);
                self.block(why.clone());
                return Err(why);
            };
            match crate::trailer::render_for(&w.node, w.id.0, sh.nonce(), &host.os, sh.user()) {
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

        // Consecutive ops on the same principal's shell form a run. Each
        // run is pipelined into its shell and collected in full before the
        // next run is sent (Assumption A3); after a failure nothing more is
        // sent on any shell.
        let mut start = 0;
        while start < window.ops.len() && !failed_seen && send_err.is_none() {
            let who = window.ops[start].node.run_as.principal.clone();
            let end = window.ops[start..]
                .iter()
                .position(|w| w.node.run_as.principal != who)
                .map_or(window.ops.len(), |n| start + n);

            // Send the run, gated on the shell's halt flag.
            let Some(sh) = self.shell_for(&who) else {
                break;
            };
            let nonce = sh.nonce().to_string();
            for (w, script) in window.ops[start..end].iter().zip(&scripts[start..end]) {
                let set = if w.node.allow_fail {
                    ""
                } else {
                    "[ $? -eq 0 ] || __tp_halt=1\n"
                };
                let gated = format!(
                    "if [ -z \"${{__tp_halt-}}\" ]; then\n{script}{set}else\n{}fi\n",
                    skip_script(&w.node, w.id.0, &nonce)
                );
                if let Err(e) = sh.send(w.id.0, &gated).await {
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
            // An op of this run with no result was lost with the shell:
            // stop here, the loop below reports it.
            if done[start..end].iter().any(|d| !d) {
                break;
            }
            start = end;
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
    let os = match read_os(&mut shell).await {
        Ok(os) => os,
        Err(e) => {
            shell.close().await;
            close_target(&target).await;
            return blocked(format!("reading os: {e}"));
        }
    };
    let host = Host::new(&name, os, Rc::new(spec.vars));
    let pb = Playbook {
        name: pb_name,
        run: pb_run,
    };
    let shells = HostShells {
        host: name.clone(),
        target,
        root: shell,
        users: BTreeMap::new(),
    };
    let mut engine = Engine::new(HostExec::new(&name, shells, sink.clone()));
    let res = engine.run_host(&pb, host).await;
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
