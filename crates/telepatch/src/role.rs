//! Playbook entry points and role boundaries.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use crate::host::{Host, TraceError};
use crate::trace::{Node, Trace};

/// Every `#[role]` function linked into this binary, by name: `#[role]`
/// adds one entry at link time. Read it through [`roles`].
#[linkme::distributed_slice]
pub static ROLES: [&'static str] = [..];

/// The names of the `#[role]` functions compiled into this binary, sorted,
/// each once. The one exact source for what `--only` may name before any
/// host is contacted: which roles a playbook *calls* is only known by tracing
/// it, and a trace awaits facts from a host.
pub fn roles() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = ROLES.iter().copied().collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Levenshtein distance between two names, by chars.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != *cb);
            cur.push(sub.min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// `--only`'s check: every name must be a role compiled into this binary.
/// The error is the usage message; the single closest role (edit distance
/// at most 2, ties to the first in sorted order) is listed first.
pub fn check_only_names<S: AsRef<str>>(names: &[S]) -> Result<(), String> {
    let known = roles();
    for n in names {
        let n = n.as_ref();
        if known.contains(&n) {
            continue;
        }
        let mut list = known.clone();
        let best = list
            .iter()
            .enumerate()
            .map(|(i, r)| (edit_distance(n, r), i))
            .min()
            .filter(|(d, _)| *d <= 2);
        if let Some((_, i)) = best {
            let r = list.remove(i);
            list.insert(0, r);
        }
        return Err(format!(
            "--only: no role named {n} in this binary; roles: {}",
            list.join(", ")
        ));
    }
    Ok(())
}

/// The future a playbook returns: `'static` and `!Send`, polled on one thread.
pub type PlaybookFuture = Pin<Box<dyn Future<Output = Result<(), TraceError>>>>;

/// A playbook: a name, an entry that takes an owned host, and optionally
/// typed vars defaults (the third of the four layers, see [`crate::vars`]).
///
/// Build it with [`Playbook::new`] and, if it has defaults,
/// [`Playbook::defaults`]; the defaults are private to the playbook, so two
/// playbooks in one binary each carry their own.
pub struct Playbook<V> {
    pub name: &'static str,
    pub run: fn(Host<V>) -> PlaybookFuture,
    defaults: Option<Defaults<V>>,
}

/// The playbook's defaults: the value builder and its TOML serializer. The
/// serializer is captured where `V: Serialize` is known, so the bound lives
/// on [`Playbook::defaults`] alone.
struct Defaults<V> {
    make: fn() -> V,
    to_toml: fn(&V) -> Result<String, String>,
}

impl<V> Clone for Defaults<V> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<V> Copy for Defaults<V> {}

fn serialize_defaults<V: serde::Serialize>(v: &V) -> Result<String, String> {
    // A `Secret` (T88) serializes as its reference, never its value: the
    // TOML built here is merged into the vars and shown in errors, so a
    // value must not appear in it.
    toml::to_string(v).map_err(|e| e.to_string())
}

impl<V> Playbook<V> {
    /// A playbook with no defaults.
    pub const fn new(name: &'static str, run: fn(Host<V>) -> PlaybookFuture) -> Playbook<V> {
        Playbook {
            name,
            run,
            defaults: None,
        }
    }

    /// Attach typed defaults: `f` builds a `V` whose fields are the
    /// playbook's third layer (below the vars file, above each role's own
    /// `Default`). Only the fields the value sets away from the role's
    /// `Default` matter in practice; a misspelled field or a wrong type is a
    /// compile error. The value must serialize to TOML (checked at load).
    pub const fn defaults(mut self, f: fn() -> V) -> Playbook<V>
    where
        V: serde::Serialize,
    {
        self.defaults = Some(Defaults {
            make: f,
            to_toml: serialize_defaults::<V>,
        });
        self
    }

    /// The defaults as TOML text (`""` for none), or
    /// `playbook <name> defaults: <why>` when the value will not serialize.
    pub fn defaults_toml(&self) -> Result<String, String> {
        match self.defaults {
            None => Ok(String::new()),
            Some(d) => (d.to_toml)(&(d.make)())
                .map_err(|e| format!("playbook {} defaults: {e}", self.name)),
        }
    }
}

/// Records `RoleEnd` when dropped. Hold it for the duration of the role body.
#[must_use = "the role ends when this guard drops"]
pub struct RoleGuard {
    trace: Rc<RefCell<Trace>>,
    name: String,
    selected: bool,
}

impl RoleGuard {
    pub(crate) fn new(trace: Rc<RefCell<Trace>>, name: String, selected: bool) -> RoleGuard {
        RoleGuard {
            trace,
            name,
            selected,
        }
    }

    /// Does `--only` run this role's body? Always true without `--only`.
    pub fn selected(&self) -> bool {
        self.selected
    }
}

/// What a role returns when `--only` skips its body: `()` and
/// `Result<(), E>` (`Ok(())`). `#[role]` returns it right after recording
/// the role's begin, so the end is recorded too and the skip leaves no ops.
pub trait Skipped {
    fn skipped() -> Self;
}

impl Skipped for () {
    fn skipped() {}
}

impl<E> Skipped for Result<(), E> {
    fn skipped() -> Self {
        Ok(())
    }
}

impl Drop for RoleGuard {
    fn drop(&mut self) {
        self.trace
            .borrow_mut()
            .push(Node::RoleEnd(std::mem::take(&mut self.name)));
    }
}
