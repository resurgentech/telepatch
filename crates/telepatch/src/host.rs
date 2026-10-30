//! `Host<V>`: the handle a role receives. Cheap to clone; single-threaded.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::ops::Deref;
use std::rc::Rc;

use crate::facts::os::Os;
use crate::key::{Key, Value};
use crate::role::RoleGuard;
use crate::trace::{Node, NodeId, OpNode, OpSpec, RunAs, Trace};

/// A playbook bug found while tracing (not a host failure).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceError(pub String);

impl TraceError {
    pub fn new(msg: impl Into<String>) -> TraceError {
        TraceError(msg.into())
    }
}

impl fmt::Display for TraceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for TraceError {}

/// One host as seen by a playbook. `os` is gathered before the playbook
/// starts, so reading it is never a resolver read. `vars` are typed per
/// playbook. Ops called on it run in the plain root shell.
pub struct Host<V> {
    name: Rc<str>,
    /// The host's OS fact.
    pub os: Rc<Os>,
    /// Playbook variables.
    pub vars: Rc<V>,
    trace: Rc<RefCell<Trace>>,
    run_as: RunAs,
}

impl<V> Clone for Host<V> {
    fn clone(&self) -> Self {
        Host {
            name: self.name.clone(),
            os: self.os.clone(),
            vars: self.vars.clone(),
            trace: self.trace.clone(),
            run_as: self.run_as.clone(),
        }
    }
}

impl<V> Host<V> {
    /// A host with an empty trace.
    pub fn new(name: impl Into<String>, os: Os, vars: Rc<V>) -> Host<V> {
        Host {
            name: name.into().into(),
            os: Rc::new(os),
            vars,
            trace: Rc::new(RefCell::new(Trace::default())),
            run_as: RunAs::root_shell(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// A copy of the trace recorded so far.
    pub fn snapshot(&self) -> Trace {
        self.trace.borrow().clone()
    }

    /// Append an op node and return its handle. Op builders (`pkg`, `file`,
    /// ...) in later steps are thin wrappers over this.
    pub fn op(&self, spec: OpSpec) -> OpHandle {
        let node = OpNode {
            writes: spec.default_writes(),
            spec,
            run_as: self.run_as.clone(),
            reads: Default::default(),
            guard: None,
            post: BTreeMap::new(),
        };
        let id = self.trace.borrow_mut().push(Node::Op(node));
        OpHandle {
            trace: self.trace.clone(),
            id,
        }
    }

    /// Toy shell op (writes `*`); replaced by `sh!` later.
    pub fn raw(&self, script: impl Into<String>) -> OpHandle {
        self.op(OpSpec::Raw {
            script: script.into(),
        })
    }

    /// Everything after this waits for everything before it.
    pub fn barrier(&self) -> NodeId {
        self.trace.borrow_mut().push(Node::Barrier)
    }

    /// Mark a role boundary; the matching end is recorded when the guard drops.
    pub fn role_enter(&self, name: &str) -> RoleGuard {
        self.trace
            .borrow_mut()
            .push(Node::RoleBegin(name.to_string()));
        RoleGuard::new(self.trace.clone(), name.to_string())
    }

    /// Ops issued through the returned scope run in a fresh login
    /// environment of `user` (also for `"root"`).
    pub fn as_user(&self, user: &str) -> Scope<V> {
        let mut host = self.clone();
        host.run_as = RunAs::login(user);
        Scope(host)
    }
}

/// A host handle scoped to a user's login environment. Derefs to [`Host`],
/// so every op is available on it.
pub struct Scope<V>(Host<V>);

impl<V> Deref for Scope<V> {
    type Target = Host<V>;
    fn deref(&self) -> &Host<V> {
        &self.0
    }
}

/// The handle an op call returns; builder methods edit the node in place.
#[derive(Clone)]
pub struct OpHandle {
    trace: Rc<RefCell<Trace>>,
    id: NodeId,
}

impl OpHandle {
    pub fn id(&self) -> NodeId {
        self.id
    }

    fn edit(&self, f: impl FnOnce(&mut OpNode)) -> &Self {
        if let Node::Op(op) = &mut self.trace.borrow_mut().nodes[self.id.0] {
            f(op);
        }
        self
    }

    /// Replace the declared read set.
    pub fn reads(&self, keys: impl IntoIterator<Item = Key>) -> &Self {
        self.edit(|op| op.reads = keys.into_iter().collect())
    }

    /// Replace the declared write set (narrowing the default).
    pub fn writes(&self, keys: impl IntoIterator<Item = Key>) -> &Self {
        self.edit(|op| op.writes = keys.into_iter().collect())
    }

    /// Declare a postcondition: after this op, `key` has `value`.
    pub fn post(&self, key: Key, value: Value) -> &Self {
        self.edit(|op| {
            op.post.insert(key, value);
        })
    }
}
