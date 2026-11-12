//! Prompt backend. Reads one line from a `BufRead`. The built-in uses stdin
//! and does not turn echo off: that needs a terminal crate this crate does
//! not take.

use std::collections::HashMap;
use std::io::{BufRead, Write};

use super::{SecretBackend, SecretError, SecretRef, SecretValue};

/// `{ prompt = "Password: " }`.
#[derive(Debug, Default, Clone, Copy)]
pub struct Prompt;

/// Write `prompt` and read one line. The newline is stripped. The line is
/// the secret; callers do not log it.
pub fn read_line(prompt: &str, mut reader: impl BufRead) -> Result<String, SecretError> {
    let mut err = std::io::stderr().lock();
    write!(err, "{prompt}").map_err(|e| SecretError::new(format!("prompt failed: {e}")))?;
    let _ = err.flush();
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| SecretError::new(format!("prompt failed: {e}")))?;
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
    Ok(line)
}

impl SecretBackend for Prompt {
    fn scheme(&self) -> &'static str {
        "prompt"
    }

    fn resolve(&self, refs: &[SecretRef]) -> Vec<Result<SecretValue, SecretError>> {
        let stdin = std::io::stdin();
        let mut cache: HashMap<&str, Result<SecretValue, SecretError>> = HashMap::new();
        refs.iter()
            .map(|r| {
                cache
                    .entry(r.path.as_str())
                    .or_insert_with(|| read_line(&r.path, stdin.lock()).map(SecretValue::new))
                    .clone()
            })
            .collect()
    }
}
