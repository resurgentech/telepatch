//! Playbook entry points and role boundaries.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use crate::host::{Host, TraceError};
use crate::trace::{Node, Trace};

/// The future a playbook returns: `'static` and `!Send`, polled on one thread.
pub type PlaybookFuture = Pin<Box<dyn Future<Output = Result<(), TraceError>>>>;

/// A playbook: a name and an entry that takes an owned host.
pub struct Playbook<V> {
    pub name: &'static str,
    pub run: fn(Host<V>) -> PlaybookFuture,
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
