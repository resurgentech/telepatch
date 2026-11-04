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
}

impl HostStatus {
    pub fn label(&self) -> &'static str {
        match self {
            HostStatus::Ok => "ok",
            HostStatus::Changed => "changed",
            HostStatus::Failed { .. } => "failed",
            HostStatus::Blocked(_) => "blocked",
            HostStatus::Error(_) => "error",
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
}

/// Packages a window's pkg ops will install, from the prepare queries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WillInstall {
    pub window: usize,
    pub manager: String,
    pub names: Vec<String>,
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
    pub fn line(&self) -> String {
        format!(
            "{} {}: {}",
            self.kind,
            self.name,
            self.detail.as_deref().unwrap_or("will create")
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
    pub will_install: Vec<WillInstall>,
    pub will_change: Vec<WillChange>,
    pub will_create: Vec<WillCreate>,
}

impl HostReport {
    pub fn new(name: &str) -> HostReport {
        HostReport {
            name: name.to_string(),
            status: HostStatus::Ok,
            ops: Vec::new(),
            skipped_roles: Vec::new(),
            will_install: Vec::new(),
            will_change: Vec::new(),
            will_create: Vec::new(),
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
    /// 0 iff every host is `ok` or `changed`.
    pub fn exit_code(&self) -> i32 {
        if self.hosts.iter().all(|h| h.status.is_success()) {
            0
        } else {
            1
        }
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
        let mut out = String::new();
        let _ = writeln!(
            out,
            "{:<name_w$}  {:<8} {:>3} {:>7} {:>6} {:>7}  DETAIL",
            "HOST", "STATUS", "OK", "CHANGED", "FAILED", "NOT RUN"
        );
        for h in &self.hosts {
            let ok = h.count(|o| matches!(o, Outcome::Unchanged | Outcome::AllowedFail));
            let detail = match &h.status {
                HostStatus::Failed { role, op, rc, .. } => {
                    format!("role {role}: {op} (rc {rc})")
                }
                HostStatus::Blocked(why) | HostStatus::Error(why) => {
                    why.lines().next().unwrap_or("").to_string()
                }
                _ => String::new(),
            };
            let row = format!(
                "{:<name_w$}  {:<8} {:>3} {:>7} {:>6} {:>7}  {}",
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

    /// The table first, then, per host that did not succeed, what went
    /// wrong: the failed op's last output lines, skipped roles, the
    /// blocking reason.
    pub fn render(&self) -> String {
        let mut out = self.table();
        for h in &self.hosts {
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
            for l in WillChange::lines(&h.will_change) {
                let _ = writeln!(out, "\n{}: {l}", h.name);
            }
            for c in &h.will_create {
                let _ = writeln!(out, "\n{}: {}", h.name, c.line());
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
                _ => {}
            }
            if !h.skipped_roles.is_empty() {
                let _ = writeln!(
                    out,
                    "{}: skipped roles: {}",
                    h.name,
                    h.skipped_roles.join(", ")
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
}
