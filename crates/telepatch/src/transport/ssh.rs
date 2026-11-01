//! The SSH transport: the user's own `ssh` binary, no agent, nothing pushed.
//!
//! A host's connection is one master, `ssh -N -o ControlMaster=yes
//! -o ControlPath=<dir>/<n> … <dest>`, kept as a child for the life of the
//! transport. Each shell is a channel on it, `ssh -o ControlMaster=no
//! -o ControlPath=<dir>/<n> … <dest> bash`, speaking the trailer protocol of
//! [`crate::pool`]. `<dir>` is a private directory under `/tmp`, made on the
//! first `open` and removed by [`SshTransport::close`] or on drop, so the
//! socket path stays well under the unix limit.
//!
//! Telepatch's own `-o` come first on every command line, and ssh takes the
//! first value it sees for an option, so they beat both the user's extra
//! options and `~/.ssh/config` for `BatchMode`, `ControlMaster`,
//! `ControlPersist` and `ControlPath` (and, on channels, `ProxyCommand`:
//! see `spawn_on`). Everything else (keys, agents,
//! `ProxyJump`, ports, users) is the user's config (goal 3, question 3).
//!
//! `open(host, Root, n)`: each channel's hello proves the connection; then
//! `id -u`; if not 0, `sudo -n true` (refusal is [`Limit::SudoRefused`]),
//! then `exec sudo -n bash` and a second hello. The ssh-side text behind
//! the last [`Limit`] for a host is kept for the report:
//! [`SshTransport::message`].
//!
//! `open(host, User(u), 1)`: a channel whose command is `sudo -n -iu <u>
//! bash`; the shell's `user()` is `u`. No hello means sudo (or `u`'s login
//! shell, as with `nologin`) refused: [`Limit::SudoRefused`] with what it
//! printed.
//!
//! Several shells: `open` fills `want` channels on the host's newest
//! connection. A channel the server refuses for `MaxSessions` (the mux
//! client prints "session request failed" or "open failed:
//! administratively prohibited") moves on to one more master connection,
//! and so on; only when a new connection cannot take a channel either does
//! `open` stop short with [`Limit::MaxSessions`]. Connections are kept per
//! host and reused by later `open` calls.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

use super::{Limit, Shells, Transport};
use crate::pool::{Shell, SpawnError};
use crate::trace::Principal;

/// How long a master may take to authenticate before the transport gives
/// up on it. ssh's own `ConnectTimeout` normally ends it sooner.
const READY_TIMEOUT: Duration = Duration::from_secs(120);

/// How long `close` waits for `-O exit` and for the master to end before
/// killing it.
const EXIT_TIMEOUT: Duration = Duration::from_secs(5);

const NO_HELLO: &str = "ssh channel closed before the hello";

static DIRS: AtomicUsize = AtomicUsize::new(0);

/// One master connection to a host.
#[derive(Debug)]
struct Conn {
    dest: String,
    socket: PathBuf,
    master: Child,
}

#[derive(Debug)]
pub struct SshTransport {
    ssh: String,
    identity: Option<PathBuf>,
    options: Vec<String>,
    env: Vec<(String, String)>,
    dir: RefCell<Option<PathBuf>>,
    next_socket: RefCell<usize>,
    conns: RefCell<HashMap<String, Vec<Conn>>>,
    messages: RefCell<HashMap<String, String>>,
}

impl Default for SshTransport {
    fn default() -> Self {
        SshTransport::new()
    }
}

/// The text a failed ssh printed, minus the `Warning:` lines ssh adds
/// before an error (known-hosts notes and the like).
fn meaningful_line(stderr: &str) -> Option<&str> {
    let mut lines = stderr.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.clone().next();
    lines.find(|l| !l.starts_with("Warning:")).or(first)
}

/// Whether ssh's stderr says the server refused one more session on the
/// connection (`MaxSessions`): what the mux client prints when the master
/// cannot open the channel.
fn is_max_sessions(stderr: &str) -> bool {
    const REFUSED: [&str; 2] = ["session request failed", "administratively prohibited"];
    REFUSED.iter().any(|p| stderr.contains(p))
}

/// Classify what a master (or a channel) printed before it exited.
fn classify(stderr: &str, fallback: &str) -> Limit {
    const CONNECT: [&str; 4] = [
        "Connection refused",
        "No route to host",
        "timed out",
        "Could not resolve",
    ];
    if stderr.contains("Permission denied") {
        Limit::AuthFailed
    } else if is_max_sessions(stderr) {
        Limit::MaxSessions
    } else if CONNECT.iter().any(|p| stderr.contains(p)) {
        Limit::ConnectRefused
    } else {
        Limit::Other(meaningful_line(stderr).unwrap_or(fallback).to_string())
    }
}

/// Run `cmd` as a raw op on `sh`; return (rc, output with stderr merged).
async fn run_raw(sh: &mut Shell, cmd: &str) -> Result<(i32, String), String> {
    use crate::facts::os::Os;
    use crate::trace::Node;
    use crate::trailer::ShellResult;
    let placeholder = Os::parse("ID=debian\n").map_err(|e| e.to_string())?;
    let h = crate::Host::new("ssh-probe", placeholder.clone(), std::rc::Rc::new(()));
    h.raw(cmd);
    let snap = h.snapshot();
    let Some(Node::Op(op)) = snap.nodes.first() else {
        return Err("probe op was not recorded".into());
    };
    sh.send_op(op, 0, &placeholder)
        .await
        .map_err(|e| format!("{cmd}: {e}"))?;
    let out = sh.wait(0).await.map_err(|e| format!("{cmd}: {e}"))?;
    let rc = out
        .trailer
        .typed::<ShellResult>()
        .map_err(|e| format!("{cmd}: {e}"))?
        .rc;
    Ok((rc, String::from_utf8_lossy(&out.output).into_owned()))
}

/// Why one root channel did not open: the Limit and the ssh-side text.
type Refusal = (Limit, String);

impl SshTransport {
    pub fn new() -> SshTransport {
        SshTransport {
            ssh: "ssh".into(),
            identity: None,
            options: Vec::new(),
            env: Vec::new(),
            dir: RefCell::new(None),
            next_socket: RefCell::new(0),
            conns: RefCell::new(HashMap::new()),
            messages: RefCell::new(HashMap::new()),
        }
    }

    /// The ssh command to run (default `ssh`).
    pub fn ssh(mut self, cmd: &str) -> Self {
        self.ssh = cmd.to_string();
        self
    }

    /// Authenticate with this key only: `-i <path> -o IdentitiesOnly=yes`.
    /// Without it the user's ssh config and agent pick the key.
    pub fn identity(mut self, path: impl AsRef<Path>) -> Self {
        self.identity = Some(path.as_ref().to_path_buf());
        self
    }

    /// An extra `-o K=V` for every ssh command. Telepatch's own options
    /// (`BatchMode`, `ControlMaster`, `ControlPersist`, `ControlPath`) come
    /// first and win.
    pub fn option(mut self, kv: &str) -> Self {
        self.options.push(kv.to_string());
        self
    }

    /// Extra environment for the local ssh processes (tests).
    pub fn env(mut self, k: &str, v: &str) -> Self {
        self.env.push((k.to_string(), v.to_string()));
        self
    }

    /// The ssh-side text behind the last [`Limit`] `open` returned for
    /// `host` (ssh's stderr, or what `sudo -n true` printed); `None` after
    /// an open that got everything it asked for.
    pub fn message(&self, host: &str) -> Option<String> {
        self.messages.borrow().get(host).cloned()
    }

    /// The argv after the ssh command: telepatch's options, `role`, the
    /// control socket, the key, the user's options, then `dest`.
    fn args(&self, role: &[&str], socket: &Path, dest: &str) -> Vec<String> {
        let mut a: Vec<String> = vec!["-T".into(), "-o".into(), "BatchMode=yes".into()];
        a.extend(role.iter().map(|s| s.to_string()));
        a.push("-o".into());
        a.push(format!("ControlPath={}", socket.display()));
        if let Some(k) = &self.identity {
            a.push("-i".into());
            a.push(k.display().to_string());
            a.push("-o".into());
            a.push("IdentitiesOnly=yes".into());
        }
        for o in &self.options {
            a.push("-o".into());
            a.push(o.clone());
        }
        a.push(dest.to_string());
        a
    }

    /// The private control directory, made on first use.
    fn control_dir(&self) -> Result<PathBuf, String> {
        if let Some(d) = self.dir.borrow().as_ref() {
            return Ok(d.clone());
        }
        use std::os::unix::fs::DirBuilderExt;
        let mut last = None;
        for _ in 0..100 {
            let d = PathBuf::from(format!(
                "/tmp/tp-{}-{}",
                std::process::id(),
                DIRS.fetch_add(1, Ordering::SeqCst)
            ));
            match std::fs::DirBuilder::new().mode(0o700).create(&d) {
                Ok(()) => {
                    *self.dir.borrow_mut() = Some(d.clone());
                    return Ok(d);
                }
                Err(e) => last = Some(e),
            }
        }
        Err(format!(
            "making an ssh control directory under /tmp: {}",
            last.map(|e| e.to_string()).unwrap_or_default()
        ))
    }

    /// The socket of the newest live master for `dest`; starts one if there
    /// is none, or always when `fresh`.
    async fn connection(&self, dest: &str, fresh: bool) -> Result<PathBuf, Refusal> {
        {
            let mut conns = self.conns.borrow_mut();
            if let Some(list) = conns.get_mut(dest) {
                // A master that has ended (or lost its socket) is of no use;
                // forget it, so a channel never silently connects directly.
                list.retain_mut(|c| matches!(c.master.try_wait(), Ok(None)) && c.socket.exists());
                if let (Some(c), false) = (list.last(), fresh) {
                    return Ok(c.socket.clone());
                }
            }
        }
        let dir = self
            .control_dir()
            .map_err(|e| (Limit::Other(e.clone()), e))?;
        let socket = {
            let mut n = self.next_socket.borrow_mut();
            *n += 1;
            dir.join(n.to_string())
        };
        let role = ["-N", "-o", "ControlMaster=yes", "-o", "ControlPersist=no"];
        let mut master = Command::new(&self.ssh)
            .args(self.args(&role, &socket, dest))
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                let m = format!("spawning {}: {e}", self.ssh);
                (Limit::Other(m.clone()), m)
            })?;
        let mut stderr = master.stderr.take().expect("piped");
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = buf.clone();
        let reader = tokio::spawn(async move {
            let mut chunk = [0u8; 4096];
            while let Ok(n) = stderr.read(&mut chunk).await {
                if n == 0 {
                    break;
                }
                let mut b = sink.lock().unwrap();
                if b.len() < 16 * 1024 {
                    b.extend_from_slice(&chunk[..n]);
                }
            }
        });

        let deadline = Instant::now() + READY_TIMEOUT;
        let failed = loop {
            if socket.exists() {
                break None;
            }
            match master.try_wait() {
                Ok(Some(status)) => break Some(format!("ssh exited with {status}")),
                Err(e) => break Some(format!("waiting for ssh: {e}")),
                Ok(None) => {}
            }
            if Instant::now() > deadline {
                let _ = master.kill().await;
                break Some(format!(
                    "ssh master not ready after {}s",
                    READY_TIMEOUT.as_secs()
                ));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        if let Some(fallback) = failed {
            let _ = tokio::time::timeout(Duration::from_secs(2), reader).await;
            let text = String::from_utf8_lossy(&buf.lock().unwrap())
                .replace("\r\n", "\n")
                .trim()
                .to_string();
            let limit = classify(&text, &fallback);
            let msg = if text.is_empty() { fallback } else { text };
            return Err((limit, msg));
        }
        self.conns
            .borrow_mut()
            .entry(dest.to_string())
            .or_default()
            .push(Conn {
                dest: dest.to_string(),
                socket: socket.clone(),
                master,
            });
        Ok(socket)
    }

    /// One channel on the connection at `socket` running `remote`; its
    /// `user()` is `user`. A channel that never says hello is a refusal:
    /// `refused` is the Limit unless ssh reports MaxSessions.
    async fn spawn_on(
        &self,
        dest: &str,
        socket: &Path,
        remote: &str,
        user: &str,
        refused: Option<Limit>,
    ) -> Result<Shell, Refusal> {
        let mut argv = vec![self.ssh.clone()];
        // `ProxyCommand=false` keeps a refused channel from falling back to
        // a direct connection: when the mux request fails (MaxSessions) ssh
        // goes on to connect on its own, which would hand back a shell on a
        // connection telepatch does not track. The mux client never uses a
        // proxy, so a working channel is unaffected; the user's own
        // ProxyCommand/ProxyJump lose to this first value.
        let role = ["-o", "ControlMaster=no", "-o", "ProxyCommand=false"];
        argv.extend(self.args(&role, socket, dest));
        argv.push(remote.into());
        match Shell::spawn(&argv, &self.env, user).await {
            Ok(sh) => Ok(sh),
            Err(SpawnError::NoHello { stderr }) => {
                let text = stderr.replace("\r\n", "\n").trim().to_string();
                let limit = if is_max_sessions(&text) {
                    Limit::MaxSessions
                } else if let Some(l) = refused {
                    l
                } else {
                    classify(&text, NO_HELLO)
                };
                let text = if text.is_empty() {
                    NO_HELLO.to_string()
                } else {
                    text
                };
                Err((limit, text))
            }
            Err(SpawnError::Spawn(e)) => {
                let m = format!("spawning {}: {e}", self.ssh);
                Err((Limit::Other(m.clone()), m))
            }
        }
    }

    /// One channel on the connection at `socket`, turned into a root shell.
    async fn root_channel(&self, dest: &str, socket: &Path) -> Result<Shell, Refusal> {
        let mut sh = self.spawn_on(dest, socket, "bash", "root", None).await?;
        let other = |m: String| (Limit::Other(m.clone()), m);
        let (rc, out) = run_raw(&mut sh, "id -u").await.map_err(other)?;
        let uid: u32 = match (rc, out.trim().parse()) {
            (0, Ok(u)) => u,
            _ => {
                sh.close().await;
                return Err(other(format!("id -u: rc {rc}: {}", out.trim())));
            }
        };
        if uid != 0 {
            let (rc, out) = run_raw(&mut sh, "sudo -n true").await.map_err(other)?;
            if rc != 0 {
                sh.close().await;
                let text = out.trim().to_string();
                let text = if text.is_empty() {
                    format!("sudo -n true exited {rc}")
                } else {
                    text
                };
                return Err((Limit::SudoRefused, text));
            }
            if let Err(e) = sh.exec("sudo -n bash", "root").await {
                return Err((Limit::SudoRefused, format!("exec sudo -n bash: {e}")));
            }
        }
        Ok(sh)
    }

    /// One channel on the connection at `socket` running `sudo -n -iu <u>
    /// bash`.
    async fn user_channel(&self, dest: &str, socket: &Path, u: &str) -> Result<Shell, Refusal> {
        let remote = format!("sudo -n -iu {} bash", crate::sh::quote(u));
        self.spawn_on(dest, socket, &remote, u, Some(Limit::SudoRefused))
            .await
            .map_err(|(l, m)| match l {
                // A login shell like nologin says its piece on stdout, which
                // the hello wait does not keep: name the likely causes.
                Limit::SudoRefused if m == NO_HELLO => (
                    l,
                    format!(
                        "sudo -n -iu {u} bash printed nothing on stderr and closed \
                         (no sudo rights, or a login shell that refuses, such as nologin)"
                    ),
                ),
                l => (l, m),
            })
    }

    async fn channel(&self, dest: &str, socket: &Path, who: &Principal) -> Result<Shell, Refusal> {
        match who {
            Principal::Root => self.root_channel(dest, socket).await,
            Principal::User(u) => self.user_channel(dest, socket, u).await,
        }
    }

    /// End every channel and master (`ssh -O exit`, then wait, then kill),
    /// and remove the control directory. The transport can open again
    /// afterwards.
    pub async fn close(&self) {
        let conns: Vec<Conn> = self
            .conns
            .borrow_mut()
            .drain()
            .flat_map(|(_, v)| v)
            .collect();
        for mut c in conns {
            let exit = Command::new(&self.ssh)
                .args(self.args(&["-O", "exit"], &c.socket, &c.dest))
                .envs(self.env.iter().map(|(k, v)| (k, v)))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .status();
            let _ = tokio::time::timeout(EXIT_TIMEOUT, exit).await;
            if tokio::time::timeout(EXIT_TIMEOUT, c.master.wait())
                .await
                .is_err()
            {
                let _ = c.master.kill().await;
            }
        }
        if let Some(d) = self.dir.borrow_mut().take() {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

impl Transport for SshTransport {
    async fn open(&self, host: &str, principal: &Principal, want: usize) -> Shells {
        let mut got = Vec::new();
        let mut limit = None;
        let mut message = None;
        while got.len() < want {
            let step: Result<Shell, Refusal> = async {
                let socket = self.connection(host, false).await?;
                match self.channel(host, &socket, principal).await {
                    Err((Limit::MaxSessions, _)) => {
                        // The connection is full: one more master. If that
                        // cannot be had or takes no channel either, the
                        // host's limit is MaxSessions.
                        let socket = self
                            .connection(host, true)
                            .await
                            .map_err(|(_, m)| (Limit::MaxSessions, m))?;
                        self.channel(host, &socket, principal).await
                    }
                    r => r,
                }
            }
            .await;
            match step {
                Ok(sh) => got.push(sh),
                Err((l, msg)) => {
                    message = Some(msg);
                    limit = Some(l);
                    break;
                }
            }
        }
        match message {
            Some(m) => {
                self.messages.borrow_mut().insert(host.into(), m);
            }
            None => {
                self.messages.borrow_mut().remove(host);
            }
        }
        Shells { got, want, limit }
    }
}

impl Drop for SshTransport {
    /// The synchronous version of [`SshTransport::close`]: `-O exit` each
    /// master (bounded wait), then kill it, then remove the directory.
    fn drop(&mut self) {
        let conns = std::mem::take(self.conns.get_mut());
        for (_, list) in conns {
            for mut c in list {
                let exit = std::process::Command::new(&self.ssh)
                    .args(self.args(&["-O", "exit"], &c.socket, &c.dest))
                    .envs(self.env.iter().map(|(k, v)| (k, v)))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
                if let Ok(mut p) = exit {
                    let end = Instant::now() + EXIT_TIMEOUT;
                    while matches!(p.try_wait(), Ok(None)) && Instant::now() < end {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    let _ = p.kill();
                    let _ = p.wait();
                }
                let _ = c.master.start_kill();
                let _ = c.master.try_wait();
            }
        }
        if let Some(d) = self.dir.get_mut().take() {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_by_stderr() {
        let f = "fallback";
        assert_eq!(
            classify("u@h: Permission denied (publickey).", f),
            Limit::AuthFailed
        );
        for s in [
            "ssh: connect to host h port 22: Connection refused",
            "ssh: connect to host h port 22: No route to host",
            "ssh: connect to host h port 22: Connection timed out",
            "ssh: Could not resolve hostname h: Name or service not known",
        ] {
            assert_eq!(classify(s, f), Limit::ConnectRefused, "{s}");
        }
        assert_eq!(
            classify("Warning: added\nHost key verification failed.\n", f),
            Limit::Other("Host key verification failed.".into())
        );
        assert_eq!(
            classify("Warning: only\n", f),
            Limit::Other("Warning: only".into())
        );
        assert_eq!(classify("", f), Limit::Other("fallback".into()));
        for s in [
            "mux_client_request_session: session request failed: Session open refused by peer",
            "channel 2: open failed: administratively prohibited: open failed",
        ] {
            assert_eq!(classify(s, f), Limit::MaxSessions, "{s}");
        }
    }
}
