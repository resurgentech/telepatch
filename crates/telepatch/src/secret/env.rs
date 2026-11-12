//! Environment-variable backend. One read of a name per `resolve` call.

use std::collections::HashMap;

use super::{SecretBackend, SecretError, SecretRef, SecretValue};

/// `{ env = "NAME" }`.
#[derive(Debug, Default, Clone, Copy)]
pub struct Env;

impl SecretBackend for Env {
    fn scheme(&self) -> &'static str {
        "env"
    }

    fn resolve(&self, refs: &[SecretRef]) -> Vec<Result<SecretValue, SecretError>> {
        let mut cache: HashMap<&str, Result<SecretValue, SecretError>> = HashMap::new();
        refs.iter()
            .map(|r| {
                cache
                    .entry(r.path.as_str())
                    .or_insert_with(|| {
                        std::env::var(&r.path)
                            .map(SecretValue::new)
                            .map_err(|_| SecretError::new(format!("no value for env {}", r.path)))
                    })
                    .clone()
            })
            .collect()
    }
}
