//! The local transport: spawn `bash` on this machine, via `sudo -n -H bash`
//! unless already root. One shell, no PTY.

use tokio::process::Command;

use super::{Limit, Shells, Transport};
use crate::pool::{Shell, SpawnError};
use crate::trace::Principal;

#[derive(Debug, Clone)]
pub struct LocalTransport {
    uid: Option<u32>,
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
            sudo: "sudo".into(),
            env: Vec::new(),
        }
    }

    /// Use this uid instead of asking `id -u` (tests).
    pub fn uid(mut self, uid: u32) -> Self {
        self.uid = Some(uid);
        self
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
        if let Principal::User(u) = principal {
            return none(
                want,
                Limit::Other(format!(
                    "principal user {u}: not supported by this transport yet"
                )),
            );
        }
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
