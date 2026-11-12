//! `Host<V>`: the handle a role receives. Cheap to clone; single-threaded.

use std::cell::RefCell;
use std::fmt;
use std::ops::Deref;
use std::rc::Rc;

use crate::facts::os::Os;
use crate::key::{Key, Value};
use crate::policy::{Allowance, Policy};
use crate::resolver::Resolver;
use crate::role::RoleGuard;
use crate::trace::{Guard, Node, NodeId, OpNode, OpSpec, RunAs, Site, Trace};

/// A playbook bug found while tracing (not a host failure).
///
/// Prints in the console form:
///
/// ```text
/// trace error: <message>[ (policy: <name>)]
///   in role <outer > inner>, <file>:<line>
///   to permit: .allow(Policy::<Name>, "<reason>")
/// ```
///
/// The second line is `  at <file>:<line>` outside any role and is left
/// out when neither is known; the third is printed for a policy error
/// only. A hard error (no `policy`) cannot be permitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceError {
    pub message: String,
    /// The role chain the error was found in, outermost first.
    pub roles: Vec<String>,
    /// The caller's source location.
    pub site: Option<Site>,
    /// Set for a policy error: the policy an `.allow(..)` would permit.
    pub policy: Option<Policy>,
}

impl TraceError {
    /// A hard error at the caller's site. Recording it through
    /// [`Host::record_error`] or an op handle replaces the site with that
    /// call's and adds the role chain.
    #[track_caller]
    pub fn new(msg: impl Into<String>) -> TraceError {
        TraceError {
            message: msg.into(),
            roles: Vec::new(),
            site: Some(Site::caller()),
            policy: None,
        }
    }

    /// A policy error, placed later (see [`Trace::locate`]).
    pub fn policy(policy: Policy, msg: impl Into<String>) -> TraceError {
        TraceError {
            message: msg.into(),
            roles: Vec::new(),
            site: None,
            policy: Some(policy),
        }
    }

    /// The message alone, without the location lines.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for TraceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "trace error: {}", self.message)?;
        if let Some(p) = self.policy {
            write!(f, " (policy: {p})")?;
        }
        let roles = self.roles.join(" > ");
        match (roles.is_empty(), &self.site) {
            (false, Some(s)) => write!(f, "\n  in role {roles}, {s}")?,
            (false, None) => write!(f, "\n  in role {roles}")?,
            (true, Some(s)) => write!(f, "\n  at {s}")?,
            (true, None) => {}
        }
        if let Some(p) = self.policy {
            write!(
                f,
                "\n  to permit: .allow(Policy::{}, \"<reason>\")",
                p.variant()
            )?;
        }
        Ok(())
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
    resolver: Rc<RefCell<Resolver>>,
    run_as: RunAs,
}

impl<V> Clone for Host<V> {
    fn clone(&self) -> Self {
        Host {
            name: self.name.clone(),
            os: self.os.clone(),
            vars: self.vars.clone(),
            trace: self.trace.clone(),
            resolver: self.resolver.clone(),
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
            resolver: Rc::new(RefCell::new(Resolver::default())),
            run_as: RunAs::root_shell(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The shell this handle's ops and facts run in.
    pub fn run_as(&self) -> &RunAs {
        &self.run_as
    }

    pub(crate) fn trace_cell(&self) -> &Rc<RefCell<Trace>> {
        &self.trace
    }

    pub(crate) fn resolver_cell(&self) -> &Rc<RefCell<Resolver>> {
        &self.resolver
    }

    /// A copy of the trace recorded so far.
    pub fn snapshot(&self) -> Trace {
        let mut t = self.trace.borrow().clone();
        let derived = derived_errors(&t);
        t.errors.extend(derived);
        t
    }

    /// The first playbook bug found so far, recorded or derived from the
    /// finished ops (an `svc` that wants nothing, a policy not allowed).
    pub(crate) fn first_error(&self) -> Option<TraceError> {
        let t = self.trace.borrow();
        t.errors
            .first()
            .cloned()
            .or_else(|| derived_errors(&t).into_iter().next())
    }

    /// Record a playbook bug found while tracing. The run fails with the
    /// first recorded error before anything is applied. The error takes
    /// the caller's site and the roles open now.
    #[track_caller]
    pub fn record_error(&self, mut e: TraceError) {
        let mut t = self.trace.borrow_mut();
        e.site = Some(Site::caller());
        if e.roles.is_empty() {
            e.roles = t.open_roles();
        }
        t.errors.push(e);
    }

    /// Append an op node and return its handle. Op builders (`pkg`, `file`,
    /// ...) are thin wrappers over this. The node records the caller's
    /// site (`#[track_caller]` all the way from the role's call).
    #[track_caller]
    pub fn op(&self, spec: OpSpec) -> OpHandle {
        let node = OpNode {
            writes: spec.default_writes(),
            post: spec.default_post(),
            spec,
            run_as: self.run_as.clone(),
            reads: Default::default(),
            guard: None,
            allow_fail: false,
            site: Some(Site::caller()),
            allowed: Vec::new(),
            at_end: false,
        };
        let id = self.trace.borrow_mut().push(Node::Op(node));
        OpHandle {
            trace: self.trace.clone(),
            id,
        }
    }

    /// Toy shell op (writes `*`); replaced by `sh!` later.
    #[track_caller]
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

/// The handle an op call returns; builder methods edit the node in place
/// and return the handle, owned (a clone: an `Rc` of the trace and the
/// node id), so a chain binds as written
/// (`let daemon = h.file(p).content(c);`) and a call on a bound handle as
/// a statement (`s.when(g);`) edits the same node. Every typed handle
/// (`FileHandle`, `SvcHandle`, ...) follows the same rule, and also takes
/// these shared methods returning its own type ([`op_builders`]).
#[derive(Clone)]
pub struct OpHandle {
    trace: Rc<RefCell<Trace>>,
    id: NodeId,
}

impl OpHandle {
    /// Record a trace error (see [`Host::record_error`]): at the caller's
    /// site, in the roles this op was traced in.
    #[track_caller]
    pub(crate) fn record_error(&self, mut e: TraceError) {
        let mut t = self.trace.borrow_mut();
        e.site = Some(Site::caller());
        if e.roles.is_empty() {
            e.roles = t.roles_at(self.id);
        }
        t.errors.push(e);
    }

    /// Permit this op to break `policy`, for `reason` (required: an empty
    /// one is a hard trace error). The plan and the run report list it.
    #[track_caller]
    pub fn allow(&self, policy: Policy, reason: impl Into<String>) -> OpHandle {
        let reason = reason.into();
        if reason.trim().is_empty() {
            self.record_error(TraceError::new(format!(
                "allow(Policy::{}, ..): the reason is required",
                policy.variant()
            )));
            return self.clone();
        }
        if reason.contains(['\n', '\r']) {
            self.record_error(TraceError::new(format!(
                "allow(Policy::{}, ..): the reason must be one line",
                policy.variant()
            )));
            return self.clone();
        }
        self.edit(|op| op.allowed.push(Allowance::new(policy, reason)))
            .clone()
    }

    pub fn id(&self) -> NodeId {
        self.id
    }

    /// Read this op's node.
    pub(crate) fn with_node<R>(&self, f: impl FnOnce(&OpNode) -> R) -> Option<R> {
        match &self.trace.borrow().nodes[self.id.0] {
            Node::Op(op) => Some(f(op)),
            _ => None,
        }
    }

    /// Trace a new op at the end of the trace, in this op's shell, at the
    /// caller's site, and return its handle: a builder that starts an op of
    /// its own from a chain (`h.svc(u).started().restart_on(&x)`).
    #[track_caller]
    pub(crate) fn follow(&self, spec: OpSpec) -> OpHandle {
        let run_as = self
            .with_node(|op| op.run_as.clone())
            .unwrap_or_else(RunAs::root_shell);
        let node = OpNode {
            writes: spec.default_writes(),
            post: spec.default_post(),
            spec,
            run_as,
            reads: Default::default(),
            guard: None,
            allow_fail: false,
            site: Some(Site::caller()),
            allowed: Vec::new(),
            at_end: false,
        };
        let id = self.trace.borrow_mut().push(Node::Op(node));
        OpHandle {
            trace: self.trace.clone(),
            id,
        }
    }

    pub(crate) fn edit(&self, f: impl FnOnce(&mut OpNode)) -> &Self {
        if let Node::Op(op) = &mut self.trace.borrow_mut().nodes[self.id.0] {
            f(op);
        }
        self
    }

    /// Re-derive the file postcondition after a builder changed the spec
    /// (`.absent()` flips it to `Exists(false)`).
    pub(crate) fn refresh_effects(&self) {
        self.edit(|op| {
            if let crate::trace::OpSpec::File(f) = &op.spec {
                let key = Key::file(&f.path);
                let v = op.spec.default_post().remove(&key);
                if let Some(v) = v {
                    op.post.insert(key, v);
                }
            }
        });
    }

    /// Replace the declared read set.
    #[track_caller]
    pub fn reads(&self, keys: impl IntoIterator<Item = Key>) -> OpHandle {
        self.edit(|op| op.reads = keys.into_iter().collect())
            .clone()
    }

    /// Replace the declared write set (narrowing the default).
    #[track_caller]
    pub fn writes(&self, keys: impl IntoIterator<Item = Key>) -> OpHandle {
        self.edit(|op| op.writes = keys.into_iter().collect())
            .clone()
    }

    /// A non-zero exit of this op does not fail the run.
    #[track_caller]
    pub fn allow_fail(&self) -> OpHandle {
        self.edit(|op| op.allow_fail = true).clone()
    }

    /// Run this op only when `guard` holds: any composition of earlier
    /// ops' `.changed()` and `.ok()` with `|`, `&` and `!`. A false guard
    /// skips the op (reported `skipped`, with the guard). A second call
    /// replaces the guard.
    #[track_caller]
    pub fn when(&self, guard: Guard) -> OpHandle {
        self.edit(|op| op.guard = Some(guard)).clone()
    }

    /// True when this op changed something (see [`Guard`] for failed and
    /// skipped ops, and for a member of a merged `pkg` op).
    pub fn changed(&self) -> Guard {
        Guard::Changed(self.id)
    }

    /// True when this op ran or was skipped, and did not fail.
    pub fn ok(&self) -> Guard {
        Guard::ExitOk(self.id)
    }

    /// Move this op to the end of its host's run: after the last window's
    /// ops, in a window of its own, in trace order with the other `at_end`
    /// ops. Until then its effects are not visible to fact reads (its
    /// postconditions answer nothing, and it is no implicit barrier).
    #[track_caller]
    pub fn at_end(&self) -> OpHandle {
        self.edit(|op| op.at_end = true).clone()
    }

    /// Declare a postcondition: after this op, `key` has `value`.
    #[track_caller]
    pub fn post(&self, key: Key, value: Value) -> OpHandle {
        self.edit(|op| {
            op.post.insert(key, value);
        })
        .clone()
    }
}

/// The handle methods every op takes (`.allow`, `.reads`, `.writes`,
/// `.allow_fail`, `.when`, `.at_end`, `.post`), on a typed handle that
/// wraps an [`OpHandle`] as its field `0`: each edits the node as the
/// `OpHandle` method does and returns the typed handle, owned, so a chain
/// keeps its type (`h.svc(u).restart().when(g)` is a `SvcHandle`).
macro_rules! op_builders {
    ($t:ty) => {
        impl $t {
            /// See [`OpHandle::allow`].
            #[track_caller]
            pub fn allow(&self, policy: $crate::policy::Policy, reason: impl Into<String>) -> Self {
                self.0.allow(policy, reason);
                self.clone()
            }
            /// See [`OpHandle::reads`].
            #[track_caller]
            pub fn reads(&self, keys: impl IntoIterator<Item = $crate::key::Key>) -> Self {
                self.0.reads(keys);
                self.clone()
            }
            /// See [`OpHandle::writes`].
            #[track_caller]
            pub fn writes(&self, keys: impl IntoIterator<Item = $crate::key::Key>) -> Self {
                self.0.writes(keys);
                self.clone()
            }
            /// See [`OpHandle::allow_fail`].
            #[track_caller]
            pub fn allow_fail(&self) -> Self {
                self.0.allow_fail();
                self.clone()
            }
            /// See [`OpHandle::when`].
            #[track_caller]
            pub fn when(&self, guard: $crate::trace::Guard) -> Self {
                self.0.when(guard);
                self.clone()
            }
            /// See [`OpHandle::at_end`].
            #[track_caller]
            pub fn at_end(&self) -> Self {
                self.0.at_end();
                self.clone()
            }
            /// See [`OpHandle::post`].
            #[track_caller]
            pub fn post(&self, key: $crate::key::Key, value: $crate::key::Value) -> Self {
                self.0.post(key, value);
                self.clone()
            }
        }
    };
}
pub(crate) use op_builders;

/// Errors derived from the finished ops rather than recorded: an `svc`
/// or `unit` that wants nothing, a policy an op trips and does not allow.
fn derived_errors(t: &Trace) -> Vec<TraceError> {
    let mut v = crate::ops::svc::nothing_wanted(t);
    v.extend(crate::ops::unit::errors(t));
    v.extend(crate::ops::pkg::rustup_conflicts(t));
    v.extend(crate::ops::pkg::file_errors(t));
    v.extend(crate::policy::violations(t));
    v.extend(crate::trace::guard_errors(t));
    v
}
