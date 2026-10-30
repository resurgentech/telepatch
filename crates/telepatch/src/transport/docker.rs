//! The docker transport: `docker exec -i <container> bash`. A host name is
//! mapped to a container id or name (the name itself if unmapped). The
//! shell runs as the container's default user, normally root.

use std::collections::HashMap;

use super::{Limit, Shells, Transport};
use crate::pool::{Shell, SpawnError};
use crate::trace::Principal;

#[derive(Debug, Clone)]
pub struct DockerTransport {
    docker: String,
    containers: HashMap<String, String>,
}

impl Default for DockerTransport {
    fn default() -> Self {
        DockerTransport::new()
    }
}

impl DockerTransport {
    pub fn new() -> DockerTransport {
        DockerTransport {
            docker: "docker".into(),
            containers: HashMap::new(),
        }
    }

    /// The docker command to run (default `docker`).
    pub fn docker(mut self, cmd: &str) -> Self {
        self.docker = cmd.to_string();
        self
    }

    /// Map host name `host` to container id/name `container`.
    pub fn container(mut self, host: &str, container: &str) -> Self {
        self.containers.insert(host.into(), container.into());
        self
    }
}

impl Transport for DockerTransport {
    async fn open(&self, host: &str, principal: &Principal, want: usize) -> Shells {
        let fail = |limit| Shells {
            got: vec![],
            want,
            limit: Some(limit),
        };
        if let Principal::User(u) = principal {
            return fail(Limit::Other(format!(
                "principal user {u}: not supported by this transport yet"
            )));
        }
        let ctr = self.containers.get(host).map_or(host, String::as_str);
        let argv = vec![
            self.docker.clone(),
            "exec".into(),
            "-i".into(),
            ctr.to_string(),
            "bash".into(),
        ];
        match Shell::spawn(&argv, &[], "root").await {
            Ok(sh) => Shells {
                got: vec![sh],
                want,
                limit: None,
            },
            Err(SpawnError::NoHello { stderr }) => fail(Limit::Other(format!(
                "docker exec {ctr} bash: {}",
                stderr.trim()
            ))),
            Err(SpawnError::Spawn(e)) => fail(Limit::Other(format!("spawning {}: {e}", argv[0]))),
        }
    }
}
