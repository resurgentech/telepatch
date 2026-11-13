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
    RebootPending(Vec<String>),
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
    /// `will ...` line, not per op.
    pub pending: usize,
    /// `sh` ops whose own test (`unless`, `creates`, `changed_if`) decides
    /// at run time: not checked.
    pub unchecked: usize,
    pub coverage: Vec<Coverage>,
    pub stopped: Option<CheckStop>,
}

impl CheckInfo {
    /// `3 will change, 12 ops, 0 blocked`.
    pub fn summary(&self, blocked: bool) -> String {
        format!(
            "{} will change, {} ops, {} blocked",
            self.pending,
            self.ops,
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
    pub reboot_pending: Vec<String>,
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
        }
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
                HostStatus::RebootPending(list) => {
                    let _ = writeln!(
                        out,
                        "\n{}: reboot pending at barrier ({})",
                        h.name,
                        list.join(", ")
                    );
                }
                _ => {}
            }
            if !h.reboot_pending.is_empty() {
                let _ = writeln!(
                    out,
                    "{}: reboot pending ({})",
                    h.name,
                    h.reboot_pending.join(", ")
                );
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
        }
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
