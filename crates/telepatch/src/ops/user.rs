//! `user(n)`: ensure a login account exists. Present only; groups are
//! append-only; the password is set only when this op creates the account.
//!
//! Root only (`useradd` edits `/etc/passwd`), so `as_user(u)` with `u` not
//! root is a trace error. The template checks every `groups_add` group
//! exists first (fail with `telepatch: user n: group g does not exist`,
//! nothing changed), then either `useradd -m` (with the shell and the
//! groups) or, for an existing account, `usermod -s` when the shell
//! differs and `usermod -aG` for the groups it lacks. `created` is true
//! only when `useradd` ran.
//!
//! The password hash is a secret (hard rule 5). It is applied by
//! `printf '%s:%s\n' user HASH | chpasswd -e`, a shell builtin feeding a
//! pipe, so it is never in the argv of a process on the host. It exists
//! only in the executed script ([`UserSpec::render`] with `reveal`); the
//! text every reader sees ([`OpSpec::script`](crate::trace::OpSpec::script),
//! the plan, `Debug`) shows `<hash>` instead. An empty hash is not applied.

use std::fmt;
use std::ops::Deref;

use crate::TraceError;
use crate::host::{Host, OpHandle};
use crate::key::Key;
use crate::sh::quote;
use crate::trace::{OpSpec, Principal};

#[derive(Clone, PartialEq, Eq)]
pub struct UserSpec {
    pub name: String,
    pub shell: Option<String>,
    /// Groups the account must be in (appended, never removed).
    pub groups_add: Vec<String>,
    /// A crypt(3) hash, applied only when this op creates the account.
    password_hash: Option<String>,
}

// Hand-written so a `{:?}` of a trace never prints the hash.
impl fmt::Debug for UserSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UserSpec")
            .field("name", &self.name)
            .field("shell", &self.shell)
            .field("groups_add", &self.groups_add)
            .field("password_hash", &self.password().map(|_| "<hash>"))
            .finish()
    }
}

impl UserSpec {
    pub fn new(name: impl Into<String>) -> UserSpec {
        UserSpec {
            name: name.into(),
            shell: None,
            groups_add: Vec::new(),
            password_hash: None,
        }
    }

    /// The hash to apply: `None` when unset or empty.
    fn password(&self) -> Option<&str> {
        self.password_hash.as_deref().filter(|h| !h.is_empty())
    }

    /// The script body. `reveal` puts the real hash in; without it the
    /// hash is the text `<hash>`.
    pub fn render(&self, reveal: bool) -> String {
        let mut s = format!("created=0\ntp_u={}\n", quote(&self.name));
        if !self.groups_add.is_empty() {
            let words: Vec<String> = self.groups_add.iter().map(|g| quote(g)).collect();
            let words = words.join(" ");
            s.push_str(&format!(
                "for tp_g in {words}; do\n  if ! getent group \"$tp_g\" >/dev/null 2>&1; then\n    echo \"telepatch: user $tp_u: group $tp_g does not exist\" >&2\n    rc=1\n  fi\ndone\n"
            ));
        }
        s.push_str("if [ \"$rc\" -eq 0 ]; then\n");
        s.push_str("if ! getent passwd \"$tp_u\" >/dev/null 2>&1; then\n");
        let mut add = String::from("  useradd -m");
        if let Some(sh) = &self.shell {
            add.push_str(&format!(" -s {}", quote(sh)));
        }
        if !self.groups_add.is_empty() {
            add.push_str(&format!(" -G {}", quote(&self.groups_add.join(","))));
        }
        add.push_str(
            " -- \"$tp_u\"\n  rc=$?\n  if [ \"$rc\" -eq 0 ]; then changed=1; created=1; fi\n",
        );
        s.push_str(&add);
        if self.password().is_some() {
            let h = match self.password() {
                Some(h) if reveal => quote(h),
                _ => "'<hash>'".to_string(),
            };
            s.push_str(&format!(
                "  if [ \"$rc\" -eq 0 ]; then\n    printf '%s:%s\\n' \"$tp_u\" {h} | chpasswd -e\n    rc=$?\n  fi\n"
            ));
        }
        s.push_str("else\n  :\n");
        if let Some(sh) = &self.shell {
            let q = quote(sh);
            s.push_str(&format!(
                "  tp_cur=$(getent passwd \"$tp_u\" | cut -d: -f7)\n  if [ \"$tp_cur\" != {q} ]; then\n    usermod -s {q} -- \"$tp_u\"\n    rc=$?\n    if [ \"$rc\" -eq 0 ]; then changed=1; fi\n  fi\n"
            ));
        }
        if !self.groups_add.is_empty() {
            let words: Vec<String> = self.groups_add.iter().map(|g| quote(g)).collect();
            let words = words.join(" ");
            s.push_str(&format!(
                "  if [ \"$rc\" -eq 0 ]; then\n    tp_have=\" $(id -nG \"$tp_u\") \"\n    tp_add=\n    for tp_g in {words}; do\n      case \"$tp_have\" in *\" $tp_g \"*) ;; *) tp_add=\"$tp_add,$tp_g\";; esac\n    done\n    if [ -n \"$tp_add\" ]; then\n      usermod -aG \"${{tp_add#,}}\" -- \"$tp_u\"\n      rc=$?\n      if [ \"$rc\" -eq 0 ]; then changed=1; fi\n    fi\n  fi\n"
            ));
        }
        s.push_str("fi\nfi\n");
        s
    }
}

/// Handle for a `user` op.
#[derive(Clone)]
pub struct UserHandle(OpHandle);

impl Deref for UserHandle {
    type Target = OpHandle;
    fn deref(&self) -> &OpHandle {
        &self.0
    }
}

crate::host::op_builders!(UserHandle);

impl UserHandle {
    fn spec(&self, f: impl FnOnce(&mut UserSpec)) -> Self {
        self.0.edit(|op| {
            if let OpSpec::User(u) = &mut op.spec {
                f(u);
            }
        });
        self.clone()
    }

    /// The login shell: set on creation and enforced on an existing
    /// account.
    #[track_caller]
    pub fn shell(&self, path: &str) -> Self {
        self.spec(|u| u.shell = Some(path.to_string()))
    }

    /// Add the account to these groups if it is not in them. Never
    /// removes one. Each group is read, so a `group` op before this one
    /// orders first.
    #[track_caller]
    pub fn groups_add<I, S>(&self, groups: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let groups: Vec<String> = groups.into_iter().map(|g| g.as_ref().to_string()).collect();
        self.spec(|u| {
            for g in &groups {
                if !u.groups_add.contains(g) {
                    u.groups_add.push(g.clone());
                }
            }
        });
        self.0.edit(|op| {
            for g in groups {
                op.reads.insert(Key::group(g));
            }
        });
        self.clone()
    }

    /// A crypt(3) hash for the password, applied only when this op
    /// creates the account. Empty: not applied (the account stays
    /// locked). Never shown in the plan, report, log or events.
    #[track_caller]
    pub fn password_hash(&self, hash: &str) -> Self {
        if hash.contains(['\n', '\r', '\0']) {
            self.0.record_error(TraceError::new(
                "user: password_hash must be a single line".to_string(),
            ));
            return self.clone();
        }
        self.spec(|u| u.password_hash = Some(hash.to_string()))
    }
}

impl<V> Host<V> {
    /// Ensure the account `name` exists. Root only.
    #[track_caller]
    pub fn user(&self, name: &str) -> UserHandle {
        let h = self.op(OpSpec::User(UserSpec::new(name)));
        if let Principal::User(u) = &self.run_as().principal {
            h.record_error(TraceError::new(format!(
                "user({name}) is root only: as_user({u}) cannot manage accounts"
            )));
        }
        UserHandle(h)
    }
}

/// A prepare probe for the window's `user` ops: prints `missing <u>` for
/// an absent account, else `shell <u> <path>` when the shell differs and
/// `groups <u> <g>...` for the wanted groups it is not in.
pub fn probe(specs: &[&UserSpec]) -> String {
    let mut s = String::new();
    for u in specs {
        s.push_str(&format!(
            "tp_u={}\nif ! getent passwd \"$tp_u\" >/dev/null 2>&1; then echo \"missing $tp_u\"; else\n  :\n",
            quote(&u.name)
        ));
        if let Some(sh) = &u.shell {
            let q = quote(sh);
            s.push_str(&format!(
                "  tp_cur=$(getent passwd \"$tp_u\" | cut -d: -f7)\n  [ \"$tp_cur\" = {q} ] || echo \"shell $tp_u \"{q}\n"
            ));
        }
        if !u.groups_add.is_empty() {
            let words: Vec<String> = u.groups_add.iter().map(|g| quote(g)).collect();
            s.push_str(&format!(
                "  tp_have=\" $(id -nG \"$tp_u\" 2>/dev/null) \"\n  tp_add=\n  for tp_g in {}; do\n    case \"$tp_have\" in *\" $tp_g \"*) ;; *) tp_add=\"$tp_add $tp_g\";; esac\n  done\n  [ -z \"$tp_add\" ] || echo \"groups $tp_u$tp_add\"\n",
                words.join(" ")
            ));
        }
        s.push_str("fi\n");
    }
    s
}

/// One thing the probe found: `(user, None)` is a user to create,
/// `(user, Some("shell -> /bin/bash"))` or `(user, Some("groups +docker"))`
/// a change to an existing one.
pub fn findings(output: &str) -> Vec<(String, Option<String>)> {
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    for l in output.lines() {
        let mut it = l.split(' ');
        let (Some(tag), Some(u)) = (it.next(), it.next()) else {
            continue;
        };
        let rest: Vec<&str> = it.collect();
        let item = match tag {
            "missing" => (u.to_string(), None),
            "shell" if rest.len() == 1 => (u.to_string(), Some(format!("shell -> {}", rest[0]))),
            "groups" if !rest.is_empty() => {
                let g: Vec<String> = rest.iter().map(|g| format!("+{g}")).collect();
                (u.to_string(), Some(format!("groups {}", g.join(" "))))
            }
            _ => continue,
        };
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn findings_parse_probe_output() {
        assert_eq!(
            findings("missing a\nnoise\nshell b /bin/zsh\ngroups b docker sudo\nmissing a\n"),
            [
                ("a".to_string(), None),
                ("b".to_string(), Some("shell -> /bin/zsh".to_string())),
                ("b".to_string(), Some("groups +docker +sudo".to_string())),
            ]
        );
    }

    #[test]
    fn debug_and_display_hide_the_hash() {
        let mut u = UserSpec::new("x");
        u.password_hash = Some("$6$secretsalt$secrethash".to_string());
        assert!(!format!("{u:?}").contains("secret"));
        assert!(!u.render(false).contains("secret"));
        assert!(u.render(false).contains("<hash>"));
        assert!(u.render(true).contains("secrethash"));
        u.password_hash = Some(String::new());
        assert!(!u.render(true).contains("chpasswd"));
    }
}
