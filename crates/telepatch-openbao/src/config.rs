//! Address and TLS settings from the environment. Read when a run resolves,
//! not when [`crate::backend`] is built.

use telepatch::SecretError;

/// `BAO_ADDR`, then `VAULT_ADDR`. No default host.
pub fn address() -> Result<String, SecretError> {
    match first_env(&["BAO_ADDR", "VAULT_ADDR"]) {
        Some(addr) => Ok(addr.trim_end_matches('/').to_string()),
        None => Err(SecretError::bare(
            "openbao: no address: set BAO_ADDR or VAULT_ADDR",
        )),
    }
}

/// Which skip-verify variable is on, if either. `BAO_SKIP_VERIFY` wins.
pub fn skip_var() -> Option<&'static str> {
    if truthy("BAO_SKIP_VERIFY") {
        Some("BAO_SKIP_VERIFY")
    } else if truthy("VAULT_SKIP_VERIFY") {
        Some("VAULT_SKIP_VERIFY")
    } else {
        None
    }
}

pub fn skip_warning() -> Option<String> {
    skip_var().map(|name| format!("openbao: certificate verification is OFF ({name})"))
}

fn first_env(names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
}

/// Go's `strconv.ParseBool` true values: `1`, `t`, `T`, `true`, `TRUE`, `True`.
fn truthy(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "t" | "true"))
}
