//! Reboot marks, the barrier and the end of the run (T102): the executor's
//! half of `.needs_reboot()` and `h.reboot().if_pending()`. Declared inside
//! [`crate::executor`] (`#[path]`), so it works on [`HostExec`]'s own state.
//!
//! # The mark
//!
//! A marked op that changed something leaves one line describing it in
//! `${TP_RUN_DIR:-/run/telepatch}/reboot-pending` on the host:
//! `<subject>\t<role>\t<file:line>` (the member's subject, `pkg
//! linux-generic`, `file /etc/x`; the op's role, or the playbook's name
//! outside any; its site). A line with no tab is an old mark: the subject
//! alone. Reading and de-duplication go by subject. The executor
//! writes it, not the op's shell: after each run of ops is collected (a
//! pipelined run owns its shell until then) it sends one script to the
//! plain root shell, which can write `/run` whichever shell the op ran in.
//! The write comes before the run's failure is acted on, so a later op that
//! failed or halted the host does not lose a mark. A merged `pkg` op marks
//! only for the members one of whose own names was installed
//! ([`crate::engine::Member::needs_reboot`], the `Known` outcome per
//! member); any other merged op marks its marked members when it changed.
//! `TP_RUN_DIR` is a test seam, like `TP_SVC_RUNDIR`.
//!
//! # The barrier
//!
//! The engine applies everything before the node, then calls
//! [`HostExec::cross_barrier`]. The mark is read from the host, not kept in
//! memory, so what an earlier run left is seen too. Empty: nothing happens.
//! Pending: the stream says so; then
//!
//! - `--reboot never`, or a local or docker target, stop the host at the
//!   barrier ([`HostStatus::RebootPending`]): later ops are reported not
//!   run, the run ends with `reboot pending at barrier`, and the stream and
//!   the report say why, where, who marked and how to finish
//!   ([`RebootStop`]);
//! - otherwise the boot id is read, `systemctl reboot` goes over the root
//!   shell, every connection is dropped, and a fresh root shell is opened
//!   every `poll` until its boot id differs or `deadline` passes (the host
//!   then fails: [`HostStatus::Failed`], the barrier is the op, rc -1, as
//!   for a failed `systemctl reboot`). The new root shell replaces the old; login shells are
//!   dropped and reopen lazily on next use; the second ("killer") shell is
//!   dropped and reopens on the next timeout; the staleness marks are
//!   cleared. The watchdog needs nothing: it lives inside one run of ops,
//!   and a barrier is never inside one.
//!
//! `--check` never reboots: at a barrier it stops the check when the host
//! is marked or a marked op before it is not known unchanged.
//!
//! # The end
//!
//! After the last window the mark is read again: anything pending is listed
//! in the report (`reboot pending at the end of the run:`; the host stays a success). With
//! `--reboot auto` on SSH the host reboots once more by the same routine.

use super::*;

/// What `--reboot` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RebootPolicy {
    /// No flag: a pending barrier reboots; the end of the run only reports.
    #[default]
    Barriers,
    /// `--reboot auto`: a pending barrier reboots, and so does the end.
    Auto,
    /// `--reboot never`: a pending barrier stops the host; the end reports.
    Never,
}

/// How long the executor waits for a rebooted host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RebootWait {
    /// From `systemctl reboot` to the host answering with a new boot id.
    pub deadline: Duration,
    /// Between two attempts to open a shell.
    pub poll: Duration,
    /// How long the old connection may take to end after the command.
    pub down: Duration,
}

impl Default for RebootWait {
    fn default() -> Self {
        RebootWait {
            deadline: Duration::from_secs(600),
            poll: Duration::from_secs(3),
            down: Duration::from_secs(30),
        }
    }
}

/// What the executor carries about reboots on one host.
#[derive(Default)]
pub(super) struct RebootState {
    pub policy: RebootPolicy,
    pub wait: RebootWait,
    /// The playbook's name: the stream's role for a barrier outside any.
    pub playbook: String,
    /// `--check`: what an op before a barrier marks, not known unchanged.
    pub check_marked: Vec<String>,
}

/// One reboot the executor did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebootRecord {
    /// The window before it.
    pub window: usize,
    /// Seconds from the command to the new boot.
    pub down_secs: u64,
    pub old_boot_id: String,
    pub new_boot_id: String,
    /// What it was for.
    pub pending: Vec<String>,
}

/// `${TP_RUN_DIR:-/run/telepatch}`: where the mark lives.
const RUN_DIR: &str = "${TP_RUN_DIR:-/run/telepatch}";

/// The mark file's name inside it.
const MARK_FILE: &str = "reboot-pending";

use crate::report::{RebootCause, RebootItem, RebootStop, reboot_end_lines};
use crate::sandbox::SBX;

/// The script that appends `lines` to the mark (shell builtins and one
/// `mkdir -p`: no op text on a command line).
fn mark_script(lines: &[String]) -> String {
    let q: Vec<String> = lines
        .iter()
        .map(|l| crate::sh::quote(&l.replace(['\n', '\r'], " ")))
        .collect();
    format!(
        "{SBX}d=\"{RUN_DIR}\"\ntp_sbx \"$d\" || d=/dev/null/refused\nmkdir -p \"$d\" && printf '%s\\n' {} >> \"$d/{MARK_FILE}\"\n",
        q.join(" ")
    )
}

/// The script that prints the mark, one line per entry (builtins only).
fn pending_script() -> String {
    format!(
        "{SBX}f=\"{RUN_DIR}/{MARK_FILE}\"\ntp_sbx \"$f\" || f=/dev/null/refused\nif [ -s \"$f\" ]; then while IFS= read -r l || [ -n \"$l\" ]; do printf '%s\\n' \"$l\"; done < \"$f\"; fi\n"
    )
}

/// The script that prints the boot id: the first line of
/// `${TP_BOOT_ID:-/proc/sys/kernel/random/boot_id}` (`TP_BOOT_ID` is a
/// test seam).
fn boot_id_script() -> String {
    "{ read -r id || [ -n \"$id\" ]; } 2>/dev/null < \"${TP_BOOT_ID:-/proc/sys/kernel/random/boot_id}\" && echo \"$id\"\n"
        .to_string()
}

/// The mark line of `item`: tabs and newlines inside a field are spaces.
fn mark_line(item: &RebootItem) -> String {
    let f = |s: &str| s.replace(['\t', '\n', '\r'], " ");
    format!(
        "{}\t{}\t{}",
        f(&item.subject),
        f(item.role.as_deref().unwrap_or("")),
        f(item.site.as_deref().unwrap_or(""))
    )
}

/// The marks in `out`: non-empty lines, one item per subject, in the order
/// the subjects first appear. A line with no tab is an old mark (the
/// subject alone); a later line for the same subject that records role or
/// site fills in what an old one lacks.
fn parse_marks(out: &str) -> Vec<RebootItem> {
    let mut v: Vec<RebootItem> = Vec::new();
    for l in out.lines() {
        let l = l.trim_end_matches('\r');
        if l.trim().is_empty() {
            continue;
        }
        let item = if l.contains('\t') {
            let mut f = l.split('\t').map(str::trim);
            let subject = f.next().unwrap_or("").to_string();
            let some = |s: Option<&str>| s.filter(|s| !s.is_empty()).map(str::to_string);
            RebootItem {
                subject,
                role: some(f.next()),
                site: some(f.next()),
            }
        } else {
            RebootItem::bare(l.trim())
        };
        if item.subject.is_empty() {
            continue;
        }
        match v.iter_mut().find(|x| x.subject == item.subject) {
            Some(x) => {
                if x.role.is_none() && x.site.is_none() {
                    *x = item;
                }
            }
            None => v.push(item),
        }
    }
    v
}

/// The members of `w` that mark the host: marked themselves, and reported
/// changed (`known` is what `HostExec::remember` stored: for a merged `pkg`
/// op a member changed only when one of its own names was installed). Each
/// is the subject, its role (`playbook` outside any role) and the op's
/// site (a merged op has its first member's).
fn marked_items(w: &WindowOp, known: &HashMap<NodeId, Known>, playbook: &str) -> Vec<RebootItem> {
    w.members
        .iter()
        .filter(|m| m.needs_reboot && known.get(&m.id).is_some_and(|k| k.changed))
        .map(|m| RebootItem {
            subject: subject(w, m),
            role: Some(if m.role.is_empty() {
                playbook.to_string()
            } else {
                m.role.clone()
            }),
            site: w.node.site.map(|s| s.to_string()),
        })
        .collect()
}

impl HostExec {
    /// The run's reboot policy, deadlines and the playbook's name.
    pub(super) fn set_reboot(&mut self, policy: RebootPolicy, wait: RebootWait, playbook: &str) {
        self.reboot = RebootState {
            policy,
            wait,
            playbook: playbook.to_string(),
            check_marked: Vec::new(),
        };
    }

    fn over_ssh(&self) -> bool {
        matches!(*self.shells.target, Target::Ssh(_))
    }

    /// Run `script` on the root shell as a probe.
    async fn root_probe(&mut self, script: &str) -> Result<(i32, String), String> {
        self.seq += 1;
        prepare::probe(&mut self.shells.root, self.seq, script).await
    }

    /// What the host's mark lists now.
    async fn pending_marks(&mut self) -> Result<Vec<RebootItem>, String> {
        let (rc, out) = self.root_probe(&pending_script()).await?;
        if rc != 0 {
            return Err(format!("rc {rc}: {}", out.trim()));
        }
        Ok(parse_marks(&out))
    }

    /// The host's boot id, read on the root shell.
    async fn boot_id(&mut self) -> Result<String, String> {
        let (_, out) = self.root_probe(&boot_id_script()).await?;
        let id = out.trim().to_string();
        if id.is_empty() {
            return Err("no boot id".to_string());
        }
        Ok(id)
    }

    /// The members of `w` that mark the host: marked, and changed by the
    /// outcome just recorded for them. Their subjects, with the index of
    /// `w`'s position in its window.
    pub(super) fn marks_of(&self, w: &WindowOp, at: usize, into: &mut Vec<(usize, RebootItem)>) {
        into.extend(
            marked_items(w, &self.known, &self.reboot.playbook)
                .into_iter()
                .map(|s| (at, s)),
        );
    }

    /// Append `marks` to the host's mark file. `Err` names why not.
    pub(super) async fn write_marks(
        &mut self,
        marks: &[(usize, RebootItem)],
    ) -> Result<(), String> {
        let lines: Vec<String> = marks.iter().map(|(_, s)| mark_line(s)).collect();
        let (rc, out) = self
            .root_probe(&mark_script(&lines))
            .await
            .map_err(|e| format!("could not record the reboot mark: {e}"))?;
        if rc != 0 {
            return Err(format!(
                "could not record the reboot mark (rc {rc}): {}",
                out.trim()
            ));
        }
        Ok(())
    }

    /// `--check`: the marked ops of `window` that a check cannot know
    /// unchanged. A check applies nothing, so none of them is known unless
    /// a probe settled it (a file that already matches).
    pub(super) fn note_check_marks(&mut self, window: &Window) {
        for w in &window.ops {
            for m in w.members.iter().filter(|m| m.needs_reboot) {
                if self.known.get(&m.id).is_some_and(|k| !k.changed) {
                    continue;
                }
                let s = subject(w, m);
                if !self.reboot.check_marked.contains(&s) {
                    self.reboot.check_marked.push(s);
                }
            }
        }
    }

    fn rsay(&self, role: &str, text: &str) {
        self.sink
            .line(&format!("== {} [{role}] {text} ==", self.name));
    }

    /// Cross the barrier `at`: see the module doc.
    pub(super) async fn cross_barrier(
        &mut self,
        window: usize,
        at: &RebootAt,
    ) -> Result<Crossed, String> {
        if self.halted {
            return Ok(Crossed::Continue);
        }
        let role = if at.role.is_empty() {
            self.reboot.playbook.clone()
        } else {
            at.role.clone()
        };
        let pending = match self.pending_marks().await {
            Ok(p) => p,
            Err(why) => {
                self.block(format!("reading the reboot mark: {why}"));
                return Ok(Crossed::Continue);
            }
        };
        if self.check.is_some() {
            let marked = !pending.is_empty() || !self.reboot.check_marked.is_empty();
            return Ok(if marked {
                Crossed::CheckStopped
            } else {
                Crossed::Continue
            });
        }
        if pending.is_empty() {
            return Ok(Crossed::Continue);
        }
        let barrier_site = at.site.map(|s| s.to_string());
        let cause = self.cannot_reboot();
        let items: Vec<serde_json::Value> = pending.iter().map(RebootItem::json).collect();
        let cause_ev = cause.map(RebootCause::label);
        if cause.is_none() {
            let subjects: Vec<&str> = pending.iter().map(|i| i.subject.as_str()).collect();
            self.rsay(&role, &format!("reboot: pending ({})", subjects.join(", ")));
        }
        self.sink
            .event(json!({"ev":"reboot_pending","host":self.name,
            "at":"barrier","role":role,"barrier_site":barrier_site,"cause":cause_ev,
            "items":items}));
        if let Some(cause) = cause {
            // The block of lines is written when the host finishes, once
            // the ops after the barrier are reported not run.
            self.sink
                .event(json!({"ev":"reboot_stopped","host":self.name,
                "role":role,"barrier_site":barrier_site,"cause":cause.label(),
                "items":items}));
            self.halted = true;
            if self.verdict.is_none() {
                self.verdict = Some(HostStatus::RebootPending(RebootStop {
                    cause,
                    role,
                    playbook: self.reboot.playbook.clone(),
                    barrier_site,
                    items: pending,
                    not_run: 0,
                }));
            }
            return Ok(Crossed::Continue);
        }
        let op = match &barrier_site {
            Some(s) => format!("h.reboot().if_pending() at {s}"),
            None => "h.reboot().if_pending()".to_string(),
        };
        self.do_reboot(window, &role, &op, pending).await
    }

    /// Why this host cannot reboot itself at a barrier, if it cannot.
    fn cannot_reboot(&self) -> Option<RebootCause> {
        if self.reboot.policy == RebootPolicy::Never {
            return Some(RebootCause::Never);
        }
        match *self.shells.target {
            Target::Ssh(_) => None,
            Target::Docker(_) => Some(RebootCause::Docker),
            Target::Local(_) => Some(RebootCause::Local),
        }
    }

    /// The host failed because the reboot did not happen: the barrier is
    /// the op, at its site, with rc -1 (`--max-fail` counts it).
    fn reboot_failed(&mut self, role: &str, op: &str, why: String) {
        self.rsay(role, &why);
        self.halted = true;
        if self.verdict.is_none() {
            self.verdict = Some(HostStatus::Failed {
                role: role.to_string(),
                // Not a node of any window.
                op_id: usize::MAX,
                op: op.to_string(),
                rc: -1,
                tail: why,
            });
        }
    }

    /// The end of the run: report what is still pending; with `--reboot
    /// auto` over SSH, reboot once more.
    pub(super) async fn end_pending(&mut self, window: usize) -> Result<(), String> {
        if self.check.is_some() || matches!(self.verdict, Some(HostStatus::RebootPending(_))) {
            return Ok(());
        }
        let probe = tokio::time::timeout(self.reboot.wait.down, self.pending_marks()).await;
        let pending = match probe {
            Ok(Ok(p)) if !p.is_empty() => p,
            // Nothing pending, or a host we cannot read any more (it
            // failed already, or its shell is gone): nothing to report.
            _ => return Ok(()),
        };
        let cause = match *self.shells.target {
            Target::Ssh(_) => None,
            Target::Docker(_) => Some(RebootCause::Docker),
            Target::Local(_) => Some(RebootCause::Local),
        };
        let reboots = self.reboot.policy == RebootPolicy::Auto && self.over_ssh() && !self.halted;
        for l in reboot_end_lines(&self.name, &pending, cause, !reboots) {
            self.sink.line(&l);
        }
        self.sink
            .event(json!({"ev":"reboot_pending","host":self.name,
            "at":"end","barrier_site":null,"cause":cause.map(RebootCause::label),
            "items":pending.iter().map(RebootItem::json).collect::<Vec<_>>()}));
        self.report.reboot_pending = pending.clone();
        self.report.reboot_cause = cause;
        if reboots
            && self
                .do_reboot(window, "reboot", "reboot at the end of the run", pending)
                .await?
                == Crossed::Rebooted
        {
            self.report.reboot_pending.clear();
        }
        Ok(())
    }

    /// Reboot the host over SSH and wait for it ([`RebootWait`]). The host
    /// fails ([`HostStatus::Failed`], `op` the op, rc -1) when the command
    /// fails or it does not come back; the result is then `Continue`.
    async fn do_reboot(
        &mut self,
        window: usize,
        role: &str,
        op: &str,
        pending: Vec<RebootItem>,
    ) -> Result<Crossed, String> {
        let wait = self.reboot.wait;
        let old = match self.boot_id().await {
            Ok(id) => id,
            Err(why) => {
                self.block(format!("reboot: reading the boot id: {why}"));
                return Ok(Crossed::Continue);
            }
        };
        let t0 = Instant::now();
        let limit = t0 + wait.deadline;
        // Not an op: no halt gate, no count in OK or CHANGED.
        self.seq += 1;
        let sent = tokio::time::timeout(
            wait.down,
            prepare::probe(&mut self.shells.root, self.seq, "systemctl reboot"),
        )
        .await;
        let alive = match sent {
            Ok(Ok((0, _))) => true,
            Ok(Ok((rc, out))) => {
                let why = format!("reboot: systemctl reboot failed (rc {rc}): {}", out.trim());
                self.reboot_failed(role, op, why);
                return Ok(Crossed::Continue);
            }
            // The shell went with the host.
            Ok(Err(_)) => false,
            Err(_) => false,
        };
        // Wait for the old connection to end (bounded): the command only
        // queued the reboot.
        if alive {
            let end = Instant::now() + wait.down;
            while Instant::now() < end {
                self.seq += 1;
                let left = end.saturating_duration_since(Instant::now());
                let probe = prepare::probe(&mut self.shells.root, self.seq, "true");
                match tokio::time::timeout(left, probe).await {
                    Ok(Ok(_)) => tokio::time::sleep(Duration::from_millis(250)).await,
                    _ => break,
                }
            }
        }
        self.shells.drop_connections().await;
        self.shells.logins.clear();
        self.shells.stale.clear();
        self.shells.killer.shell = None;
        let mut last = "no answer".to_string();
        loop {
            let now = Instant::now();
            if now >= limit {
                let why = format!(
                    "reboot: host did not come back within {} ({last})",
                    crate::trace::fmt_duration(wait.deadline)
                );
                self.reboot_failed(role, op, why);
                return Ok(Crossed::Continue);
            }
            match tokio::time::timeout(limit - now, self.reopen_root()).await {
                Ok(Ok((sh, id))) if id != old => {
                    self.shells.root = sh;
                    let down = t0.elapsed().as_secs();
                    self.rsay(
                        "reboot",
                        &format!("down {down}s, boot id changed, shells reopened"),
                    );
                    let subjects: Vec<String> = pending.iter().map(|i| i.subject.clone()).collect();
                    self.sink.event(json!({"ev":"reboot","host":self.name,
                        "down_s":down,"old_boot_id":old,"new_boot_id":id,"items":subjects}));
                    self.report.reboots.push(RebootRecord {
                        window,
                        down_secs: down,
                        old_boot_id: old,
                        new_boot_id: id,
                        pending: subjects,
                    });
                    return Ok(Crossed::Rebooted);
                }
                Ok(Ok((sh, _))) => {
                    last = "boot id unchanged".to_string();
                    close_shell(&self.sink, &self.name, "root", sh).await;
                }
                Ok(Err(why)) => last = why,
                // The deadline cut an attempt short: keep what the
                // earlier ones said.
                Err(_) if last == "no answer" => last = "no answer in time".to_string(),
                Err(_) => {}
            }
            close_target(&self.shells.target).await;
            let left = limit.saturating_duration_since(Instant::now());
            tokio::time::sleep(wait.poll.min(left)).await;
        }
    }

    /// A fresh root shell through the transport (the ssh transport starts
    /// a new master: the old one is gone), set up, and the boot id it sees.
    async fn reopen_root(&mut self) -> Result<(Shell, String), String> {
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
        if let Err(e) = init_shell(
            &mut sh,
            cache_owner(&self.reboot.playbook, self.check.is_some()).as_deref(),
        )
        .await
        {
            sh.close().await;
            return Err(format!("could not set up the root shell: {e}"));
        }
        self.seq += 1;
        match prepare::probe(&mut sh, self.seq, &boot_id_script()).await {
            Ok((_, out)) if !out.trim().is_empty() => Ok((sh, out.trim().to_string())),
            Ok(_) => {
                sh.close().await;
                Err("no boot id".to_string())
            }
            Err(e) => {
                sh.close().await;
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Member;
    use crate::ops::pkg::{Manager, PkgSpec};

    fn merged(names: [(&str, bool, usize); 2]) -> WindowOp {
        let spec = PkgSpec::new(
            Manager::Apt,
            names.iter().map(|n| n.0.to_string()).collect(),
        );
        let writes = spec.keys().into_iter().collect();
        let node = OpNode {
            writes,
            post: Default::default(),
            spec: OpSpec::Pkg(spec),
            run_as: RunAs::root_shell(),
            reads: Default::default(),
            guard: None,
            allow_fail: false,
            site: None,
            allowed: Vec::new(),
            at_end: false,
            timeout: None,
            retry: 0,
            needs_reboot: true,
        };
        WindowOp {
            id: NodeId(names[0].2),
            node,
            members: names
                .iter()
                .map(|(n, marked, id)| Member {
                    id: NodeId(*id),
                    role: "r".into(),
                    pkg_names: vec![n.to_string()],
                    needs_reboot: *marked,
                })
                .collect(),
        }
    }

    fn known(changed: &[usize], unchanged: &[usize]) -> HashMap<NodeId, Known> {
        let mut k = HashMap::new();
        for (ids, c) in [(changed, true), (unchanged, false)] {
            for i in ids {
                k.insert(
                    NodeId(*i),
                    Known {
                        ok: true,
                        changed: c,
                        started: false,
                    },
                );
            }
        }
        k
    }

    fn subjects(v: Vec<RebootItem>) -> Vec<String> {
        v.into_iter().map(|i| i.subject).collect()
    }

    #[test]
    fn a_merged_op_marks_only_the_marked_members_that_changed() {
        let w = merged([("nvidia-driver-570", true, 3), ("cuda-toolkit", false, 7)]);
        // Both changed: only the marked member marks.
        assert_eq!(
            subjects(marked_items(&w, &known(&[3, 7], &[]), "pb")),
            ["pkg nvidia-driver-570"]
        );
        // The marked member's own name was not installed: no mark, even
        // though the merged op as a whole changed.
        assert!(marked_items(&w, &known(&[7], &[3]), "pb").is_empty());
        // Never reported: no mark.
        assert!(marked_items(&w, &known(&[], &[]), "pb").is_empty());
        // Both marked, one changed.
        let w = merged([("a", true, 3), ("b", true, 7)]);
        assert_eq!(
            subjects(marked_items(&w, &known(&[7], &[3]), "pb")),
            ["pkg b"]
        );
    }

    #[test]
    fn a_mark_names_the_role_or_the_playbook_and_the_site() {
        let mut w = merged([("a", true, 3), ("b", true, 7)]);
        w.members[1].role = String::new();
        w.node.site = Some(crate::trace::Site {
            file: "src/roles/k.rs",
            line: 31,
        });
        let got = marked_items(&w, &known(&[3, 7], &[]), "gpu");
        assert_eq!(got[0].role.as_deref(), Some("r"));
        assert_eq!(got[1].role.as_deref(), Some("gpu"), "outside any role");
        assert_eq!(got[0].site.as_deref(), Some("src/roles/k.rs:31"));
        assert_eq!(mark_line(&got[0]), "pkg a\tr\tsrc/roles/k.rs:31");
    }

    #[test]
    fn the_mark_script_uses_builtins_and_one_mkdir_and_keeps_a_line_a_line() {
        let s = mark_script(&["pkg a".into(), "sh echo 'x'\ny".into()]);
        // The sandbox switch's function first (T124), then the script.
        let s = s.strip_prefix(SBX).expect("starts with tp_sbx");
        assert!(
            s.starts_with(
                "d=\"${TP_RUN_DIR:-/run/telepatch}\"\ntp_sbx \"$d\" || d=/dev/null/refused\nmkdir -p \"$d\" && printf"
            ),
            "{s}"
        );
        assert!(s.ends_with(">> \"$d/reboot-pending\"\n"), "{s}");
        // One line each: a newline in a subject is a space.
        assert_eq!(s.matches('\n').count(), 3, "{s}");
    }

    #[test]
    fn old_and_new_mark_lines_read_and_dedup_by_subject() {
        let got = parse_marks(
            "pkg a\n\npkg a\n pkg b \nfile /x\trole x\tsrc/a.rs:1\npkg b\tkernel\tsrc/b.rs:9\nfile /y\t\t\n",
        );
        assert_eq!(
            subjects(got.clone()),
            ["pkg a", "pkg b", "file /x", "file /y"]
        );
        // An old line is bare; a later new line for its subject fills it in.
        assert_eq!(got[0], RebootItem::bare("pkg a"));
        assert_eq!(got[1].role.as_deref(), Some("kernel"));
        assert_eq!(got[1].site.as_deref(), Some("src/b.rs:9"));
        // The first new line for a subject wins over a later one.
        assert_eq!(got[2].role.as_deref(), Some("role x"));
        // A new line with empty fields is not "old" but has nothing to say.
        assert_eq!(got[3], RebootItem::bare("file /y"));
    }
}
