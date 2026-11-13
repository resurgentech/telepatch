//! The executor over the SSH transport, against a stub `ssh` (no network,
//! no real ssh or sudo): each op and probe goes to its principal's shell,
//! a change of principal waits for the previous shell, and a refused user
//! shell blocks the host before the window's root ops are sent.
//!
//! The stub is the one in `tests/ssh_transport.rs`, with one addition:
//! every channel copies what it receives on stdin to `chan-<pid>.in` (a
//! background `tee` into a fifo the channel's `bash` reads, so the channel
//! process is still the exec'd `bash`), whose first line is `# remote:
//! <command>` (`bash` for the root channel, `sudo -n -iu '<u>' bash` for a
//! user's). That is how the test tells which channel ran an op.
//!
//! `as_user(u)` ops render a login shell that looks `u` up with `getent`,
//! so `u` is the current OS user (the stub has no other users).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::report::{HostStatus, Outcome, Report};
use telepatch::transport::ssh::SshTransport;
use telepatch::{Host, Playbook, PlaybookFuture, sh};

/// Write an executable via a child `sh` so no write fd lives in this test
/// process (a sibling thread's fork could inherit it; exec then fails with
/// ETXTBSY).
fn write_exe(p: &Path, body: &str) {
    use std::io::Write;
    let mut c = std::process::Command::new("sh")
        .arg("-c")
        .arg("cat > \"$1\" && chmod 755 \"$1\"")
        .arg("sh")
        .arg(p)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin
        .take()
        .unwrap()
        .write_all(format!("#!/bin/sh\n{body}\n").as_bytes())
        .unwrap();
    assert!(c.wait().unwrap().success());
}

const SSH: &str = r#"
d="$TP_STUB_DIR"
master=; cp=; ctl=
while [ $# -gt 0 ]; do
  case "$1" in
    -T) shift ;;
    -N) master=1; shift ;;
    -i) shift 2 ;;
    -O) ctl="$2"; shift 2 ;;
    -o) case "$2" in ControlPath=*) [ -n "$cp" ] || cp="${2#ControlPath=}" ;; esac; shift 2 ;;
    -*) echo "stub ssh: unknown flag $1" >&2; exit 2 ;;
    *) break ;;
  esac
done
shift
if [ -n "$ctl" ]; then
  [ "$ctl" = exit ] || exit 2
  [ -e "$cp.pid" ] && kill $(cat "$cp.pid") 2>/dev/null
  rm -f "$cp" "$cp.pid"
  exit 0
fi
echo $$ >> "$d/pids"
if [ -n "$master" ]; then
  echo $$ > "$cp.pid"
  : > "$cp"
  exec sleep 100000
fi
[ -e "$cp" ] || { echo "stub ssh: no master at $cp" >&2; exit 255; }
if [ -n "$TP_STUB_MAX_CHAN" ]; then
  echo $$ >> "$d/nchan"
  [ "$(wc -l < "$d/nchan")" -le "$TP_STUB_MAX_CHAN" ] || { echo "stub ssh: session request failed" >&2; exit 255; }
fi
log="$d/chan-$$.in"
printf '# remote: %s\n' "$*" > "$log"
mkfifo "$log.fifo"
tee -a "$log" <&0 >"$log.fifo" 2>/dev/null &
exec bash -c "exec $*" <"$log.fifo"
"#;

const ID: &str = r#"
if [ "$1" = -u ]; then echo "${TP_STUB_UID:-1000}"; exit 0; fi
exec /usr/bin/id "$@"
"#;

const SUDO: &str = r#"
printf '%s\n' "$*" >> "$TP_STUB_DIR/sudo.log"
[ "$1" = -n ] || exit 2
shift
if [ "$1" = -H ]; then shift; HOME=/root; export HOME; fi
if [ "$1" = -iu ]; then
  [ -z "$TP_STUB_NO_USER" ] || { echo "sudo: unknown user $2" >&2; exit 1; }
  shift 2
  exec "$@"
fi
TP_STUB_UID=0 exec "$@"
"#;

struct Stub {
    dir: tempfile::TempDir,
    ssh: String,
    path: String,
}

impl Stub {
    fn new() -> Stub {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let ssh = dir.path().join("ssh");
        write_exe(&ssh, SSH);
        write_exe(&bin.join("id"), ID);
        write_exe(&bin.join("sudo"), SUDO);
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
        Stub {
            dir,
            ssh: ssh.display().to_string(),
            path,
        }
    }

    fn transport(&self) -> SshTransport {
        SshTransport::new()
            .ssh(&self.ssh)
            .env("TP_STUB_DIR", &self.dir.path().display().to_string())
            .env("PATH", &self.path)
    }

    fn work(&self) -> PathBuf {
        let w = self.dir.path().join("work");
        std::fs::create_dir_all(&w).unwrap();
        w
    }

    /// (remote command, everything the channel received) per channel.
    fn channels(&self) -> Vec<(String, String)> {
        let mut out = vec![];
        for e in std::fs::read_dir(self.dir.path()).unwrap().flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if !(n.starts_with("chan-") && n.ends_with(".in")) {
                continue;
            }
            let text = std::fs::read_to_string(e.path()).unwrap();
            let (head, rest) = text.split_once('\n').unwrap_or((&text, ""));
            let remote = head.strip_prefix("# remote: ").unwrap_or(head).to_string();
            out.push((remote, rest.to_string()));
        }
        out
    }

    /// The remote commands of the channels whose input contains `mark`.
    fn ran(&self, mark: &str) -> Vec<String> {
        self.channels()
            .into_iter()
            .filter(|(_, input)| input.contains(mark))
            .map(|(r, _)| r)
            .collect()
    }

    fn pids(&self) -> Vec<u32> {
        std::fs::read_to_string(self.dir.path().join("pids"))
            .unwrap_or_default()
            .lines()
            .map(|l| l.trim().parse().unwrap())
            .collect()
    }
}

fn alive(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => false,
        Ok(s) => {
            let state = s.rsplit_once(')').map(|(_, r)| r.trim_start());
            !matches!(state.and_then(|r| r.chars().next()), Some('Z') | Some('X'))
        }
    }
}

fn survivors(pids: &[u32]) -> Vec<u32> {
    for _ in 0..100 {
        if pids.iter().all(|&p| !alive(p)) {
            return vec![];
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    pids.iter().copied().filter(|&p| alive(p)).collect()
}

fn me() -> String {
    std::env::var("USER").unwrap_or_else(|_| "root".into())
}

/// The playbook's variables: the work dir and the user to address.
#[derive(Clone)]
struct Vars {
    dir: PathBuf,
    user: String,
}

fn go(pb: &Playbook<Vars>, stub: &Stub, t: SshTransport) -> (Report, String) {
    let out = Rc::new(RefCell::new(String::new()));
    let report = run(
        pb,
        vec![HostSpec {
            groups: Vec::new(),
            name: "bot@h".into(),
            target: Target::Ssh(t),
            vars: Vars {
                dir: stub.work(),
                user: me(),
            },
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    let text = out.borrow().clone();
    (report, text)
}

fn f(h: &Host<Vars>, name: &str) -> String {
    h.vars.dir.join(name).display().to_string()
}

const ROOT: &str = "bash";

fn user_remote() -> String {
    format!("sudo -n -iu '{}' bash", me())
}

/// Root op, as_user op, root op in one window (ordering), then an as_user
/// fact read, then a root op in a later window.
fn routed(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move {
        let log = f(&h, "order");
        let u = h.as_user(&h.vars.user);
        sh!(h, "echo MARK_ROOT_1; echo 1 >> {log}");
        sh!(u, "echo MARK_USER_1; sleep 0.3; echo 2 >> {log}");
        sh!(h, "echo MARK_ROOT_2; echo 3 >> {log}");
        let found = u.bin_exists("tp-mark-fact-zz").await;
        let seen = f(&h, "fact");
        let v = if found { "yes" } else { "no" };
        sh!(h, "echo MARK_ROOT_3; echo {v} > {seen}");
        Ok(())
    })
}

#[test]
fn ops_and_fact_reads_go_to_their_principal_shell() {
    let stub = Stub::new();
    let pb = Playbook::new("routed", routed);
    let (r, out) = go(&pb, &stub, stub.transport());
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert_eq!(r.hosts[0].status, HostStatus::Changed);

    let user = user_remote();
    assert_eq!(stub.ran("MARK_ROOT_1"), [ROOT], "{:#?}", stub.channels());
    assert_eq!(stub.ran("MARK_ROOT_2"), [ROOT]);
    assert_eq!(stub.ran("MARK_ROOT_3"), [ROOT]);
    assert_eq!(stub.ran("MARK_USER_1"), std::slice::from_ref(&user));
    assert_eq!(stub.ran("tp-mark-fact-zz"), std::slice::from_ref(&user));
    // One root channel. The user's shell is opened once, and again after
    // each changed `sh!` op that writes `*` (T114 rule 5: it may have
    // changed a profile): once before the user op, once before the fact
    // read.
    let mut remotes: Vec<String> = stub.channels().into_iter().map(|(r, _)| r).collect();
    remotes.sort();
    assert_eq!(
        remotes,
        [ROOT.to_string(), user.clone(), user.clone(), user],
        "{remotes:?}"
    );
    assert!(out.contains("login shell"), "{out}");

    // The root op after the user op waited for it (Assumption A3).
    let order = std::fs::read_to_string(stub.work().join("order")).unwrap();
    assert_eq!(order, "1\n2\n3\n");
    // The fact was answered (no such bin) and the stream says what ran.
    let fact = std::fs::read_to_string(stub.work().join("fact")).unwrap();
    assert_eq!(fact, "no\n");
    assert!(out.contains("bot@h | MARK_USER_1"), "{out}");

    // User shells, the root shell and the connection all closed.
    assert_eq!(survivors(&stub.pids()), Vec::<u32>::new());
}

#[test]
fn refused_user_shell_blocks_before_the_window_s_root_ops() {
    fn pb_(h: Host<Vars>) -> PlaybookFuture {
        Box::pin(async move {
            let marker = f(&h, "marker");
            sh!(h, "echo MARK_ROOT_BLOCKED; touch {marker}");
            let u = h.as_user(&h.vars.user);
            sh!(u, "echo MARK_USER_BLOCKED");
            Ok(())
        })
    }
    let stub = Stub::new();
    let pb = Playbook::new("refused", pb_);
    let (r, out) = go(&pb, &stub, stub.transport().env("TP_STUB_NO_USER", "1"));
    let HostStatus::Blocked(why) = &r.hosts[0].status else {
        panic!("{:?}\n{out}", r.hosts[0].status)
    };
    let me = me();
    assert!(
        why.contains(&format!(
            "sudo -n -iu {me} refused: sudo: unknown user {me}"
        )),
        "{why}"
    );
    assert!(!stub.work().join("marker").exists(), "nothing applied");
    assert!(stub.ran("MARK_ROOT_BLOCKED").is_empty(), "root op not sent");
    assert!(stub.ran("MARK_USER_BLOCKED").is_empty());
    assert_eq!(r.exit_code(), 1);
    assert_eq!(survivors(&stub.pids()), Vec::<u32>::new());
}

#[test]
fn a_failure_on_a_user_shell_stops_sends_on_the_root_shell() {
    fn pb_(h: Host<Vars>) -> PlaybookFuture {
        Box::pin(async move {
            let after = f(&h, "after");
            let u = h.as_user(&h.vars.user);
            sh!(u, "echo MARK_USER_FAIL; exit 4");
            sh!(h, "echo MARK_ROOT_AFTER; touch {after}");
            Ok(())
        })
    }
    let stub = Stub::new();
    let pb = Playbook::new("ufail", pb_);
    let (r, out) = go(&pb, &stub, stub.transport());
    let HostStatus::Failed { rc, op, .. } = &r.hosts[0].status else {
        panic!("{:?}\n{out}", r.hosts[0].status)
    };
    assert_eq!(*rc, 4);
    assert!(op.contains("MARK_USER_FAIL"), "{op}");
    assert!(!stub.work().join("after").exists());
    assert!(stub.ran("MARK_ROOT_AFTER").is_empty(), "never sent");
    let outcomes: Vec<Outcome> = r.hosts[0].ops.iter().map(|o| o.outcome).collect();
    assert_eq!(outcomes, [Outcome::Failed, Outcome::NotRun], "{out}");
    assert_eq!(survivors(&stub.pids()), Vec::<u32>::new());
}

#[test]
fn root_limits_map_to_clear_blocked_reasons() {
    let stub = Stub::new();
    fn pb_(h: Host<Vars>) -> PlaybookFuture {
        Box::pin(async move {
            sh!(h, "true");
            Ok(())
        })
    }
    let pb = Playbook::new("noop", pb_);
    // A master that cannot connect: the stub ssh named as a missing binary
    // is a spawn error; use a stub that fails the master instead.
    let fail = stub.dir.path().join("ssh-fail");
    write_exe(
        &fail,
        r#"printf '%b\n' "$TP_STUB_MASTER_FAIL" >&2; exit 255"#,
    );
    for (stderr, want) in [
        (
            "ssh: connect to host h port 22: Connection refused",
            "ssh could not connect",
        ),
        (
            "bot@h: Permission denied (publickey).",
            "ssh authentication failed",
        ),
    ] {
        let t = SshTransport::new()
            .ssh(&fail.display().to_string())
            .env("TP_STUB_MASTER_FAIL", stderr);
        let (r, _) = go(&pb, &stub, t);
        let HostStatus::Blocked(why) = &r.hosts[0].status else {
            panic!("{:?}", r.hosts[0].status)
        };
        assert!(why.contains(want) && why.contains(stderr), "{why}");
    }
    // sudo -n true refused: the sudo wording plus sudo's own text.
    let sudo_fail = stub.dir.path().join("bin2");
    std::fs::create_dir(&sudo_fail).unwrap();
    write_exe(
        &sudo_fail.join("sudo"),
        r#"echo "sudo: a password is required" >&2; exit 1"#,
    );
    write_exe(&sudo_fail.join("id"), ID);
    let t = SshTransport::new()
        .ssh(&stub.ssh)
        .env("TP_STUB_DIR", &stub.dir.path().display().to_string())
        .env(
            "PATH",
            &format!("{}:{}", sudo_fail.display(), std::env::var("PATH").unwrap()),
        );
    let (r, _) = go(&pb, &stub, t);
    let HostStatus::Blocked(why) = &r.hosts[0].status else {
        panic!("{:?}", r.hosts[0].status)
    };
    assert!(
        why.contains("sudo refused") && why.contains("sudo: a password is required"),
        "{why}"
    );
}

#[test]
fn local_target_blocks_a_user_window_with_the_transport_limit() {
    fn op_(h: Host<Vars>) -> PlaybookFuture {
        Box::pin(async move {
            let marker = f(&h, "marker");
            sh!(h, "touch {marker}");
            let u = h.as_user(&h.vars.user);
            sh!(u, "true");
            Ok(())
        })
    }
    fn read_(h: Host<Vars>) -> PlaybookFuture {
        Box::pin(async move {
            let u = h.as_user(&h.vars.user);
            let _ = u.bin_exists("sh").await;
            Ok(())
        })
    }
    for (name, run_) in [
        ("op", op_ as fn(Host<Vars>) -> PlaybookFuture),
        ("read", read_),
    ] {
        let d = tempfile::tempdir().unwrap();
        let pb = Playbook::new(name, run_);
        let r = run(
            &pb,
            vec![HostSpec {
                groups: Vec::new(),
                name: "local".into(),
                target: Target::Local(telepatch::transport::local::LocalTransport::new().uid(0)),
                vars: Vars {
                    dir: d.path().to_path_buf(),
                    user: me(),
                },
            }],
            RunOpts {
                log: None,
                stdout: Stdout::Quiet,
            },
        );
        let HostStatus::Blocked(why) = &r.hosts[0].status else {
            panic!("{name}: {:?}", r.hosts[0].status)
        };
        assert!(
            why.contains(&format!(
                "principal user {}: not supported by this transport yet",
                me()
            )),
            "{name}: {why}"
        );
        assert!(!why.contains("switching users"), "{name}: {why}");
        assert!(!d.path().join("marker").exists(), "{name}: nothing applied");
    }
}

/// `h.user(u)` then `as_user(u)` in one role: the engine ends the window
/// before the as_user op, so the `getent` check and the user's shell come
/// after the account exists (the stub's `u` already exists, so the user op
/// is a no-op here; what shows is where the check runs).
#[test]
fn user_created_by_the_run_is_checked_and_opened_in_window_two() {
    fn pb_(h: Host<Vars>) -> PlaybookFuture {
        Box::pin(async move {
            h.user(&h.vars.user);
            let u = h.as_user(&h.vars.user);
            sh!(u, "echo MARK_USER_AFTER_CREATE");
            Ok(())
        })
    }
    let stub = Stub::new();
    let pb = Playbook::new("created", pb_);
    let (r, out) = go(&pb, &stub, stub.transport());
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert_eq!(
        stub.ran("MARK_USER_AFTER_CREATE"),
        [user_remote()],
        "{:#?}",
        stub.channels()
    );
    let root = stub
        .channels()
        .into_iter()
        .find(|(r, _)| r == ROOT)
        .unwrap()
        .1;
    let create = root.find("useradd").expect("user op sent on root");
    let check = root.find("echo \"ok \"").expect("users check sent");
    assert!(
        create < check,
        "check ran before the account was made:\n{root}"
    );
    assert_eq!(survivors(&stub.pids()), Vec::<u32>::new());
}

// ---- .timeout: the second shell ----

fn timed(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move {
        let later = f(&h, "later");
        sh!(h, "echo MARK_TIMED; sleep 4.31").timeout(std::time::Duration::from_secs(1));
        sh!(h, "touch {later}");
        Ok(())
    })
}

/// Stops what a test left running, by a marker in its command line.
struct Reaper(&'static str);
impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = std::process::Command::new("pkill")
            .args(["-KILL", "-f", self.0])
            .status();
    }
}

#[test]
fn a_timed_out_op_is_killed_through_a_second_channel() {
    let _r = Reaper("sleep 4.31");
    let stub = Stub::new();
    let pb = Playbook::new("timed", timed);
    let t = std::time::Instant::now();
    let (r, out) = go(&pb, &stub, stub.transport());
    assert!(
        t.elapsed() < std::time::Duration::from_millis(3800),
        "{:?}",
        t.elapsed()
    );
    let HostStatus::Failed { op, .. } = &r.hosts[0].status else {
        panic!("{:?}\n{out}", r.hosts[0].status)
    };
    assert!(op.ends_with(", timed out after 1s"), "{op}");
    assert!(out.contains("-> FAILED (timed out after 1s)"), "{out}");
    assert!(!stub.work().join("later").exists());
    // The root shell and the second shell, both `bash`.
    let remotes: Vec<String> = stub.channels().into_iter().map(|(r, _)| r).collect();
    assert_eq!(remotes, [ROOT, ROOT], "{remotes:?}");
    assert_eq!(survivors(&stub.pids()), Vec::<u32>::new());
}

#[test]
fn a_host_that_gives_one_shell_fails_as_a_timeout_and_drops_the_connection() {
    let _r = Reaper("sleep 4.31");
    let stub = Stub::new();
    let pb = Playbook::new("one", timed);
    let t = std::time::Instant::now();
    let (r, out) = go(&pb, &stub, stub.transport().env("TP_STUB_MAX_CHAN", "1"));
    assert!(
        t.elapsed() < std::time::Duration::from_millis(3800),
        "{:?}",
        t.elapsed()
    );
    let HostStatus::Failed { op, .. } = &r.hosts[0].status else {
        panic!("{:?}\n{out}", r.hosts[0].status)
    };
    assert!(
        op.ends_with(", timed out after 1s; no second shell to stop it"),
        "{op}"
    );
    assert!(
        out.contains("-> FAILED (timed out after 1s; no second shell to stop it)"),
        "{out}"
    );
    assert!(!stub.work().join("later").exists());
    assert_eq!(r.exit_code(), 1);
    // The shell and the connection are closed: no ssh process is left.
    assert_eq!(survivors(&stub.pids()), Vec::<u32>::new());
}
