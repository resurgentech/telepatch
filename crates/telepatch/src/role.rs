//! Playbook entry points and role boundaries.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use crate::host::{Host, TraceError};
use crate::trace::{Node, Trace};

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
}

impl RoleGuard {
    pub(crate) fn new(trace: Rc<RefCell<Trace>>, name: String) -> RoleGuard {
        RoleGuard { trace, name }
    }
}

impl Drop for RoleGuard {
    fn drop(&mut self) {
        self.trace
            .borrow_mut()
            .push(Node::RoleEnd(std::mem::take(&mut self.name)));
    }
}
