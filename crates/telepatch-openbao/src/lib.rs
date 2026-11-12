//! OpenBao (and Vault) KV reader. HTTP and TLS stay in this crate.
//!
//! `backend()` is what a playbook passes to `Main::secrets`. Prepare calls
//! [`SecretBackend::resolve`] once per scheme. This crate reads the token
//! from the environment, learns the KV version per mount, then reads each
//! distinct path once.

mod auth;
mod config;
mod kv;

use std::collections::HashMap;

use serde_json::Value;
use telepatch::{SecretBackend, SecretError, SecretRef, SecretValue};
use ureq::Agent;

pub use auth::{Auth, Http, Reply, Token, TokenFromEnv};
pub use kv::Mount;

/// The `bao` scheme.
pub struct OpenBao {
    auth: Box<dyn Auth>,
}

impl OpenBao {
    pub fn from_env() -> OpenBao {
        OpenBao {
            auth: Box::new(TokenFromEnv),
        }
    }

    pub fn auth(mut self, auth: impl Auth + 'static) -> OpenBao {
        self.auth = Box::new(auth);
        self
    }

    /// The prepare line when certificate checks are off.
    pub fn warnings(&self) -> Vec<String> {
        config::skip_warning().into_iter().collect()
    }
}

pub fn backend() -> OpenBao {
    OpenBao::from_env().auth(TokenFromEnv)
}

impl SecretBackend for OpenBao {
    fn scheme(&self) -> &'static str {
        "bao"
    }

    fn warnings(&self) -> Vec<String> {
        OpenBao::warnings(self)
    }

    fn resolve(&self, refs: &[SecretRef]) -> Vec<Result<SecretValue, SecretError>> {
        if refs.is_empty() {
            return Vec::new();
        }
        let address = match config::address() {
            Ok(a) => a,
            Err(e) => return vec![Err(e); refs.len()],
        };
        let client = Client::new(&address, config::skip_var().is_some());
        let token = match self.auth.token(&client) {
            Ok(t) => t,
            Err(e) => return vec![Err(e); refs.len()],
        };
        read_all(&client, token.as_str(), refs)
    }
}

struct Client {
    agent: Agent,
    address: String,
}

impl Client {
    fn new(address: &str, skip_verify: bool) -> Client {
        let tls = ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::Rustls)
            .disable_verification(skip_verify)
            .build();
        let config = Agent::config_builder()
            .http_status_as_error(false)
            .tls_config(tls)
            .build();
        Client {
            agent: config.new_agent(),
            address: address.to_string(),
        }
    }
}

impl Http for Client {
    fn get(&self, path: &str, token: Option<&str>) -> Result<Reply, SecretError> {
        let url = format!("{}{path}", self.address);
        let mut req = self.agent.get(&url);
        if let Some(token) = token {
            req = req.header("X-Vault-Token", token);
        }
        match req.call() {
            Ok(mut resp) => {
                let status = resp.status().as_u16();
                let body = resp.body_mut().read_to_string().unwrap_or_default();
                Ok(Reply { status, body })
            }
            Err(err) => Err(SecretError::new(format!("reading {path} failed ({err})"))),
        }
    }
}

fn read_all(
    client: &Client,
    token: &str,
    refs: &[SecretRef],
) -> Vec<Result<SecretValue, SecretError>> {
    let mut mounts: HashMap<String, Result<Mount, SecretError>> = HashMap::new();
    let mut docs: HashMap<String, Result<Value, SecretError>> = HashMap::new();
    refs.iter()
        .map(|r| one(client, token, r, &mut mounts, &mut docs))
        .collect()
}

fn one(
    client: &Client,
    token: &str,
    r: &SecretRef,
    mounts: &mut HashMap<String, Result<Mount, SecretError>>,
    docs: &mut HashMap<String, Result<Value, SecretError>>,
) -> Result<SecretValue, SecretError> {
    let mount = mount_for(client, token, &r.path, mounts)?;
    let url = kv::data_url(&mount, &r.path);
    if !docs.contains_key(&url) {
        let fetched = fetch_doc(client, token, &url, &r.path);
        docs.insert(url.clone(), fetched);
    }
    let doc = docs
        .get(&url)
        .expect("just inserted")
        .as_ref()
        .map_err(Clone::clone)?;
    kv::take_field(&mount, doc, &r.field, &r.path)
}

fn mount_for(
    client: &Client,
    token: &str,
    secret_path: &str,
    mounts: &mut HashMap<String, Result<Mount, SecretError>>,
) -> Result<Mount, SecretError> {
    if let Some((key, _)) = mounts
        .iter()
        .find(|(prefix, _)| kv::covers(prefix, secret_path))
    {
        let key = key.clone();
        return mounts.get(&key).expect("just found").clone();
    }
    let path = format!(
        "/v1/sys/internal/ui/mounts/{}",
        secret_path.trim_matches('/')
    );
    let got = match client.get(&path, Some(token)) {
        Ok(reply) => match reply.status {
            200 => kv::parse_mount(&reply.body, secret_path),
            404 => Err(SecretError::new(format!("no secret at {secret_path}"))),
            401 | 403 => Err(SecretError::new(format!(
                "permission denied reading {secret_path}"
            ))),
            code => Err(SecretError::new(format!(
                "reading {secret_path} failed (HTTP {code})"
            ))),
        },
        Err(err) => Err(err),
    };
    let prefix = got
        .as_ref()
        .map(|m| m.prefix.clone())
        .unwrap_or_else(|_| secret_path.trim_matches('/').to_string());
    mounts.insert(prefix.clone(), got);
    mounts.get(&prefix).expect("just inserted").clone()
}

fn fetch_doc(
    client: &Client,
    token: &str,
    url: &str,
    secret_path: &str,
) -> Result<Value, SecretError> {
    let reply = client.get(url, Some(token))?;
    match reply.status {
        200 => serde_json::from_str(&reply.body)
            .map_err(|_| SecretError::new(format!("no secret at {secret_path}"))),
        404 => Err(SecretError::new(format!("no secret at {secret_path}"))),
        401 | 403 => Err(SecretError::new(format!(
            "permission denied reading {secret_path}"
        ))),
        code => Err(SecretError::new(format!(
            "reading {secret_path} failed (HTTP {code})"
        ))),
    }
}
