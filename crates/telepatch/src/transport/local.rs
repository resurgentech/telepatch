//! The local transport: spawn `bash` on this machine, via `sudo -n -H bash`
//! unless already root. One shell, no PTY.
//!
//! `Principal::User(u)` (T120) is `u`'s login shell: for the calling user, a
//! plain shell turned into `env -i HOME=.. USER=.. LOGNAME=.. SHELL=..
//! PATH=.. bash -l` with no sudo (as root's login shell is); for another
//! user, `<sudo> -n -iu u bash`, as over ssh, from root or not. A refusal
//! (no sudo rights, no sudo on the machine, a login shell such as nologin)
//! is [`Limit::SudoRefused`], its words in [`LocalTransport::message`].

use std::cell::RefCell;
use std::rc::Rc;

use tokio::process::Command;

use super::{Limit, Shells, Transport};
use crate::pool::{Shell, SpawnError};
use crate::trace::Principal;

#[derive(Debug, Clone)]
pub struct LocalTransport {
    uid: Option<u32>,
    user: Option<String>,
    refusal: Rc<RefCell<Option<String>>>,
    sudo: String,
    env: Vec<(String, String)>,
}

impl Default for LocalTransport {
    fn default() -> Self {
        LocalTransport::new()
    }
}

impl LocalTransport {
    pub fn new() -> LocalTransport {
        LocalTransport {
            uid: None,
            user: None,
            refusal: Rc::default(),
            sudo: "sudo".into(),
            env: Vec::new(),
        }
    }

    /// Use this uid instead of asking `id -u` (tests).
    pub fn uid(mut self, uid: u32) -> Self {
        self.uid = Some(uid);
        self
    }

    /// Use this as the calling user's name instead of asking `id -un`
    /// (tests). It decides whether `as_user(u)` is the caller (a login
    /// shell with no sudo) or another user (`sudo -n -iu u bash`).
    pub fn user(mut self, name: &str) -> Self {
        self.user = Some(name.to_string());
        self
    }

    /// Why the last `Principal::User` open was refused, in words (the text
    /// the host is blocked with); `None` after an open that worked.
    pub fn message(&self) -> Option<String> {
        self.refusal.borrow().clone()
    }

    /// The sudo command to run (default `sudo`); called as `<sudo> -n -H bash`.
    pub fn sudo(mut self, cmd: &str) -> Self {
        self.sudo = cmd.to_string();
        self
    }

    /// Extra environment for the spawned process.
    pub fn env(mut self, k: &str, v: &str) -> Self {
        self.env.push((k.to_string(), v.to_string()));
        self
    }

    /// The environment of a test [`Sandbox`](crate::testing::Sandbox).
    pub fn sandboxed(mut self, sb: &crate::testing::Sandbox) -> Self {
        self.env.extend(sb.env());
        self
    }

    /// [`LocalTransport::sandboxed`] for a sandbox rooted at a directory the
    /// test already owns ([`Sandbox::at`](crate::testing::Sandbox::at)).
    pub fn sandboxed_at(self, dir: &std::path::Path) -> Self {
        self.sandboxed(&crate::testing::Sandbox::at(dir))
    }

    async fn current_uid(&self) -> Result<u32, String> {
        if let Some(u) = self.uid {
            return Ok(u);
        }
        let out = Command::new("id")
            .arg("-u")
            .output()
            .await
            .map_err(|e| format!("running id -u: {e}"))?;
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .map_err(|_| "id -u printed no number".to_string())
    }

    /// The calling user's name: the [`LocalTransport::user`] seam, else `id -un`.
    async fn current_user(&self) -> Result<String, String> {
        if let Some(u) = &self.user {
            return Ok(u.clone());
        }
        let out = Command::new("id")
            .arg("-un")
            .output()
            .await
            .map_err(|e| format!("running id -un: {e}"))?;
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if name.is_empty() {
            return Err("id -un printed no name".to_string());
        }
        Ok(name)
    }

    /// `u`'s login shell (T120). When `u` is the calling user there is no
    /// sudo to make: a plain shell of the caller, turned into `u`'s login
    /// shell by [`crate::executor::start_login`] (`env -i .. bash -l`, as
    /// root's login shell is). Any other user is `<sudo> -n -iu u bash`,
    /// as over ssh, whether the caller is root or not; a refusal is
    /// [`Limit::SudoRefused`] and [`LocalTransport::message`] has its
    /// words. A machine with no `sudo` is the same refusal.
    async fn open_user(&self, u: &str, want: usize) -> Shells {
        *self.refusal.borrow_mut() = None;
        let caller = match self.current_user().await {
            Ok(n) => n,
            Err(e) => return none(want, Limit::Other(e)),
        };
        let one = |sh: Shell| Shells {
            got: vec![sh],
            want,
            limit: None,
        };
        if caller == u {
            let argv = vec!["bash".to_string()];
            return match Shell::spawn(&argv, &self.env, u).await {
                Ok(sh) => match crate::executor::start_login(sh, u).await {
                    Ok((sh, _home)) => one(sh),
                    Err(e) => none(want, Limit::Other(e)),
                },
                Err(SpawnError::NoHello { stderr }) => none(
                    want,
                    Limit::Other(format!("bash exited: {}", stderr.trim())),
                ),
                Err(SpawnError::Spawn(e)) => {
                    none(want, Limit::Other(format!("spawning bash: {e}")))
                }
            };
        }
        let head = format!(
            "sudo -n -iu {u} bash refused on this machine (no sudo rights for {u}, \
             or a login shell that refuses, such as nologin)"
        );
        let argv: Vec<String> = [self.sudo.as_str(), "-n", "-iu", u, "bash"]
            .map(String::from)
            .to_vec();
        let why = match Shell::spawn(&argv, &self.env, u).await {
            Ok(sh) => return one(sh),
            // A login shell like nologin says its piece on stdout, which the
            // hello wait does not keep: the head names the likely causes.
            Err(SpawnError::NoHello { stderr }) if stderr.trim().is_empty() => {
                format!("{head}: it printed nothing on stderr and closed")
            }
            Err(SpawnError::NoHello { stderr }) => format!("{head}: {}", stderr.trim()),
            Err(SpawnError::Spawn(e)) => format!("{head}: spawning {}: {e}", self.sudo),
        };
        *self.refusal.borrow_mut() = Some(why);
        none(want, Limit::SudoRefused)
    }
}

fn none(want: usize, limit: Limit) -> Shells {
    Shells {
        got: vec![],
        want,
        limit: Some(limit),
    }
}

impl Transport for LocalTransport {
    async fn open(&self, _host: &str, principal: &Principal, want: usize) -> Shells {
        // Under the test switch, a root shell started by sudo would lose it
        // (`env_reset`): refuse here, where this process still has it, a
        // local run that names no sandbox. `cargo run -- run --local` lands
        // here, since `.cargo/config.toml` sets the switch for it too.
        if crate::sandbox::required() && !self.env.iter().any(|(k, _)| k == "TP_SANDBOX") {
            return none(
                want,
                Limit::Other(format!(
                    "{}; use the built binary (target/…/tp-basic, just dist) or unset TP_SANDBOX_REQUIRED",
                    crate::sandbox::refusal("the local shell")
                )),
            );
        }
        if let Principal::User(u) = principal {
            return self.open_user(u, want).await;
        }
        let uid = match self.current_uid().await {
            Ok(u) => u,
            Err(e) => return none(want, Limit::Other(e)),
        };
        let via_sudo = uid != 0;
        let argv: Vec<String> = if via_sudo {
            // -H: root's HOME even under a caller's `!env_reset` (see ssh.rs).
            vec![self.sudo.clone(), "-n".into(), "-H".into(), "bash".into()]
        } else {
            vec!["bash".into()]
        };
        match Shell::spawn(&argv, &self.env, "root").await {
            Ok(sh) => Shells {
                got: vec![sh],
                want,
                limit: None,
            },
            Err(SpawnError::NoHello { .. }) if via_sudo => none(want, Limit::SudoRefused),
            Err(SpawnError::NoHello { stderr }) => none(
                want,
                Limit::Other(format!("bash exited: {}", stderr.trim())),
            ),
            Err(SpawnError::Spawn(e)) => {
                none(want, Limit::Other(format!("spawning {}: {e}", argv[0])))
            }
        }
    }
}
