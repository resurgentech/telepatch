//! Reboot marks, the barrier and the end of the run (T102): the executor's
//! half of `.needs_reboot()` and `h.reboot().if_pending()`. Declared inside
//! [`crate::executor`] (`#[path]`), so it works on [`HostExec`]'s own state.
//!
//! # The mark
//!
//! A marked op that changed something leaves one line describing it in
//! `${TP_RUN_DIR:-/run/telepatch}/reboot-pending` on the host (the line is
//! the member's subject: `pkg linux-generic`, `file /etc/x`). The executor
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
//!   run, the run ends with `reboot pending at barrier`;
//! - otherwise the boot id is read, `systemctl reboot` goes over the root
//!   shell, every connection is dropped, and a fresh root shell is opened
//!   every `poll` until its boot id differs or `deadline` passes (the host
//!   then fails). The new root shell replaces the old; login shells are
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
//! in the report (`reboot pending (..)`; the host stays a success). With
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

/// The script that appends `lines` to the mark (shell builtins and one
/// `mkdir -p`: no op text on a command line).
fn mark_script(lines: &[String]) -> String {
    let q: Vec<String> = lines
        .iter()
        .map(|l| crate::sh::quote(&l.replace(['\n', '\r'], " ")))
        .collect();
    format!(
        "d=\"{RUN_DIR}\"\nmkdir -p \"$d\" && printf '%s\\n' {} >> \"$d/{MARK_FILE}\"\n",
        q.join(" ")
    )
}

/// The script that prints the mark, one line per entry (builtins only).
fn pending_script() -> String {
    format!(
        "f=\"{RUN_DIR}/{MARK_FILE}\"\nif [ -s \"$f\" ]; then while IFS= read -r l || [ -n \"$l\" ]; do printf '%s\\n' \"$l\"; done < \"$f\"; fi\n"
    )
}

/// The script that prints the boot id: the first line of
/// `${TP_BOOT_ID:-/proc/sys/kernel/random/boot_id}` (`TP_BOOT_ID` is a
/// test seam).
fn boot_id_script() -> String {
    "{ read -r id || [ -n \"$id\" ]; } 2>/dev/null < \"${TP_BOOT_ID:-/proc/sys/kernel/random/boot_id}\" && echo \"$id\"\n"
        .to_string()
}

/// The marks in `out`: non-empty lines, each once, in order.
fn parse_marks(out: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for l in out.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if !v.iter().any(|x| x == l) {
            v.push(l.to_string());
        }
    }
    v
}

/// The subjects of the members of `w` that mark the host: marked
/// themselves, and reported changed (`known` is what `HostExec::remember`
/// stored: for a merged `pkg` op a member changed only when one of its own
/// names was installed).
fn marked_subjects(w: &WindowOp, known: &HashMap<NodeId, Known>) -> Vec<String> {
    w.members
        .iter()
        .filter(|m| m.needs_reboot && known.get(&m.id).is_some_and(|k| k.changed))
        .map(|m| subject(w, m))
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
    async fn pending_marks(&mut self) -> Result<Vec<String>, String> {
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
    pub(super) fn marks_of(&self, w: &WindowOp, at: usize, into: &mut Vec<(usize, String)>) {
        into.extend(marked_subjects(w, &self.known).into_iter().map(|s| (at, s)));
    }

    /// Append `marks` to the host's mark file. `Err` names why not.
    pub(super) async fn write_marks(&mut self, marks: &[(usize, String)]) -> Result<(), String> {
        let lines: Vec<String> = marks.iter().map(|(_, s)| s.clone()).collect();
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
        self.rsay(&role, &format!("reboot: pending ({})", pending.join(", ")));
        self.sink
            .event(json!({"ev":"reboot_pending","host":self.name,
            "at":"barrier","role":role,"items":pending}));
        if self.reboot.policy == RebootPolicy::Never || !self.over_ssh() {
            self.rsay(&role, "reboot pending at barrier");
            self.sink
                .event(json!({"ev":"reboot_stopped","host":self.name,
                "role":role,"items":pending}));
            self.halted = true;
            if self.verdict.is_none() {
                self.verdict = Some(HostStatus::RebootPending(pending));
            }
            return Ok(Crossed::Continue);
        }
        self.do_reboot(window, &role, pending).await
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
        self.rsay(
            "reboot",
            &format!("pending at the end of the run ({})", pending.join(", ")),
        );
        self.sink
            .event(json!({"ev":"reboot_pending","host":self.name,
            "at":"end","items":pending}));
        self.report.reboot_pending = pending.clone();
        if self.reboot.policy == RebootPolicy::Auto
            && self.over_ssh()
            && !self.halted
            && self.do_reboot(window, "reboot", pending).await? == Crossed::Rebooted
        {
            self.report.reboot_pending.clear();
        }
        Ok(())
    }

    /// Reboot the host over SSH and wait for it ([`RebootWait`]). The host
    /// is halted with a [`HostStatus::Blocked`] reason when it does not
    /// come back; the result is then `Continue`.
    async fn do_reboot(
        &mut self,
        window: usize,
        role: &str,
        pending: Vec<String>,
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
                self.rsay(role, &why);
                self.block(why);
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
                self.rsay(role, &why);
                self.block(why);
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
                    self.sink.event(json!({"ev":"reboot","host":self.name,
                        "down_s":down,"old_boot_id":old,"new_boot_id":id,"items":pending}));
                    self.report.reboots.push(RebootRecord {
                        window,
                        down_secs: down,
                        old_boot_id: old,
                        new_boot_id: id,
                        pending,
                    });
                    return Ok(Crossed::Rebooted);
                }
                Ok(Ok((sh, _))) => {
                    last = "boot id unchanged".to_string();
                    sh.close().await;
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
        if let Err(e) = init_shell(&mut sh).await {
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

    #[test]
    fn a_merged_op_marks_only_the_marked_members_that_changed() {
        let w = merged([("nvidia-driver-570", true, 3), ("cuda-toolkit", false, 7)]);
        // Both changed: only the marked member marks.
        assert_eq!(
            marked_subjects(&w, &known(&[3, 7], &[])),
            ["pkg nvidia-driver-570"]
        );
        // The marked member's own name was not installed: no mark, even
        // though the merged op as a whole changed.
        assert!(marked_subjects(&w, &known(&[7], &[3])).is_empty());
        // Never reported: no mark.
        assert!(marked_subjects(&w, &known(&[], &[])).is_empty());
        // Both marked, one changed.
        let w = merged([("a", true, 3), ("b", true, 7)]);
        assert_eq!(marked_subjects(&w, &known(&[7], &[3])), ["pkg b"]);
    }

    #[test]
    fn the_mark_script_uses_builtins_and_one_mkdir_and_keeps_a_line_a_line() {
        let s = mark_script(&["pkg a".into(), "sh echo 'x'\ny".into()]);
        assert!(
            s.starts_with("d=\"${TP_RUN_DIR:-/run/telepatch}\"\nmkdir -p \"$d\" && printf"),
            "{s}"
        );
        assert!(s.ends_with(">> \"$d/reboot-pending\"\n"), "{s}");
        // One line each: a newline in a subject is a space.
        assert_eq!(s.matches('\n').count(), 2, "{s}");
        assert_eq!(parse_marks("pkg a\n\npkg a\n pkg b \n"), ["pkg a", "pkg b"]);
    }
}
