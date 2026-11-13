//! `wait_for(cond)`: an op with no change of its own that holds the role
//! until a condition is true on the host, and fails it when the deadline
//! passes.
//!
//! ```ignore
//! h.svc("postgresql").enabled().started();
//! h.wait_for(Port::tcp(5432)).timeout(60 * SEC);
//! h.wait_for(Wait::path("/run/app.sock"));
//! h.wait_for(Wait::unit("postgresql.service"));
//! ```
//!
//! The condition is polled on the host, once at once and then every second:
//!
//! - [`Port::tcp`]: a TCP connection to `localhost:<port>` succeeds (bash's
//!   `/dev/tcp`; no `nc`, `ss` or `curl` needed);
//! - [`Wait::path`]: `[ -e <path> ]`;
//! - [`Wait::unit`]: `systemctl is-active --quiet <unit>`. Like a `svc` op,
//!   it is `skipped` (rc 0) on a host with no `systemctl` or no running
//!   systemd.
//!
//! The deadline is the op's `.timeout(d)` (default [`DEFAULT_WAIT`]). It is
//! kept by the body itself (`$SECONDS`): the op polls every second and,
//! at the deadline, exits 124 with `not reached within d`, which the
//! executor reports as `timed out after d`, failing the role. The
//! controller's own clock for the op is `d` plus 15 seconds
//! ([`crate::trace::OpNode::enforced_timeout`]), a backstop for a probe
//! that hangs. It never reports `changed`, and it reads `*` and writes
//! nothing: every op before it that writes is ordered before it, and
//! every later op that writes is ordered after it, so none is moved
//! across it.

use std::ops::Deref;
use std::time::Duration;

use crate::host::{Host, OpHandle};
use crate::key::Key;
use crate::sh::quote;
use crate::trace::OpSpec;

/// A second, for `.timeout(60 * SEC)`.
pub const SEC: Duration = Duration::from_secs(1);
/// A minute, for `.timeout(5 * MIN)`.
pub const MIN: Duration = Duration::from_secs(60);

/// The deadline of a `wait_for` with no `.timeout(..)`.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(5 * 60);

/// A port to wait for: `Port::tcp(5432)`.
pub struct Port;

impl Port {
    /// A TCP port on the host that accepts a connection.
    pub fn tcp(n: u16) -> Wait {
        Wait::Tcp(n)
    }
}

/// What a `wait_for` waits for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wait {
    /// A TCP port on the host that accepts a connection.
    Tcp(u16),
    /// A path that exists.
    Path(String),
    /// A systemd unit that is active.
    Unit(String),
}

impl Wait {
    pub fn path(p: impl Into<String>) -> Wait {
        Wait::Path(p.into())
    }

    pub fn unit(u: impl Into<String>) -> Wait {
        Wait::Unit(u.into())
    }

    /// `port tcp/5432`, `path /run/x`, `unit pg.service`: the plan's and
    /// the report's words, after `wait_for`.
    pub fn describe(&self) -> String {
        match self {
            Wait::Tcp(n) => format!("port tcp/{n}"),
            Wait::Path(p) => format!("path {p}"),
            Wait::Unit(u) => format!("unit {u}"),
        }
    }

    /// The bash body: polls until the condition holds or `deadline` has
    /// passed (rc 124 and a message on stderr). Never sets `changed`.
    pub fn render(&self, deadline: Duration) -> String {
        let secs = deadline.as_secs().max(1);
        let (setup, test, none) = match self {
            Wait::Tcp(n) => (
                String::new(),
                format!("(exec 3<>/dev/tcp/localhost/{n}) 2>/dev/null"),
                String::new(),
            ),
            Wait::Path(p) => (String::new(), format!("[ -e {} ]", quote(p)), String::new()),
            Wait::Unit(u) => (
                format!("tp_u={}\n", quote(u)),
                "systemctl is-active --quiet \"$tp_u\"".to_string(),
                "if [ -d \"${TP_SVC_RUNDIR:-/run/systemd/system}\" ] && command -v systemctl >/dev/null 2>&1; then :; else skipped=1; fi\n".to_string(),
            ),
        };
        let what = self.describe();
        let mut o = String::from("rc=0; changed=0; skipped=0\n");
        o.push_str(&setup);
        o.push_str(&none);
        o.push_str(&format!(
            "if [ \"$skipped\" -eq 0 ]; then\n  tp_end=$((SECONDS+{secs}))\n  until {test}; do\n    if [ \"$SECONDS\" -ge \"$tp_end\" ]; then\n      echo {} >&2\n      rc=124\n      break\n    fi\n    sleep 1\n  done\nfi\n",
            quote(&format!(
                "telepatch: wait_for {what}: not reached within {secs}s"
            ))
        ));
        o
    }
}

impl<V> Host<V> {
    /// Wait until `cond` holds on the host (see the module doc). Add
    /// `.timeout(d)` for the deadline; the role fails when it passes.
    #[track_caller]
    pub fn wait_for(&self, cond: Wait) -> WaitHandle {
        let h = self.op(OpSpec::Wait(cond));
        // Everything before it is done; it changes nothing itself.
        h.reads([Key::Any]);
        h.writes([]);
        WaitHandle(h)
    }
}

/// Handle for a `wait_for` op.
#[derive(Clone)]
pub struct WaitHandle(OpHandle);

impl Deref for WaitHandle {
    type Target = OpHandle;
    fn deref(&self) -> &OpHandle {
        &self.0
    }
}

crate::host::op_builders!(WaitHandle);

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Run `w`'s body in bash with `deadline`; `(rc, skipped, stderr)`.
    fn run(
        w: &Wait,
        deadline: Duration,
        path: Option<&str>,
        rundir: Option<&str>,
    ) -> (i32, bool, String) {
        let script = format!(
            "{}\necho \"skipped=$skipped\"\nexit $rc\n",
            w.render(deadline)
        );
        let mut c = Command::new("bash");
        c.arg("-c").arg(script);
        if let Some(p) = path {
            c.env("PATH", p);
        }
        if let Some(r) = rundir {
            c.env("TP_SVC_RUNDIR", r);
        }
        let o = c.output().unwrap();
        let out = String::from_utf8_lossy(&o.stdout);
        (
            o.status.code().unwrap(),
            out.contains("skipped=1"),
            String::from_utf8_lossy(&o.stderr).to_string(),
        )
    }

    fn stub_systemctl(d: &std::path::Path, rc: i32) -> String {
        let bin = d.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let f = bin.join("systemctl");
        std::fs::write(&f, format!("#!/bin/sh\nexit {rc}\n")).unwrap();
        std::fs::set_permissions(&f, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        format!("{}:/usr/bin:/bin", bin.display())
    }

    #[test]
    fn the_words_of_each_condition() {
        assert_eq!(Port::tcp(5432).describe(), "port tcp/5432");
        assert_eq!(Wait::path("/run/x").describe(), "path /run/x");
        assert_eq!(Wait::unit("pg.service").describe(), "unit pg.service");
    }

    #[test]
    fn a_path_that_exists_passes_at_once_and_a_missing_one_exits_124_at_the_deadline() {
        let d = tempfile::tempdir().unwrap();
        let there = d.path().to_str().unwrap();
        assert_eq!(run(&Wait::path(there), SEC, None, None).0, 0);
        let (rc, _, err) = run(&Wait::path(format!("{there}/nope")), SEC, None, None);
        assert_eq!(rc, 124);
        assert!(err.contains("not reached within 1s"), "{err}");
    }

    #[test]
    fn a_path_with_quotes_is_one_word() {
        let d = tempfile::tempdir().unwrap();
        let odd = d.path().join("it's here");
        std::fs::write(&odd, "").unwrap();
        assert_eq!(
            run(&Wait::path(odd.to_str().unwrap()), SEC, None, None).0,
            0
        );
    }

    #[test]
    fn a_unit_waits_on_systemctl_is_active_and_skips_where_systemd_is_not_running() {
        let d = tempfile::tempdir().unwrap();
        let rundir = d.path().to_str().unwrap();
        let active = stub_systemctl(d.path(), 0);
        let (rc, skipped, _) = run(&Wait::unit("x.service"), SEC, Some(&active), Some(rundir));
        assert_eq!((rc, skipped), (0, false));
        let inactive = stub_systemctl(d.path(), 3);
        let (rc, _, err) = run(&Wait::unit("x.service"), SEC, Some(&inactive), Some(rundir));
        assert_eq!(rc, 124, "{err}");
        // No running systemd (the run directory is absent): skipped, rc 0.
        let (rc, skipped, _) = run(
            &Wait::unit("x.service"),
            SEC,
            Some(&inactive),
            Some("/nonexistent-rundir"),
        );
        assert_eq!((rc, skipped), (0, true));
    }

    #[test]
    fn it_reads_everything_writes_nothing_and_never_counts_as_a_hole() {
        let h = Host::new(
            "t",
            crate::facts::os::Os::parse("ID=debian\nVERSION_ID=12\n").unwrap(),
            std::rc::Rc::new(()),
        );
        let id = h.wait_for(Port::tcp(1)).id();
        let t = h.snapshot();
        let crate::trace::Node::Op(op) = &t.nodes[id.0] else {
            panic!()
        };
        assert!(op.writes.is_empty() && op.post.is_empty());
        assert_eq!(op.reads.iter().collect::<Vec<_>>(), vec![&Key::Any]);
        assert!(op.spec.has_inverse());
    }
}
