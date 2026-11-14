//! The shell pool: a persistent shell process you talk to over stdin/stdout.
//!
//! [`Shell::spawn`] runs any argv (`bash`, `sudo -n -H bash`, later
//! `docker exec -i ctr bash`, `ssh host bash`) and speaks the trailer
//! protocol of [`crate::trailer`] with it. Transports differ only in the
//! argv; everything else lives here.
//!
//! Runtime: tokio. The engine's futures are `!Send`, so a current-thread
//! runtime is expected, but nothing here needs a `LocalSet`: the only
//! spawned task is the stdout reader, which owns plain bytes and a channel.
//!
//! Protocol: right after spawn the shell gets `exec 2>&1` (stderr is merged
//! into the op stream) and a hello trailer whose arrival proves the shell
//! came up. Ops are written with [`Shell::send`] without waiting. Ops run
//! one at a time in write order, so bytes before a trailer belong to the
//! oldest op still in flight. A line shaped like a trailer but with another
//! nonce is plain output.
//!
//! Besides the trailer, an op that runs prints one start line,
//! `\n__tpS_<nonce> <id> <pid> <shell_pid>\n` (see [`crate::trailer`]), which
//! becomes [`Event::Started`] and is never output. A skipped op prints no
//! start line. Under `set -m` (the executor sets it) `pid` is the op's
//! process group, so a controller can signal the op from a second shell.
//!
//! A controller that stopped an op itself (the watchdog in
//! [`crate::executor`]) calls [`Shell::abandon`]: the op stays in flight
//! so later bytes are still attributed right, but nothing more is
//! reported for it (no output, no trailer, no `TrailerLost`), and an
//! abandoned op alone does not keep [`Shell::next_event`] waiting.

mod split;

use std::collections::{HashMap, HashSet, VecDeque};
use std::process::Stdio;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::facts::os::Os;
use crate::trace::OpNode;
use crate::trailer::{RenderError, Trailer, render_for};

pub use split::{Piece, Splitter};

const HELLO: usize = usize::MAX;

/// Why an op produced no result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellError {
    /// The shell process ended (or its pipe broke) before the op's trailer.
    Died { message: String },
    /// A later op's trailer arrived but this op's never did (its subshell
    /// was killed before the EXIT trap ran).
    TrailerLost,
    /// The op could not be rendered for this shell.
    Render(RenderError),
    /// `wait` for an id that is not in flight and has no stored result.
    UnknownOp(usize),
}

impl std::fmt::Display for ShellError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShellError::Died { message } => write!(f, "shell died: {message}"),
            ShellError::TrailerLost => write!(f, "op ended without a trailer"),
            ShellError::Render(e) => write!(f, "{e}"),
            ShellError::UnknownOp(id) => write!(f, "op {id} is not in flight"),
        }
    }
}

impl std::error::Error for ShellError {}

/// An op's collected result: its output and its parsed trailer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpOutput {
    pub output: Vec<u8>,
    pub trailer: Trailer,
}

/// One step of the stream, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The op's subshell has started: `pid` is its `$BASHPID`, which is
    /// its process group id under `set -m`, and `shell_pid` is the
    /// persistent shell's `$$` (to check identity before a kill). A guard
    /// skip or a halt skip never starts, so its `Done` has no `Started`.
    /// Seeing it also means every earlier op still in flight has ended
    /// without a trailer ([`ShellError::TrailerLost`] arrives first).
    Started {
        id: usize,
        pid: i32,
        shell_pid: i32,
    },
    Output(usize, Vec<u8>),
    Done(usize, Trailer),
    Failed(usize, ShellError),
}

/// Spawning a shell failed.
#[derive(Debug)]
pub enum SpawnError {
    /// The process could not be started at all.
    Spawn(std::io::Error),
    /// The process started but closed its output before the hello (sudo
    /// refused, ssh failed, ...). `stderr` is what it printed.
    NoHello { stderr: String },
}

enum Raw {
    Bytes(Vec<u8>),
    Started { id: usize, pid: i32, shell_pid: i32 },
    Trailer(Trailer),
    Eof,
}

/// A persistent shell. Ops go in with [`Shell::send`], results come out of
/// [`Shell::wait`] or [`Shell::next_event`] (do not mix the two).
pub struct Shell {
    stdin: Option<ChildStdin>,
    rx: UnboundedReceiver<Raw>,
    child: Child,
    /// The program the local child runs, for the close warning.
    argv0: String,
    /// How long [`Shell::close`] waits for the child before killing it.
    close_bound: std::time::Duration,
    nonce: String,
    user: String,
    inflight: VecDeque<usize>,
    /// Ops in `inflight` the controller gave up on ([`Shell::abandon`]).
    abandoned: HashSet<usize>,
    pending: VecDeque<Event>,
    partial: HashMap<usize, Vec<u8>>,
    done: HashMap<usize, Result<OpOutput, ShellError>>,
    dead: Option<String>,
}

/// How long [`Shell::close`] waits for the local child after `exit`.
pub const CLOSE_BOUND: std::time::Duration = std::time::Duration::from_secs(10);
/// How long it waits once more after killing the child.
const KILL_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

fn new_nonce() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut s = String::new();
    for _ in 0..2 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u32(std::process::id());
        s.push_str(&format!("{:016x}", h.finish()));
    }
    s
}

impl Shell {
    /// Spawn `argv` (with extra `env`) and complete the hello handshake.
    /// `user` is who the shell is (what [`render_for`] compares against).
    pub async fn spawn(
        argv: &[String],
        env: &[(String, String)],
        user: &str,
    ) -> Result<Shell, SpawnError> {
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(SpawnError::Spawn)?;
        let stdin = child.stdin.take();
        let mut stdout = child.stdout.take().expect("piped");
        let mut stderr = child.stderr.take().expect("piped");
        let err_task = tokio::spawn(async move {
            let mut s = String::new();
            let _ = (&mut stderr).take(8192).read_to_string(&mut s).await;
            s
        });
        let nonce = new_nonce();

        let (tx, rx) = unbounded_channel();
        let reader_nonce = nonce.clone();
        tokio::spawn(async move {
            let mut sp = Splitter::new(&reader_nonce);
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match stdout.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        for p in sp.push(&buf[..n]) {
                            let raw = match p {
                                Piece::Output(b) => Raw::Bytes(b),
                                Piece::Trailer(t) => Raw::Trailer(t),
                                Piece::Started { id, pid, shell_pid } => {
                                    Raw::Started { id, pid, shell_pid }
                                }
                            };
                            if tx.send(raw).is_err() {
                                return;
                            }
                        }
                    }
                }
            }
            if let Some(rest) = sp.finish() {
                let _ = tx.send(Raw::Bytes(rest));
            }
            let _ = tx.send(Raw::Eof);
        });

        let mut sh = Shell {
            stdin,
            rx,
            child,
            argv0: argv[0].clone(),
            close_bound: CLOSE_BOUND,
            nonce,
            user: user.to_string(),
            inflight: VecDeque::new(),
            abandoned: HashSet::new(),
            pending: VecDeque::new(),
            partial: HashMap::new(),
            done: HashMap::new(),
            dead: None,
        };
        let hello = format!("exec 2>&1\nprintf '\\n__tp_{} {HELLO} {{}}\\n'\n", sh.nonce);
        let _ = sh.send(HELLO, &hello).await;
        match sh.wait(HELLO).await {
            Ok(_) => Ok(sh),
            Err(_) => {
                let stderr = tokio::time::timeout(std::time::Duration::from_secs(2), err_task)
                    .await
                    .ok()
                    .and_then(|r| r.ok())
                    .unwrap_or_default();
                Err(SpawnError::NoHello { stderr })
            }
        }
    }

    /// Replace the shell process with `command` (written as `exec
    /// <command>`, so it reads the rest of stdin) and redo the hello
    /// handshake; on success the shell is `user`. Used to turn a login
    /// shell into root with `sudo -n -H bash`. No op may be in flight. On
    /// failure the shell is dead.
    pub async fn exec(&mut self, command: &str, user: &str) -> Result<(), ShellError> {
        let script = format!(
            "exec {command}\nexec 2>&1\nprintf '\\n__tp_{} {HELLO} {{}}\\n'\n",
            self.nonce
        );
        self.send(HELLO, &script).await?;
        self.wait(HELLO).await?;
        self.user = user.to_string();
        Ok(())
    }

    /// The nonce this shell's trailers carry.
    pub fn nonce(&self) -> &str {
        &self.nonce
    }

    /// Who this shell is ("root" for a root shell).
    pub fn user(&self) -> &str {
        &self.user
    }

    /// Render `op` for this shell ([`render_for`]) and send it as `id`.
    pub async fn send_op(&mut self, op: &OpNode, id: usize, os: &Os) -> Result<(), ShellError> {
        let script = render_for(op, id, &self.nonce, os, &self.user).map_err(ShellError::Render)?;
        self.send(id, &script).await
    }

    /// Write `script` (which must end in a trailer for `id`) without
    /// waiting for anything.
    pub async fn send(&mut self, id: usize, script: &str) -> Result<(), ShellError> {
        // Sending an id again after the controller abandoned it (a retry
        // of a killed op): the stale entry goes, or the new attempt's
        // trailer would be taken for the old one's and swallowed.
        if self.abandoned.remove(&id)
            && let Some(pos) = self.inflight.iter().position(|&i| i == id)
        {
            self.inflight.remove(pos);
        }
        self.inflight.push_back(id);
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(self.died("stdin closed"));
        };
        let r = async {
            stdin.write_all(script.as_bytes()).await?;
            stdin.flush().await
        }
        .await;
        r.map_err(|e| self.died(&e.to_string()))
    }

    fn died(&self, why: &str) -> ShellError {
        ShellError::Died {
            message: self.dead.clone().unwrap_or_else(|| why.to_string()),
        }
    }

    /// Give up on op `id` (the controller stopped it and will not get its
    /// trailer, or does not care): it is no longer reported. Events already
    /// queued for it are dropped; its later output, its trailer, a
    /// `TrailerLost` for it and a shell death are swallowed. A no-op for an
    /// id not in flight.
    pub fn abandon(&mut self, id: usize) {
        if !self.inflight.contains(&id) {
            return;
        }
        self.abandoned.insert(id);
        self.pending.retain(|e| match e {
            Event::Started { id: i, .. } | Event::Output(i, _) => *i != id,
            Event::Done(i, _) | Event::Failed(i, _) => *i != id,
        });
    }

    /// End this shell now, without waiting for anything: close stdin, kill
    /// the process, stop reading. Ops still in flight end as
    /// [`ShellError::Died`] (abandoned ones are not reported). Anything the
    /// shell started stays running; the caller knows no PID to stop.
    pub fn abort(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.start_kill();
        self.rx.close();
    }

    /// The next event, or `None` when no op is in flight (an abandoned op
    /// does not count).
    pub async fn next_event(&mut self) -> Option<Event> {
        loop {
            if let Some(e) = self.pending.pop_front() {
                return Some(e);
            }
            if self.inflight.iter().all(|i| self.abandoned.contains(i)) {
                return None;
            }
            let raw = self.rx.recv().await.unwrap_or(Raw::Eof);
            match raw {
                Raw::Bytes(b) => {
                    if let Some(&id) = self.inflight.front()
                        && !self.abandoned.contains(&id)
                    {
                        return Some(Event::Output(id, b));
                    }
                }
                Raw::Started { id, pid, shell_pid } => {
                    let Some(pos) = self.inflight.iter().position(|&i| i == id) else {
                        continue;
                    };
                    // Ops run one at a time: an earlier op still in flight
                    // ended without printing its trailer (it was killed).
                    for _ in 0..pos {
                        let lost = self.inflight.pop_front().unwrap();
                        self.lose(lost);
                    }
                    self.pending
                        .push_back(Event::Started { id, pid, shell_pid });
                }
                Raw::Trailer(t) => {
                    let Some(pos) = self.inflight.iter().position(|&i| i == t.id) else {
                        continue; // not ours; cannot attribute it
                    };
                    for _ in 0..pos {
                        let lost = self.inflight.pop_front().unwrap();
                        self.lose(lost);
                    }
                    self.inflight.pop_front();
                    if !self.abandoned.remove(&t.id) {
                        self.pending.push_back(Event::Done(t.id, t));
                    }
                }
                Raw::Eof => {
                    let status = match self.child.try_wait() {
                        Ok(Some(s)) => format!("exited with {s}"),
                        _ => "closed its output".to_string(),
                    };
                    self.dead = Some(format!("shell {status}"));
                    for id in std::mem::take(&mut self.inflight) {
                        if self.abandoned.remove(&id) {
                            continue;
                        }
                        let e = self.died("eof");
                        self.pending.push_back(Event::Failed(id, e));
                    }
                }
            }
        }
    }

    /// `id` left `inflight` without a trailer: report it, unless the
    /// controller abandoned it.
    fn lose(&mut self, id: usize) {
        if !self.abandoned.remove(&id) {
            self.pending
                .push_back(Event::Failed(id, ShellError::TrailerLost));
        }
    }

    /// Collect the result of op `id`, pumping the stream until it is done.
    /// Results of other ops seen on the way are kept for their own `wait`.
    pub async fn wait(&mut self, id: usize) -> Result<OpOutput, ShellError> {
        loop {
            if let Some(r) = self.done.remove(&id) {
                return r;
            }
            if !self.inflight.contains(&id) && self.pending.is_empty() {
                return Err(ShellError::UnknownOp(id));
            }
            match self.next_event().await {
                Some(Event::Started { .. }) => {}
                Some(Event::Output(i, b)) => self.partial.entry(i).or_default().extend(b),
                Some(Event::Done(i, trailer)) => {
                    let output = self.partial.remove(&i).unwrap_or_default();
                    self.done.insert(i, Ok(OpOutput { output, trailer }));
                }
                Some(Event::Failed(i, e)) => {
                    self.partial.remove(&i);
                    self.done.insert(i, Err(e));
                }
                None => return Err(ShellError::UnknownOp(id)),
            }
        }
    }

    /// The program the local child runs (`bash`, `ssh`, `docker`, ...).
    pub fn argv0(&self) -> &str {
        &self.argv0
    }

    /// Change how long [`Shell::close`] waits before it kills the local
    /// child. Tests only; the default is [`CLOSE_BOUND`].
    pub fn set_close_bound(&mut self, bound: std::time::Duration) {
        self.close_bound = bound;
    }

    /// End the shell as a person at a keyboard would: type `exit`, close
    /// stdin, wait. `exit` ends the shell (and the remote bash behind `ssh`
    /// or `docker exec`) even when the EOF is lost on the way. The wait is
    /// bounded: past the close bound the local child is killed, and a
    /// further [`KILL_GRACE`] is all it gets. Returns `true` when the bound
    /// was hit. Call it with no op in flight.
    pub async fn close(mut self) -> bool {
        let mut stdin = self.stdin.take();
        let polite = async {
            if let Some(w) = stdin.as_mut() {
                let _ = w.write_all(b"exit\n").await;
                let _ = w.flush().await;
            }
            drop(stdin.take());
            let _ = self.child.wait().await;
        };
        if tokio::time::timeout(self.close_bound, polite).await.is_ok() {
            return false;
        }
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(KILL_GRACE, self.child.wait()).await;
        true
    }
}
