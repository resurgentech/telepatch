//! Effect keys and the values postconditions assign to them.

use std::fmt;

/// A fine-grained effect address. Ordering edges between ops derive from
/// [`Key::overlaps`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Key {
    Pkg(String),
    File(String),
    User(String),
    Group(String),
    Svc(String),
    Repo(String),
    /// A git checkout at this path.
    Git(String),
    /// A command on `PATH`; see [`Key::overlaps`].
    Bin(String),
    /// Everything under one user's home directory, `home:<user>`; see
    /// [`Key::overlaps`].
    Home(String),
    /// Everything:the effects of this op cannot be enumerated.
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
    pub fn group(n: impl Into<String>) -> Key {
        Key::Group(n.into())
    }
    pub fn svc(n: impl Into<String>) -> Key {
        Key::Svc(n.into())
    }
    pub fn repo(n: impl Into<String>) -> Key {
        Key::Repo(n.into())
    }
    pub fn git(p: impl Into<String>) -> Key {
        Key::Git(p.into())
    }
    pub fn bin(n: impl Into<String>) -> Key {
        Key::Bin(n.into())
    }
    pub fn home(user: impl Into<String>) -> Key {
        Key::Home(user.into())
    }

    /// `*` overlaps everything; otherwise keys overlap when equal. Paths are
    /// compared whole (no directory-prefix semantics yet).
    ///
    /// `bin:<cmd>` is the exception: installing a package or writing a file
    /// can create a binary, so it also overlaps every `pkg:` and `file:`
    /// key (coarse on purpose; a binary's path is not known up front).
    ///
    /// `home:<user>` is everything under a user's home directory, whose
    /// path is only known on the host: it overlaps every `file:` and `bin:`
    /// key (paths are not compared), `*` and an equal `home:` key, and
    /// nothing else. A cargo op writes it, so a fact read under the home
    /// (`exists("/root/.cargo/bin/x")`, `bin_exists("x")`) waits for the op.
    pub fn overlaps(&self, other: &Key) -> bool {
        let creates_bin = |k: &Key| matches!(k, Key::Pkg(_) | Key::File(_));
        let in_home = |k: &Key| matches!(k, Key::File(_) | Key::Bin(_));
        matches!(self, Key::Any)
            || matches!(other, Key::Any)
            || self == other
            || (matches!(self, Key::Bin(_)) && creates_bin(other))
            || (matches!(other, Key::Bin(_)) && creates_bin(self))
            || (matches!(self, Key::Home(_)) && in_home(other))
            || (matches!(other, Key::Home(_)) && in_home(self))
            || same_git_file(self, other)
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Key::Pkg(n) => write!(f, "pkg:{n}"),
            Key::File(n) => write!(f, "file:{n}"),
            Key::User(n) => write!(f, "user:{n}"),
            Key::Group(n) => write!(f, "group:{n}"),
            Key::Svc(n) => write!(f, "svc:{n}"),
            Key::Repo(n) => write!(f, "repo:{n}"),
            Key::Git(n) => write!(f, "git:{n}"),
            Key::Bin(n) => write!(f, "bin:{n}"),
            Key::Home(u) => write!(f, "home:{u}"),
            Key::Any => f.write_str("*"),
        }
    }
}

/// A checkout and a file op at the same path name one path.
fn same_git_file(a: &Key, b: &Key) -> bool {
    match (a, b) {
        (Key::Git(p), Key::File(q)) | (Key::File(p), Key::Git(q)) => p == q,
        _ => false,
    }
}

/// What a postcondition guarantees, or a fact read returned.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Value {
    Installed,
    Exists(bool),
    Text(String),
    /// The thing asked about does not exist (e.g. no such binary).
    Absent,
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Installed => f.write_str("Installed"),
            Value::Exists(b) => write!(f, "Exists({b})"),
            Value::Text(s) => write!(f, "{s:?}"),
            Value::Absent => f.write_str("Absent"),
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
            (Key::group("docker"), "group:docker"),
            (Key::svc("nginx"), "svc:nginx"),
            (Key::repo("hashicorp"), "repo:hashicorp"),
            (Key::git("/srv/a"), "git:/srv/a"),
            (Key::bin("cargo"), "bin:cargo"),
            (Key::home("deploy"), "home:deploy"),
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
        assert!(Key::group("a").overlaps(&Key::group("a")));
        assert!(!Key::group("a").overlaps(&Key::group("b")));
        assert!(!Key::group("a").overlaps(&Key::user("a")));
        assert!(Key::group("a").overlaps(&Key::Any));
        assert!(!Key::file("/etc").overlaps(&Key::file("/etc/x")));
        assert!(Key::bin("a").overlaps(&Key::pkg("zzz")));
        assert!(Key::file("/x").overlaps(&Key::bin("a")));
        assert!(Key::bin("a").overlaps(&Key::bin("a")));
        assert!(!Key::bin("a").overlaps(&Key::bin("b")));
        assert!(!Key::bin("a").overlaps(&Key::svc("a")));
    }

    /// `home:<user>` stands for everything under a user's home: it
    /// overlaps every `file:` and `bin:` key (paths are not compared), `*`
    /// and an equal `home:` key, nothing else.
    #[test]
    fn home_overlap() {
        let h = Key::home("root");
        for k in [
            Key::file("/root/.cargo/bin/x"),
            Key::file("/etc/x"),
            Key::bin("cargo"),
            Key::Any,
            Key::home("root"),
        ] {
            assert!(h.overlaps(&k), "{k}");
            assert!(k.overlaps(&h), "{k}");
        }
        for k in [
            Key::home("deploy"),
            Key::pkg("cargo"),
            Key::pkg("cargo:root:x"),
            Key::user("root"),
            Key::repo("x"),
        ] {
            assert!(!h.overlaps(&k), "{k}");
            assert!(!k.overlaps(&h), "{k}");
        }
    }
}
