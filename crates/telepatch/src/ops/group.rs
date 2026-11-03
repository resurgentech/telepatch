//! `group(n)`: ensure a group exists. Present only: no removal, no gid.
//!
//! Root only (`groupadd` edits `/etc/group`), so `as_user(u)` with `u` not
//! root is a trace error. The template asks `getent group n` and runs
//! `groupadd` (`-r` for a system group) only when the group is absent;
//! `created` is true only then, and `changed` equals it.

use std::ops::Deref;

use crate::TraceError;
use crate::host::{Host, OpHandle};
use crate::sh::quote;
use crate::trace::{OpSpec, Principal};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupSpec {
    pub name: String,
    /// `groupadd -r`: a system group (gid from the system range).
    pub system: bool,
}

impl GroupSpec {
    pub fn new(name: impl Into<String>) -> GroupSpec {
        GroupSpec {
            name: name.into(),
            system: false,
        }
    }

    pub fn render(&self) -> String {
        format!(
            "created=0\ntp_g={}\nif ! getent group \"$tp_g\" >/dev/null 2>&1; then\n  groupadd{} -- \"$tp_g\"\n  rc=$?\n  if [ \"$rc\" -eq 0 ]; then changed=1; created=1; fi\nfi\n",
            quote(&self.name),
            if self.system { " -r" } else { "" }
        )
    }
}

/// Handle for a `group` op.
#[derive(Clone)]
pub struct GroupHandle(OpHandle);

impl Deref for GroupHandle {
    type Target = OpHandle;
    fn deref(&self) -> &OpHandle {
        &self.0
    }
}

impl GroupHandle {
    /// Create the group as a system group (`groupadd -r`).
    pub fn system(&self) -> &Self {
        self.0.edit(|op| {
            if let OpSpec::Group(g) = &mut op.spec {
                g.system = true;
            }
        });
        self
    }
}

impl<V> Host<V> {
    /// Ensure the group `name` exists. Root only.
    pub fn group(&self, name: &str) -> GroupHandle {
        let h = self.op(OpSpec::Group(GroupSpec::new(name)));
        if let Principal::User(u) = &self.run_as().principal {
            h.record_error(TraceError::new(format!(
                "group({name}) is root only: as_user({u}) cannot manage groups"
            )));
        }
        GroupHandle(h)
    }
}

/// A prepare probe: prints `missing <name>` for each group that does not
/// exist.
pub fn probe(names: &[String]) -> String {
    let mut s = String::new();
    for n in names {
        let q = quote(n);
        s.push_str(&format!(
            "getent group {q} >/dev/null 2>&1 || echo \"missing \"{q}\n"
        ));
    }
    s
}

/// The names [`probe`]'s output lists as missing.
pub fn missing(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|l| l.strip_prefix("missing "))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_parses_probe_output() {
        assert_eq!(missing("missing a\nnoise\nmissing b c\n"), ["a", "b c"]);
        let p = probe(&["it's".to_string()]);
        assert!(p.contains(r"'it'\''s'"), "{p}");
    }
}
