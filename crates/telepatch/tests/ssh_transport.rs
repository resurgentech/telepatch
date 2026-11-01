//! SSH transport against a stub `ssh`: the stub runs the remote command
//! with local `bash` and scripts failures through environment variables.
//! No network, no real ssh, no real sudo.
//!
//! The stub understands what the transport sends:
//! - master: `-N` (with `-o ControlMaster=yes -o ControlPath=P`): records
//!   its pid, creates `P`, and sleeps until `-O exit` kills it; with
//!   `TP_STUB_MASTER_FAIL` set it prints that to stderr and exits 255.
//! - `-O exit`: kills the master and every channel registered on `P`.
//! - channel (no `-N`): fails unless `P` exists, registers its pid on `P`,
//!   then `exec bash -c "exec <command>"`.
//!
//! Fake `id -u` prints `$TP_STUB_UID` (default 1000); fake `sudo -n CMD`
//! logs its argv, refuses when `TP_STUB_SUDO_FAIL` is set, and otherwise
//! runs CMD with `TP_STUB_UID=0`.

use std::path::Path;

use telepatch::facts::os::Os;
use telepatch::pool::Shell;
use telepatch::trace::Principal;
use telepatch::trailer::ShellResult;
use telepatch::transport::ssh::SshTransport;
use telepatch::transport::{Limit, Transport, read_os};
use telepatch::{Host, Node};

/// Write an executable script. It is written by a child `sh`, not by this
/// process: a write fd held here can leak into a sibling test thread's
/// fork and make exec fail with ETXTBSY ("Text file busy").
fn script(dir: &Path, name: &str, body: &str) -> String {
    let p = dir.join(name);
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg("cat > \"$1\" && chmod 755 \"$1\"")
        .arg("sh")
        .arg(&p)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("#!/bin/sh\n{body}\n").as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    p.display().to_string()
}

const SSH: &str = r#"
d="$TP_STUB_DIR"
for a in "$@"; do printf '%s\n' "$a"; done >> "$d/argv"
echo -- >> "$d/argv"
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
  [ -e "$cp.chans" ] && kill $(cat "$cp.chans") 2>/dev/null
  rm -f "$cp" "$cp.pid" "$cp.chans"
  echo "Exit request sent." >&2
  exit 0
fi
echo $$ >> "$d/pids"
if [ -n "$master" ]; then
  if [ -n "$TP_STUB_MASTER_FAIL" ]; then printf '%b\n' "$TP_STUB_MASTER_FAIL" >&2; exit 255; fi
  echo $$ > "$cp.pid"
  : > "$cp"
  exec sleep 100000
fi
[ -e "$cp" ] || { echo "stub ssh: no master at $cp" >&2; exit 255; }
echo $$ >> "$cp.chans"
exec bash -c "exec $*"
"#;

const ID: &str = r#"
if [ "$1" = -u ]; then echo "${TP_STUB_UID:-1000}"; exit 0; fi
exec /usr/bin/id "$@"
"#;

const SUDO: &str = r#"
printf '%s\n' "$*" >> "$TP_STUB_DIR/sudo.log"
[ "$1" = -n ] || exit 2
shift
if [ -n "$TP_STUB_SUDO_FAIL" ]; then echo "sudo: a password is required" >&2; exit 1; fi
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
        let ssh = script(dir.path(), "ssh", SSH);
        script(&bin, "id", ID);
        script(&bin, "sudo", SUDO);
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
        Stub { dir, ssh, path }
    }

    fn transport(&self) -> SshTransport {
        SshTransport::new()
            .ssh(&self.ssh)
            .env("TP_STUB_DIR", &self.dir.path().display().to_string())
            .env("PATH", &self.path)
    }

    /// Each stub invocation's argv (without argv[0]).
    fn argvs(&self) -> Vec<Vec<String>> {
        let text = std::fs::read_to_string(self.dir.path().join("argv")).unwrap_or_default();
        let mut out = vec![];
        let mut cur = vec![];
        for l in text.lines() {
            if l == "--" {
                out.push(std::mem::take(&mut cur));
            } else {
                cur.push(l.to_string());
            }
        }
        out
    }

    fn sudo_log(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("sudo.log")).unwrap_or_default()
    }

    fn pids(&self) -> Vec<u32> {
        std::fs::read_to_string(self.dir.path().join("pids"))
            .unwrap_or_default()
            .lines()
            .map(|l| l.trim().parse().unwrap())
            .collect()
    }
}

/// A process is gone when /proc has no entry or it is a zombie (nobody has
/// reaped it yet, but it runs nothing).
fn alive(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => false,
        Ok(s) => {
            let state = s.rsplit_once(')').map(|(_, r)| r.trim_start());
            !matches!(state.and_then(|r| r.chars().next()), Some('Z') | Some('X'))
        }
    }
}

/// Wait up to 5 s for every pid in `pids` to be gone; return the survivors.
async fn survivors(pids: &[u32]) -> Vec<u32> {
    for _ in 0..100 {
        let left: Vec<u32> = pids.iter().copied().filter(|&p| alive(p)).collect();
        if left.is_empty() {
            return left;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    pids.iter().copied().filter(|&p| alive(p)).collect()
}

fn os() -> Os {
    Os::parse("ID=debian\nVERSION_ID=12\n").unwrap()
}

/// Run `cmd` as a raw op on `sh` and return (rc, output).
async fn run(sh: &mut Shell, cmd: &str) -> (i32, String) {
    let h = Host::new("t", os(), std::rc::Rc::new(()));
    h.raw(cmd);
    let Node::Op(op) = &h.snapshot().nodes[0] else {
        unreachable!()
    };
    sh.send_op(op, 0, &h.os).await.unwrap();
    let r = sh.wait(0).await.unwrap();
    (
        r.trailer.typed::<ShellResult>().unwrap().rc,
        String::from_utf8(r.output).unwrap(),
    )
}

fn value_after<'a>(argv: &'a [String], flag: &str, prefix: &str) -> Vec<&'a str> {
    argv.windows(2)
        .filter(|w| w[0] == flag && w[1].starts_with(prefix))
        .map(|w| w[1].as_str())
        .collect()
}

#[tokio::test]
async fn argv_shape_of_master_channel_and_exit() {
    let stub = Stub::new();
    let key = stub.dir.path().join("id_test");
    let t = stub
        .transport()
        .env("TP_STUB_UID", "0")
        .identity(&key)
        .option("ServerAliveInterval=7")
        .option("ControlPath=/tmp/user-wins-not");
    let dest = "ssh://bot@example.test:2222";
    let s = t.open(dest, &Principal::Root, 1).await;
    assert!(s.limit.is_none(), "{:?} {:?}", s.limit, t.message(dest));
    assert_eq!(s.got.len(), 1);
    t.close().await;

    let argvs = stub.argvs();
    assert_eq!(argvs.len(), 3, "master, channel, -O exit: {argvs:#?}");
    let (master, chan, exit) = (&argvs[0], &argvs[1], &argvs[2]);

    for a in &argvs {
        assert_eq!(a[0], "-T", "{a:?}");
        assert_eq!(
            value_after(a, "-o", "BatchMode="),
            ["BatchMode=yes"],
            "{a:?}"
        );
    }
    // Key and IdentitiesOnly on the connection.
    assert_eq!(value_after(master, "-i", ""), [key.display().to_string()]);
    assert_eq!(
        value_after(master, "-o", "IdentitiesOnly="),
        ["IdentitiesOnly=yes"]
    );

    // Master: -N, ControlMaster=yes, ControlPersist=no, its own ControlPath
    // first (so it beats the user's), the user's extras after; dest last.
    assert!(master.contains(&"-N".to_string()), "{master:?}");
    assert_eq!(
        value_after(master, "-o", "ControlMaster="),
        ["ControlMaster=yes"]
    );
    assert_eq!(
        value_after(master, "-o", "ControlPersist="),
        ["ControlPersist=no"]
    );
    let cps = value_after(master, "-o", "ControlPath=");
    assert_eq!(cps.len(), 2, "{master:?}");
    let cp = cps[0].strip_prefix("ControlPath=").unwrap().to_string();
    assert_eq!(cps[1], "ControlPath=/tmp/user-wins-not");
    assert!(cp.starts_with("/tmp/"), "{cp}");
    assert!(cp.len() < 100, "unix socket path too long: {cp}");
    assert!(
        master
            .iter()
            .position(|a| a == "ServerAliveInterval=7")
            .unwrap()
            > master
                .iter()
                .position(|a| a == &format!("ControlPath={cp}"))
                .unwrap()
    );
    assert_eq!(master.last().unwrap(), dest, "{master:?}");

    // Channel: ControlMaster=no on the same path, dest then `bash`.
    assert!(!chan.contains(&"-N".to_string()), "{chan:?}");
    assert_eq!(
        value_after(chan, "-o", "ControlMaster="),
        ["ControlMaster=no"]
    );
    assert_eq!(
        value_after(chan, "-o", "ControlPath=")[0],
        format!("ControlPath={cp}")
    );
    assert_eq!(&chan[chan.len() - 2..], [dest, "bash"], "{chan:?}");

    // Exit: -O exit on the same path and dest.
    assert_eq!(value_after(exit, "-O", ""), ["exit"]);
    assert_eq!(
        value_after(exit, "-o", "ControlPath=")[0],
        format!("ControlPath={cp}")
    );
    assert_eq!(exit.last().unwrap(), dest);

    // The control directory is gone after close.
    let dir = Path::new(&cp).parent().unwrap();
    assert!(!dir.exists(), "{} still exists", dir.display());
}

#[tokio::test]
async fn master_failures_classify_into_limits() {
    let cases: &[(&str, Limit)] = &[
        (
            "bot@h: Permission denied (publickey,password).",
            Limit::AuthFailed,
        ),
        (
            "ssh: connect to host h port 22: Connection refused",
            Limit::ConnectRefused,
        ),
        (
            "ssh: connect to host h port 22: No route to host",
            Limit::ConnectRefused,
        ),
        (
            "ssh: connect to host h port 22: Connection timed out",
            Limit::ConnectRefused,
        ),
        (
            "ssh: Could not resolve hostname h: Name or service not known",
            Limit::ConnectRefused,
        ),
        (
            "kex_exchange_identification: read: Connection reset by peer\\nsecond line",
            Limit::Other("kex_exchange_identification: read: Connection reset by peer".into()),
        ),
        (
            "Warning: Permanently added 'h' (ED25519) to the list of known hosts.\\nHost key verification failed.",
            Limit::Other("Host key verification failed.".into()),
        ),
    ];
    for (stderr, want) in cases {
        let stub = Stub::new();
        let t = stub.transport().env("TP_STUB_MASTER_FAIL", stderr);
        let s = t.open("bot@h", &Principal::Root, 1).await;
        assert!(s.got.is_empty(), "{stderr}");
        assert_eq!(s.limit.as_ref(), Some(want), "{stderr}");
        let first = stderr.split("\\n").last().unwrap();
        let msg = t.message("bot@h").unwrap_or_default();
        assert!(msg.contains(first), "{stderr}: message {msg:?}");
        // No channel was tried after the master failed.
        assert_eq!(stub.argvs().len(), 1, "{stderr}: {:#?}", stub.argvs());
    }
}

#[tokio::test]
async fn sudo_refused_is_a_limit_with_its_message() {
    let stub = Stub::new();
    let t = stub.transport().env("TP_STUB_SUDO_FAIL", "1");
    let s = t.open("bot@h", &Principal::Root, 1).await;
    assert!(s.got.is_empty());
    assert_eq!(s.limit, Some(Limit::SudoRefused));
    let msg = t.message("bot@h").unwrap_or_default();
    assert!(msg.contains("sudo: a password is required"), "{msg:?}");
    assert_eq!(stub.sudo_log(), "-n true\n");
    t.close().await;
    assert_eq!(survivors(&stub.pids()).await, Vec::<u32>::new());
}

#[tokio::test]
async fn non_root_login_becomes_root_through_sudo() {
    let stub = Stub::new();
    let t = stub.transport();
    let mut s = t.open("bot@h", &Principal::Root, 1).await;
    assert!(s.limit.is_none(), "{:?} {:?}", s.limit, t.message("bot@h"));
    let mut sh = s.got.remove(0);
    assert_eq!(sh.user(), "root");
    assert_eq!(stub.sudo_log(), "-n true\n-n bash\n");
    // The shell is the sudo'd bash: fake id now says 0.
    assert_eq!(run(&mut sh, "id -u").await, (0, "0\n".to_string()));
    sh.close().await;
    t.close().await;
}

#[tokio::test]
async fn uid_zero_skips_sudo() {
    let stub = Stub::new();
    // sudo would refuse; it must not be asked.
    let t = stub
        .transport()
        .env("TP_STUB_UID", "0")
        .env("TP_STUB_SUDO_FAIL", "1");
    let mut s = t.open("root@h", &Principal::Root, 1).await;
    assert!(s.limit.is_none(), "{:?} {:?}", s.limit, t.message("root@h"));
    assert_eq!(s.got[0].user(), "root");
    assert_eq!(stub.sudo_log(), "");
    assert_eq!(run(&mut s.got[0], "echo hi").await, (0, "hi\n".to_string()));
    t.close().await;
}

#[tokio::test]
async fn hello_and_read_os_through_the_stub() {
    let stub = Stub::new();
    let t = stub.transport();
    let mut s = t.open("bot@h", &Principal::Root, 1).await;
    assert!(s.limit.is_none(), "{:?} {:?}", s.limit, t.message("bot@h"));
    let got = read_os(&mut s.got[0]).await.unwrap();
    // The stub runs the remote side locally: the answer is this machine's.
    let text = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .unwrap();
    assert_eq!(got, Os::parse(&text).unwrap());
    t.close().await;
}

#[tokio::test]
async fn user_principal_is_not_supported_yet() {
    let stub = Stub::new();
    let t = stub.transport();
    let s = t.open("bot@h", &Principal::User("alice".into()), 1).await;
    assert!(s.got.is_empty());
    assert!(
        matches!(&s.limit, Some(Limit::Other(m)) if m.contains("not supported")),
        "{:?}",
        s.limit
    );
}

#[tokio::test]
async fn close_ends_master_and_live_channels() {
    let stub = Stub::new();
    let t = stub.transport();
    // Keep the shell open: close must end its channel too.
    let s = t.open("bot@h", &Principal::Root, 1).await;
    assert_eq!(s.got.len(), 1, "{:?} {:?}", s.limit, t.message("bot@h"));
    let pids = stub.pids();
    assert_eq!(pids.len(), 2, "master and one channel: {pids:?}");
    assert!(pids.iter().all(|&p| alive(p)));
    t.close().await;
    assert_eq!(survivors(&pids).await, Vec::<u32>::new());
    drop(s);
}

#[tokio::test]
async fn drop_ends_master_and_live_channels() {
    let stub = Stub::new();
    let t = stub.transport();
    let s = t.open("bot@h", &Principal::Root, 1).await;
    assert_eq!(s.got.len(), 1, "{:?} {:?}", s.limit, t.message("bot@h"));
    let pids = stub.pids();
    assert_eq!(pids.len(), 2, "master and one channel: {pids:?}");
    let cp = value_after(&stub.argvs()[0], "-o", "ControlPath=")[0]
        .strip_prefix("ControlPath=")
        .unwrap()
        .to_string();
    drop(t);
    assert_eq!(survivors(&pids).await, Vec::<u32>::new());
    assert!(!Path::new(&cp).parent().unwrap().exists());
    drop(s);
}

#[tokio::test]
async fn second_open_reuses_the_connection() {
    let stub = Stub::new();
    let t = stub.transport().env("TP_STUB_UID", "0");
    let a = t.open("bot@h", &Principal::Root, 1).await;
    let b = t.open("bot@h", &Principal::Root, 1).await;
    assert_eq!((a.got.len(), b.got.len()), (1, 1));
    let masters = stub
        .argvs()
        .iter()
        .filter(|a| a.contains(&"-N".to_string()))
        .count();
    assert_eq!(masters, 1);
    t.close().await;
}
