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
  # A host that is down: the next TP_STUB_DOWN master starts are refused.
  if [ -e "$d/down" ] && [ "$(cat "$d/down")" -gt 0 ]; then
    echo $(( $(cat "$d/down") - 1 )) > "$d/down"
    echo "ssh: connect to host h port 22: Connection refused" >&2
    exit 255
  fi
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

/// `systemctl reboot` of a fake host: records the call, takes a new boot id
/// (the file `TP_BOOT_ID` names, unless `TP_STUB_SAME_BOOT`), loses `/run`
/// (`TP_RUN_DIR`, the mark copied to `mark.at-reboot` first), goes down for
/// `TP_STUB_DOWN` connection attempts, and ends every ssh process the stub
/// has started, the one running this command's shell included. Any other
/// `systemctl` call is recorded and does nothing.
const SYSTEMCTL: &str = r#"
printf '%s\n' "$*" >> "$TP_STUB_DIR/systemctl.log"
[ "$1" = reboot ] || exit 0
[ -z "$TP_STUB_REBOOT_FAIL" ] || { echo "Failed to reboot: Access denied" >&2; exit 1; }
[ ! -e "$TP_RUN_DIR/reboot-pending" ] || cp "$TP_RUN_DIR/reboot-pending" "$TP_STUB_DIR/mark.at-reboot"
[ -n "$TP_STUB_SAME_BOOT" ] || printf 'boot-%s\n' "$$" > "$TP_BOOT_ID"
rm -r -f "$TP_RUN_DIR"
echo "${TP_STUB_DOWN:-2}" > "$TP_STUB_DIR/down"
cat "$TP_STUB_DIR/pids" >> "$TP_STUB_DIR/pids.dead"
dying=$(cat "$TP_STUB_DIR/pids")
: > "$TP_STUB_DIR/pids"
kill -9 $dying 2>/dev/null
exit 0
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
        write_exe(&bin.join("systemctl"), SYSTEMCTL);
        std::fs::write(dir.path().join("boot_id"), "boot-1\n").unwrap();
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

    /// The transport of a host whose boot id and `/run` the stub
    /// `systemctl` can change, plus `extra` environment.
    fn rtransport(&self, extra: &[(&str, &str)]) -> SshTransport {
        let mut t = self
            .transport()
            .env("TP_BOOT_ID", &self.boot_file().display().to_string())
            .env("TP_RUN_DIR", &self.run_dir().display().to_string());
        for (k, v) in extra {
            t = t.env(k, v);
        }
        t
    }

    fn boot_file(&self) -> PathBuf {
        self.dir.path().join("boot_id")
    }

    fn run_dir(&self) -> PathBuf {
        self.dir.path().join("run")
    }

    fn boot_id(&self) -> String {
        std::fs::read_to_string(self.boot_file())
            .unwrap()
            .trim()
            .to_string()
    }

    /// How many times the stub `systemctl` was asked to reboot.
    fn reboots(&self) -> usize {
        std::fs::read_to_string(self.dir.path().join("systemctl.log"))
            .unwrap_or_default()
            .lines()
            .filter(|l| *l == "reboot")
            .count()
    }

    /// What the mark held when the host rebooted.
    fn mark_at_reboot(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("mark.at-reboot")).unwrap_or_default()
    }

    /// The channels (by file name) whose input contains `mark`.
    fn chans(&self, mark: &str) -> Vec<String> {
        let mut v = vec![];
        for e in std::fs::read_dir(self.dir.path()).unwrap().flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with("chan-")
                && n.ends_with(".in")
                && std::fs::read_to_string(e.path()).unwrap().contains(mark)
            {
                v.push(n);
            }
        }
        v.sort();
        v
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

    /// Every ssh process the stub started, the ones a fake reboot ended
    /// included.
    fn pids(&self) -> Vec<u32> {
        ["pids", "pids.dead"]
            .iter()
            .flat_map(|f| {
                std::fs::read_to_string(self.dir.path().join(f))
                    .unwrap_or_default()
                    .lines()
                    .map(|l| l.trim().parse().unwrap())
                    .collect::<Vec<u32>>()
            })
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

// ---- reboot marks and the barrier (T102) ----
//
// The fake host: `systemctl reboot` is a stub on the stub PATH ([`SYSTEMCTL`])
// that takes a new boot id, loses `/run`, refuses the next connections and
// ends the stub's ssh processes. `TP_RUN_DIR` stands for `/run/telepatch`
// and `TP_BOOT_ID` for `/proc/sys/kernel/random/boot_id`. Nothing here ever
// runs a real `systemctl`.

use telepatch::executor::{CheckOpts, RebootPolicy, RebootWait, RunControls, run_check, run_with};

/// Short waits: the fake host is down for a few connection attempts.
fn fast() -> RunControls {
    RunControls {
        reboot_wait: RebootWait {
            deadline: std::time::Duration::from_secs(20),
            poll: std::time::Duration::from_millis(50),
            down: std::time::Duration::from_secs(5),
        },
        ..Default::default()
    }
}

fn go_with(pb: &Playbook<Vars>, stub: &Stub, t: SshTransport, c: RunControls) -> (Report, String) {
    let out = Rc::new(RefCell::new(String::new()));
    let report = run_with(
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
        None,
        c,
    );
    let text = out.borrow().clone();
    (report, text)
}

fn read(h: &Host<Vars>, name: &str) -> String {
    std::fs::read_to_string(h.vars.dir.join(name)).unwrap_or_default()
}

/// A marked file op, a barrier, then a root op and a user op that note the
/// boot id they ran under. A root op and a user op before the barrier note
/// theirs.
fn gpu(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move {
        let u = h.as_user(&h.vars.user);
        {
            let _r = h.role_enter("gpu_driver");
            h.file(&f(&h, "a")).content("driver\n").needs_reboot();
        }
        let (b4, b4u) = (f(&h, "boot_before"), f(&h, "boot_before_user"));
        sh!(h, "echo MARK_BEFORE_ROOT; cat \"$TP_BOOT_ID\" > {b4}");
        sh!(u, "echo MARK_BEFORE_USER; cat \"$TP_BOOT_ID\" > {b4u}");
        h.reboot().if_pending();
        let (af, afu) = (f(&h, "boot_after"), f(&h, "boot_after_user"));
        sh!(h, "echo MARK_AFTER_ROOT; cat \"$TP_BOOT_ID\" > {af}");
        sh!(u, "echo MARK_AFTER_USER; cat \"$TP_BOOT_ID\" > {afu}");
        Ok(())
    })
}

#[test]
fn a_marked_change_is_marked_and_the_barrier_reboots_once_and_reopens_every_shell() {
    let stub = Stub::new();
    let pb = Playbook::new("gpu", gpu);
    let (r, out) = go_with(&pb, &stub, stub.rtransport(&[]), fast());
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert_eq!(r.hosts[0].status, HostStatus::Changed, "{out}");
    let a = stub.work().join("a").display().to_string();

    // The mark listed the change when the host went down; one reboot.
    assert_eq!(stub.mark_at_reboot(), format!("file {a}\n"));
    assert_eq!(stub.reboots(), 1);
    assert_ne!(stub.boot_id(), "boot-1");
    assert!(out.contains("-> changed; needs reboot"), "{out}");
    assert!(
        out.contains(&format!(
            "== bot@h [gpu_driver] reboot: pending (file {a}) =="
        )) || out.contains(&format!("== bot@h [gpu] reboot: pending (file {a}) ==")),
        "{out}"
    );
    assert!(
        out.contains("== bot@h [reboot] down ")
            && out.contains("s, boot id changed, shells reopened =="),
        "{out}"
    );

    // Before the barrier the old boot, after it the new one.
    let h = Host::new(
        "x",
        telepatch::facts::os::Os::parse("ID=debian\n").unwrap(),
        Rc::new(Vars {
            dir: stub.work(),
            user: me(),
        }),
    );
    assert_eq!(read(&h, "boot_before"), "boot-1\n");
    assert_eq!(read(&h, "boot_before_user"), "boot-1\n");
    assert_eq!(read(&h, "boot_after"), format!("{}\n", stub.boot_id()));
    assert_eq!(read(&h, "boot_after_user"), format!("{}\n", stub.boot_id()));

    // The ops after the barrier ran on new channels: a new root channel,
    // and a new login channel for the user op.
    let (rb, ra) = (
        stub.chans("MARK_BEFORE_ROOT"),
        stub.chans("MARK_AFTER_ROOT"),
    );
    assert_eq!((rb.len(), ra.len()), (1, 1), "{:#?}", stub.channels());
    assert_ne!(rb, ra);
    let (ub, ua) = (
        stub.chans("MARK_BEFORE_USER"),
        stub.chans("MARK_AFTER_USER"),
    );
    assert_eq!((ub.len(), ua.len()), (1, 1));
    assert_ne!(ub, ua);
    assert_eq!(stub.ran("MARK_AFTER_USER"), [user_remote()]);
    assert_eq!(stub.ran("MARK_AFTER_ROOT"), [ROOT]);

    // The report: one reboot, nothing pending, the table says nothing of it.
    let rep = &r.hosts[0];
    assert_eq!(rep.reboots.len(), 1, "{rep:?}");
    assert_eq!(rep.reboots[0].old_boot_id, "boot-1");
    assert_eq!(rep.reboots[0].pending, [format!("file {a}")]);
    assert!(rep.reboot_pending.is_empty());
    // Not an op: the reboot counts for nothing in OK or CHANGED.
    assert_eq!(rep.ops.len(), 5, "{:?}", rep.ops);
    assert_eq!(survivors(&stub.pids()), Vec::<u32>::new());
}

/// Two roles each mark, one barrier.
fn two_roles(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move {
        {
            let _r = h.role_enter("gpu_driver");
            h.file(&f(&h, "a")).content("1\n").needs_reboot();
        }
        {
            let _r = h.role_enter("kernel_update");
            h.file(&f(&h, "b")).content("2\n").needs_reboot();
        }
        h.reboot().if_pending();
        sh!(h, "echo MARK_AFTER_TWO");
        Ok(())
    })
}

#[test]
fn two_marked_ops_in_two_roles_cost_one_reboot() {
    let stub = Stub::new();
    let pb = Playbook::new("two", two_roles);
    let (r, out) = go_with(&pb, &stub, stub.rtransport(&[]), fast());
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert_eq!(stub.reboots(), 1, "{out}");
    let (a, b) = (
        stub.work().join("a").display().to_string(),
        stub.work().join("b").display().to_string(),
    );
    assert_eq!(stub.mark_at_reboot(), format!("file {a}\nfile {b}\n"));
    assert!(
        out.contains(&format!("reboot: pending (file {a}, file {b}) ==")),
        "{out}"
    );
    assert_eq!(r.hosts[0].reboots.len(), 1);
}

#[test]
fn a_marked_op_that_changed_nothing_leaves_no_mark_and_the_barrier_does_nothing() {
    let stub = Stub::new();
    // The file is already as the op wants it.
    std::fs::write(stub.work().join("a"), "driver\n").unwrap();
    fn pb_(h: Host<Vars>) -> PlaybookFuture {
        Box::pin(async move {
            h.file(&f(&h, "a")).content("driver\n").needs_reboot();
            h.reboot().if_pending();
            sh!(h, "echo MARK_AFTER_NOTHING");
            Ok(())
        })
    }
    let pb = Playbook::new("quiet", pb_);
    let (r, out) = go_with(&pb, &stub, stub.rtransport(&[]), fast());
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert_eq!(stub.reboots(), 0, "{out}");
    assert!(
        !stub.dir.path().join("systemctl.log").exists(),
        "no systemctl call"
    );
    assert_eq!(stub.boot_id(), "boot-1");
    assert!(!out.contains("reboot"), "{out}");
    assert_eq!(stub.ran("MARK_AFTER_NOTHING"), [ROOT]);
    // One root channel for the whole run: nothing was reopened.
    assert_eq!(stub.channels().len(), 1, "{:#?}", stub.channels());
    assert!(r.hosts[0].reboots.is_empty() && r.hosts[0].reboot_pending.is_empty());
}

#[test]
fn a_mark_an_earlier_run_left_is_seen_by_the_barrier() {
    let stub = Stub::new();
    std::fs::create_dir_all(stub.run_dir()).unwrap();
    std::fs::write(stub.run_dir().join("reboot-pending"), "pkg linux-generic\n").unwrap();
    fn pb_(h: Host<Vars>) -> PlaybookFuture {
        Box::pin(async move {
            h.reboot().if_pending();
            sh!(h, "echo MARK_AFTER_OLD");
            Ok(())
        })
    }
    let pb = Playbook::new("old", pb_);
    let (r, out) = go_with(&pb, &stub, stub.rtransport(&[]), fast());
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert_eq!(stub.reboots(), 1, "{out}");
    assert_eq!(stub.mark_at_reboot(), "pkg linux-generic\n");
    assert!(
        out.contains("[old] reboot: pending (pkg linux-generic) =="),
        "{out}"
    );
    assert_eq!(stub.chans("MARK_AFTER_OLD").len(), 1);
}

/// A marked op, a barrier, an op after it.
fn stops(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move {
        {
            let _r = h.role_enter("kernel_update");
            h.file(&f(&h, "before")).content("1\n").needs_reboot();
        }
        h.reboot().if_pending();
        {
            let _r = h.role_enter("cuda_workloads");
            let after = f(&h, "after");
            sh!(h, "echo MARK_AFTER_STOP; touch {after}");
        }
        Ok(())
    })
}

#[test]
fn reboot_never_stops_at_a_pending_barrier() {
    let stub = Stub::new();
    let pb = Playbook::new("stops", stops);
    let c = RunControls {
        reboot: RebootPolicy::Never,
        ..fast()
    };
    let (r, out) = go_with(&pb, &stub, stub.rtransport(&[]), c);
    let rep = &r.hosts[0];
    let before = stub.work().join("before").display().to_string();
    assert_eq!(
        rep.status,
        HostStatus::RebootPending(vec![format!("file {before}")]),
        "{out}"
    );
    assert!(!rep.status.is_success());
    assert_eq!(r.exit_code(), 1);
    // The op before the barrier is applied, the one after it did not run.
    assert_eq!(
        std::fs::read_to_string(stub.work().join("before")).unwrap(),
        "1\n"
    );
    assert!(!stub.work().join("after").exists());
    assert!(stub.ran("MARK_AFTER_STOP").is_empty(), "never sent");
    let outcomes: Vec<Outcome> = rep.ops.iter().map(|o| o.outcome).collect();
    assert_eq!(outcomes, [Outcome::Changed, Outcome::NotRun], "{out}");
    assert_eq!(stub.reboots(), 0);
    assert!(
        out.contains("== bot@h [stops] reboot pending at barrier =="),
        "{out}"
    );
    let text = r.render();
    assert!(text.contains("reboot pending at barrier"), "{text}");
    assert!(text.contains("skipped roles: cuda_workloads"), "{text}");
    assert!(r.table().contains("reboot pending"), "{}", r.table());
    // Only the mark is left on the host.
    assert_eq!(
        std::fs::read_to_string(stub.run_dir().join("reboot-pending")).unwrap(),
        format!("file {before}\n")
    );
    assert_eq!(survivors(&stub.pids()), Vec::<u32>::new());
}

#[test]
fn a_host_that_never_comes_back_fails_after_the_deadline() {
    let stub = Stub::new();
    let pb = Playbook::new("stops", stops);
    let c = RunControls {
        reboot_wait: RebootWait {
            deadline: std::time::Duration::from_millis(1500),
            poll: std::time::Duration::from_millis(50),
            down: std::time::Duration::from_secs(5),
        },
        ..Default::default()
    };
    let t0 = std::time::Instant::now();
    let (r, out) = go_with(
        &pb,
        &stub,
        stub.rtransport(&[("TP_STUB_DOWN", "1000000")]),
        c,
    );
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(15),
        "{:?}",
        t0.elapsed()
    );
    let HostStatus::Blocked(why) = &r.hosts[0].status else {
        panic!("{:?}\n{out}", r.hosts[0].status)
    };
    assert!(
        why.starts_with("reboot: host did not come back within 1.5s"),
        "{why}"
    );
    assert!(why.contains("ssh could not connect"), "{why}");
    assert_eq!(r.exit_code(), 1);
    assert_eq!(stub.reboots(), 1);
    assert!(stub.ran("MARK_AFTER_STOP").is_empty());
    assert_eq!(survivors(&stub.pids()), Vec::<u32>::new());
}

#[test]
fn a_host_that_answers_with_the_same_boot_id_has_not_rebooted() {
    let stub = Stub::new();
    let pb = Playbook::new("stops", stops);
    let c = RunControls {
        reboot_wait: RebootWait {
            deadline: std::time::Duration::from_millis(1500),
            poll: std::time::Duration::from_millis(50),
            down: std::time::Duration::from_secs(5),
        },
        ..Default::default()
    };
    let t = stub.rtransport(&[("TP_STUB_SAME_BOOT", "1"), ("TP_STUB_DOWN", "0")]);
    let (r, out) = go_with(&pb, &stub, t, c);
    let HostStatus::Blocked(why) = &r.hosts[0].status else {
        panic!("{:?}\n{out}", r.hosts[0].status)
    };
    assert!(
        why.starts_with("reboot: host did not come back within 1.5s")
            && why.contains("boot id unchanged"),
        "{why}"
    );
    assert!(stub.ran("MARK_AFTER_STOP").is_empty());
    assert_eq!(survivors(&stub.pids()), Vec::<u32>::new());
}

#[test]
fn a_failing_systemctl_reboot_fails_the_host_with_its_message() {
    let stub = Stub::new();
    let pb = Playbook::new("stops", stops);
    let (r, out) = go_with(
        &pb,
        &stub,
        stub.rtransport(&[("TP_STUB_REBOOT_FAIL", "1")]),
        fast(),
    );
    let HostStatus::Blocked(why) = &r.hosts[0].status else {
        panic!("{:?}\n{out}", r.hosts[0].status)
    };
    assert!(
        why.contains("systemctl reboot failed (rc 1): Failed to reboot: Access denied"),
        "{why}"
    );
    assert!(stub.ran("MARK_AFTER_STOP").is_empty());
}

/// A marked op and no barrier.
fn marked_only(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move {
        {
            let _r = h.role_enter("kernel_update");
            h.file(&f(&h, "k")).content("k\n").needs_reboot();
        }
        sh!(h, "echo MARK_LAST");
        Ok(())
    })
}

#[test]
fn a_reboot_pending_at_the_end_is_reported_and_the_host_stays_a_success() {
    let stub = Stub::new();
    let pb = Playbook::new("end", marked_only);
    let (r, out) = go_with(&pb, &stub, stub.rtransport(&[]), fast());
    let rep = &r.hosts[0];
    let k = stub.work().join("k").display().to_string();
    assert_eq!(rep.status, HostStatus::Changed, "{out}");
    assert_eq!(r.exit_code(), 0);
    assert_eq!(rep.reboot_pending, [format!("file {k}")]);
    assert_eq!(stub.reboots(), 0);
    assert!(
        out.contains(&format!(
            "== bot@h [reboot] pending at the end of the run (file {k}) =="
        )),
        "{out}"
    );
    let text = r.render();
    assert!(
        text.contains(&format!("bot@h: reboot pending (file {k})")),
        "{text}"
    );
    assert!(r.table().contains("reboot pending"), "{}", r.table());
}

#[test]
fn reboot_auto_reboots_once_at_the_end_and_waits_for_the_host() {
    let stub = Stub::new();
    let pb = Playbook::new("end", marked_only);
    let c = RunControls {
        reboot: RebootPolicy::Auto,
        ..fast()
    };
    let (r, out) = go_with(&pb, &stub, stub.rtransport(&[]), c);
    let rep = &r.hosts[0];
    assert_eq!(rep.status, HostStatus::Changed, "{out}");
    assert_eq!(r.exit_code(), 0);
    assert_eq!(stub.reboots(), 1, "{out}");
    assert!(rep.reboot_pending.is_empty(), "{rep:?}");
    assert_eq!(rep.reboots.len(), 1);
    assert!(out.contains("boot id changed, shells reopened =="), "{out}");
    // No op ran after it.
    let last = stub.chans("MARK_LAST");
    assert_eq!(last.len(), 1);
    assert_eq!(survivors(&stub.pids()), Vec::<u32>::new());
}

#[test]
fn reboot_auto_with_nothing_pending_does_not_reboot() {
    let stub = Stub::new();
    fn pb_(h: Host<Vars>) -> PlaybookFuture {
        Box::pin(async move {
            sh!(h, "echo MARK_PLAIN");
            Ok(())
        })
    }
    let pb = Playbook::new("plain", pb_);
    let c = RunControls {
        reboot: RebootPolicy::Auto,
        ..fast()
    };
    let (r, out) = go_with(&pb, &stub, stub.rtransport(&[]), c);
    assert_eq!(r.exit_code(), 0, "{out}");
    assert_eq!(stub.reboots(), 0);
    assert!(r.hosts[0].reboot_pending.is_empty());
}

/// A timeout op killed before the barrier opens the second shell; the one
/// after the barrier must be stopped through a second shell of the new boot.
fn timeouts(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move {
        sh!(h, "echo MARK_T_BEFORE; sleep 4.32")
            .timeout(std::time::Duration::from_secs(1))
            .allow_fail();
        h.file(&f(&h, "m")).content("m\n").needs_reboot();
        h.reboot().if_pending();
        sh!(h, "echo MARK_T_AFTER; sleep 4.31").timeout(std::time::Duration::from_secs(1));
        Ok(())
    })
}

#[test]
fn a_timeout_after_the_barrier_is_still_killed_through_a_second_shell_of_the_new_boot() {
    let _r1 = Reaper("sleep 4.32");
    let _r2 = Reaper("sleep 4.31");
    let stub = Stub::new();
    let pb = Playbook::new("timeouts", timeouts);
    let t0 = std::time::Instant::now();
    let (r, out) = go_with(&pb, &stub, stub.rtransport(&[]), fast());
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(8),
        "{:?}",
        t0.elapsed()
    );
    let HostStatus::Failed { op, .. } = &r.hosts[0].status else {
        panic!("{:?}\n{out}", r.hosts[0].status)
    };
    assert!(op.ends_with(", timed out after 1s"), "{op}");
    assert_eq!(stub.reboots(), 1, "{out}");
    // Two kills, each by a second shell of its own boot.
    let killers = stub.chans("kill -TERM");
    assert_eq!(killers.len(), 2, "{:#?}", stub.channels());
    // The one after the barrier is a channel the first boot never had.
    assert_ne!(killers[0], killers[1]);
    assert_eq!(stub.chans("MARK_T_AFTER").len(), 1);
    assert_eq!(survivors(&stub.pids()), Vec::<u32>::new());
}

// ---- --check never reboots ----

fn check_go(pb: &Playbook<Vars>, stub: &Stub) -> (Report, String) {
    let out = Rc::new(RefCell::new(String::new()));
    let report = run_check(
        pb,
        vec![HostSpec {
            groups: Vec::new(),
            name: "bot@h".into(),
            target: Target::Ssh(stub.rtransport(&[])),
            vars: Vars {
                dir: stub.work(),
                user: me(),
            },
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
        CheckOpts::default(),
    );
    let text = out.borrow().clone();
    (report, text)
}

#[test]
fn a_check_stops_at_a_barrier_a_marked_op_may_make_pending() {
    let stub = Stub::new();
    let pb = Playbook::new("stops", stops);
    let (r, out) = check_go(&pb, &stub);
    let stop = r.hosts[0]
        .check
        .as_ref()
        .unwrap()
        .stopped
        .as_ref()
        .expect(&out);
    assert!(stop.reboot);
    assert!(
        stop.line().ends_with(": reboot pending at barrier")
            && stop.line().starts_with("check stopped at "),
        "{}",
        stop.line()
    );
    assert!(
        r.render().contains("reboot pending at barrier"),
        "{}",
        r.render()
    );
    assert_eq!(stub.reboots(), 0);
    assert!(
        !stub.work().join("before").exists(),
        "a check applies nothing"
    );
    assert!(stub.ran("MARK_AFTER_STOP").is_empty());
}

#[test]
fn a_check_passes_a_barrier_nothing_can_make_pending() {
    let stub = Stub::new();
    std::fs::write(stub.work().join("before"), "1\n").unwrap();
    let pb = Playbook::new("stops", stops);
    let (r, out) = check_go(&pb, &stub);
    let info = r.hosts[0].check.as_ref().unwrap();
    assert!(info.stopped.is_none(), "{out}");
    // The op after the barrier was traced and counted.
    assert_eq!(info.ops, 2, "{out}");
    assert_eq!(stub.reboots(), 0);
}

#[test]
fn a_check_stops_at_a_barrier_when_the_host_is_already_marked() {
    let stub = Stub::new();
    std::fs::write(stub.work().join("before"), "1\n").unwrap();
    std::fs::create_dir_all(stub.run_dir()).unwrap();
    std::fs::write(stub.run_dir().join("reboot-pending"), "pkg linux-generic\n").unwrap();
    let pb = Playbook::new("stops", stops);
    let (r, out) = check_go(&pb, &stub);
    assert!(
        r.hosts[0]
            .check
            .as_ref()
            .unwrap()
            .stopped
            .as_ref()
            .is_some_and(|s| s.reboot),
        "{out}"
    );
    assert_eq!(stub.reboots(), 0);
}

/// A marked op, then an op that fails in the same window: the change is
/// marked all the same.
fn marked_then_failing(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move {
        h.file(&f(&h, "a")).content("1\n").needs_reboot();
        sh!(h, "echo MARK_BOOM; exit 3");
        Ok(())
    })
}

#[test]
fn a_mark_is_not_lost_when_a_later_op_of_the_window_fails() {
    let stub = Stub::new();
    let pb = Playbook::new("boom", marked_then_failing);
    let (r, out) = go_with(&pb, &stub, stub.rtransport(&[]), fast());
    assert!(
        matches!(r.hosts[0].status, HostStatus::Failed { rc: 3, .. }),
        "{out}"
    );
    let a = stub.work().join("a").display().to_string();
    assert_eq!(
        std::fs::read_to_string(stub.run_dir().join("reboot-pending")).unwrap(),
        format!("file {a}\n")
    );
    // The failed host is still listed as needing a reboot.
    assert_eq!(r.hosts[0].reboot_pending, [format!("file {a}")]);
    assert_eq!(stub.reboots(), 0);
}

/// A marked op in a user's login shell: the mark is written through the
/// root shell, which can write `/run`.
fn marked_as_user(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move {
        let u = h.as_user(&h.vars.user);
        let a = f(&h, "a");
        sh!(u, "echo MARK_USER_MARKED; echo x > {a}").needs_reboot();
        Ok(())
    })
}

#[test]
fn a_marked_op_in_a_login_shell_is_marked_through_the_root_shell() {
    let stub = Stub::new();
    let pb = Playbook::new("user", marked_as_user);
    let (r, out) = go_with(&pb, &stub, stub.rtransport(&[]), fast());
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    let mark = std::fs::read_to_string(stub.run_dir().join("reboot-pending")).unwrap();
    assert!(
        mark.starts_with("sh echo MARK_USER_MARKED; echo x > "),
        "{mark}"
    );
    // Only the root shell was sent the mark script; the user's shell saw
    // the op alone.
    assert_eq!(stub.ran("reboot-pending"), [ROOT]);
    let mut ran = stub.ran("MARK_USER_MARKED");
    ran.sort();
    let mut want = vec![ROOT.to_string(), user_remote()];
    want.sort();
    assert_eq!(
        ran, want,
        "the user ran the op; the root shell was sent its mark"
    );
    assert_eq!(r.hosts[0].reboot_pending.len(), 1);
}
