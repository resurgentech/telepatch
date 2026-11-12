//! `Auth` and the environment-token implementation. No token file.

use std::fmt;

use telepatch::SecretError;
use zeroize::Zeroize;

use crate::kv;

/// A token. Wiped on drop. `Debug` is `<secret>`.
pub struct Token(String);

impl Token {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<secret>")
    }
}

/// One GET. The body is the response text; the status is the HTTP status.
pub struct Reply {
    pub status: u16,
    pub body: String,
}

/// What [`Auth::token`] uses to call `auth/token/lookup-self`.
pub trait Http {
    fn get(&self, path: &str, token: Option<&str>) -> Result<Reply, SecretError>;
}

/// How the backend obtains a token. AppRole would be another implementor.
pub trait Auth: Send + Sync {
    fn token(&self, http: &dyn Http) -> Result<Token, SecretError>;
}

/// `BAO_TOKEN`, then `VAULT_TOKEN`.
#[derive(Debug, Default, Clone, Copy)]
pub struct TokenFromEnv;

impl Auth for TokenFromEnv {
    fn token(&self, http: &dyn Http) -> Result<Token, SecretError> {
        let raw = first(&["BAO_TOKEN", "VAULT_TOKEN"])
            .ok_or_else(|| SecretError::bare("openbao: no token: set BAO_TOKEN or VAULT_TOKEN"))?;
        let reply = http.get("/v1/auth/token/lookup-self", Some(&raw))?;
        if reply.status == 401 || reply.status == 403 {
            return Err(SecretError::new(
                "permission denied reading auth/token/lookup-self",
            ));
        }
        if reply.status != 200 {
            return Err(SecretError::new(format!(
                "reading auth/token/lookup-self failed (HTTP {})",
                reply.status
            )));
        }
        if let Some(day) = kv::expiry_day(&reply.body)? {
            return Err(SecretError::bare(format!(
                "openbao: token expired on {day}: log in again"
            )));
        }
        Ok(Token(raw))
    }
}

fn first(names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
}
