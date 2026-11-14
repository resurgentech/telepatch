//! What a run produced: one [`HostReport`] per host, the status table and
//! the exit code.

use std::fmt::Write;

/// How a host ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostStatus {
    /// Everything ran, nothing changed.
    Ok,
    /// Everything ran, something changed.
    Changed,
    /// An op failed (`rc != 0`, not `allow_fail`). Its role failed; later
    /// ops on the host did not run. `tail` is the last 40 lines of the
    /// op's output.
    Failed {
        role: String,
        op_id: usize,
        op: String,
        rc: i32,
        tail: String,
    },
    /// Prepare (or opening the host) failed; nothing was applied by the
    /// window that failed.
    Blocked(String),
    /// The playbook itself is wrong (trace error) or the engine gave up.
    Error(String),
    /// Never started: `--serial` held it back and `--max-fail` stopped the
    /// run first. Nothing ran on it. Counts as not a success.
    NotStarted(String),
    /// The run stopped at a reboot barrier it cannot cross (`--local`, the
    /// docker transport, `--reboot never`) with a reboot pending: what is
    /// listed needs it. Everything before the barrier is applied, nothing
    /// after it ran; the next run, after the reboot, goes past it. Not a
    /// success: the playbook did not complete.
    RebootPending(RebootStop),
}

/// One thing a pending reboot is for: the op that marked the host. A mark
/// an earlier run left in the old format has no role and no site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebootItem {
    /// `pkg linux-generic`, `file /etc/default/grub`, `file P (secret)`.
    pub subject: String,
    /// The op's role, or the playbook's name outside any role.
    pub role: Option<String>,
    /// The op's `file:line`.
    pub site: Option<String>,
}

impl RebootItem {
    /// A subject whose run recorded nothing else (an old mark).
    pub fn bare(subject: &str) -> RebootItem {
        RebootItem {
            subject: subject.to_string(),
            role: None,
            site: None,
        }
    }

    /// Who marked, as one `marked by` line's tail.
    fn by(&self) -> String {
        match (&self.role, &self.site) {
            (Some(r), Some(s)) => format!("role {r}, {s}"),
            (Some(r), None) => format!("role {r}"),
            (None, Some(s)) => s.clone(),
            (None, None) => "marked by an earlier run; role and site not recorded".to_string(),
        }
    }

    /// The event's shape: `{subject, role, site}`, null for what is unknown.
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({"subject":self.subject,"role":self.role,"site":self.site})
    }
}

/// Why a host cannot reboot itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebootCause {
    /// `--reboot never` was given.
    Never,
    /// The `--local` transport: the machine is the operator's.
    Local,
    /// The docker transport: a container has no host to reboot.
    Docker,
}

impl RebootCause {
    /// The event's word.
    pub fn label(self) -> &'static str {
        match self {
            RebootCause::Never => "never",
            RebootCause::Local => "local",
            RebootCause::Docker => "docker",
        }
    }

    fn why(self) -> &'static str {
        match self {
            RebootCause::Never => "--reboot never was given",
            RebootCause::Local => "the --local transport cannot reboot this machine",
            RebootCause::Docker => "the docker transport cannot reboot a container",
        }
    }

    /// The `to finish` lines of a stop at a barrier.
    fn finish(self) -> Vec<&'static str> {
        match self {
            RebootCause::Never => vec![
                "rerun without --reboot never, or with --reboot auto, to reboot here",
                "(or: reboot the machine yourself, then run again)",
            ],
            RebootCause::Local => vec!["reboot this machine yourself, then run again"],
            RebootCause::Docker => vec!["restart the container yourself, then run again"],
        }
    }

    /// The `to finish` line at the end of a run.
    fn end_finish(c: Option<RebootCause>) -> &'static str {
        match c {
            Some(RebootCause::Local) => "reboot this machine yourself, then run again",
            Some(RebootCause::Docker) => "restart the container yourself, then run again",
            _ => "reboot the host, or rerun with --reboot auto",
        }
    }
}

/// A run stopped at a reboot barrier it cannot cross.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebootStop {
    pub cause: RebootCause,
    /// The role the barrier was traced in, or the playbook's name outside
    /// any: the stream's bracket.
    pub role: String,
    pub playbook: String,
    /// The `h.reboot().if_pending()` call's `file:line`.
    pub barrier_site: Option<String>,
    /// What the reboot is for.
    pub items: Vec<RebootItem>,
    /// Ops reported not run after the barrier.
    pub not_run: usize,
}

/// `   label:     first` and the continuation lines under it.
fn labelled(label: &str, lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if i == 0 {
                format!("   {:<11}{l}", format!("{label}:"))
            } else {
                format!("   {:<11}{l}", "")
            }
        })
        .collect()
}

/// The `marked by` lines for `items`, subjects padded to one column.
fn marked_by(items: &[RebootItem]) -> Vec<String> {
    let w = items.iter().map(|i| i.subject.len()).max().unwrap_or(0) + 3;
    let lines: Vec<String> = items
        .iter()
        .map(|i| format!("{:<w$}{}", i.subject, i.by()))
        .collect();
    labelled("marked by", &lines)
}

impl RebootStop {
    /// The header and detail lines of the stop (stream and report).
    pub fn lines(&self, host: &str) -> Vec<String> {
        let mut v = vec![format!(
            "== {host} [{}] reboot pending at barrier: stopped, not rebooted ==",
            self.role
        )];
        v.extend(labelled("why", &[self.cause.why().to_string()]));
        let at = match &self.barrier_site {
            Some(s) => format!(" at {s}"),
            None => String::new(),
        };
        v.extend(labelled(
            "barrier",
            &[format!(
                "h.reboot().if_pending(){at} (playbook {})",
                self.playbook
            )],
        ));
        v.extend(marked_by(&self.items));
        let n = self.not_run;
        v.extend(labelled(
            "not run",
            &[format!(
                "{n} op{} after the barrier",
                if n == 1 { "" } else { "s" }
            )],
        ));
        let f: Vec<String> = self.cause.finish().iter().map(|s| s.to_string()).collect();
        v.extend(labelled("to finish", &f));
        v
    }
}

/// The end-of-run list: what the host still needs a reboot for. `finish`
/// is false when the run is about to reboot the host itself.
pub fn reboot_end_lines(
    host: &str,
    items: &[RebootItem],
    cause: Option<RebootCause>,
    finish: bool,
) -> Vec<String> {
    let mut v = vec![format!(
        "== {host} [reboot] reboot pending at the end of the run: =="
    )];
    v.extend(marked_by(items));
    if finish {
        v.extend(labelled(
            "to finish",
            &[RebootCause::end_finish(cause).to_string()],
        ));
    }
    v
}

impl HostStatus {
    pub fn label(&self) -> &'static str {
        match self {
            HostStatus::Ok => "ok",
            HostStatus::Changed => "changed",
            HostStatus::Failed { .. } => "failed",
            HostStatus::Blocked(_) => "blocked",
            HostStatus::Error(_) => "error",
            HostStatus::NotStarted(_) => "skipped",
            HostStatus::RebootPending(_) => "reboot pending",
        }
    }

    pub fn is_success(&self) -> bool {
        matches!(self, HostStatus::Ok | HostStatus::Changed)
    }
}

/// What happened to one op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Ran, changed nothing (including an `sh!` skipped by `unless` /
    /// `creates`).
    Unchanged,
    Changed,
    /// `rc != 0` and not `allow_fail`.
    Failed,
    /// `rc != 0` with `allow_fail`.
    AllowedFail,
    /// Never ran: an earlier op on the host failed.
    NotRun,
    /// Its guard (`.when(..)`) was false: not run, not failed, not changed.
    /// Counted with OK in the table.
    Skipped,
}

/// One executed (possibly merged) op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpRecord {
    pub id: usize,
    /// `(trace id, role)` of every trace op this stands for.
    pub members: Vec<(usize, String)>,
    pub desc: String,
    pub outcome: Outcome,
    pub rc: i32,
    pub changed: bool,
    /// The policies the op was permitted to break, with their reasons.
    pub allowed: Vec<crate::policy::Allowance>,
    /// The op's guard as the plan prints it (`changed(#3) | changed(#5)`);
    /// `None` for an unguarded op.
    pub when: Option<String>,
}

/// Packages a window's pkg ops will install, from the prepare queries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WillInstall {
    pub window: usize,
    pub manager: String,
    pub names: Vec<String>,
}

/// Packages an absent op will remove: only names the prepare probe found
/// installed. The line is `pkg: will remove cmake`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WillRemove {
    pub names: Vec<String>,
}

impl WillRemove {
    pub fn line(&self) -> String {
        format!("pkg: will remove {}", self.names.join(", "))
    }
}

/// Names an absent pkg op actually removed. The inverse is to reinstall
/// them (T98).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removed {
    pub id: usize,
    pub names: Vec<String>,
}

/// A git checkout this run created or moved. The inverse (T98) is to
/// remove `path` when `created`, otherwise to check out `was_rev`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCheckout {
    pub id: usize,
    pub path: String,
    pub created: bool,
    /// HEAD before the op moved the checkout. Empty when `created`.
    pub was_rev: String,
}

/// A resource op that finished. `irreversible` when it defined no inverse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceRecord {
    pub id: usize,
    pub kind: String,
    pub name: String,
    pub irreversible: bool,
}

/// A non-distro package a window's op will change, from the prepare
/// probe of its source. Only items that will change are listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WillChange {
    pub window: usize,
    /// The source word: `script`, `cargo`, `rustup`.
    pub source: String,
    /// The op's principal (`root` for `h`).
    pub user: String,
    pub name: String,
    /// `None` when absent; `"unknown"` when the probe failed or found no
    /// version.
    pub installed: Option<String>,
    /// `present`, the pinned version, or `<v> (latest)`
    /// (`unknown (latest)` when the lookup failed).
    pub wanted: String,
}

impl WillChange {
    /// `window N pkg[script] root uv: 0.9.2 -> 0.9.5`.
    pub fn line(&self) -> String {
        Self::joined_line(std::slice::from_ref(self))
    }

    /// One line for items that agree on window, source, user, installed
    /// and wanted (names joined with `, `); the first item's fields speak.
    fn joined_line(group: &[WillChange]) -> String {
        let w = &group[0];
        let names: Vec<&str> = group.iter().map(|g| g.name.as_str()).collect();
        format!(
            "window {} pkg[{}] {} {}: {} -> {}",
            w.window,
            w.source,
            w.user,
            names.join(", "),
            w.installed.as_deref().unwrap_or("absent"),
            w.wanted
        )
    }

    /// The printed lines of `items`: each run of consecutive items that
    /// agree on window, source, user, installed and wanted becomes one line
    /// with the names joined by `, ` (the JSON event stays one per name).
    pub fn lines(items: &[WillChange]) -> Vec<String> {
        let same = |a: &WillChange, b: &WillChange| {
            (a.window, &a.source, &a.user, &a.installed, &a.wanted)
                == (b.window, &b.source, &b.user, &b.installed, &b.wanted)
        };
        let mut out = Vec::new();
        let mut start = 0;
        for i in 1..=items.len() {
            if i == items.len() || !same(&items[start], &items[i]) {
                out.push(Self::joined_line(&items[start..i]));
                start = i;
            }
        }
        out
    }
}

/// Something a window's op will create, from the prepare probe of its
/// kind (`group`, `user`, `repo`). Only items that do not exist yet are listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WillCreate {
    pub window: usize,
    /// The primitive: `group`, `user` or `repo`.
    pub kind: String,
    pub name: String,
    /// What a `user` that exists will get (`groups +docker`,
    /// `shell -> /bin/bash`); `None` for a create.
    pub detail: Option<String>,
}

impl WillCreate {
    /// `group docker: will create`, `user bot: groups +docker`.
    /// A user whose password is set is `user n <secret>: ...`.
    pub fn line(&self) -> String {
        format!(
            "{} {}: {}",
            self.kind,
            self.name,
            self.detail.as_deref().unwrap_or("will create")
        )
    }
}

/// How prepare found a download: in the cache, downloaded and verified,
/// or downloaded with no checksum (staged for its op, never cached).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchState {
    Cached,
    Fetched,
    Unverified,
}

impl FetchState {
    /// The words of the prepare line.
    pub fn words(self) -> &'static str {
        match self {
            FetchState::Cached => "cached",
            FetchState::Fetched => "fetched, sha256 ok",
            FetchState::Unverified => "fetched, not verified",
        }
    }
}

/// One download a window's file ops need, from the prepare of
/// [`crate::ops::fetch`]. A failed one blocks the host instead and is not
/// listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchLine {
    pub window: usize,
    pub url: String,
    /// `None` for an unverified fetch.
    pub sha256: Option<String>,
    pub state: FetchState,
}

impl FetchLine {
    /// `fetch https://x/y.tgz: cached`.
    pub fn line(&self) -> String {
        format!("fetch {}: {}", self.url, self.state.words())
    }
}

/// A window prepare split ([`crate::engine::Split`]): neither curl nor
/// wget was on the host, so the ops up to the one installing a downloader
/// ran as the window and the rest as the next. Nothing of the window had
/// been sent when it split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowSplit {
    pub window: usize,
    /// The installing op's id, and the downloader it names.
    pub after: usize,
    pub tool: String,
    /// The first op after it that needs a download.
    pub consumer: usize,
}

impl WindowSplit {
    /// `window 1: neither curl nor wget is on the host; split after #0
    /// (pkg curl): #1 runs in window 2`.
    pub fn line(&self) -> String {
        format!(
            "window {}: neither curl nor wget is on the host; split after #{} (pkg {}): #{} runs in window {}",
            self.window,
            self.after,
            self.tool,
            self.consumer,
            self.window + 1
        )
    }
}

/// One `systemctl daemon-reload` telepatch ran: the flush at the end of a
/// window, the reload a changed unit asks for before the next `svc` op, or
/// the one systemd's `NeedDaemonReload` asks for. Not an op: it is not
/// counted in the table. A failed flush fails the host
/// ([`HostStatus::Failed`]). A skipped one (no running systemd) is absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonReload {
    pub window: usize,
    pub rc: i32,
    /// Why it ran: `end of window`, `before #N svc <unit> (unit <names>
    /// changed)`, or `before #N <desc> (systemd NeedDaemonReload)`.
    pub reason: String,
}

impl DaemonReload {
    /// `window 1: systemd daemon-reload: end of window`, with
    /// `: FAILED rc=N` when it failed.
    pub fn line(&self) -> String {
        let mut l = format!(
            "window {}: systemd daemon-reload: {}",
            self.window, self.reason
        );
        if self.rc != 0 {
            let _ = write!(l, ": FAILED rc={}", self.rc);
        }
        l
    }
}

/// One role's `[coverage]`: its ops, and how many have an inverse
/// ([`crate::trace::OpSpec::has_inverse`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coverage {
    /// The role as the report names it (nested roles joined with ` > `;
    /// `(no role)` outside any).
    pub role: String,
    pub ops: usize,
    pub reversible: usize,
    /// `sh` ops (`sh!` or the toy `Raw`) without an inverse: the holes.
    pub sh_without_undo: usize,
}

impl Coverage {
    /// `docker: 6 ops, 6 reversible`, or `rust_build: 14 ops, 12
    /// reversible, 2 sh without undo` when there are holes.
    pub fn line(&self) -> String {
        let mut s = format!(
            "{}: {} ops, {} reversible",
            self.role, self.ops, self.reversible
        );
        if self.sh_without_undo > 0 {
            s.push_str(&format!(", {} sh without undo", self.sh_without_undo));
        }
        s
    }
}

/// A unified diff of one file a `--check --diff` found would change. Never
/// made for a secret file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub window: usize,
    pub path: String,
    pub diff: String,
}

/// Where a `--check` stopped tracing: at a fact read that needs the
/// effect of a pending change (an implicit barrier), which a dry run
/// cannot apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckStop {
    /// `file:line` of the read (or of the reboot barrier), when known.
    pub site: Option<String>,
    /// The keys the read asked about (`file:/etc/x`).
    pub reads: Vec<String>,
    /// Stopped at a reboot barrier that may be pending, not at a read.
    pub reboot: bool,
}

impl CheckStop {
    /// `check stopped at src/roles/x.rs:12: this read needs the changes above`.
    pub fn line(&self) -> String {
        if self.reboot {
            return match &self.site {
                Some(s) => format!("check stopped at {s}: reboot pending at barrier"),
                None => "check stopped: reboot pending at barrier".to_string(),
            };
        }
        match &self.site {
            Some(s) => format!("check stopped at {s}: this read needs the changes above"),
            None => "check stopped: this read needs the changes above".to_string(),
        }
    }
}

/// What a `--check` found on one host.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CheckInfo {
    /// Trace ops seen (up to the stop, if any).
    pub ops: usize,
    /// Findings that say something will change: one per printed
    /// `will ...` line, not per op. Sets the exit status.
    pub pending: usize,
    /// `sh` ops whose own test (`unless`, `creates`, `changed_if`) decides
    /// at run time: not checked.
    pub unchecked: usize,
    /// Trace ops (merged-pkg members one each) with a pending finding.
    pub will_ops: usize,
    /// Trace ops prepare probed and found settled.
    pub ok_ops: usize,
    pub coverage: Vec<Coverage>,
    pub stopped: Option<CheckStop>,
}

impl CheckInfo {
    /// `4 will change, 41 ok, 0 blocked`: trace ops, not findings. Ops
    /// decided at run time are in neither count.
    pub fn summary(&self, blocked: bool) -> String {
        format!(
            "{} will change, {} ok, {} blocked",
            self.will_ops,
            self.ok_ops,
            usize::from(blocked)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostReport {
    pub name: String,
    pub status: HostStatus,
    pub ops: Vec<OpRecord>,
    /// Roles with ops that never ran because the host stopped, in trace
    /// order, excluding the failed role.
    pub skipped_roles: Vec<String>,
    /// Roles `--only` skipped, in trace order (empty without `--only`).
    pub not_selected: Vec<String>,
    /// `--only` roles of this binary this host's trace never reached
    /// (`None` when the trace did not finish, or without `--only`).
    pub only_untraced: Option<Vec<String>>,
    pub will_install: Vec<WillInstall>,
    pub will_remove: Vec<WillRemove>,
    /// Names absent pkg ops removed, in the order the ops finished.
    pub removed: Vec<Removed>,
    /// Git checkouts this run created or moved, in the order the ops finished.
    pub git: Vec<GitCheckout>,
    /// Resource ops that finished (rc 0), including one whose plan was no
    /// change. `irreversible` is set when the resource defined no inverse.
    pub resources: Vec<ResourceRecord>,
    pub will_change: Vec<WillChange>,
    pub will_create: Vec<WillCreate>,
    /// Every download the host's file ops needed, in window order.
    pub fetches: Vec<FetchLine>,
    /// Every `sh!` in the ops the executor received, each site once. Printed
    /// as a warning; never changes the status.
    pub sh_sites: Vec<crate::trace::Site>,
    /// Each `daemon-reload` that ran, in order. Not counted in OK or CHANGED.
    pub daemon_reloads: Vec<DaemonReload>,
    /// Each window prepare split for a missing downloader, in order.
    pub splits: Vec<WindowSplit>,
    /// A unified diff per file that will change (`--check --diff`).
    pub diffs: Vec<FileDiff>,
    /// Set by a `--check` run.
    pub check: Option<CheckInfo>,
    /// What the host still needs a reboot for at the end of the run (the
    /// marks `.needs_reboot()` ops left, an earlier run's included). The
    /// host's status is not changed by it. Empty after a reboot.
    pub reboot_pending: Vec<RebootItem>,
    /// Why the host cannot reboot itself, when it cannot (`Local`,
    /// `Docker`; `None` over SSH): the end-of-run list words its way out
    /// by it.
    pub reboot_cause: Option<RebootCause>,
    /// Each reboot the run did, in order.
    pub reboots: Vec<crate::executor::RebootRecord>,
}

impl HostReport {
    pub fn new(name: &str) -> HostReport {
        HostReport {
            name: name.to_string(),
            status: HostStatus::Ok,
            ops: Vec::new(),
            skipped_roles: Vec::new(),
            not_selected: Vec::new(),
            only_untraced: None,
            will_install: Vec::new(),
            will_remove: Vec::new(),
            removed: Vec::new(),
            git: Vec::new(),
            resources: Vec::new(),
            will_change: Vec::new(),
            will_create: Vec::new(),
            fetches: Vec::new(),
            sh_sites: Vec::new(),
            daemon_reloads: Vec::new(),
            splits: Vec::new(),
            diffs: Vec::new(),
            check: None,
            reboot_pending: Vec::new(),
            reboot_cause: None,
            reboots: Vec::new(),
        }
    }

    fn count(&self, f: impl Fn(Outcome) -> bool) -> usize {
        self.ops.iter().filter(|o| f(o.outcome)).count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    pub hosts: Vec<HostReport>,
}

impl Report {
    /// 0 iff every host is `ok` or `changed`. For a `--check`: 1 when a
    /// host is blocked (or failed to open or trace), else 2 when any host
    /// has changes pending, else 0.
    pub fn exit_code(&self) -> i32 {
        if !self.hosts.iter().all(|h| h.status.is_success()) {
            1
        } else if self.is_check() && self.hosts.iter().any(|h| h.status == HostStatus::Changed) {
            2
        } else {
            0
        }
    }

    /// The one closing warning for `--only` roles no host traced: names
    /// every finished trace left untraced, at least one trace finished.
    /// Empty when there is nothing to warn about.
    fn only_warning(&self) -> String {
        let finished: Vec<&Vec<String>> = self
            .hosts
            .iter()
            .filter_map(|h| h.only_untraced.as_ref())
            .collect();
        let Some(first) = finished.first() else {
            return String::new();
        };
        let names: Vec<&str> = first
            .iter()
            .filter(|n| finished.iter().all(|u| u.contains(n)))
            .map(String::as_str)
            .collect();
        if names.is_empty() {
            return String::new();
        }
        format!(
            "\nwarning: --only {}: no host traced {}\n",
            names.join(", "),
            if names.len() == 1 {
                "this role"
            } else {
                "these roles"
            }
        )
    }

    /// Is this the report of a `--check`?
    pub fn is_check(&self) -> bool {
        self.hosts.iter().any(|h| h.check.is_some())
    }

    /// What a `--check` adds to the lines it streamed: per host what
    /// blocked it, and where it stopped (the last line of that host).
    fn render_check(&self) -> String {
        let mut out = String::new();
        for h in &self.hosts {
            match &h.status {
                HostStatus::Blocked(why) => {
                    let _ = writeln!(out, "{}: blocked: {why}", h.name);
                }
                HostStatus::Error(why) => {
                    let _ = writeln!(out, "{}: error: {why}", h.name);
                }
                HostStatus::NotStarted(why) => {
                    let _ = writeln!(out, "{}: not started: {why}", h.name);
                }
                HostStatus::Failed { role, op, rc, .. } => {
                    let _ = writeln!(out, "{}: role {role} failed: {op} (rc {rc})", h.name);
                }
                _ => {}
            }
            if let Some(stop) = h.check.as_ref().and_then(|c| c.stopped.as_ref()) {
                let _ = writeln!(out, "{}: {}", h.name, stop.line());
            }
            if !h.not_selected.is_empty() {
                let _ = writeln!(
                    out,
                    "{}: skipped by --only: {}",
                    h.name,
                    h.not_selected.join(", ")
                );
            }
            for r in h.only_untraced.iter().flatten() {
                let _ = writeln!(out, "{}: --only {r}: not traced on this host", h.name);
            }
        }
        out.push_str(&self.only_warning());
        out
    }

    /// The per-host status table.
    pub fn table(&self) -> String {
        let name_w = self
            .hosts
            .iter()
            .map(|h| h.name.len())
            .max()
            .unwrap_or(0)
            .max(4);
        let status_w = self
            .hosts
            .iter()
            .map(|h| h.status.label().len())
            .max()
            .unwrap_or(0)
            .max(8);
        let mut out = String::new();
        let _ = writeln!(
            out,
            "{:<name_w$}  {:<status_w$} {:>3} {:>7} {:>6} {:>7}  DETAIL",
            "HOST", "STATUS", "OK", "CHANGED", "FAILED", "NOT RUN"
        );
        for h in &self.hosts {
            let ok = h.count(|o| {
                matches!(
                    o,
                    Outcome::Unchanged | Outcome::AllowedFail | Outcome::Skipped
                )
            });
            let detail = match &h.status {
                HostStatus::Failed { role, op, rc, .. } => {
                    format!("role {role}: {op} (rc {rc})")
                }
                HostStatus::Blocked(why) | HostStatus::Error(why) | HostStatus::NotStarted(why) => {
                    why.lines().next().unwrap_or("").to_string()
                }
                HostStatus::RebootPending(_) => "reboot pending at barrier".to_string(),
                _ if !h.reboot_pending.is_empty() => "reboot pending".to_string(),
                _ => String::new(),
            };
            let row = format!(
                "{:<name_w$}  {:<status_w$} {:>3} {:>7} {:>6} {:>7}  {}",
                h.name,
                h.status.label(),
                ok,
                h.count(|o| o == Outcome::Changed),
                h.count(|o| o == Outcome::Failed),
                h.count(|o| o == Outcome::NotRun),
                detail
            );
            let _ = writeln!(out, "{}", row.trim_end());
        }
        out
    }

    /// The table first, then per host what prepare found and every
    /// allowance (`host: #id desc: allowed <policy>: <reason>`), then, per
    /// host that did not succeed, what went wrong: the failed op's last output lines, skipped roles, the
    /// blocking reason.
    pub fn render(&self) -> String {
        if self.is_check() {
            return self.render_check();
        }
        let mut out = self.table();
        for h in &self.hosts {
            for sp in &h.splits {
                let _ = writeln!(
                    out,
                    "
{}: {}",
                    h.name,
                    sp.line()
                );
            }
            for w in &h.will_install {
                let _ = writeln!(
                    out,
                    "\n{}: window {} will install ({}): {}",
                    h.name,
                    w.window,
                    w.manager,
                    w.names.join(", ")
                );
            }
            for w in &h.will_remove {
                let _ = writeln!(out, "\n{}: {}", h.name, w.line());
            }
            for l in WillChange::lines(&h.will_change) {
                let _ = writeln!(out, "\n{}: {l}", h.name);
            }
            for c in &h.will_create {
                let _ = writeln!(out, "\n{}: {}", h.name, c.line());
            }
            for fl in &h.fetches {
                let _ = writeln!(out, "\n{}: {}", h.name, fl.line());
            }
            for d in &h.daemon_reloads {
                let _ = writeln!(out, "\n{}: {}", h.name, d.line());
            }
            for o in &h.ops {
                for a in &o.allowed {
                    let _ = writeln!(out, "\n{}: #{} {}: {a}", h.name, o.id, o.desc);
                }
            }
            for (i, s) in h.sh_sites.iter().enumerate() {
                let nl = if i == 0 { "\n" } else { "" };
                let _ = writeln!(out, "{nl}{}: {}", h.name, crate::plan::sh_warning(s));
            }
        }
        for h in &self.hosts {
            match &h.status {
                HostStatus::Failed {
                    role, op, rc, tail, ..
                } => {
                    let _ = writeln!(out, "\n{}: role {role} failed: {op} (rc {rc})", h.name);
                    if !tail.is_empty() {
                        let _ = writeln!(out, "last output lines:");
                        for l in tail.lines() {
                            let _ = writeln!(out, "  | {l}");
                        }
                    }
                }
                HostStatus::Blocked(why) => {
                    let _ = writeln!(out, "\n{}: blocked: {why}", h.name);
                }
                HostStatus::Error(why) => {
                    let _ = writeln!(out, "\n{}: error: {why}", h.name);
                }
                HostStatus::NotStarted(why) => {
                    let _ = writeln!(out, "\n{}: not started: {why}", h.name);
                }
                HostStatus::RebootPending(stop) => {
                    out.push('\n');
                    for l in stop.lines(&h.name) {
                        let _ = writeln!(out, "{l}");
                    }
                }
                _ => {}
            }
            if !h.reboot_pending.is_empty() {
                out.push('\n');
                for l in reboot_end_lines(&h.name, &h.reboot_pending, h.reboot_cause, true) {
                    let _ = writeln!(out, "{l}");
                }
            }
            for r in &h.reboots {
                let _ = writeln!(
                    out,
                    "{}: rebooted once, down {}s ({})",
                    h.name,
                    r.down_secs,
                    r.pending.join(", ")
                );
            }
            if !h.skipped_roles.is_empty() {
                let _ = writeln!(
                    out,
                    "{}: skipped roles: {}",
                    h.name,
                    h.skipped_roles.join(", ")
                );
            }
            if !h.not_selected.is_empty() {
                let _ = writeln!(
                    out,
                    "{}: skipped by --only: {}",
                    h.name,
                    h.not_selected.join(", ")
                );
            }
            for r in h.only_untraced.iter().flatten() {
                let _ = writeln!(out, "{}: --only {r}: not traced on this host", h.name);
            }
        }
        out.push_str(&self.only_warning());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wc(window: usize, name: &str, installed: Option<&str>, wanted: &str) -> WillChange {
        WillChange {
            window,
            source: "cargo".into(),
            user: "root".into(),
            name: name.into(),
            installed: installed.map(str::to_string),
            wanted: wanted.into(),
        }
    }

    #[test]
    fn render_lists_every_allowance() {
        use crate::policy::{Allowance, Policy};
        let mut h = HostReport::new("ws1");
        h.status = HostStatus::Changed;
        h.ops.push(OpRecord {
            id: 3,
            members: vec![(3, "vendor_agent".into())],
            desc: "file /etc/systemd/system/legacy.service".into(),
            outcome: Outcome::Changed,
            rc: 0,
            changed: true,
            allowed: vec![Allowance::new(
                Policy::UnitViaFile,
                "vendor ships this unit",
            )],
            when: None,
        });
        let r = Report { hosts: vec![h] }.render();
        assert!(
            r.contains(
                "\nws1: #3 file /etc/systemd/system/legacy.service: allowed unit-via-file: vendor ships this unit\n"
            ),
            "{r}"
        );
    }

    #[test]
    fn consecutive_equal_items_join_into_one_line() {
        let items = [
            wc(1, "cargo-deb", None, "present"),
            wc(1, "cargo-generate-rpm", None, "present"),
            wc(1, "other", Some("1.0.0"), "present"),
            wc(1, "again", None, "present"),
            wc(2, "late", None, "present"),
        ];
        assert_eq!(
            WillChange::lines(&items),
            [
                "window 1 pkg[cargo] root cargo-deb, cargo-generate-rpm: absent -> present",
                "window 1 pkg[cargo] root other: 1.0.0 -> present",
                "window 1 pkg[cargo] root again: absent -> present",
                "window 2 pkg[cargo] root late: absent -> present",
            ]
        );
        assert!(WillChange::lines(&[]).is_empty());
        assert_eq!(items[0].line(), WillChange::lines(&items[..1])[0]);
    }

    #[test]
    fn render_lists_sh_warnings_and_exit_code_stays() {
        use crate::trace::Site;
        let mut h = HostReport::new("ws1");
        h.status = HostStatus::Changed;
        h.sh_sites = vec![Site {
            file: "src/roles/x.rs",
            line: 7,
        }];
        let r = Report { hosts: vec![h] };
        assert!(
            r.render()
                .ends_with("\nws1: warning: sh! in a role body at src/roles/x.rs:7\n"),
            "{}",
            r.render()
        );
        assert_eq!(r.exit_code(), 0);
    }
}

// ---- cache usage and purge over an inventory (T123) ----

/// A byte count as `MB` (below a GB) or `GB`, one decimal, 1024-based and
/// truncated, as the end-of-run cache line prints it.
pub fn human_bytes(bytes: u64) -> String {
    const MB: u128 = 1 << 20;
    const GB: u128 = 1 << 30;
    let b = bytes as u128;
    let (unit, name) = if b >= GB { (GB, "GB") } else { (MB, "MB") };
    let tenths = b * 10 / unit;
    format!("{}.{} {name}", tenths / 10, tenths % 10)
}

/// Epoch seconds as `YYYY-MM-DD HH:MM` (UTC).
pub fn utc_minute(epoch: u64) -> String {
    let days = (epoch / 86_400) as i64;
    let secs = epoch % 86_400;
    // Days since 1970-01-01 to a civil date (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}",
        secs / 3600,
        secs % 3600 / 60
    )
}

fn items_of(n: usize) -> String {
    format!("{n} {}", if n == 1 { "item" } else { "items" })
}

/// One cached file, as `cache usage` reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheItem {
    pub sum: String,
    pub bytes: u64,
    /// The record's last-used time; `None` when it has no line for the item.
    pub last: Option<u64>,
    pub owners: Vec<String>,
}

/// One registered cache directory on a host: what `cache usage` read in it
/// (`items`), or what `cache purge` did there.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CacheDir {
    pub path: String,
    pub items: Vec<CacheItem>,
    /// Purge: the items removed, `(sum, bytes)`.
    pub removed: Vec<(String, u64)>,
    /// Purge `--owner`: items that lost the owner and stay.
    pub disowned: usize,
}

impl CacheDir {
    pub fn bytes(&self) -> u64 {
        self.items.iter().map(|i| i.bytes).sum()
    }
    fn removed_bytes(&self) -> u64 {
        self.removed.iter().map(|r| r.1).sum()
    }
    /// The oldest and newest recorded last-used time.
    fn span(&self) -> Option<(u64, u64)> {
        let t: Vec<u64> = self.items.iter().filter_map(|i| i.last).collect();
        Some((*t.iter().min()?, *t.iter().max()?))
    }
    /// `(owner, items, bytes)` per owner, sorted by owner name; an item with
    /// two owners counts under each.
    fn owners(&self) -> Vec<(String, usize, u64)> {
        let mut m: std::collections::BTreeMap<&str, (usize, u64)> = Default::default();
        for i in &self.items {
            for o in &i.owners {
                let e = m.entry(o).or_default();
                e.0 += 1;
                e.1 += i.bytes;
            }
        }
        m.into_iter()
            .map(|(o, (n, b))| (o.to_string(), n, b))
            .collect()
    }
    fn unowned(&self) -> (usize, u64) {
        let u = self.items.iter().filter(|i| i.owners.is_empty());
        (u.clone().count(), u.map(|i| i.bytes).sum())
    }
}

/// How `cache usage` or `cache purge` ended on one host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheStatus {
    Ok,
    /// Unreachable, refused sudo, in use by a running run, or a refusal
    /// the script gave; the reason.
    Failed(String),
}

/// One host of a `cache usage` or `cache purge`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheHost {
    pub name: String,
    pub status: CacheStatus,
    pub dirs: Vec<CacheDir>,
    pub warnings: Vec<String>,
    /// Directories a purge removed.
    pub removed_dirs: Vec<String>,
}

impl CacheHost {
    pub fn failed(name: &str, why: impl Into<String>) -> CacheHost {
        CacheHost {
            name: name.to_string(),
            status: CacheStatus::Failed(why.into()),
            dirs: Vec::new(),
            warnings: Vec::new(),
            removed_dirs: Vec::new(),
        }
    }

    /// Read what `cache_usage_script` / `cache_purge_script` printed. A line
    /// that is not one of theirs is kept as a warning, not dropped.
    pub fn parse(name: &str, out: &str) -> CacheHost {
        let mut h = CacheHost::failed(name, "");
        h.status = CacheStatus::Ok;
        for line in out.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            match (f[0], f.len()) {
                ("D", 2) => h.dirs.push(CacheDir {
                    path: f[1].to_string(),
                    ..Default::default()
                }),
                ("I", 5) if !h.dirs.is_empty() => {
                    let item = CacheItem {
                        sum: f[1].to_string(),
                        bytes: f[2].parse().unwrap_or(0),
                        last: f[3].parse().ok(),
                        owners: f[4]
                            .split(',')
                            .filter(|o| !o.is_empty())
                            .map(String::from)
                            .collect(),
                    };
                    h.dirs.last_mut().unwrap().items.push(item);
                }
                ("X", 3) if !h.dirs.is_empty() => h
                    .dirs
                    .last_mut()
                    .unwrap()
                    .removed
                    .push((f[1].to_string(), f[2].parse().unwrap_or(0))),
                ("U", 2) if !h.dirs.is_empty() => h.dirs.last_mut().unwrap().disowned += 1,
                ("R", 2) => h.removed_dirs.push(f[1].to_string()),
                ("W", 2) => h.warnings.push(f[1].to_string()),
                ("E", 2) => {
                    h.status = CacheStatus::Failed(f[1].to_string());
                    return h;
                }
                ("L", 1) => {
                    h.status = CacheStatus::Failed("in use by a running run".to_string());
                    return h;
                }
                _ if line.trim().is_empty() => {}
                _ => h.warnings.push(line.to_string()),
            }
        }
        h
    }

    pub fn is_ok(&self) -> bool {
        self.status == CacheStatus::Ok
    }
    pub fn items(&self) -> usize {
        self.dirs.iter().map(|d| d.items.len()).sum()
    }
    pub fn bytes(&self) -> u64 {
        self.dirs.iter().map(CacheDir::bytes).sum()
    }
    pub fn removed_items(&self) -> usize {
        self.dirs.iter().map(|d| d.removed.len()).sum()
    }
    pub fn removed_bytes(&self) -> u64 {
        self.dirs.iter().map(CacheDir::removed_bytes).sum()
    }
}

/// The hosts of a `cache usage` or `cache purge`, in the order given.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CacheReport {
    pub hosts: Vec<CacheHost>,
}

impl CacheReport {
    /// 0 when every host was read or purged, 1 when any failed.
    pub fn exit_code(&self) -> i32 {
        i32::from(self.hosts.iter().any(|h| !h.is_ok()))
    }

    fn ok_hosts(&self) -> impl Iterator<Item = &CacheHost> {
        self.hosts.iter().filter(|h| h.is_ok())
    }

    fn failures(&self, o: &mut String) {
        let bad: Vec<&CacheHost> = self.hosts.iter().filter(|h| !h.is_ok()).collect();
        if bad.is_empty() {
            return;
        }
        let _ = writeln!(o, "\nfailed hosts:");
        for h in bad {
            if let CacheStatus::Failed(w) = &h.status {
                let _ = writeln!(o, "  {}: {w}", h.name);
            }
        }
    }

    fn head(h: &CacheHost, o: &mut String) -> bool {
        if let CacheStatus::Failed(w) = &h.status {
            let _ = writeln!(o, "host {}: failed: {w}", h.name);
            return false;
        }
        let _ = writeln!(o, "host {}", h.name);
        true
    }

    /// The `cache usage` table.
    pub fn render_usage(&self) -> String {
        let mut o = String::new();
        for h in &self.hosts {
            if !Self::head(h, &mut o) {
                continue;
            }
            for d in &h.dirs {
                let mut line = format!(
                    "  {}: {}, {}",
                    d.path,
                    items_of(d.items.len()),
                    human_bytes(d.bytes())
                );
                if let Some((a, b)) = d.span() {
                    let _ = write!(line, ", last used {} .. {}", utc_minute(a), utc_minute(b));
                }
                let _ = writeln!(o, "{line}");
                for (name, n, b) in d.owners() {
                    let _ = writeln!(o, "    owner {name}: {}, {}", items_of(n), human_bytes(b));
                }
                let (n, b) = d.unowned();
                if n > 0 {
                    let _ = writeln!(o, "    unowned: {}, {}", items_of(n), human_bytes(b));
                }
            }
            for w in &h.warnings {
                let _ = writeln!(o, "  warning: {w}");
            }
            let _ = writeln!(
                o,
                "  host total: {}, {}",
                items_of(h.items()),
                human_bytes(h.bytes())
            );
        }
        let (items, bytes) = self
            .ok_hosts()
            .fold((0, 0), |a, h| (a.0 + h.items(), a.1 + h.bytes()));
        let _ = writeln!(
            o,
            "\nrun total: {}, {}, {}",
            if self.ok_hosts().count() == 1 {
                "1 host".to_string()
            } else {
                format!("{} hosts", self.ok_hosts().count())
            },
            items_of(items),
            human_bytes(bytes)
        );
        self.failures(&mut o);
        o
    }

    /// `cache usage --json`: one document, sizes in bytes, times in epoch
    /// seconds (`null` when the record has none).
    pub fn usage_json(&self) -> serde_json::Value {
        use serde_json::json;
        let hosts: Vec<_> = self
            .hosts
            .iter()
            .map(|h| match &h.status {
                CacheStatus::Failed(w) => json!({"host": h.name, "status": "failed", "reason": w}),
                CacheStatus::Ok => {
                    let dirs: Vec<_> = h
                        .dirs
                        .iter()
                        .map(|d| {
                            let (un, ub) = d.unowned();
                            json!({
                                "dir": d.path,
                                "items": d.items.len(),
                                "bytes": d.bytes(),
                                "oldest": d.span().map(|s| s.0),
                                "newest": d.span().map(|s| s.1),
                                "owners": d.owners().into_iter()
                                    .map(|(o, n, b)| json!({"owner": o, "items": n, "bytes": b}))
                                    .collect::<Vec<_>>(),
                                "unowned": {"items": un, "bytes": ub},
                            })
                        })
                        .collect();
                    json!({
                        "host": h.name, "status": "ok", "items": h.items(),
                        "bytes": h.bytes(), "dirs": dirs, "warnings": h.warnings,
                    })
                }
            })
            .collect();
        let (items, bytes) = self
            .ok_hosts()
            .fold((0, 0), |a, h| (a.0 + h.items(), a.1 + h.bytes()));
        json!({
            "hosts": hosts,
            "total": {
                "hosts": self.ok_hosts().count(),
                "failed": self.hosts.iter().filter(|h| !h.is_ok()).count(),
                "items": items,
                "bytes": bytes,
            },
        })
    }

    /// `cache purge` without `--yes`: what `--yes` would remove, from a
    /// usage read. With an owner, only the items that name it count: those
    /// it alone owns are deleted, the rest lose it.
    pub fn render_would_purge(&self, owner: Option<&str>) -> String {
        let mut o = String::from("cache purge: nothing changed; add --yes to remove\n\n");
        let (mut items, mut bytes) = (0, 0);
        for h in &self.hosts {
            if !Self::head(h, &mut o) {
                continue;
            }
            let (mut hi, mut hb) = (0, 0);
            for d in &h.dirs {
                match owner {
                    None => {
                        let _ = writeln!(
                            o,
                            "  {}: would remove {}, {}",
                            d.path,
                            items_of(d.items.len()),
                            human_bytes(d.bytes())
                        );
                        hi += d.items.len();
                        hb += d.bytes();
                    }
                    Some(p) => {
                        let named: Vec<&CacheItem> = d
                            .items
                            .iter()
                            .filter(|i| i.owners.iter().any(|x| x == p))
                            .collect();
                        let gone: Vec<&&CacheItem> =
                            named.iter().filter(|i| i.owners.len() == 1).collect();
                        let gb: u64 = gone.iter().map(|i| i.bytes).sum();
                        let _ = writeln!(
                            o,
                            "  {}: would remove owner {p} from {}, delete {}, {}",
                            d.path,
                            items_of(named.len()),
                            items_of(gone.len()),
                            human_bytes(gb)
                        );
                        hi += gone.len();
                        hb += gb;
                    }
                }
            }
            for w in &h.warnings {
                let _ = writeln!(o, "  warning: {w}");
            }
            let _ = writeln!(
                o,
                "  host total: would remove {}, {}",
                items_of(hi),
                human_bytes(hb)
            );
            items += hi;
            bytes += hb;
        }
        let _ = writeln!(
            o,
            "\nwould remove {}, {} on {} ok host(s)",
            items_of(items),
            human_bytes(bytes),
            self.ok_hosts().count()
        );
        self.failures(&mut o);
        o
    }

    /// What `cache purge --yes` did.
    pub fn render_purge(&self, owner: Option<&str>) -> String {
        let mut o = String::new();
        for h in &self.hosts {
            if !Self::head(h, &mut o) {
                continue;
            }
            for d in &h.dirs {
                match owner {
                    None => {
                        let _ = writeln!(
                            o,
                            "  {}: removed {}, {}",
                            d.path,
                            items_of(d.removed.len()),
                            human_bytes(d.removed_bytes())
                        );
                    }
                    Some(p) => {
                        let _ = writeln!(
                            o,
                            "  {}: removed owner {p} from {}, deleted {}, {}",
                            d.path,
                            items_of(d.disowned + d.removed.len()),
                            items_of(d.removed.len()),
                            human_bytes(d.removed_bytes())
                        );
                    }
                }
            }
            for r in &h.removed_dirs {
                let _ = writeln!(o, "  removed directory {r}");
            }
            for w in &h.warnings {
                let _ = writeln!(o, "  warning: {w}");
            }
            let _ = writeln!(
                o,
                "  host total: removed {}, {}",
                items_of(h.removed_items()),
                human_bytes(h.removed_bytes())
            );
        }
        let (items, bytes) = self.ok_hosts().fold((0, 0), |a, h| {
            (a.0 + h.removed_items(), a.1 + h.removed_bytes())
        });
        let _ = writeln!(
            o,
            "\nremoved {}, {} on {} host(s)",
            items_of(items),
            human_bytes(bytes),
            self.ok_hosts().count()
        );
        self.failures(&mut o);
        o
    }
}
