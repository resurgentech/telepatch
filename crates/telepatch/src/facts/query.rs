//! Fact questions as data: what is asked, which effect key it reads, the
//! shell probe the real executor runs, and how a postcondition answers it.

use std::fmt;

use crate::key::{Key, Value};
use crate::trace::RunAs;

/// A question about a host that the playbook awaits.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Fact {
    /// `exists(path)` -> `Value::Exists(bool)`.
    Exists(String),
    /// `bin_exists(cmd)` -> `Value::Exists(bool)`.
    BinExists(String),
    /// `bin_version(cmd)` -> `Value::Text(first line)` or `Value::Absent`.
    BinVersion(String),
}

impl Fact {
    pub fn exists(path: impl Into<String>) -> Fact {
        Fact::Exists(path.into())
    }
    pub fn bin_exists(cmd: impl Into<String>) -> Fact {
        Fact::BinExists(cmd.into())
    }
    pub fn bin_version(cmd: impl Into<String>) -> Fact {
        Fact::BinVersion(cmd.into())
    }

    /// The effect key this fact reads. Writes that overlap it invalidate a
    /// confirmed answer and can be the writer a postcondition answers from.
    /// `exists(p)` reads `file:p`; `bin_*` read `bin:cmd`, which `*`, any
    /// `pkg:` and any `file:` key overlap (see [`Key::overlaps`]).
    pub fn effect_key(&self) -> Key {
        match self {
            Fact::Exists(p) => Key::file(p),
            Fact::BinExists(c) | Fact::BinVersion(c) => Key::bin(c),
        }
    }

    /// Shell script the executor runs (in the fact's RunAs shell) to answer
    /// it. Output is fed to [`Fact::parse`].
    pub fn probe(&self) -> String {
        match self {
            Fact::Exists(p) => {
                format!("if test -e {}; then echo yes; else echo no; fi", quote(p))
            }
            Fact::BinExists(c) => format!(
                "if command -v {} >/dev/null 2>&1; then echo yes; else echo no; fi",
                quote(c)
            ),
            Fact::BinVersion(c) => format!(
                "if command -v {c} >/dev/null 2>&1; then echo present; {c} --version 2>&1 | head -n 1; else echo missing; fi",
                c = quote(c)
            ),
        }
    }

    /// Turn the probe's stdout into the fact's value.
    pub fn parse(&self, stdout: &str) -> Value {
        let mut lines = stdout.lines();
        let first = lines.next().unwrap_or("").trim();
        match self {
            Fact::Exists(_) | Fact::BinExists(_) => Value::Exists(first == "yes"),
            Fact::BinVersion(_) => {
                if first == "present" {
                    Value::Text(lines.next().unwrap_or("").trim().to_string())
                } else {
                    Value::Absent
                }
            }
        }
    }

    /// What a writer's postcondition value for this fact's effect key says
    /// about the fact, or `None` when it does not determine it.
    pub fn from_post(&self, post: &Value) -> Option<Value> {
        match (self, post) {
            (Fact::Exists(_) | Fact::BinExists(_), Value::Exists(b)) => Some(Value::Exists(*b)),
            (Fact::BinExists(_), Value::Text(_)) => Some(Value::Exists(true)),
            (Fact::BinExists(_), Value::Absent) => Some(Value::Exists(false)),
            (Fact::BinVersion(_), Value::Text(t)) => Some(Value::Text(t.clone())),
            (Fact::BinVersion(_), Value::Exists(false) | Value::Absent) => Some(Value::Absent),
            _ => None,
        }
    }
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

impl fmt::Display for Fact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fact::Exists(p) => write!(f, "exists({p})"),
            Fact::BinExists(c) => write!(f, "bin_exists({c})"),
            Fact::BinVersion(c) => write!(f, "bin_version({c})"),
        }
    }
}

/// A fact plus the shell it is asked in. Root's `which cargo` and a login
/// environment's are different facts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FactQuery {
    pub run_as: RunAs,
    pub fact: Fact,
}

impl fmt::Display for FactQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} as {}", self.fact, self.run_as)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_probe_output() {
        assert_eq!(Fact::exists("/x").parse("yes\n"), Value::Exists(true));
        assert_eq!(Fact::bin_exists("a").parse("no\n"), Value::Exists(false));
        assert_eq!(
            Fact::bin_version("a").parse("present\ncargo 1.90.0\n"),
            Value::Text("cargo 1.90.0".into())
        );
        assert_eq!(Fact::bin_version("a").parse("missing\n"), Value::Absent);
    }

    #[test]
    fn probe_quotes() {
        assert!(Fact::exists("/a b'c").probe().contains("'/a b'\\''c'"));
    }
}
