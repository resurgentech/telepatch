//! The trace: a flat, ordered, per-host log of nodes. Nothing here executes.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::facts::os::Os;
use crate::key::{Key, Value};

/// Index of a node in its host's trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub usize);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Who an op's shell is. Mirrors the design's `Principal`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Principal {
    Root,
    User(String),
}

/// Where an op runs. Ops issued on `h` run in the plain root shell
/// (`login: false`). Ops issued through `h.as_user(u)` run in a fresh login
/// environment of `u` (`login: true`), including `as_user("root")`: no
/// privilege change, but the login environment.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RunAs {
    pub principal: Principal,
    pub login: bool,
}

impl RunAs {
    pub fn root_shell() -> RunAs {
        RunAs {
            principal: Principal::Root,
            login: false,
        }
    }
    pub fn login(user: &str) -> RunAs {
        RunAs {
            principal: if user == "root" {
                Principal::Root
            } else {
                Principal::User(user.to_string())
            },
            login: true,
        }
    }
}

impl fmt::Display for RunAs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let who = match &self.principal {
            Principal::Root => "root",
            Principal::User(u) => u,
        };
        if self.login {
            write!(f, "login:{who}")
        } else {
            write!(f, "shell:{who}")
        }
    }
}

/// The inputs of an op. Concrete variants (not trait objects) so the
/// optimizer can merge them; later steps add `Pkg`, `File`, ...
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpSpec {
    /// A placeholder shell line; replaced by `sh!` in a later step.
    Raw { script: String },
    /// A shell command from `sh!`.
    Shell(crate::sh::ShellSpec),
    /// Package install (`h.pkg`, `h.pip`, ...).
    Pkg(crate::ops::pkg::PkgSpec),
    /// File, directory, line edits (`h.file`).
    File(crate::ops::file::FileSpec),
    /// Group exists (`h.group`).
    Group(crate::ops::group::GroupSpec),
    /// Account exists (`h.user`).
    User(crate::ops::user::UserSpec),
    /// A package repository (`h.repo`).
    Repo(crate::ops::repo::RepoSpec),
    /// A systemd unit enabled and/or started (`h.svc`).
    Svc(crate::ops::svc::SvcSpec),
    /// A unit file or drop-in under `/etc/systemd/system` (`h.unit`).
    Unit(crate::ops::unit::UnitSpec),
    /// A git checkout (`h.git`).
    Git(crate::ops::git::GitSpec),
    /// An author-defined resource (`h.resource`).
    Resource(crate::resource::ResourceSpec),
}

impl OpSpec {
    /// Does the op leave what an undo needs to reverse it: its result
    /// carries the inverse data (T75, T92, T95, T97), or the trace holds
    /// the inverse. `--check` counts these per role (`[coverage]`); the
    /// undo log that would run them is a later goal.
    ///
    /// - `pkg` from the system manager or pip: the trailer's `installed`
    ///   (remove those) or the names it removed (reinstall them);
    ///   `build-dep` and the other sources report no such list.
    /// - `group`: `created`. `svc`: `was_enabled`, `was_active`,
    ///   `was_masked`. `git`: `created`, `was_rev`. `resource`: its
    ///   `inverse` script, when it defined one.
    /// - Not yet: `file`, `unit` and `repo` (the note's table captures the
    ///   previous content; no template does yet), `user` (only the
    ///   created case is captured, not the fields of an existing account)
    ///   and `sh` (no `.undo`).
    ///
    /// Exhaustive on purpose: a new op kind must decide.
    pub fn has_inverse(&self) -> bool {
        use crate::ops::pkg::{PkgMode, Source};
        match self {
            OpSpec::Pkg(p) => {
                p.mode == PkgMode::Install && matches!(p.source, Source::Distro(_) | Source::Pip)
            }
            OpSpec::Group(_) | OpSpec::Svc(_) | OpSpec::Git(_) => true,
            OpSpec::Resource(r) => r.inverse.is_some(),
            OpSpec::Raw { .. }
            | OpSpec::Shell(_)
            | OpSpec::File(_)
            | OpSpec::User(_)
            | OpSpec::Repo(_)
            | OpSpec::Unit(_) => false,
        }
    }

    /// Short kind name used in plans.
    pub fn kind(&self) -> &'static str {
        match self {
            OpSpec::Raw { .. } | OpSpec::Shell(_) => "sh",
            OpSpec::Pkg(p) => p.kind(),
            OpSpec::File(_) => "file",
            OpSpec::Group(_) => "group",
            OpSpec::User(_) => "user",
            OpSpec::Repo(_) => "repo",
            OpSpec::Svc(_) => "svc",
            OpSpec::Unit(_) => "unit",
            OpSpec::Git(_) => "git",
            OpSpec::Resource(r) => r.kind,
        }
    }

    /// Which trailer result type this op's template emits.
    pub fn op_kind(&self) -> crate::trailer::OpKind {
        use crate::trailer::OpKind;
        match self {
            OpSpec::Raw { .. } | OpSpec::Shell(_) => OpKind::Sh,
            OpSpec::Pkg(_) => OpKind::Pkg,
            OpSpec::File(_) => OpKind::File,
            OpSpec::Group(_) => OpKind::Group,
            OpSpec::User(_) => OpKind::User,
            OpSpec::Repo(_) => OpKind::Repo,
            OpSpec::Svc(_) => OpKind::Svc,
            // A unit is a file: its result is `rc` and `changed`.
            OpSpec::Unit(_) => OpKind::File,
            OpSpec::Git(_) => OpKind::Git,
            OpSpec::Resource(_) => OpKind::Resource,
        }
    }

    /// The shell script this op runs on a host with the given `os`, as
    /// every reader sees it (plan, tests): secrets are shown as `<secret>`.
    /// What the shell receives is [`OpSpec::exec_script`].
    pub fn script(&self, os: &Os) -> String {
        match self {
            OpSpec::User(u) => u.render(false),
            OpSpec::File(f) => f.render_mode(false),
            OpSpec::Unit(u) => u.render_mode(false),
            _ => self.exec_script(os),
        }
    }

    /// The script as sent to the host: [`OpSpec::script`] with secrets in.
    pub fn exec_script(&self, _os: &Os) -> String {
        match self {
            OpSpec::User(u) => u.render(true),
            OpSpec::Raw { script } => script.clone(),
            OpSpec::Shell(s) => crate::ops::shell::body(s),
            OpSpec::Pkg(p) => p.render(),
            OpSpec::File(f) => f.render_mode(true),
            OpSpec::Group(g) => g.render(),
            OpSpec::Repo(r) => r.render(),
            OpSpec::Svc(s) => s.render(),
            OpSpec::Unit(u) => u.render_mode(true),
            OpSpec::Git(g) => g.render(),
            OpSpec::Resource(r) => r.exec_body(),
        }
    }

    /// Effects of an op of this kind before the caller narrows them.
    pub fn default_writes(&self) -> BTreeSet<Key> {
        match self {
            OpSpec::Raw { .. } | OpSpec::Shell(_) => BTreeSet::from([Key::Any]),
            OpSpec::Pkg(p) => p.default_writes(),
            OpSpec::File(f) => BTreeSet::from([Key::file(&f.path)]),
            OpSpec::Group(g) => BTreeSet::from([Key::group(&g.name)]),
            OpSpec::User(u) => BTreeSet::from([Key::user(&u.name)]),
            OpSpec::Repo(r) => r.default_writes(),
            OpSpec::Svc(s) => BTreeSet::from([Key::svc(s.key_name())]),
            OpSpec::Unit(u) => u.default_writes(),
            OpSpec::Git(g) => g.default_writes(),
            OpSpec::Resource(r) => BTreeSet::from([r.key.clone()]),
        }
    }

    /// Postconditions of an op of this kind before the caller adds more.
    pub fn default_post(&self) -> BTreeMap<Key, Value> {
        use crate::ops::file::FileKind;
        match self {
            OpSpec::Raw { .. } | OpSpec::Shell(_) => BTreeMap::new(),
            OpSpec::Pkg(p) => p.default_post(),
            OpSpec::File(f) => BTreeMap::from([(
                Key::file(&f.path),
                Value::Exists(f.kind != FileKind::Absent),
            )]),
            OpSpec::Group(g) => BTreeMap::from([(Key::group(&g.name), Value::Exists(true))]),
            OpSpec::User(u) => BTreeMap::from([(Key::user(&u.name), Value::Exists(true))]),
            OpSpec::Repo(r) => r.default_post(),
            OpSpec::Svc(s) => BTreeMap::from([(Key::svc(s.key_name()), Value::Exists(true))]),
            OpSpec::Unit(u) => u.default_post(),
            OpSpec::Git(g) => g.default_post(),
            OpSpec::Resource(_) => BTreeMap::new(),
        }
    }
}

/// Run an op only if an earlier op's output says so. Built with an op
/// handle's `.changed()` / `.ok()` and composed with `|`, `&` and `!`;
/// set with `.when(g)`. Evaluated on the host (see [`crate::executor`]).
///
/// A source that ran and exited 0 is `ok`; it is `changed` when its result
/// says so (for a member of a merged `pkg` op: when one of that member's
/// own names was installed). A source that failed (an `allow_fail` op) is
/// neither, so a `changed()` guard on it is false: the guarded op is
/// skipped, not failed. A source that was itself skipped by its guard (or
/// skipped itself, as an `sh!` `creates`) is `ok` and not `changed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Guard {
    Changed(NodeId),
    ExitOk(NodeId),
    Not(Box<Guard>),
    Or(Box<Guard>, Box<Guard>),
    And(Box<Guard>, Box<Guard>),
}

impl Guard {
    /// The ops this guard reads, each once, first appearance first.
    pub fn sources(&self) -> Vec<NodeId> {
        fn walk(g: &Guard, out: &mut Vec<NodeId>) {
            match g {
                Guard::Changed(n) | Guard::ExitOk(n) => {
                    if !out.contains(n) {
                        out.push(*n);
                    }
                }
                Guard::Not(g) => walk(g, out),
                Guard::Or(a, b) | Guard::And(a, b) => {
                    walk(a, out);
                    walk(b, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(self, &mut out);
        out
    }

    /// Binding strength for printing: `|` < `&` < `!` and atoms.
    fn rank(&self) -> u8 {
        match self {
            Guard::Or(..) => 0,
            Guard::And(..) => 1,
            _ => 2,
        }
    }
}

impl std::ops::BitOr for Guard {
    type Output = Guard;
    fn bitor(self, rhs: Guard) -> Guard {
        Guard::Or(Box::new(self), Box::new(rhs))
    }
}

impl std::ops::BitAnd for Guard {
    type Output = Guard;
    fn bitand(self, rhs: Guard) -> Guard {
        Guard::And(Box::new(self), Box::new(rhs))
    }
}

impl std::ops::Not for Guard {
    type Output = Guard;
    fn not(self) -> Guard {
        Guard::Not(Box::new(self))
    }
}

impl fmt::Display for Guard {
    /// `changed(#3)`, `exit_ok(#3)`, `!g`, `a | b`, `a & b`; parentheses
    /// only where binding needs them.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let part = |f: &mut fmt::Formatter<'_>, g: &Guard, min: u8| {
            if g.rank() < min {
                write!(f, "({g})")
            } else {
                write!(f, "{g}")
            }
        };
        match self {
            Guard::Changed(n) => write!(f, "changed({n})"),
            Guard::ExitOk(n) => write!(f, "exit_ok({n})"),
            Guard::Not(g) => {
                f.write_str("!")?;
                part(f, g, 2)
            }
            Guard::Or(a, b) => {
                part(f, a, 0)?;
                f.write_str(" | ")?;
                part(f, b, 0)
            }
            Guard::And(a, b) => {
                part(f, a, 1)?;
                f.write_str(" & ")?;
                part(f, b, 1)
            }
        }
    }
}

/// Guards that cannot be evaluated: one that names a node that is not an
/// op of this host, an op traced after the guarded one, or an `at_end` op
/// from an op that is not `at_end` (it would run after the guarded op).
/// Each error is placed at the guarded op.
pub fn guard_errors(t: &Trace) -> Vec<crate::host::TraceError> {
    let mut out = Vec::new();
    for (i, n) in t.nodes.iter().enumerate() {
        let Node::Op(op) = n else { continue };
        let Some(g) = &op.guard else { continue };
        for s in g.sources() {
            let msg = match t.nodes.get(s.0) {
                Some(Node::Op(_)) if s.0 >= i => {
                    format!("when(..): the guard names {s}, which is traced after this op")
                }
                Some(Node::Op(src)) if src.at_end && !op.at_end => format!(
                    "when(..): the guard names {s}, which runs at the end (at_end); this op does not"
                ),
                Some(Node::Op(_)) => continue,
                _ => format!("when(..): the guard names {s}, which is not an op on this host"),
            };
            let e = crate::host::TraceError {
                message: msg,
                roles: Vec::new(),
                site: None,
                policy: None,
            };
            out.push(t.locate(NodeId(i), e));
        }
    }
    out
}

/// How a fact read was answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Confirmed,
    Postcondition(NodeId),
    /// No answer without a round trip; the engine flushed and read it.
    Block,
}

/// One op. `post` is data (key -> value guaranteed afterwards) rather than
/// the design's `fn(&Key) -> Option<Value>`, so traces stay `Clone`, `Eq`
/// and printable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpNode {
    pub spec: OpSpec,
    pub run_as: RunAs,
    pub reads: BTreeSet<Key>,
    pub writes: BTreeSet<Key>,
    pub guard: Option<Guard>,
    pub post: BTreeMap<Key, Value>,
    /// A failure of this op does not abort the run (honoured by the executor).
    pub allow_fail: bool,
    /// Where the op was called (`h.file(..)` in the role), for its trace
    /// errors. `None` for nodes the engine makes itself.
    pub site: Option<Site>,
    /// Policies this op is permitted to break, with their reasons.
    pub allowed: Vec<crate::policy::Allowance>,
    /// `.at_end()`: the op runs after the host's last window, in a window
    /// of its own.
    pub at_end: bool,
}

/// A source location: the caller of an op builder (`#[track_caller]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Site {
    pub file: &'static str,
    pub line: u32,
}

impl Site {
    /// The caller of the `#[track_caller]` chain this is called in.
    #[track_caller]
    pub fn caller() -> Site {
        let l = std::panic::Location::caller();
        Site {
            file: l.file(),
            line: l.line(),
        }
    }
}

impl fmt::Display for Site {
    /// `file:line`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.file, self.line)
    }
}

/// A trace node. `Op` is far the largest; nodes live in one `Vec` per
/// host and are not moved around, so it is not boxed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum Node {
    Op(OpNode),
    FactRead {
        key: Key,
        value: Value,
        source: Source,
    },
    Barrier,
    RoleBegin(String),
    RoleEnd(String),
}

/// A host's ordered node log (the host itself is implicit: one trace per host).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Trace {
    pub nodes: Vec<Node>,
    /// Playbook bugs found while tracing (bad package name, unsupported
    /// os family, ...). A non-empty list fails the run before anything is
    /// applied.
    pub errors: Vec<crate::host::TraceError>,
    /// Failed `h.assert` messages, in trace order. Prepare blocks the host
    /// with them before the window's ops run.
    pub assertions: Vec<String>,
    /// `--only`: the roles whose bodies run (see [`Trace::selects`]).
    /// `None` runs every role.
    pub only: Option<std::rc::Rc<std::collections::BTreeSet<String>>>,
    /// The roles `--only` skipped, in trace order, each once.
    pub not_selected: Vec<String>,
}

fn role_stack(nodes: &[Node]) -> Vec<String> {
    let mut stack = Vec::new();
    for n in nodes {
        match n {
            Node::RoleBegin(r) => stack.push(r.clone()),
            Node::RoleEnd(_) => {
                stack.pop();
            }
            _ => {}
        }
    }
    stack
}

impl Trace {
    /// The site of every `sh!` in the trace, first appearance first, each once.
    pub fn sh_sites(&self) -> Vec<Site> {
        sh_sites(self.nodes.iter().filter_map(|n| match n {
            Node::Op(op) => Some(op),
            _ => None,
        }))
    }

    pub fn push(&mut self, node: Node) -> NodeId {
        self.nodes.push(node);
        NodeId(self.nodes.len() - 1)
    }

    /// The roles open at node `id` (outermost first), from the
    /// `RoleBegin`/`RoleEnd` marks before it.
    pub fn roles_at(&self, id: NodeId) -> Vec<String> {
        role_stack(&self.nodes[..id.0.min(self.nodes.len())])
    }

    /// The roles open now: those a node pushed next would be traced in.
    pub fn open_roles(&self) -> Vec<String> {
        role_stack(&self.nodes)
    }

    /// Does `--only` run the body of a role named `name`, entered now? A
    /// role entered inside a running role is part of it and runs whole; a
    /// role entered at the top runs only if it is named. (A skipped body
    /// never runs, so no skipped role is ever open here.)
    pub fn selects(&self, name: &str) -> bool {
        match &self.only {
            None => true,
            Some(only) => only.contains(name) || !self.open_roles().is_empty(),
        }
    }

    /// Every role name traced so far, first appearance first, each once.
    pub fn traced_roles(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for n in &self.nodes {
            if let Node::RoleBegin(r) = n
                && !out.contains(r)
            {
                out.push(r.clone());
            }
        }
        out
    }

    /// `e` placed at op `id`: its site (unless it has one) and the roles
    /// the op was traced in (unless it has them).
    pub fn locate(&self, id: NodeId, mut e: crate::host::TraceError) -> crate::host::TraceError {
        if e.site.is_none()
            && let Some(Node::Op(op)) = self.nodes.get(id.0)
        {
            e.site = op.site;
        }
        if e.roles.is_empty() {
            e.roles = self.roles_at(id);
        }
        e
    }

    /// FactReads that needed a round trip.
    pub fn blocking_reads(&self) -> usize {
        self.nodes
            .iter()
            .filter(|n| {
                matches!(
                    n,
                    Node::FactRead {
                        source: Source::Block,
                        ..
                    }
                )
            })
            .count()
    }
}

/// The `sh!` sites of `ops`, in order, each once.
pub fn sh_sites<'a>(ops: impl IntoIterator<Item = &'a OpNode>) -> Vec<Site> {
    let mut out: Vec<Site> = Vec::new();
    for op in ops {
        if let OpSpec::Shell(s) = &op.spec
            && let Some(site) = s.site
            && !out.contains(&site)
        {
            out.push(site);
        }
    }
    out
}
