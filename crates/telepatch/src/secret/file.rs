//! File backend. One read of a path per `resolve` call. A single trailing
//! newline is trimmed so a hash file written by a text editor still matches
//! `chpasswd -e`.

use std::collections::HashMap;

use super::{SecretBackend, SecretError, SecretRef, SecretValue};

/// `{ file = "/path" }`.
#[derive(Debug, Default, Clone, Copy)]
pub struct File;

fn read_secret(path: &str) -> Result<SecretValue, SecretError> {
    let bytes = std::fs::read(path)
        .map_err(|e| SecretError::new(format!("cannot read file {path}: {e}")))?;
    let mut text = String::from_utf8(bytes)
        .map_err(|_| SecretError::new(format!("file {path} is not utf-8")))?;
    if text.ends_with('\n') {
        text.pop();
        if text.ends_with('\r') {
            text.pop();
        }
    }
    Ok(SecretValue::new(text))
}

impl SecretBackend for File {
    fn scheme(&self) -> &'static str {
        "file"
    }

    fn resolve(&self, refs: &[SecretRef]) -> Vec<Result<SecretValue, SecretError>> {
        let mut cache: HashMap<&str, Result<SecretValue, SecretError>> = HashMap::new();
        refs.iter()
            .map(|r| {
                cache
                    .entry(r.path.as_str())
                    .or_insert_with(|| read_secret(&r.path))
                    .clone()
            })
            .collect()
    }
}
