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
/// (`login: false`). Ops issued through `h.as_user(u)` run in `u`'s
/// persistent login shell (`login: true`), one per user per host, opened on
/// first use and fed over stdin like every other shell. That includes
/// `as_user("root")`: no privilege change, but root's login environment
/// (its profile read once, when the shell opens), in a shell of its own
/// beside the plain root shell.
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
    /// Wait for a condition on the host (`h.wait_for`).
    Wait(crate::ops::wait::Wait),
    /// `h.as_user(u).relogin()`: no host effect; marks `u`'s login shell
    /// stale at its place in the trace (rule 6, [`OpNode::stales`]). The
    /// name is the user (`"root"` for `as_user("root")`).
    Relogin(String),
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
    /// - `file` and `unit` (T106): `existed`, the previous type, mode and
    ///   owner, the topmost directory the op created, and the backup of
    ///   what it overwrites or removes (`FileResult`), saved on the
    ///   target's own filesystem, indexed under the run's directory.
    /// - `repo` (T107; not `RepoKind::Unset`, which fails when it runs):
    ///   whether the op created the key and the source file, and the backup
    ///   of a source it rewrote (`RepoResult`).
    /// - `user` (T107): `created`, the shell `usermod -s` replaced and the
    ///   groups `usermod -aG` added; never a password hash.
    /// - `sh` (T108): only with `.undo(..)`; the command is recorded, not
    ///   run. `Raw` (the toy op) never.
    ///
    /// Exhaustive on purpose: a new op kind must decide.
    pub fn has_inverse(&self) -> bool {
        use crate::ops::pkg::{PkgMode, Source};
        match self {
            OpSpec::Pkg(p) => {
                p.mode == PkgMode::Install && matches!(p.source, Source::Distro(_) | Source::Pip)
            }
            OpSpec::Group(_)
            | OpSpec::Svc(_)
            | OpSpec::Git(_)
            | OpSpec::File(_)
            | OpSpec::Unit(_)
            | OpSpec::User(_) => true,
            OpSpec::Repo(r) => r.kind != crate::ops::repo::RepoKind::Unset,
            OpSpec::Resource(r) => r.inverse.is_some(),
            // Changes nothing, so there is nothing to undo: not a hole.
            OpSpec::Wait(_) | OpSpec::Relogin(_) => true,
            // The supplied command, or none.
            OpSpec::Shell(s) => s.undo.is_some(),
            OpSpec::Raw { .. } => false,
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
            OpSpec::Wait(_) => "wait",
            OpSpec::Relogin(_) => "relogin",
        }
    }

    /// Which trailer result type this op's template emits.
    pub fn op_kind(&self) -> crate::trailer::OpKind {
        use crate::trailer::OpKind;
        match self {
            OpSpec::Raw { .. } | OpSpec::Shell(_) | OpSpec::Wait(_) | OpSpec::Relogin(_) => {
                OpKind::Sh
            }
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
            OpSpec::Wait(w) => format!("wait_for {}", w.describe()),
            OpSpec::Relogin(u) => format!("relogin {u}"),
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
            // The body keeps its own deadline, which only the op's node
            // knows ([`crate::trailer`] renders it with the node's).
            OpSpec::Wait(w) => w.render(crate::ops::wait::DEFAULT_WAIT),
            // Nothing to run: the controller marks the shell stale when
            // the op's result arrives.
            OpSpec::Relogin(_) => "rc=0; changed=0; skipped=0\n".to_string(),
        }
    }

    /// How long an op of this kind may run when it sets no
    /// `.timeout(..)`: `pkg` long (a source build or a big download), `sh`,
    /// raw, resources and git medium, `file` and `repo` short, `svc`
    /// shorter, `unit`, `group` and `user` shortest. The controller
    /// enforces it ([`crate::executor`]); `wait_for` 5 minutes.
    pub fn default_timeout(&self) -> std::time::Duration {
        use std::time::Duration;
        const MIN: u64 = 60;
        Duration::from_secs(match self {
            OpSpec::Pkg(_) => 30 * MIN,
            OpSpec::Raw { .. } | OpSpec::Shell(_) | OpSpec::Resource(_) | OpSpec::Git(_) => {
                10 * MIN
            }
            OpSpec::File(_) | OpSpec::Repo(_) => 5 * MIN,
            OpSpec::Svc(_) => 2 * MIN,
            OpSpec::Unit(_) | OpSpec::Group(_) | OpSpec::User(_) | OpSpec::Relogin(_) => MIN,
            OpSpec::Wait(_) => return crate::ops::wait::DEFAULT_WAIT,
        })
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
            OpSpec::Wait(_) | OpSpec::Relogin(_) => BTreeSet::new(),
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
            OpSpec::Resource(_) | OpSpec::Wait(_) | OpSpec::Relogin(_) => BTreeMap::new(),
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
    /// `.timeout(d)`: how long one run of this op may take, counted from
    /// its start line; the controller stops it after that. `None`: the
    /// kind's default ([`OpSpec::default_timeout`]).
    pub timeout: Option<std::time::Duration>,
    /// `.retry(n)`: a failed or timed-out op is sent again by the
    /// controller, up to `n` more times, before it counts as failed. The
    /// op ends its pipelined run.
    pub retry: u32,
    /// `.needs_reboot()`: when this op changes something, the change takes
    /// effect only after a reboot. The executor writes a mark on the host
    /// ([`crate::reboot`]); a `h.reboot().if_pending()` barrier reads it.
    pub needs_reboot: bool,
}

/// Which login shells a changed op stales ([`OpNode::stales`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaleScope {
    /// One user's login shell (`"root"` for `as_user("root")`).
    User(String),
    /// Every login shell on the host.
    Every,
}

impl StaleScope {
    /// Does this scope reach the login shell of `user`?
    pub fn hits(&self, user: &str) -> bool {
        match self {
            StaleScope::Every => true,
            StaleScope::User(u) => u == user,
        }
    }
}

/// What an op does to login shells, decided from its spec, `run_as` and
/// write keys alone ([`OpNode::stales`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stales {
    pub scope: StaleScope,
    /// The rule's short name, as the stream line says it: `user op`,
    /// `pkg source`, `distro pkg`, `profile write`, `sh writes *`,
    /// `relogin`.
    pub rule: &'static str,
    /// The shells go stale whether or not the op changed something
    /// (`relogin()` only); every other rule applies to a changed op.
    pub always: bool,
}

/// The login name of a principal: `"root"` for root.
pub fn login_name(p: &Principal) -> &str {
    match p {
        Principal::Root => "root",
        Principal::User(u) => u,
    }
}

/// The files a login shell reads in its user's home, and the directory of
/// `environment.d` drop-ins.
const HOME_PROFILE_FILES: [&str; 4] = [".profile", ".bash_profile", ".bash_login", ".bashrc"];
const HOME_ENV_DIR: &str = ".config/environment.d/";

/// The user and home-relative rest of a path under a user's home: `~/x`
/// (the op's own principal), `~u/x`, an absolute path under a home in
/// `homes`, else under `/root` or `/home/<u>`.
fn home_path<'a>(
    path: &'a str,
    own: &'a str,
    homes: &'a BTreeMap<String, String>,
) -> Option<(&'a str, &'a str)> {
    if let Some(t) = path.strip_prefix('~') {
        let (user, rest) = t.split_once('/')?;
        return Some((if user.is_empty() { own } else { user }, rest));
    }
    for (u, h) in homes {
        if let Some(rest) = path
            .strip_prefix(h.as_str())
            .and_then(|r| r.strip_prefix('/'))
            .filter(|_| !h.is_empty())
        {
            return Some((u, rest));
        }
    }
    if let Some(rest) = path.strip_prefix("/root/") {
        return Some(("root", rest));
    }
    let t = path.strip_prefix("/home/")?;
    let (user, rest) = t.split_once('/')?;
    Some((user, rest))
}

/// Rule 4: a write to a profile path stales every login shell (the
/// system files) or its user's (the files in a home).
fn profile_scope(path: &str, own: &str, homes: &BTreeMap<String, String>) -> Option<StaleScope> {
    let system = matches!(
        path,
        "/etc/profile" | "/etc/bash.bashrc" | "/etc/environment"
    ) || path
        .strip_prefix("/etc/profile.d/")
        .is_some_and(|r| !r.is_empty());
    if system {
        return Some(StaleScope::Every);
    }
    let (user, rest) = home_path(path, own, homes)?;
    let profile = HOME_PROFILE_FILES.contains(&rest)
        || rest
            .strip_prefix(HOME_ENV_DIR)
            .is_some_and(|r| !r.is_empty());
    (profile && !user.is_empty()).then(|| StaleScope::User(user.to_string()))
}

impl OpNode {
    /// What this op does to login shells when it changes something (T114,
    /// rules 1-6), decided from the spec, `run_as` and write keys; `None`
    /// when it stales none. A login shell read the profile once, at open:
    ///
    /// 1. a `user` op stales that user's shell (a `group` op would for its
    ///    members, but a group spec names none);
    /// 2. a `pkg` op with a non-distro source (pip, cargo, mise, rustup,
    ///    script, binary release) stales its principal's shell; run as
    ///    root, every shell, since the source can drop a file in
    ///    `/etc/profile.d`;
    /// 3. a distro `pkg` op stales every shell, whoever runs it;
    /// 4. any op whose write keys include a profile path: `/etc/profile`,
    ///    `/etc/profile.d/*`, `/etc/bash.bashrc`, `/etc/environment` stale
    ///    every shell; `~u/.profile`, `.bash_profile`, `.bash_login`,
    ///    `.bashrc` and `.config/environment.d/*` stale `u`'s. `~` is the
    ///    op's own principal; an absolute path counts when it is under a
    ///    home in `homes` (the homes the executor learned), else under
    ///    `/root` or `/home/<u>`. Narrowed `sh`, raw and resource writes
    ///    follow this rule;
    /// 5. an `sh`, raw or resource op that writes `*` stales every shell;
    /// 6. `relogin()` stales its user's shell, changed or not.
    ///
    /// When several rules apply the broadest wins; two different users'
    /// profiles in one op's writes stale every shell.
    pub fn stales(&self, homes: &BTreeMap<String, String>) -> Option<Stales> {
        use crate::ops::pkg::Source;
        let own = login_name(&self.run_as.principal);
        let mut found: Vec<(StaleScope, &'static str)> = Vec::new();
        match &self.spec {
            OpSpec::Relogin(u) => {
                return Some(Stales {
                    scope: StaleScope::User(u.clone()),
                    rule: "relogin",
                    always: true,
                });
            }
            OpSpec::User(u) => found.push((StaleScope::User(u.name.clone()), "user op")),
            OpSpec::Pkg(p) => match p.source {
                Source::Distro(_) => found.push((StaleScope::Every, "distro pkg")),
                _ => found.push((
                    match &self.run_as.principal {
                        Principal::Root => StaleScope::Every,
                        Principal::User(u) => StaleScope::User(u.clone()),
                    },
                    "pkg source",
                )),
            },
            _ => {}
        }
        if matches!(
            self.spec,
            OpSpec::Raw { .. } | OpSpec::Shell(_) | OpSpec::Resource(_)
        ) && self.writes.contains(&Key::Any)
        {
            found.push((StaleScope::Every, "sh writes *"));
        }
        for k in &self.writes {
            if let Key::File(path) = k
                && let Some(s) = profile_scope(path, own, homes)
            {
                found.push((s, "profile write"));
            }
        }
        let first = found.first()?.clone();
        let every = found.iter().find(|(s, _)| *s == StaleScope::Every);
        let (scope, rule) = match every {
            Some(e) => e.clone(),
            // Different users: no single user's shell holds them all.
            None if found.iter().any(|(s, _)| *s != first.0) => (StaleScope::Every, first.1),
            None => first,
        };
        Some(Stales {
            scope,
            rule,
            always: false,
        })
    }

    /// Can this op, if it changes, stale the login shell it runs in
    /// ([`OpNode::stales`], decided before it runs)? Such an op ends its
    /// pipelined run: nothing queued behind it may run in the old
    /// environment. An op of the plain root shell has no login shell.
    pub fn stales_own_shell(&self, homes: &BTreeMap<String, String>) -> bool {
        self.run_as.login
            && self
                .stales(homes)
                .is_some_and(|s| s.scope.hits(login_name(&self.run_as.principal)))
    }
}

/// What the controller adds to a `wait_for`'s own deadline.
pub const WAIT_BACKSTOP: std::time::Duration = std::time::Duration::from_secs(15);

impl OpNode {
    /// The timeout the controller enforces: the op's own, else its
    /// kind's default.
    pub fn effective_timeout(&self) -> std::time::Duration {
        self.timeout.unwrap_or_else(|| self.spec.default_timeout())
    }

    /// The controller's deadline for one attempt: [`Self::effective_timeout`],
    /// and for a `wait_for`, which keeps its own deadline in its body, 15
    /// seconds more (a backstop for a probe that hangs).
    pub fn enforced_timeout(&self) -> std::time::Duration {
        let t = self.effective_timeout();
        match self.spec {
            OpSpec::Wait(_) => t + WAIT_BACKSTOP,
            _ => t,
        }
    }
}

/// `90s` as `1m30s`, `5m`, `2s`, `1.5s`, `250ms`: how plans and reports
/// write a duration.
pub fn fmt_duration(d: std::time::Duration) -> String {
    let ms = d.as_millis();
    if ms == 0 {
        return "0s".to_string();
    }
    if ms < 1000 {
        return format!("{ms}ms");
    }
    let (secs, frac) = (ms / 1000, ms % 1000);
    let mut o = String::new();
    if secs >= 3600 {
        o.push_str(&format!("{}h", secs / 3600));
    }
    if secs % 3600 >= 60 {
        o.push_str(&format!("{}m", secs % 3600 / 60));
    }
    if secs % 60 > 0 || frac > 0 || o.is_empty() {
        if frac > 0 {
            let f = format!("{frac:03}");
            o.push_str(&format!("{}.{}s", secs % 60, f.trim_end_matches('0')));
        } else {
            o.push_str(&format!("{}s", secs % 60));
        }
    }
    o
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
    /// `h.reboot().if_pending()`: a hard fence. Everything before it is
    /// applied, the host reboots once if anything is pending, and what
    /// follows runs on the rebooted host. Never part of a window: the
    /// engine splits the pending ops at it ([`crate::engine`]).
    Reboot {
        site: Site,
    },
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

#[cfg(test)]
mod limit_tests {
    use std::time::Duration;

    use super::*;
    use crate::ops::pkg::{Manager, PkgSpec};

    fn node(spec: OpSpec) -> OpNode {
        OpNode {
            writes: spec.default_writes(),
            post: spec.default_post(),
            spec,
            run_as: RunAs::root_shell(),
            reads: Default::default(),
            guard: None,
            allow_fail: false,
            site: None,
            allowed: Vec::new(),
            at_end: false,
            timeout: None,
            retry: 0,
            needs_reboot: false,
        }
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn each_kind_has_a_default_timeout_pkg_long_sh_medium_svc_short() {
        let pkg = node(OpSpec::Pkg(PkgSpec::new(Manager::Apt, vec!["curl".into()])));
        let sh = node(OpSpec::Raw {
            script: "true".into(),
        });
        let svc = node(OpSpec::Svc(crate::ops::svc::SvcSpec::new("docker")));
        let grp = node(OpSpec::Group(crate::ops::group::GroupSpec::new("g")));
        let file = node(OpSpec::File(crate::ops::file::FileSpec::new("/x")));
        let git = node(OpSpec::Git(crate::ops::git::GitSpec::new("u")));
        assert_eq!(pkg.effective_timeout(), secs(30 * 60));
        assert_eq!(sh.effective_timeout(), secs(10 * 60));
        assert_eq!(git.effective_timeout(), secs(10 * 60));
        assert_eq!(file.effective_timeout(), secs(5 * 60));
        assert_eq!(svc.effective_timeout(), secs(2 * 60));
        assert_eq!(grp.effective_timeout(), secs(60));
    }

    #[test]
    fn an_explicit_timeout_overrides_the_default() {
        let mut sh = node(OpSpec::Raw {
            script: "true".into(),
        });
        sh.timeout = Some(secs(7));
        assert_eq!(sh.effective_timeout(), secs(7));
    }

    #[test]
    fn wait_for_defaults_to_five_minutes_and_the_controller_adds_a_backstop() {
        let mut w = node(OpSpec::Wait(crate::ops::wait::Wait::Tcp(1)));
        assert_eq!(w.effective_timeout(), secs(5 * 60));
        assert_eq!(w.enforced_timeout(), secs(5 * 60 + 15));
        w.timeout = Some(secs(60));
        assert_eq!(w.enforced_timeout(), secs(75));
        // Every other kind: the controller's deadline is the limit.
        let mut sh = node(OpSpec::Raw {
            script: "true".into(),
        });
        sh.timeout = Some(secs(7));
        assert_eq!(sh.enforced_timeout(), secs(7));
    }

    // ---- staleness: which login shells a changed op stales (T114) ----

    fn run_as(mut n: OpNode, who: RunAs) -> OpNode {
        n.run_as = who;
        n
    }

    fn pkg_from(source: crate::ops::pkg::Source, who: RunAs) -> OpNode {
        let mut spec = PkgSpec::from_source(source, vec!["x".into()]);
        if let Principal::User(u) = &who.principal {
            spec.user = u.clone();
        }
        run_as(node(OpSpec::Pkg(spec)), who)
    }

    fn st(n: &OpNode) -> Option<(StaleScope, &'static str)> {
        n.stales(&BTreeMap::new()).map(|s| (s.scope, s.rule))
    }

    fn user_scope(u: &str) -> StaleScope {
        StaleScope::User(u.to_string())
    }

    #[test]
    fn rule_1_a_user_op_stales_that_users_shell() {
        let n = node(OpSpec::User(crate::ops::user::UserSpec::new("alice")));
        assert_eq!(st(&n), Some((user_scope("alice"), "user op")));
        // Whoever runs it.
        let n = run_as(n, RunAs::login("root"));
        assert_eq!(st(&n), Some((user_scope("alice"), "user op")));
        let n = node(OpSpec::User(crate::ops::user::UserSpec::new("root")));
        assert_eq!(st(&n), Some((user_scope("root"), "user op")));
    }

    #[test]
    fn rule_1_a_group_op_names_no_members_so_it_stales_nothing() {
        let n = node(OpSpec::Group(crate::ops::group::GroupSpec::new("docker")));
        assert_eq!(st(&n), None);
    }

    #[test]
    fn rule_2_a_non_distro_source_stales_the_users_shell_or_every_shell_as_root() {
        use crate::ops::pkg::Source;
        let alice = RunAs::login("alice");
        for src in [Source::Pip, Source::Cargo { locked: false }, Source::Mise] {
            assert_eq!(
                st(&pkg_from(src.clone(), alice.clone())),
                Some((user_scope("alice"), "pkg source")),
                "{src:?}"
            );
            for root in [RunAs::root_shell(), RunAs::login("root")] {
                assert_eq!(
                    st(&pkg_from(src.clone(), root)),
                    Some((StaleScope::Every, "pkg source")),
                    "{src:?}"
                );
            }
        }
    }

    #[test]
    fn rule_3_a_distro_pkg_stales_every_shell_whoever_runs_it() {
        use crate::ops::pkg::Source;
        let d = || Source::Distro(Manager::Apt);
        for who in [
            RunAs::root_shell(),
            RunAs::login("root"),
            RunAs::login("alice"),
        ] {
            assert_eq!(
                st(&pkg_from(d(), who)),
                Some((StaleScope::Every, "distro pkg"))
            );
        }
    }

    #[test]
    fn rule_4_profile_paths_stale_every_shell_or_the_owners() {
        let file = |path: &str, who: RunAs| {
            run_as(
                node(OpSpec::File(crate::ops::file::FileSpec::new(path))),
                who,
            )
        };
        let root = RunAs::login("root");
        for p in [
            "/etc/profile",
            "/etc/profile.d/mise.sh",
            "/etc/bash.bashrc",
            "/etc/environment",
        ] {
            assert_eq!(
                st(&file(p, root.clone())),
                Some((StaleScope::Every, "profile write")),
                "{p}"
            );
        }
        // `~` is the principal's home, `~u` is u's.
        for p in [
            "~/.profile",
            "~/.bash_profile",
            "~/.bash_login",
            "~/.bashrc",
            "~/.config/environment.d/10-x.conf",
        ] {
            assert_eq!(
                st(&file(p, RunAs::login("alice"))),
                Some((user_scope("alice"), "profile write")),
                "{p}"
            );
            assert_eq!(
                st(&file(p, root.clone())),
                Some((user_scope("root"), "profile write")),
                "{p}"
            );
        }
        assert_eq!(
            st(&file("~bob/.bashrc", root.clone())),
            Some((user_scope("bob"), "profile write"))
        );
        // An absolute path in a known home, and the default homes.
        assert_eq!(
            st(&file("/home/carol/.profile", root.clone())),
            Some((user_scope("carol"), "profile write"))
        );
        assert_eq!(
            st(&file("/root/.bash_profile", RunAs::root_shell())),
            Some((user_scope("root"), "profile write"))
        );
        let homes = BTreeMap::from([("root".to_string(), "/var/root".to_string())]);
        let n = file("/var/root/.bashrc", RunAs::root_shell());
        assert_eq!(
            n.stales(&homes).map(|s| (s.scope, s.rule)),
            Some((user_scope("root"), "profile write"))
        );
        assert_eq!(st(&n), None, "unknown home: not a profile path");
        // Not profile paths.
        for p in [
            "/etc/profile.d",
            "/etc/profiled/x",
            "/etc/hosts",
            "~/.vimrc",
            "~/notes/.bashrc",
            "/home/carol/.config/other/x",
            "/tmp/.bashrc",
        ] {
            assert_eq!(st(&file(p, root.clone())), None, "{p}");
        }
    }

    #[test]
    fn rule_4_a_narrowed_sh_follows_its_write_keys_and_a_unit_is_a_file() {
        let mut n = run_as(
            node(OpSpec::Raw {
                script: "true".into(),
            }),
            RunAs::login("alice"),
        );
        n.writes = BTreeSet::from([Key::file("~/.bashrc"), Key::file("/tmp/y")]);
        assert_eq!(st(&n), Some((user_scope("alice"), "profile write")));
        n.writes = BTreeSet::from([Key::file("/etc/profile.d/x.sh")]);
        assert_eq!(st(&n), Some((StaleScope::Every, "profile write")));
        n.writes = BTreeSet::from([Key::file("/tmp/y"), Key::pkg("x")]);
        assert_eq!(st(&n), None);
        // The broadest of several wins.
        n.writes = BTreeSet::from([Key::file("~/.bashrc"), Key::file("/etc/environment")]);
        assert_eq!(st(&n), Some((StaleScope::Every, "profile write")));
        let unit = node(OpSpec::Unit(crate::ops::unit::UnitSpec::new("a.service")));
        assert_eq!(st(&unit), None);
    }

    #[test]
    fn rule_5_an_sh_raw_or_resource_op_that_writes_star_stales_every_shell() {
        let raw = node(OpSpec::Raw {
            script: "true".into(),
        });
        assert!(raw.writes.contains(&Key::Any));
        for who in [RunAs::root_shell(), RunAs::login("alice")] {
            assert_eq!(
                st(&run_as(raw.clone(), who)),
                Some((StaleScope::Every, "sh writes *"))
            );
        }
        // Narrowed to something that is no profile: nothing.
        let mut narrowed = raw;
        narrowed.writes = BTreeSet::from([Key::file("/tmp/y")]);
        assert_eq!(st(&narrowed), None);
        // No writes at all: nothing.
        narrowed.writes.clear();
        assert_eq!(st(&narrowed), None);
    }

    #[test]
    fn rule_6_relogin_stales_its_user_and_always() {
        let n = run_as(node(OpSpec::Relogin("alice".into())), RunAs::login("alice"));
        let s = n.stales(&BTreeMap::new()).unwrap();
        assert_eq!(
            (s.scope, s.rule, s.always),
            (user_scope("alice"), "relogin", true)
        );
        // The others stale only when they changed.
        let u = node(OpSpec::User(crate::ops::user::UserSpec::new("alice")));
        assert!(!u.stales(&BTreeMap::new()).unwrap().always);
    }

    #[test]
    fn an_op_that_stales_nothing_and_the_scope_arithmetic() {
        let svc = node(OpSpec::Svc(crate::ops::svc::SvcSpec::new("docker")));
        assert_eq!(st(&svc), None);
        let git = node(OpSpec::Git(crate::ops::git::GitSpec::new("u")));
        assert_eq!(st(&git), None);
        let w = node(OpSpec::Wait(crate::ops::wait::Wait::Tcp(1)));
        assert_eq!(st(&w), None);
        assert!(StaleScope::Every.hits("anyone"));
        assert!(user_scope("a").hits("a"));
        assert!(!user_scope("a").hits("b"));
    }

    #[test]
    fn an_op_stales_its_own_shell_when_the_scope_reaches_its_login_shell() {
        let homes = BTreeMap::new();
        let user_op = node(OpSpec::User(crate::ops::user::UserSpec::new("alice")));
        // Run in alice's own login shell: its own. In root's: not its own.
        assert!(run_as(user_op.clone(), RunAs::login("alice")).stales_own_shell(&homes));
        assert!(!run_as(user_op.clone(), RunAs::login("root")).stales_own_shell(&homes));
        assert!(!run_as(user_op, RunAs::root_shell()).stales_own_shell(&homes));
        // Every login shell includes the op's own; the plain root shell has none.
        let raw = node(OpSpec::Raw {
            script: "true".into(),
        });
        assert!(run_as(raw.clone(), RunAs::login("alice")).stales_own_shell(&homes));
        assert!(!run_as(raw, RunAs::root_shell()).stales_own_shell(&homes));
        let re = run_as(node(OpSpec::Relogin("alice".into())), RunAs::login("alice"));
        assert!(re.stales_own_shell(&homes));
    }

    #[test]
    fn durations_print_the_way_a_person_writes_them() {
        let ms = Duration::from_millis;
        assert_eq!(fmt_duration(ms(250)), "250ms");
        assert_eq!(fmt_duration(secs(2)), "2s");
        assert_eq!(fmt_duration(ms(1500)), "1.5s");
        assert_eq!(fmt_duration(secs(90)), "1m30s");
        assert_eq!(fmt_duration(secs(300)), "5m");
        assert_eq!(fmt_duration(secs(3600 + 120)), "1h2m");
        assert_eq!(fmt_duration(Duration::ZERO), "0s");
    }
}
