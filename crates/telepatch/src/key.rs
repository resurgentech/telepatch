//! Effect keys and the values postconditions assign to them.

use std::fmt;

/// A fine-grained effect address. Ordering edges between ops derive from
/// [`Key::overlaps`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Key {
    Pkg(String),
    File(String),
    User(String),
    Svc(String),
    Repo(String),
    /// Everything: the effects of this op cannot be enumerated.
    Any,
}

impl Key {
    pub fn pkg(n: impl Into<String>) -> Key {
        Key::Pkg(n.into())
    }
    pub fn file(p: impl Into<String>) -> Key {
        Key::File(p.into())
    }
    pub fn user(n: impl Into<String>) -> Key {
        Key::User(n.into())
    }
    pub fn svc(n: impl Into<String>) -> Key {
        Key::Svc(n.into())
    }
    pub fn repo(n: impl Into<String>) -> Key {
        Key::Repo(n.into())
    }

    /// `*` overlaps everything; otherwise keys overlap when equal. Paths are
    /// compared whole (no directory-prefix semantics yet).
    pub fn overlaps(&self, other: &Key) -> bool {
        matches!(self, Key::Any) || matches!(other, Key::Any) || self == other
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Key::Pkg(n) => write!(f, "pkg:{n}"),
            Key::File(n) => write!(f, "file:{n}"),
            Key::User(n) => write!(f, "user:{n}"),
            Key::Svc(n) => write!(f, "svc:{n}"),
            Key::Repo(n) => write!(f, "repo:{n}"),
            Key::Any => f.write_str("*"),
        }
    }
}

/// What a postcondition guarantees, or a fact read returned.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Value {
    Installed,
    Exists(bool),
    Text(String),
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Installed => f.write_str("Installed"),
            Value::Exists(b) => write!(f, "Exists({b})"),
            Value::Text(s) => write!(f, "{s:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display() {
        let cases = [
            (Key::pkg("nginx"), "pkg:nginx"),
            (Key::file("/etc/x"), "file:/etc/x"),
            (Key::user("deploy"), "user:deploy"),
            (Key::svc("nginx"), "svc:nginx"),
            (Key::repo("hashicorp"), "repo:hashicorp"),
            (Key::Any, "*"),
        ];
        for (k, s) in cases {
            assert_eq!(k.to_string(), s);
        }
    }

    #[test]
    fn overlap() {
        assert!(Key::Any.overlaps(&Key::pkg("a")));
        assert!(Key::pkg("a").overlaps(&Key::Any));
        assert!(Key::Any.overlaps(&Key::Any));
        assert!(Key::pkg("a").overlaps(&Key::pkg("a")));
        assert!(!Key::pkg("a").overlaps(&Key::pkg("b")));
        assert!(!Key::pkg("a").overlaps(&Key::svc("a")));
        assert!(!Key::file("/etc").overlaps(&Key::file("/etc/x")));
    }
}
