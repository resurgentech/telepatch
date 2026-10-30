//! The shell pool: a persistent shell process you talk to over stdin/stdout.
//!
//! [`Shell::spawn`] runs any argv (`bash`, `sudo -n bash`, later
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

mod split;

use std::collections::{HashMap, VecDeque};
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
    Trailer(Trailer),
    Eof,
}

/// A persistent shell. Ops go in with [`Shell::send`], results come out of
/// [`Shell::wait`] or [`Shell::next_event`] (do not mix the two).
pub struct Shell {
    stdin: Option<ChildStdin>,
    rx: UnboundedReceiver<Raw>,
    child: Child,
    nonce: String,
    user: String,
    inflight: VecDeque<usize>,
    pending: VecDeque<Event>,
    partial: HashMap<usize, Vec<u8>>,
    done: HashMap<usize, Result<OpOutput, ShellError>>,
    dead: Option<String>,
}

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
            nonce,
            user: user.to_string(),
            inflight: VecDeque::new(),
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

    /// The next event, or `None` when no op is in flight.
    pub async fn next_event(&mut self) -> Option<Event> {
        loop {
            if let Some(e) = self.pending.pop_front() {
                return Some(e);
            }
            if self.inflight.is_empty() {
                return None;
            }
            let raw = self.rx.recv().await.unwrap_or(Raw::Eof);
            match raw {
                Raw::Bytes(b) => {
                    if let Some(&id) = self.inflight.front() {
                        return Some(Event::Output(id, b));
                    }
                }
                Raw::Trailer(t) => {
                    let Some(pos) = self.inflight.iter().position(|&i| i == t.id) else {
                        continue; // not ours; cannot attribute it
                    };
                    for _ in 0..pos {
                        let lost = self.inflight.pop_front().unwrap();
                        self.pending
                            .push_back(Event::Failed(lost, ShellError::TrailerLost));
                    }
                    self.inflight.pop_front();
                    self.pending.push_back(Event::Done(t.id, t));
                }
                Raw::Eof => {
                    let status = match self.child.try_wait() {
                        Ok(Some(s)) => format!("exited with {s}"),
                        _ => "closed its output".to_string(),
                    };
                    self.dead = Some(format!("shell {status}"));
                    for id in std::mem::take(&mut self.inflight) {
                        let e = self.died("eof");
                        self.pending.push_back(Event::Failed(id, e));
                    }
                }
            }
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

    /// Close stdin and wait for the process to end.
    pub async fn close(mut self) {
        drop(self.stdin.take());
        let _ = self.child.wait().await;
    }
}
