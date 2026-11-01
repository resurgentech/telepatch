//! Live SSH: an `ubuntu:24.04` container running sshd on a localhost port,
//! with a throwaway key generated into a temp dir. User `tp` has NOPASSWD
//! sudo, user `nosudo` has none; both accept the key. Run with
//! `just test-live-ssh` (needs docker and the network for apt).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use telepatch::trace::Principal;
use telepatch::transport::ssh::SshTransport;
use telepatch::transport::{Limit, Transport, read_os};

const SETUP: &str = r#"set -e
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq openssh-server sudo >/dev/null
mkdir -p /run/sshd
for u in tp nosudo; do
  useradd -m -s /bin/bash "$u"
  usermod -p '*' "$u"
  install -d -m 700 -o "$u" -g "$u" "/home/$u/.ssh"
  printf '%s\n' "$TP_PUBKEY" > "/home/$u/.ssh/authorized_keys"
  chown "$u:$u" "/home/$u/.ssh/authorized_keys"
  chmod 600 "/home/$u/.ssh/authorized_keys"
done
useradd -m -s /bin/bash alice
useradd -m -s /usr/sbin/nologin carol
echo 'tp ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/tp
chmod 440 /etc/sudoers.d/tp
exec /usr/sbin/sshd -D -e $TP_SSHD_ARGS
"#;

fn keygen(path: &Path) {
    let st = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "tp-live", "-f"])
        .arg(path)
        .stdin(Stdio::null())
        .status()
        .expect("running ssh-keygen");
    assert!(st.success(), "ssh-keygen failed");
}

static CONTAINERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// `ssh_processes` sees every transport of this process, so the tests that
/// start a container and count processes run one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    SERIAL.lock().await
}

pub struct Sshd {
    pub name: String,
    pub port: u16,
    pub key: PathBuf,
    pub wrong_key: PathBuf,
    _keys: tempfile::TempDir,
}

impl Sshd {
    /// Start the container and wait until sshd listens. Panics on failure.
    pub fn start() -> Sshd {
        Sshd::start_with("")
    }

    /// Like [`Sshd::start`], with extra sshd arguments (`-o MaxSessions=1`).
    pub fn start_with(sshd_args: &str) -> Sshd {
        let keys = tempfile::tempdir().unwrap();
        let key = keys.path().join("id_tp");
        let wrong_key = keys.path().join("id_wrong");
        keygen(&key);
        keygen(&wrong_key);
        let pubkey = std::fs::read_to_string(key.with_extension("pub")).unwrap();
        let name = format!(
            "tp-test-sshd-{}-{}",
            std::process::id(),
            CONTAINERS.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        );
        let out = Command::new("docker")
            .args(["run", "-d", "--rm", "--name", &name, "-p", "127.0.0.1::22"])
            .arg("-e")
            .arg(format!("TP_PUBKEY={}", pubkey.trim()))
            .arg("-e")
            .arg(format!("TP_SSHD_ARGS={sshd_args}"))
            .args(["ubuntu:24.04", "bash", "-c", SETUP])
            .stdin(Stdio::null())
            .output()
            .expect("running docker");
        assert!(
            out.status.success(),
            "docker run: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let mut d = Sshd {
            name,
            port: 0,
            key,
            wrong_key,
            _keys: keys,
        };
        let end = Instant::now() + Duration::from_secs(600);
        loop {
            let logs = Command::new("docker")
                .args(["logs", &d.name])
                .stdin(Stdio::null())
                .output()
                .expect("docker logs");
            let text = String::from_utf8_lossy(&logs.stderr).into_owned()
                + &String::from_utf8_lossy(&logs.stdout);
            if text.contains("Server listening") {
                break;
            }
            assert!(
                logs.status.success() && Instant::now() < end,
                "sshd did not come up:\n{text}"
            );
            std::thread::sleep(Duration::from_millis(500));
        }
        let out = Command::new("docker")
            .args(["port", &d.name, "22/tcp"])
            .output()
            .expect("docker port");
        let text = String::from_utf8_lossy(&out.stdout);
        d.port = text
            .lines()
            .next()
            .and_then(|l| l.rsplit_once(':'))
            .and_then(|(_, p)| p.trim().parse().ok())
            .unwrap_or_else(|| panic!("docker port: {text:?}"));
        d
    }

    pub fn dest(&self, user: &str) -> String {
        format!("ssh://{user}@127.0.0.1:{}", self.port)
    }
}

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// A transport that trusts the throwaway host key and uses `key` only.
fn transport(key: &Path) -> SshTransport {
    SshTransport::new()
        .identity(key)
        .option("StrictHostKeyChecking=no")
        .option("UserKnownHostsFile=/dev/null")
        .option("ConnectTimeout=10")
}

/// Processes whose command line names this process's control directories.
fn ssh_processes() -> Vec<String> {
    let mark = format!("ControlPath=/tmp/tp-{}-", std::process::id());
    let mut out = vec![];
    for e in std::fs::read_dir("/proc").unwrap().flatten() {
        let Ok(cmd) = std::fs::read(e.path().join("cmdline")) else {
            continue;
        };
        let cmd = String::from_utf8_lossy(&cmd).replace('\0', " ");
        if cmd.contains(&mark) {
            out.push(format!("{}: {cmd}", e.file_name().to_string_lossy()));
        }
    }
    out
}

/// Run `cmd` as a raw op on `sh`; return (rc, output).
async fn run_raw_op(sh: &mut telepatch::pool::Shell, cmd: &str) -> (i32, String) {
    let os = crate::fixture("ubuntu2404");
    let h = telepatch::Host::new("t", os.clone(), std::rc::Rc::new(()));
    h.raw(cmd);
    let telepatch::Node::Op(op) = &h.snapshot().nodes[0] else {
        unreachable!()
    };
    sh.send_op(op, 0, &os).await.unwrap();
    let r = sh.wait(0).await.unwrap();
    (
        r.trailer
            .typed::<telepatch::trailer::ShellResult>()
            .unwrap()
            .rc,
        String::from_utf8(r.output).unwrap(),
    )
}

async fn none_left() {
    for _ in 0..100 {
        if ssh_processes().is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("ssh processes left: {:#?}", ssh_processes());
}

#[tokio::test]
#[ignore = "needs docker and the network"]
async fn ssh_root_shell_auth_and_sudo_against_sshd() {
    let _serial = serial().await;
    let d = Sshd::start();

    // tp: root shell, `os` equals the ubuntu2404 fixture.
    let t = transport(&d.key);
    let dest = d.dest("tp");
    let mut s = t.open(&dest, &Principal::Root, 1).await;
    assert!(s.limit.is_none(), "{:?}: {:?}", s.limit, t.message(&dest));
    let mut sh = s.got.remove(0);
    assert_eq!(sh.user(), "root");
    let os = read_os(&mut sh).await.unwrap();
    assert_eq!(os, crate::fixture("ubuntu2404"));
    println!("tp: root shell, os = {os:?}");
    assert!(
        !ssh_processes().is_empty(),
        "master and channel are running"
    );
    t.close().await;
    none_left().await;
    drop(sh);

    // Wrong key: AuthFailed.
    let t = transport(&d.wrong_key);
    let s = t.open(&dest, &Principal::Root, 1).await;
    assert!(s.got.is_empty());
    assert_eq!(s.limit, Some(Limit::AuthFailed), "{:?}", t.message(&dest));
    println!("wrong key: {:?}: {:?}", s.limit, t.message(&dest));

    // nosudo: SudoRefused, with sudo's own words kept.
    let t2 = transport(&d.key);
    let nd = d.dest("nosudo");
    let s = t2.open(&nd, &Principal::Root, 1).await;
    assert!(s.got.is_empty());
    assert_eq!(s.limit, Some(Limit::SudoRefused), "{:?}", t2.message(&nd));
    let msg = t2.message(&nd).unwrap();
    assert!(msg.contains("sudo"), "{msg}");
    println!("nosudo: {:?}: {msg:?}", s.limit);

    drop(t);
    drop(t2);
    none_left().await;
}

#[tokio::test]
#[ignore = "needs docker and the network"]
async fn ssh_max_sessions_opens_masters_and_user_shells() {
    let _serial = serial().await;
    let d = Sshd::start_with("-o MaxSessions=1");
    let t = transport(&d.key);
    let dest = d.dest("tp");

    // MaxSessions 1: three channels need three masters.
    let mut s = t.open(&dest, &Principal::Root, 3).await;
    assert_eq!(s.got.len(), 3, "{:?}: {:?}", s.limit, t.message(&dest));
    assert!(s.limit.is_none());
    let masters = || {
        ssh_processes()
            .iter()
            .filter(|p| p.contains("ControlMaster=yes"))
            .count()
    };
    assert_eq!(masters(), 3, "{:#?}", ssh_processes());
    println!("MaxSessions 1: want 3, got 3, masters {}", masters());
    for sh in s.got.iter_mut() {
        assert_eq!(sh.user(), "root");
        let (rc, out) = run_raw_op(sh, "id -un").await;
        assert_eq!((rc, out.as_str()), (0, "root\n"));
    }

    // alice: a User shell on a fourth connection (the three are full).
    let mut u = t.open(&dest, &Principal::User("alice".into()), 1).await;
    assert_eq!(u.got.len(), 1, "{:?}: {:?}", u.limit, t.message(&dest));
    let mut sh = u.got.remove(0);
    assert_eq!(sh.user(), "alice");
    let (rc, out) = run_raw_op(&mut sh, "id -un").await;
    assert_eq!((rc, out.as_str()), (0, "alice\n"));
    let (rc, out) = run_raw_op(&mut sh, "printf '%s\\n' \"$HOME\"").await;
    assert_eq!((rc, out.as_str()), (0, "/home/alice\n"));
    println!(
        "alice: id -un = alice, HOME = /home/alice, masters {}",
        masters()
    );

    // Unknown user and a user whose login shell refuses: SudoRefused.
    for who in ["nosuch", "carol"] {
        let r = t.open(&dest, &Principal::User(who.into()), 1).await;
        assert!(r.got.is_empty(), "{who}");
        assert_eq!(
            r.limit,
            Some(Limit::SudoRefused),
            "{who}: {:?}",
            t.message(&dest)
        );
        println!("{who}: {:?}: {:?}", r.limit, t.message(&dest));
    }

    t.close().await;
    drop(s);
    drop(sh);
    none_left().await;
}

#[tokio::test]
#[ignore = "needs ssh"]
async fn ssh_closed_port_is_connect_refused() {
    // Bind a port, then free it: nothing listens there.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("id");
    keygen(&key);
    let t = transport(&key);
    let dest = format!("ssh://tp@127.0.0.1:{port}");
    let s = t.open(&dest, &Principal::Root, 1).await;
    assert!(s.got.is_empty());
    assert_eq!(
        s.limit,
        Some(Limit::ConnectRefused),
        "{:?}",
        t.message(&dest)
    );
    println!("closed port: {:?}: {:?}", s.limit, t.message(&dest));
}
