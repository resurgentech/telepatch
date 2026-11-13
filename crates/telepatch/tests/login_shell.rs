//! `as_user` ops run in the user's one persistent login shell (T114).
//!
//! Local transport only: `LocalTransport::new().uid(0)` (no sudo) with a
//! fake `getent` on the root shell's PATH, so `as_user("root")` opens its
//! login shell from a temp HOME (`exec env -i HOME=... bash -l` typed into
//! one more root shell). The profile in that HOME counts its own reads in
//! a file, and exports what the stubs below need, because `env -i` leaves
//! the login shell none of the root shell's environment.
//!
//! Slice B (staleness: the profile edited mid-run, `relogin()`) adds its
//! tests under the marked section at the end.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::report::{HostStatus, Outcome, Report};
use telepatch::transport::local::LocalTransport;
use telepatch::{Host, Playbook, PlaybookFuture};

fn write_exe(p: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(p, format!("#!/bin/bash\n{body}\n")).unwrap();
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A temp tree: the fake HOME with its profile, a PATH directory with a
/// fake `getent` and stub `systemctl` / `systemd-analyze`, the unit
/// directory and the booted marker directory.
struct Rig {
    d: tempfile::TempDir,
}

impl Rig {
    fn new() -> Rig {
        let rig = Rig {
            d: tempfile::tempdir().unwrap(),
        };
        let d = rig.d.path();
        let (home, bin) = (d.join("home"), d.join("bin"));
        for n in ["home", "bin", "etc-systemd-system", "run-systemd-system"] {
            std::fs::create_dir(d.join(n)).unwrap();
        }
        write_exe(
            &bin.join("getent"),
            &format!(
                "[ \"$1 $2\" = \"passwd root\" ] || exit 2\n\
                 echo 'root:x:0:0::{}:/bin/bash'",
                home.display()
            ),
        );
        write_exe(&bin.join("systemd-analyze"), "exit 0");
        write_exe(
            &bin.join("systemctl"),
            &format!(
                r#"D={}
echo "systemctl $*" >> $D/log
u="${{@: -1}}"
s=$(cat "$D/state.$u" 2>/dev/null || echo disabled)
case "$1" in
  is-enabled) echo "$s"; case "$s" in enabled|static) exit 0;; *) exit 1;; esac;;
  enable) echo enabled > "$D/state.$u";;
  is-active) if [ -e "$D/active.$u" ]; then echo active; exit 0; else echo inactive; exit 3; fi;;
  show)
    case "$*" in
      *UnitFileState*) exit 1;;
      *) echo no;;
    esac;;
  daemon-reload) ;;
  start|restart) touch "$D/active.$u";;
  *) exit 64;;
esac"#,
                d.display()
            ),
        );
        std::fs::write(home.join(".bash_profile"), rig.profile(1)).unwrap();
        rig
    }

    /// The profile in the fake HOME, exporting `TP_MARK=<mark>`.
    fn profile(&self, mark: u32) -> String {
        let d = self.d.path().display();
        format!(
            "echo x >> {d}/counter\n\
             export TP_MARK={mark}\n\
             export PATH={d}/bin:$PATH\n\
             export TP_SVC_RUNDIR={d}/run-systemd-system\n\
             export TP_UNIT_DIR={d}/etc-systemd-system\n\
             export TP_FETCH_STAGE={d}/stage\n"
        )
    }

    fn dir(&self) -> PathBuf {
        self.d.path().to_path_buf()
    }

    /// A passwd database with no entry for root.
    fn no_passwd(&self) {
        write_exe(&self.d.path().join("bin/getent"), "exit 2");
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.d.path().join(name)).unwrap_or_default()
    }

    fn lines(&self, name: &str) -> usize {
        self.read(name).lines().count()
    }

    /// The `systemctl` calls that changed something.
    fn reloads(&self) -> usize {
        self.read("log")
            .lines()
            .filter(|l| *l == "systemctl daemon-reload")
            .count()
    }

    fn run(&self, name: &'static str, f: fn(Host<PathBuf>) -> PlaybookFuture) -> (Report, String) {
        let (tx, rx) = std::sync::mpsc::channel();
        let vars = self.dir();
        let d = self.dir();
        std::thread::spawn(move || {
            let pb = Playbook::new(name, f);
            let out = Rc::new(RefCell::new(String::new()));
            let path = format!(
                "{}:{}",
                d.join("bin").display(),
                std::env::var("PATH").unwrap()
            );
            let r = run(
                &pb,
                vec![HostSpec {
                    groups: Vec::new(),
                    name: "h1".into(),
                    target: Target::Local(
                        LocalTransport::new()
                            .uid(0)
                            .sandboxed_at(&d)
                            .env("PATH", &path)
                            .env("TP_ROOTVAR", "x")
                            .env("TP_FETCH_STAGE", &d.join("stage").display().to_string())
                            .env(
                                "TP_SVC_RUNDIR",
                                &d.join("run-systemd-system").display().to_string(),
                            )
                            .env(
                                "TP_UNIT_DIR",
                                &d.join("etc-systemd-system").display().to_string(),
                            ),
                    ),
                    vars,
                }],
                RunOpts {
                    log: None,
                    stdout: Stdout::Capture(out.clone()),
                },
            );
            let _ = tx.send((r, out.borrow().clone()));
        });
        rx.recv_timeout(Duration::from_secs(60))
            .expect("the run did not finish in 60s")
    }
}

fn p(h: &Host<PathBuf>, n: &str) -> String {
    h.vars.join(n).display().to_string()
}

// ---- the login environment -------------------------------------------------

fn env_pb(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let (o1, o2, plain) = (p(&h, "o1"), p(&h, "o2"), p(&h, "plain"));
        let show = "echo \"mark=$TP_MARK home=$HOME user=$USER leak=${TP_ROOTVAR-unset}\"";
        h.as_user("root").raw(format!("{show} > {o1}"));
        h.as_user("root").raw(format!("{show} > {o2}"));
        h.raw(format!("{show} > {plain}"));
        Ok(())
    })
}

#[test]
fn the_login_environment_is_read_once_and_is_clean() {
    let rig = Rig::new();
    let (r, out) = rig.run("env", env_pb);
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    let want = format!(
        "mark=1 home={}/home user=root leak=unset\n",
        rig.d.path().display()
    );
    assert_eq!(rig.read("o1"), want, "{out}");
    assert_eq!(rig.read("o2"), want, "{out}");
    // The profile ran once for two ops; a variable of the root shell did
    // not reach the login shell, and the plain op still sees it.
    assert_eq!(rig.lines("counter"), 1, "{out}");
    assert!(
        rig.read("plain").contains("leak=x"),
        "{}",
        rig.read("plain")
    );
}

// ---- the refusal -----------------------------------------------------------

fn one_login_op(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let o = p(&h, "o");
        h.as_user("root").raw(format!("echo hi > {o}"));
        Ok(())
    })
}

#[test]
fn a_missing_passwd_entry_refuses_to_open_the_login_shell() {
    let rig = Rig::new();
    rig.no_passwd();
    let (r, out) = rig.run("nopw", one_login_op);
    let HostStatus::Blocked(why) = &r.hosts[0].status else {
        panic!("{:?}\n{out}", r.hosts[0].status)
    };
    assert!(why.contains("no passwd entry"), "{why}");
    assert!(why.contains("root"), "{why}");
    assert_eq!(rig.read("o"), "", "the op ran");
}

// ---- a timeout in a login shell ----------------------------------------------

fn timeout_pb(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let (a, b) = (p(&h, "pid_a"), p(&h, "pid_b"));
        h.as_user("root")
            .raw(format!("echo $$ > {a}\nsleep 30"))
            .timeout(Duration::from_millis(500))
            .allow_fail();
        h.as_user("root").raw(format!("echo $$ > {b}"));
        Ok(())
    })
}

#[test]
fn a_timeout_in_a_login_shell_kills_the_op_and_the_shell_stays_usable() {
    let rig = Rig::new();
    let t = std::time::Instant::now();
    let (r, out) = rig.run("timeout", timeout_pb);
    assert!(t.elapsed() < Duration::from_secs(25), "{:?}", t.elapsed());
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    let ops = &r.hosts[0].ops;
    assert_eq!(ops[0].outcome, Outcome::AllowedFail, "{out}");
    assert_eq!(ops[1].outcome, Outcome::Unchanged, "{out}");
    let (a, b) = (rig.read("pid_a"), rig.read("pid_b"));
    assert!(!a.trim().is_empty() && a == b, "one shell: {a:?} {b:?}");
}

// ---- guards ------------------------------------------------------------------

fn guard_pb(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let (f1, f2, f3, f4) = (p(&h, "f1"), p(&h, "f2"), p(&h, "f3"), p(&h, "f4"));
        let log = p(&h, "marks");
        let u = h.as_user("root");
        // Same login shell: a variable.
        let a = u.file(&f1).content("a\n");
        u.raw(format!("echo SAME >> {log}")).when(a.changed());
        // Root source, login target: a constant (an earlier run).
        let b = h.file(&f2).content("b\n");
        u.raw(format!("echo ROOT_TO_LOGIN >> {log}"))
            .when(b.changed());
        // Login source, root target: a constant.
        let c = u.file(&f3).content("c\n");
        h.raw(format!("echo LOGIN_TO_ROOT >> {log}"))
            .when(c.changed());
        // Login source, login target, a run apart: still the same shell.
        h.file(&f4).content("d\n");
        u.raw(format!("echo AFTER >> {log}")).when(c.changed());
        Ok(())
    })
}

#[test]
fn guards_read_as_user_ops_in_the_same_shell_and_across_shells() {
    let rig = Rig::new();
    let (r1, out1) = rig.run("guards", guard_pb);
    assert_eq!(r1.exit_code(), 0, "{}\n{out1}", r1.render());
    assert_eq!(
        rig.read("marks"),
        "SAME\nROOT_TO_LOGIN\nLOGIN_TO_ROOT\nAFTER\n",
        "{out1}"
    );
    let (r2, out2) = rig.run("guards", guard_pb);
    assert_eq!(r2.exit_code(), 0, "{}\n{out2}", r2.render());
    assert_eq!(
        rig.read("marks"),
        "SAME\nROOT_TO_LOGIN\nLOGIN_TO_ROOT\nAFTER\n",
        "nothing changed, nothing ran again: {out2}"
    );
}

fn run_var_pb(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let (a, b) = (p(&h, "run_login"), p(&h, "run_plain"));
        h.as_user("root").raw(format!("echo \"$__tp_run\" > {a}"));
        h.raw(format!("echo \"$__tp_run\" > {b}"));
        Ok(())
    })
}

#[test]
fn the_run_variable_is_set_in_the_login_shell() {
    let rig = Rig::new();
    let (r, out) = rig.run("runvar", run_var_pb);
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    let (a, b) = (rig.read("run_login"), rig.read("run_plain"));
    assert!(!a.trim().is_empty(), "{out}");
    assert_eq!(a, b, "one run, one staging name in every shell");
}

// ---- the unit flag across shells ---------------------------------------------

fn unit_root_svc_login(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        h.unit("a.service")
            .content("[Service]\nExecStart=/bin/true\n");
        h.as_user("root").svc("a").enabled();
        Ok(())
    })
}

fn unit_login_svc_root(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        h.as_user("root")
            .unit("a.service")
            .content("[Service]\nExecStart=/bin/true\n");
        h.svc("a").enabled();
        Ok(())
    })
}

fn unit_login_only(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        h.as_user("root")
            .unit("a.service")
            .content("[Service]\nExecStart=/bin/true\n");
        Ok(())
    })
}

fn unit_login_svc_login(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let u = h.as_user("root");
        u.unit("a.service")
            .content("[Service]\nExecStart=/bin/true\n");
        u.svc("a").enabled();
        Ok(())
    })
}

/// One `daemon-reload`, before the svc op's first call.
fn reload_once_before_svc(rig: &Rig, out: &str) {
    let log = rig.read("log");
    assert_eq!(rig.reloads(), 1, "{log}\n{out}");
    let reload = log
        .lines()
        .position(|c| c == "systemctl daemon-reload")
        .unwrap();
    let op = log
        .lines()
        .position(|c| c.starts_with("systemctl show -p UnitFileState,ActiveState"))
        .expect(&log);
    assert!(reload < op, "{log}");
}

#[test]
fn a_unit_in_the_root_shell_reloads_before_a_svc_op_in_the_login_shell() {
    let rig = Rig::new();
    let (r, out) = rig.run("u1", unit_root_svc_login);
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    reload_once_before_svc(&rig, &out);
    assert!(!out.contains("end of window"), "no flush needed: {out}");
}

#[test]
fn a_unit_in_the_login_shell_reloads_before_a_svc_op_in_the_root_shell() {
    let rig = Rig::new();
    let (r, out) = rig.run("u2", unit_login_svc_root);
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    reload_once_before_svc(&rig, &out);
    assert!(!out.contains("end of window"), "no flush needed: {out}");
}

#[test]
fn a_unit_in_the_login_shell_alone_is_reloaded_by_the_flush_of_the_root_shell() {
    let rig = Rig::new();
    let (r, out) = rig.run("u3", unit_login_only);
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert_eq!(rig.reloads(), 1, "{}\n{out}", rig.read("log"));
    assert!(
        out.contains("systemd daemon-reload: end of window"),
        "{out}"
    );
}

#[test]
fn a_unit_and_a_svc_op_in_the_login_shell_reload_once() {
    let rig = Rig::new();
    let (r, out) = rig.run("u4", unit_login_svc_login);
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    reload_once_before_svc(&rig, &out);
    assert!(!out.contains("end of window"), "no flush needed: {out}");
}

// ---- slice B: staleness (the profile edited mid-run, relogin()) ------------

/// The profile the playbooks below write: mark 2, the same exports.
fn profile2(h: &Host<PathBuf>) -> String {
    let d = h.vars.display();
    format!(
        "echo x >> {d}/counter\n\
         export TP_MARK=2\n\
         export PATH={d}/bin:$PATH\n\
         export TP_SVC_RUNDIR={d}/run-systemd-system\n\
         export TP_UNIT_DIR={d}/etc-systemd-system\n\
             export TP_FETCH_STAGE={d}/stage\n"
    )
}

fn mark_to(h: &Host<PathBuf>, f: &str) -> String {
    format!("echo $TP_MARK > {}", p(h, f))
}

/// The ids of the ops of host 0, in report order.
fn op_ids(r: &Report) -> Vec<usize> {
    r.hosts[0].ops.iter().map(|o| o.id).collect()
}

fn rule4_pb(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let u = h.as_user("root");
        u.raw(mark_to(&h, "o1"));
        u.file("~/.bash_profile").content(profile2(&h));
        u.raw(mark_to(&h, "o2"));
        Ok(())
    })
}

#[test]
fn a_changed_profile_write_reopens_the_login_shell_before_the_next_op() {
    let rig = Rig::new();
    let (r, out) = rig.run("rule4", rule4_pb);
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert_eq!(rig.read("o1"), "1\n", "{out}");
    assert_eq!(rig.read("o2"), "2\n", "{out}");
    assert_eq!(rig.lines("counter"), 2, "the profile ran once more: {out}");
    let file_id = op_ids(&r)[1];
    assert!(
        out.contains(&format!(
            "\n   login shell root reopened: profile write after #{file_id}\n"
        )),
        "{out}"
    );
}

fn no_trigger_pb(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let u = h.as_user("root");
        u.raw(format!("echo $$ > {}", p(&h, "a")));
        // Changed, and nothing to do with a profile.
        h.file(&p(&h, "unrelated")).content("x\n");
        u.file(&p(&h, "unrelated2")).content("x\n");
        u.raw(format!("echo $$ > {}", p(&h, "b")));
        Ok(())
    })
}

#[test]
fn an_unrelated_changed_op_keeps_the_same_login_shell() {
    let rig = Rig::new();
    let (r, out) = rig.run("notrigger", no_trigger_pb);
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    let (a, b) = (rig.read("a"), rig.read("b"));
    assert!(!a.trim().is_empty() && a == b, "{a:?} {b:?}\n{out}");
    assert_eq!(rig.lines("counter"), 1, "{out}");
    assert!(!out.contains("reopened"), "{out}");
}

fn unchanged_pb(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let u = h.as_user("root");
        u.raw(format!("echo $$ > {}", p(&h, "a")));
        // The profile as it is: nothing changes.
        let same = std::fs::read_to_string(h.vars.join("home/.bash_profile")).unwrap();
        u.file("~/.bash_profile").content(same);
        u.raw(format!("echo $$ > {}", p(&h, "b")));
        Ok(())
    })
}

#[test]
fn an_unchanged_profile_write_stales_nothing() {
    let rig = Rig::new();
    let (r, out) = rig.run("unchanged", unchanged_pb);
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert_eq!(r.hosts[0].ops[1].outcome, Outcome::Unchanged, "{out}");
    let (a, b) = (rig.read("a"), rig.read("b"));
    assert!(!a.trim().is_empty() && a == b, "{a:?} {b:?}\n{out}");
    assert_eq!(rig.lines("counter"), 1, "{out}");
    assert!(!out.contains("reopened"), "{out}");
}

fn relogin_pb(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let u = h.as_user("root");
        u.raw(format!("echo $$ > {}", p(&h, "a")));
        u.relogin();
        u.raw(format!("echo $$ > {}", p(&h, "b")));
        Ok(())
    })
}

#[test]
fn relogin_reopens_the_login_shell() {
    let rig = Rig::new();
    let (r, out) = rig.run("relogin", relogin_pb);
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    let (a, b) = (rig.read("a"), rig.read("b"));
    assert!(
        !a.trim().is_empty() && !b.trim().is_empty() && a != b,
        "{a:?} {b:?}\n{out}"
    );
    assert_eq!(rig.lines("counter"), 2, "{out}");
    let id = op_ids(&r)[1];
    assert!(
        out.contains(&format!(
            "\n   login shell root reopened: relogin after #{id}\n"
        )),
        "{out}"
    );
    // The relogin op is an op like another, with no change of its own.
    assert_eq!(r.hosts[0].ops[1].outcome, Outcome::Unchanged, "{out}");
    assert!(out.contains("relogin root"), "{out}");
}

fn relogin_last_pb(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let u = h.as_user("root");
        u.raw(format!("echo $$ > {}", p(&h, "a")));
        u.relogin();
        Ok(())
    })
}

#[test]
fn a_login_shell_never_used_again_is_never_reopened() {
    let rig = Rig::new();
    let (r, out) = rig.run("relogin_last", relogin_last_pb);
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert_eq!(rig.lines("counter"), 1, "{out}");
    assert!(!out.contains("reopened"), "{out}");
}

fn pipelined_pb(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let u = h.as_user("root");
        // One window, one shell: the profile write ends the run, so the
        // three ops queued behind it all run in the new environment.
        u.file("~/.bash_profile").content(profile2(&h));
        u.raw(mark_to(&h, "o1"));
        u.raw(mark_to(&h, "o2"));
        u.raw(mark_to(&h, "o3"));
        Ok(())
    })
}

#[test]
fn an_op_that_stales_its_own_shell_ends_its_pipelined_run() {
    let rig = Rig::new();
    let (r, out) = rig.run("pipelined", pipelined_pb);
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    for f in ["o1", "o2", "o3"] {
        assert_eq!(rig.read(f), "2\n", "{f}: {out}");
    }
    assert_eq!(rig.lines("counter"), 2, "reopened once: {out}");
}

fn other_shell_pb(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let u = h.as_user("root");
        u.raw(mark_to(&h, "o1"));
        // The plain root shell writes root's profile by its absolute path
        // (root's home as `getent` said it): rule 4 stales root's login
        // shell all the same.
        h.file(&p(&h, "home/.bash_profile")).content(profile2(&h));
        u.raw(mark_to(&h, "o2"));
        Ok(())
    })
}

#[test]
fn a_profile_written_by_another_shell_by_absolute_path_stales_the_login_shell() {
    let rig = Rig::new();
    let (r, out) = rig.run("othershell", other_shell_pb);
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert_eq!(rig.read("o1"), "1\n", "{out}");
    assert_eq!(rig.read("o2"), "2\n", "{out}");
    assert!(out.contains("reopened: profile write"), "{out}");
}

fn timeout_after_reopen_pb(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let u = h.as_user("root");
        u.raw(mark_to(&h, "o1"));
        u.relogin();
        u.raw("sleep 30")
            .timeout(Duration::from_millis(500))
            .allow_fail();
        u.raw(format!("echo \"$__tp_run\" > {}", p(&h, "run")));
        u.raw(mark_to(&h, "o2"));
        Ok(())
    })
}

#[test]
fn init_and_the_watchdog_work_in_a_reopened_shell() {
    let rig = Rig::new();
    let t = std::time::Instant::now();
    let (r, out) = rig.run("reopen_timeout", timeout_after_reopen_pb);
    assert!(t.elapsed() < Duration::from_secs(25), "{:?}", t.elapsed());
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert_eq!(r.hosts[0].ops[2].outcome, Outcome::AllowedFail, "{out}");
    assert_eq!(rig.read("o2"), "1\n", "{out}");
    // `init_shell` ran in the reopened shell: `__tp_run` is set there.
    assert!(!rig.read("run").trim().is_empty(), "{out}");
}

// ---- the sandbox in the login shell (T128) -----------------------------------

/// What the login shell sees of the test switch and the path seams: `env`
/// for the names, then each value the root shell carries.
fn sandbox_env_pb(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let o = p(&h, "sbx");
        h.as_user("root").raw(format!(
            "{{ env | sed -n 's/^\\(TP_[A-Z_]*\\)=.*/\\1/p' | sort | tr '\\n' ' '; echo; \
             for v in TP_SANDBOX_REQUIRED TP_SANDBOX TP_FETCH_STAGE TP_FETCH_CACHE TP_RUN_DIR \
             TP_FS_ROOTS TP_UNIT_DIR TP_SVC_RUNDIR; do echo \"$v=${{!v-unset}}\"; done; \
             echo \"stage=$TP_FETCH_STAGE\"; }} > {o}"
        ));
        Ok(())
    })
}

#[test]
fn the_login_shell_keeps_the_sandbox_and_the_path_seams() {
    let rig = Rig::new();
    // A profile that exports none of the seams: what the login shell has
    // comes from the root shell's `env -i` line alone.
    let d = rig.dir();
    std::fs::write(
        d.join("home/.bash_profile"),
        format!("export PATH={}/bin:$PATH\n", d.display()),
    )
    .unwrap();
    let (r, out) = rig.run("sbxenv", sandbox_env_pb);
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    let got = rig.read("sbx");
    let s = |n: &str| d.join(n).display().to_string();
    // `Rig::run` sets the sandbox at the rig's dir, with its own stage, unit
    // and svc dirs; nothing in the root shell's environment else leaks.
    for want in [
        "TP_SANDBOX_REQUIRED=1".to_string(),
        format!("TP_SANDBOX={}", d.display()),
        format!("TP_FETCH_STAGE={}", s("stage")),
        format!("TP_FETCH_CACHE={}", s("cache")),
        format!("TP_RUN_DIR={}", s("run")),
        format!("TP_FS_ROOTS={}", s("fs")),
        format!("TP_UNIT_DIR={}", s("etc-systemd-system")),
        format!("TP_SVC_RUNDIR={}", s("run-systemd-system")),
    ] {
        assert!(
            got.lines().any(|l| l == want),
            "missing {want}:\n{got}\n{out}"
        );
    }
    assert!(
        !got.contains("TP_ROOTVAR"),
        "a non-seam variable leaked:\n{got}"
    );
}

/// A capturing op in the login shell over a file that exists, so its undo
/// data is saved; the next op lists where that went.
fn sandbox_capture_pb(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let u = h.as_user("root");
        u.file(&p(&h, "target")).content("new\n");
        u.raw(format!(
            "find \"$TP_FETCH_STAGE/$__tp_run\" -type f > {}",
            p(&h, "listing")
        ));
        Ok(())
    })
}

#[test]
fn a_capturing_op_in_the_login_shell_saves_its_undo_data_in_the_sandbox() {
    let rig = Rig::new();
    let d = rig.dir();
    std::fs::write(
        d.join("home/.bash_profile"),
        format!("export PATH={}/bin:$PATH\n", d.display()),
    )
    .unwrap();
    std::fs::write(d.join("target"), "old\n").unwrap();
    let (r, out) = rig.run("sbxcap", sandbox_capture_pb);
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert_eq!(rig.read("target"), "new\n");
    let listing = rig.read("listing");
    let stage = format!("{}/", d.join("stage").display());
    assert!(
        listing.lines().any(|l| l.ends_with("/old")),
        "no undo data saved:\n{listing}\n{out}"
    );
    assert!(
        listing.lines().all(|l| l.starts_with(&stage)),
        "undo data outside the sandbox stage:\n{listing}"
    );
}
