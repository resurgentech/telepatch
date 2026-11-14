//! `h.as_user(u)` on the local transport (T120).
//!
//! No second user, no privilege and no real `sudo` here. Two paths:
//!
//! - `u` is the calling user: no sudo, `exec env -i HOME=.. USER=.. LOGNAME=..
//!   SHELL=.. PATH=.. bash -l` typed into a plain shell. The tests use the
//!   `.uid(0)` / `.user(<me>)` seams (a root shell's guards for a user who
//!   is in fact not root) and a stub `sudo` that fails loudly if it is
//!   called. Every "what already works" bullet runs through this shell.
//! - another user: `<sudo> -n -iu <u> bash` through the `.sudo(cmd)` seam,
//!   whose stub records its arguments, refuses, or says nothing.
//!
//! What needs a real second user (the VM case `local_user` of tp_basic's
//! `scripts/test-vm.sh`, run in T104) is not faked here.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::report::{HostStatus, Outcome, Report};
use telepatch::trace::Principal;
use telepatch::transport::local::LocalTransport;
use telepatch::transport::{Limit, Transport};
use telepatch::{Host, Playbook, PlaybookFuture};

fn write_exe(p: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(p, format!("#!/bin/bash\n{body}\n")).unwrap();
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The calling user, as `id -un` says.
fn me() -> String {
    let o = std::process::Command::new("id")
        .arg("-un")
        .output()
        .unwrap();
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

/// A temp tree that is also the test's sandbox: the fake HOME with its
/// profile, a PATH directory with a `getent` that knows the calling user
/// and a `sudo` that fails loudly, the unit directory and the booted marker
/// directory.
struct Rig {
    d: tempfile::TempDir,
    me: String,
}

impl Rig {
    fn new() -> Rig {
        let rig = Rig {
            d: tempfile::tempdir().unwrap(),
            me: me(),
        };
        let d = rig.d.path();
        let (home, bin) = (d.join("home"), d.join("bin"));
        for n in ["home", "bin", "etc-systemd-system", "run-systemd-system"] {
            std::fs::create_dir(d.join(n)).unwrap();
        }
        write_exe(
            &bin.join("getent"),
            &format!(
                "case \"$1 $2\" in\n\
                   \"passwd {me}\") echo '{me}:x:1000:1000::{}:/bin/bash';;\n\
                   \"passwd bob\") echo 'bob:x:1001:1001::/home/bob:/bin/bash';;\n\
                   *) exit 2;;\n\
                 esac",
                home.display(),
                me = rig.me
            ),
        );
        write_exe(&bin.join("systemd-analyze"), "exit 0");
        write_exe(&bin.join("systemctl"), "exit 0");
        write_exe(
            &bin.join("sudo-never"),
            &format!("echo \"$*\" >> {}/sudo_called\nexit 99", d.display()),
        );
        std::fs::write(home.join(".bash_profile"), rig.profile(1)).unwrap();
        rig
    }

    /// A PATH that starts with the stubs.
    fn path(&self) -> String {
        format!(
            "{}:{}",
            self.d.path().join("bin").display(),
            std::env::var("PATH").unwrap()
        )
    }

    fn dir(&self) -> PathBuf {
        self.d.path().to_path_buf()
    }

    /// The profile in the fake HOME, counting its reads and exporting
    /// `TP_MARK=<mark>`.
    fn profile(&self, mark: u32) -> String {
        let d = self.d.path().display();
        format!(
            "echo x >> {d}/counter\n\
             export TP_MARK={mark}\n\
             export PATH={d}/bin:$PATH\n"
        )
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.d.path().join(name)).unwrap_or_default()
    }

    fn lines(&self, name: &str) -> usize {
        self.read(name).lines().count()
    }

    fn no_sudo_called(&self) {
        assert_eq!(
            self.read("sudo_called"),
            "",
            "the calling user's login shell must not call sudo"
        );
    }

    /// The calling-user transport: a root shell's guards, the user as `me`,
    /// a `sudo` that fails when it is called.
    fn transport(&self) -> LocalTransport {
        let d = self.d.path();
        let path = format!(
            "{}:{}",
            d.join("bin").display(),
            std::env::var("PATH").unwrap()
        );
        LocalTransport::new()
            .uid(0)
            .user(&self.me)
            .sandboxed_at(d)
            .sudo(&d.join("bin/sudo-never").display().to_string())
            .env("PATH", &path)
            .env("TP_ROOTVAR", "x")
            .env("TP_RUN_DIR", &d.join("run").display().to_string())
            .env(
                "TP_SVC_RUNDIR",
                &d.join("run-systemd-system").display().to_string(),
            )
            .env(
                "TP_UNIT_DIR",
                &d.join("etc-systemd-system").display().to_string(),
            )
    }

    fn run(&self, name: &'static str, f: fn(Host<PathBuf>) -> PlaybookFuture) -> (Report, String) {
        self.run_on(name, f, self.transport())
    }

    fn run_on(
        &self,
        name: &'static str,
        f: fn(Host<PathBuf>) -> PlaybookFuture,
        t: LocalTransport,
    ) -> (Report, String) {
        let pb = Playbook::new(name, f);
        let out = Rc::new(RefCell::new(String::new()));
        let r = run(
            &pb,
            vec![HostSpec {
                groups: Vec::new(),
                name: "h1".into(),
                target: Target::Local(t),
                vars: self.dir(),
            }],
            RunOpts {
                log: None,
                stdout: Stdout::Capture(out.clone()),
            },
        );
        let text = out.borrow().clone();
        (r, text)
    }
}

fn p(h: &Host<PathBuf>, n: &str) -> String {
    h.vars.join(n).display().to_string()
}

fn op_ids(r: &Report) -> Vec<usize> {
    r.hosts[0].ops.iter().map(|o| o.id).collect()
}

// ---- the calling user: env -i, no sudo -----------------------------------------

fn env_pb(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let me = me();
        let (o1, o2, plain) = (p(&h, "o1"), p(&h, "o2"), p(&h, "plain"));
        let show = "echo \"mark=$TP_MARK home=$HOME user=$USER logname=$LOGNAME id=$(id -un) leak=${TP_ROOTVAR-unset}\"";
        h.as_user(&me).raw(format!("{show} > {o1}"));
        h.as_user(&me).raw(format!("{show} > {o2}"));
        h.raw(format!("{show} > {plain}"));
        Ok(())
    })
}

#[test]
fn the_calling_user_gets_a_clean_login_shell_without_sudo() {
    let rig = Rig::new();
    let (r, out) = rig.run("env", env_pb);
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    let want = format!(
        "mark=1 home={d}/home user={me} logname={me} id={me} leak=unset\n",
        d = rig.d.path().display(),
        me = rig.me
    );
    assert_eq!(rig.read("o1"), want, "{out}");
    assert_eq!(rig.read("o2"), want, "{out}");
    // One profile read for two ops; the plain op still sees the root shell's
    // variable.
    assert_eq!(rig.lines("counter"), 1, "{out}");
    assert!(rig.read("plain").contains("leak=x"));
    rig.no_sudo_called();
}

// ---- T114: reopen rules and relogin ---------------------------------------------

fn reopen_pb(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let u = h.as_user(&me());
        let d = h.vars.display();
        u.raw(format!("echo $TP_MARK > {}", p(&h, "m1")));
        u.file("~/.bash_profile").content(format!(
            "echo x >> {d}/counter\nexport TP_MARK=2\nexport PATH={d}/bin:$PATH\n"
        ));
        u.raw(format!("echo $TP_MARK > {}", p(&h, "m2")));
        u.relogin();
        u.raw(format!("echo $TP_MARK > {}", p(&h, "m3")));
        Ok(())
    })
}

#[test]
fn a_profile_write_and_relogin_reopen_the_users_login_shell() {
    let rig = Rig::new();
    let (r, out) = rig.run("reopen", reopen_pb);
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert_eq!(rig.read("m1"), "1\n", "{out}");
    assert_eq!(rig.read("m2"), "2\n", "{out}");
    assert_eq!(rig.read("m3"), "2\n", "{out}");
    // The initial read, the profile-write reopen and the relogin reopen.
    assert_eq!(rig.lines("counter"), 3, "{out}");
    let ids = op_ids(&r);
    for want in [
        format!(
            "\n   login shell {} reopened: profile write after #{}\n",
            rig.me, ids[1]
        ),
        format!(
            "\n   login shell {} reopened: relogin after #{}\n",
            rig.me, ids[3]
        ),
    ] {
        assert!(out.contains(&want), "missing {want:?} in\n{out}");
    }
    rig.no_sudo_called();
}

// ---- T101: the watchdog, its killer a second local root shell ---------------------

fn timeout_pb(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let u = h.as_user(&me());
        let (a, b) = (p(&h, "pid_a"), p(&h, "pid_b"));
        u.raw(format!("echo $$ > {a}\nsleep 30"))
            .timeout(Duration::from_millis(500))
            .allow_fail();
        u.raw(format!("echo $$ > {b}"));
        Ok(())
    })
}

#[test]
fn a_timeout_in_the_users_shell_is_killed_and_the_shell_stays_usable() {
    let rig = Rig::new();
    let t = std::time::Instant::now();
    let (r, out) = rig.run("timeout", timeout_pb);
    // The timeout is reported once the group is gone (T125), which may be
    // after the grace: allow for it, but far less than the 30 s sleep.
    assert!(t.elapsed() < Duration::from_secs(25), "{:?}", t.elapsed());
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    let ops = &r.hosts[0].ops;
    assert_eq!(ops[0].outcome, Outcome::AllowedFail, "{out}");
    assert_eq!(ops[1].outcome, Outcome::Unchanged, "{out}");
    let (a, b) = (rig.read("pid_a"), rig.read("pid_b"));
    assert!(!a.trim().is_empty() && a == b, "one shell: {a:?} {b:?}");
    rig.no_sudo_called();
}

// ---- T106: the preparation op and the undo data ----------------------------------

fn undo_pb(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let u = h.as_user(&me());
        u.file(&p(&h, "target")).content("new\n");
        u.raw(format!(
            "find \"$TP_FETCH_STAGE/$__tp_run\" -type f > {}\n\
             ls -d \"$TP_FETCH_STAGE/$__tp_run/undo/$USER\" >> {}",
            p(&h, "listing"),
            p(&h, "listing")
        ));
        Ok(())
    })
}

#[test]
fn the_preparation_op_and_the_capture_work_for_the_users_shell_inside_the_sandbox() {
    let rig = Rig::new();
    std::fs::write(rig.d.path().join("target"), "old\n").unwrap();
    let (r, out) = rig.run("undo", undo_pb);
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert_eq!(rig.read("target"), "new\n");
    let listing = rig.read("listing");
    let stage = format!("{}/", rig.d.path().join("stage").display());
    assert!(
        listing.lines().any(|l| l.ends_with("/old")),
        "no undo data saved:\n{listing}\n{out}"
    );
    assert!(
        listing.lines().all(|l| l.starts_with(&stage)),
        "undo data outside the sandbox stage:\n{listing}"
    );
    // The preparation op made the user's own directory.
    assert!(
        listing
            .lines()
            .any(|l| l.ends_with(&format!("/undo/{}", rig.me))),
        "no undo/<user> directory:\n{listing}"
    );
    assert!(
        std::fs::read_dir(rig.d.path().join("stage")).is_ok_and(|mut d| d.next().is_none()),
        "the run's stage is removed at the end"
    );
    rig.no_sudo_called();
}

// ---- T119: staging for an op of the user's shell ---------------------------------

fn sha256(bytes: &[u8]) -> String {
    use std::io::Write;
    let mut c = std::process::Command::new("sha256sum")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(bytes).unwrap();
    let o = c.wait_with_output().unwrap();
    String::from_utf8(o.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

const BODY: &[u8] = b"hello from the www\n";

fn fetch_pb(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let u = h.as_user(&me());
        let f = h
            .fetch(format!("file://{}", p(&h, "www/a.txt")))
            .sha256(sha256(BODY));
        u.file(&p(&h, "out/a.txt")).from(&f);
        Ok(())
    })
}

#[test]
fn a_download_for_the_users_op_is_staged_placed_and_cleaned_up() {
    if std::process::Command::new("curl")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: curl is not on this machine");
        return;
    }
    let rig = Rig::new();
    let body = BODY;
    std::fs::create_dir_all(rig.d.path().join("www")).unwrap();
    std::fs::create_dir_all(rig.d.path().join("out")).unwrap();
    std::fs::write(rig.d.path().join("www/a.txt"), body).unwrap();

    let (r, out) = rig.run("fetch", fetch_pb);
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert_eq!(rig.read("out/a.txt").as_bytes(), body, "{out}");
    assert!(
        std::fs::read_dir(rig.d.path().join("stage")).is_ok_and(|mut d| d.next().is_none()),
        "the staging directory is cleaned"
    );
    rig.no_sudo_called();
}

// ---- T102/T118: the local barrier stop -------------------------------------------

fn barrier_pb(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let u = h.as_user(&me());
        u.file(&p(&h, "before")).content("1\n").needs_reboot();
        h.reboot().if_pending();
        u.raw(format!("touch {}", p(&h, "after")));
        Ok(())
    })
}

#[test]
fn a_user_op_marking_a_reboot_stops_at_the_barrier_with_the_local_message() {
    let rig = Rig::new();
    let (r, out) = rig.run("barrier", barrier_pb);
    assert_eq!(r.exit_code(), 1, "{}\n{out}", r.render());
    assert!(
        matches!(r.hosts[0].status, HostStatus::RebootPending(_)),
        "{:?}\n{out}",
        r.hosts[0].status
    );
    assert!(
        out.contains("   why:       the --local transport cannot reboot this machine\n"),
        "{out}"
    );
    assert!(
        out.contains("   to finish: reboot this machine yourself, then run again\n"),
        "{out}"
    );
    assert_eq!(rig.read("before"), "1\n", "applied before the barrier");
    assert!(
        !rig.d.path().join("after").exists(),
        "ran after the barrier"
    );
    rig.no_sudo_called();
}

// ---- T115: check runs --------------------------------------------------------------

fn check_pb(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let u = h.as_user(&me());
        let a = u.check("echo \"mark=$TP_MARK user=$USER\"");
        let b = u.check("exit 3");
        let (a, b) = (a.await, b.await);
        h.raw(format!(
            "printf '%s|%s|%s\\n' {} {} {} > {}",
            telepatch::sh::quote(&a.stdout.trim().replace(' ', "_")),
            a.rc,
            b.rc,
            p(&h, "checks")
        ));
        Ok(())
    })
}

#[test]
fn checks_of_the_users_shell_run_in_it() {
    let rig = Rig::new();
    let (r, out) = rig.run("checks", check_pb);
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert_eq!(
        rig.read("checks"),
        format!("mark=1_user={}|0|3\n", rig.me),
        "{out}"
    );
    rig.no_sudo_called();
}

// ---- no passwd entry -----------------------------------------------------------------

fn one_op(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let u = h.as_user(&me());
        u.raw(format!("echo hi > {}", p(&h, "o")));
        Ok(())
    })
}

#[test]
fn a_missing_passwd_entry_blocks_the_host() {
    let rig = Rig::new();
    write_exe(&rig.d.path().join("bin/getent"), "exit 2");
    let (r, out) = rig.run("nopw", one_op);
    let HostStatus::Blocked(why) = &r.hosts[0].status else {
        panic!("{:?}\n{out}", r.hosts[0].status)
    };
    // Prepare finds the account missing before the shell would open.
    assert!(why.contains("does not exist"), "{why}");
    assert!(why.contains(&rig.me), "{why}");
    assert_eq!(rig.read("o"), "", "the op ran");
}

// ---- the sandbox switch applies to the User path --------------------------------------

#[tokio::test]
async fn the_controller_side_refusal_applies_to_a_user_shell() {
    assert!(
        std::env::var("TP_SANDBOX_REQUIRED").is_ok_and(|v| v == "1"),
        "the switch is set by .cargo/config.toml"
    );
    let t = LocalTransport::new().uid(0).user("alice");
    let s = t.open("local", &Principal::User("alice".into()), 1).await;
    assert!(s.got.is_empty());
    match s.limit {
        Some(Limit::Other(m)) => assert!(m.contains("refusing the local shell"), "{m}"),
        l => panic!("{l:?}"),
    }
}

// ---- another user: sudo -n -iu <u> bash, through the .sudo(cmd) seam -----------------

/// The stub the caller `alice` reaches for `bob`: records `-n -iu bob bash`
/// and execs the `bash` it was given.
#[tokio::test]
async fn another_user_is_sudo_n_iu_u_bash() {
    let rig = Rig::new();
    let d = rig.d.path();
    write_exe(
        &d.join("bin/sudo-ok"),
        &format!(
            "echo \"$*\" > {}/sudo_args\n[ \"$1 $2 $3 $4\" = \"-n -iu bob bash\" ] || exit 2\n\
             shift 3\nexec \"$@\"",
            d.display()
        ),
    );
    let t = LocalTransport::new()
        .uid(1000)
        .user("alice")
        .sandboxed_at(d)
        .env("PATH", &rig.path())
        .sudo(&d.join("bin/sudo-ok").display().to_string());
    let s = t.open("local", &Principal::User("bob".into()), 1).await;
    assert!(s.limit.is_none(), "{:?}", s.limit);
    assert_eq!(s.got.len(), 1);
    assert_eq!(s.got[0].user(), "bob");
    assert_eq!(rig.read("sudo_args"), "-n -iu bob bash\n");
}

#[tokio::test]
async fn the_root_caller_reaches_another_user_through_sudo_too() {
    let rig = Rig::new();
    let d = rig.d.path();
    write_exe(
        &d.join("bin/sudo-ok"),
        &format!(
            "echo \"$*\" > {}/sudo_args\nshift 3\nexec \"$@\"",
            d.display()
        ),
    );
    let t = LocalTransport::new()
        .uid(0)
        .user("root")
        .sandboxed_at(d)
        .env("PATH", &rig.path())
        .sudo(&d.join("bin/sudo-ok").display().to_string());
    let s = t.open("local", &Principal::User("bob".into()), 1).await;
    assert!(s.limit.is_none(), "{:?}", s.limit);
    assert_eq!(rig.read("sudo_args"), "-n -iu bob bash\n");
}

fn bob_op(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        h.as_user("bob").raw(format!("echo hi > {}", p(&h, "o")));
        Ok(())
    })
}

fn blocked(rig: &Rig, sudo_body: &str) -> String {
    let d = rig.d.path();
    write_exe(&d.join("bin/sudo-stub"), sudo_body);
    let t = LocalTransport::new()
        .uid(0)
        .user("alice")
        .sandboxed_at(d)
        .env("PATH", &rig.path())
        .sudo(&d.join("bin/sudo-stub").display().to_string());
    let (r, out) = rig.run_on("bob", bob_op, t);
    let HostStatus::Blocked(why) = &r.hosts[0].status else {
        panic!("{:?}\n{out}", r.hosts[0].status)
    };
    assert_eq!(rig.read("o"), "", "the op ran");
    why.clone()
}

#[test]
fn a_refusing_sudo_blocks_the_host_with_the_text() {
    let rig = Rig::new();
    let why = blocked(&rig, "echo 'sudo: a password is required' >&2; exit 1");
    assert_eq!(
        why,
        "sudo -n -iu bob bash refused on this machine (no sudo rights for bob, or a login \
         shell that refuses, such as nologin): sudo: a password is required",
        "{why}"
    );
}

#[test]
fn a_silent_sudo_gives_the_nologin_hint() {
    let rig = Rig::new();
    // nologin prints on stdout, which the hello wait does not keep.
    let why = blocked(
        &rig,
        "echo 'This account is currently not available.'; exit 1",
    );
    assert!(
        why.starts_with(
            "sudo -n -iu bob bash refused on this machine (no sudo rights for bob, or a login \
             shell that refuses, such as nologin)"
        ),
        "{why}"
    );
    assert!(why.contains("nothing on stderr"), "{why}");
}

#[test]
fn a_machine_with_no_sudo_gets_a_clear_refusal() {
    let rig = Rig::new();
    let d = rig.d.path();
    let t = LocalTransport::new()
        .uid(0)
        .user("alice")
        .sandboxed_at(d)
        .env("PATH", &rig.path())
        .sudo(&d.join("bin/no-such-sudo").display().to_string());
    let (r, out) = rig.run_on("bob", bob_op, t);
    let HostStatus::Blocked(why) = &r.hosts[0].status else {
        panic!("{:?}\n{out}", r.hosts[0].status)
    };
    assert!(why.contains("sudo -n -iu bob bash refused"), "{why}");
    assert!(why.contains("sudo"), "{why}");
    assert!(why.contains("no-such-sudo"), "{why}");
}

#[tokio::test]
async fn a_refusal_is_the_sudo_refused_limit() {
    let rig = Rig::new();
    let d = rig.d.path();
    write_exe(&d.join("bin/sudo-stub"), "exit 1");
    let t = LocalTransport::new()
        .uid(1000)
        .user("alice")
        .sandboxed_at(d)
        .env("PATH", &rig.path())
        .sudo(&d.join("bin/sudo-stub").display().to_string());
    let s = t.open("local", &Principal::User("bob".into()), 1).await;
    assert!(s.got.is_empty());
    assert_eq!(s.limit, Some(Limit::SudoRefused));
}

#[tokio::test]
async fn the_transport_refuses_a_user_with_no_passwd_entry() {
    let rig = Rig::new();
    write_exe(&rig.d.path().join("bin/getent"), "exit 2");
    let s = rig
        .transport()
        .open("local", &Principal::User(rig.me.clone()), 1)
        .await;
    assert!(s.got.is_empty());
    match s.limit {
        Some(Limit::Other(m)) => {
            assert!(m.contains("no passwd entry"), "{m}");
            assert!(m.contains(&rig.me), "{m}");
        }
        l => panic!("{l:?}"),
    }
}
