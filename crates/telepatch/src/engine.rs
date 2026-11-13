//! The engine loop: drive one playbook future per host; flush on a read.
//!
//! Per host: poll the future. `Done` -> apply everything pending. `Pending`
//! with registered reads -> (1) apply non-mutating nodes only, (2) if a read
//! needs the effect of a pending mutation and no postcondition answered it,
//! apply pending mutations up to the last such writer (an implicit barrier),
//! (3) read the facts in one round trip, store them as confirmed, and poll
//! again. Explicit `barrier()` nodes never force execution; they only fence
//! hoisting of non-mutating nodes (and are edges for the optimizer, T8).
//!
//! A reboot node ([`Node::Reboot`], `h.reboot().if_pending()`) is a hard
//! fence. No window ever holds ops from both sides of it: before a read
//! blocks, and again when the trace ends, the engine applies every pending
//! op before each uncrossed reboot node, asks the executor to cross it
//! ([`Executor::reboot`]) and only then goes on, so a read after the node,
//! and every op after it, meets the rebooted host. The executor decides
//! what crossing is (nothing pending: nothing; a halted host: nothing).
//!
//! A check-only executor ([`Executor::check_only`]) applies nothing, so an
//! implicit barrier cannot be crossed: the engine hands the window before
//! it to the executor (prepare runs for it) and stops tracing with
//! [`EngineError::CheckStopped`].
//!
//! A window whose op installs `curl` or `wget` before an op that downloads
//! carries a [`Split`]. The executor may decline such a window for its head
//! ([`Applied::Head`]) when prepare finds no downloader on the host; the
//! engine then applies the head as that window and forms the rest again as
//! the next. A declined window leaves no trace in the engine's carried
//! state.

use std::collections::BTreeSet;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use crate::facts::os::Os;
use crate::facts::query::FactQuery;
use crate::host::{Host, TraceError};
use crate::key::{Key, Value};
use crate::ops::pkg::apt_update_flags;
use crate::optimize::{Item, optimize};
use crate::plan::node_roles;
use crate::resolver::Resolved;
use crate::role::Playbook;
use crate::trace::{Node, NodeId, OpNode, OpSpec, Principal, Site};

/// The host an executor call is about.
#[derive(Debug, Clone)]
pub struct HostInfo {
    pub name: String,
    pub os: Rc<Os>,
}

/// Why a window of ops was handed to the executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowKind {
    /// Non-mutating nodes flushed because the playbook blocked on a read.
    NonMutating,
    /// Pending mutations applied because a read needs their effect.
    ImplicitBarrier,
    /// The trace ended: everything left.
    Final,
    /// After the final window: the `.at_end()` ops.
    AtEnd,
}

/// A trace op folded into a [`WindowOp`], with the role that traced it
/// (nested roles joined with ` > `; empty outside any role).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub id: NodeId,
    pub role: String,
    /// The names this member asked for, for a `pkg` op (empty otherwise):
    /// a member of a merged `pkg` op changed when one of them was installed.
    pub pkg_names: Vec<String>,
    /// `.needs_reboot()` on this member's own op: it marks the host when it
    /// changed (a merged op's other members may not be marked).
    pub needs_reboot: bool,
}

impl Member {
    /// The member for trace op `id`, traced in `role`.
    pub fn of(id: NodeId, role: &str, op: &OpNode) -> Member {
        Member {
            id,
            role: role.to_string(),
            pkg_names: match &op.spec {
                OpSpec::Pkg(p) => p.member_names(),
                _ => Vec::new(),
            },
            needs_reboot: op.needs_reboot,
        }
    }
}

/// One op of a window. `id` is the trace id of the member whose position
/// the op takes: the earliest for a merged `pkg` op, the latest for equal
/// guarded ops merged with their guards or-joined. `members` lists the
/// trace ops it stands for in trace order: one for an ordinary op, several
/// for a merged op (whose `node` then carries the union of names, effects
/// and postconditions).
#[derive(Debug, Clone)]
pub struct WindowOp {
    pub id: NodeId,
    pub node: OpNode,
    pub members: Vec<Member>,
}

/// One batch of ops for the executor. `number` counts round trips on this
/// host (1-based).
#[derive(Debug, Clone)]
pub struct Window {
    pub number: usize,
    pub kind: WindowKind,
    pub ops: Vec<WindowOp>,
    /// Where the window splits when prepare finds neither `curl` nor
    /// `wget` on the host ([`split_candidate`]).
    pub split: Option<Split>,
    /// Failed `h.assert` messages. Prepare returns these and applies nothing.
    pub assertions: Vec<String>,
}

/// A window's split candidate: an op of the window installs a downloader
/// that a later op of the window needs. Decided in prepare: with `curl` or
/// `wget` on the host the window runs whole; without, the executor declines
/// it ([`Applied::Head`]) and the engine runs `ops[..=at]` as this window
/// and the rest, formed again, as the next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Split {
    /// The index in `ops` of the installing op: the head is `ops[..=at]`.
    pub at: usize,
    /// The installing op's id.
    pub installer: NodeId,
    /// The downloader it names: `curl` (when it names both) or `wget`.
    pub tool: String,
    /// The first op after it that needs a download.
    pub consumer: NodeId,
}

/// What the executor did with a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// It ran (or, on a halted host, reported its ops not run).
    All,
    /// Nothing of it was sent or reported: prepare found no downloader,
    /// and only ops after the window's [`Split`] needed one. The engine
    /// applies the first `n` ops (`ops[..n]`) as this window, then the
    /// rest as the next.
    Head(usize),
}

/// Does `op` need a download in prepare: a file op placing or unpacking a
/// fetch, or a binary release?
pub fn needs_download(op: &OpNode) -> bool {
    match &op.spec {
        OpSpec::File(f) => f.src.is_some(),
        OpSpec::Pkg(p) => {
            p.file.is_some() || matches!(p.source, crate::ops::pkg::Source::BinaryRelease(_))
        }
        _ => false,
    }
}

/// The downloader `op` installs: a distro `pkg` op (not build-dep) naming
/// `curl` or `wget`.
fn installs_downloader(op: &OpNode) -> Option<&'static str> {
    let OpSpec::Pkg(p) = &op.spec else {
        return None;
    };
    if !matches!(p.source, crate::ops::pkg::Source::Distro(_))
        || p.mode != crate::ops::pkg::PkgMode::Install
    {
        return None;
    }
    ["curl", "wget"]
        .into_iter()
        .find(|t| p.names.iter().any(|n| n == t))
}

/// The split candidate of a scheduled window: the first op that installs
/// `curl` or `wget` ([`installs_downloader`]), when an op after it needs a
/// download ([`needs_download`]).
pub fn split_candidate(ops: &[WindowOp]) -> Option<Split> {
    let at = ops
        .iter()
        .position(|o| installs_downloader(&o.node).is_some())?;
    let consumer = ops[at + 1..].iter().find(|o| needs_download(&o.node))?;
    Some(Split {
        at,
        installer: ops[at].id,
        tool: installs_downloader(&ops[at].node)?.to_string(),
        consumer: consumer.id,
    })
}

/// The seam where the optimizer plugs in: turn the items being flushed (ops
/// and explicit barriers, in trace order) into the window the executor
/// runs. Same-host `pkg` ops merge ([`crate::optimize`]), then the apt
/// cache rule runs over the merged schedule. `apt_stale` is the host's apt
/// cache state, carried from window to window.
pub fn prepare_window(
    number: usize,
    kind: WindowKind,
    items: Vec<Item>,
    apt_stale: &mut bool,
) -> Window {
    let mut ops = optimize(items).schedule();
    let nodes: Vec<OpNode> = ops.iter().map(|o| o.node.clone()).collect();
    let (flags, stale) = apt_update_flags(*apt_stale, &nodes);
    *apt_stale = stale;
    for (op, flag) in ops.iter_mut().zip(flags) {
        if let OpSpec::Pkg(p) = &mut op.node.spec {
            p.update_cache = flag;
        }
    }
    let split = split_candidate(&ops);
    Window {
        number,
        kind,
        ops,
        split,
        assertions: Vec::new(),
    }
}

/// What the engine carries from window to window of one host.
#[derive(Debug, Clone, Default)]
struct Carry {
    /// The host's apt cache state (see [`prepare_window`]).
    apt_stale: bool,
    /// What earlier windows asked of rustup.
    rustup: crate::ops::pkg::RustupSeen,
}

impl Carry {
    /// `o` was applied in the window just prepared.
    fn note(&mut self, o: &WindowOp) {
        if let OpSpec::Pkg(p) = &o.node.spec {
            self.rustup
                .record(p, o.node.guard.is_none() && !o.node.allow_fail);
        }
    }
}

/// What the engine tells an executor besides running things.
#[derive(Debug, Clone)]
pub enum Observation {
    /// A read answered without a round trip.
    Resolved { window: usize, read: Resolved },
    /// A read needed the effect of pending mutations; they are applied next.
    ImplicitBarrier { window: usize, reads: Vec<Key> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    Playbook(TraceError),
    /// The playbook future is pending on something that is not a fact read.
    Stalled,
    Exec(String),
    /// A check-only run ([`Executor::check_only`]) reached an implicit
    /// barrier: a fact read that needs the effect of a pending change. The
    /// window before it was prepared; nothing after it was traced.
    CheckStopped {
        /// The role's call of the read (else the last pending change it
        /// waits for).
        site: Option<Site>,
        reads: Vec<Key>,
    },
    /// A check-only run reached a reboot barrier that may be pending (the
    /// host is marked, or an op before it that marks may change): a dry run
    /// never reboots, so what follows cannot be checked.
    CheckStoppedAtReboot {
        site: Option<Site>,
    },
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::Playbook(e) => write!(f, "playbook error: {e}"),
            EngineError::Stalled => {
                f.write_str("playbook is pending on something other than a fact")
            }
            EngineError::Exec(e) => write!(f, "executor error: {e}"),
            EngineError::CheckStopped { site, .. } => match site {
                Some(s) => write!(f, "check stopped at {s}: this read needs the changes above"),
                None => f.write_str("check stopped: this read needs the changes above"),
            },
            EngineError::CheckStoppedAtReboot { site } => match site {
                Some(s) => write!(f, "check stopped at {s}: reboot pending at barrier"),
                None => f.write_str("check stopped: reboot pending at barrier"),
            },
        }
    }
}
impl std::error::Error for EngineError {}

/// Where a reboot barrier is, for the executor's stream line.
#[derive(Debug, Clone)]
pub struct RebootAt {
    /// The playbook's `h.reboot().if_pending()` call.
    pub site: Option<Site>,
    /// The role the barrier was traced in (empty outside any role).
    pub role: String,
}

/// What crossing a reboot barrier came to ([`Executor::reboot`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Crossed {
    /// Nothing was pending, or the host cannot cross (halted, or the run
    /// stops at the barrier): the engine carries on, and the executor's
    /// halted host reports what follows as not run.
    Continue,
    /// The host rebooted and came back: facts read before are stale.
    Rebooted,
    /// A check-only run met a barrier it cannot cross.
    CheckStopped,
}

/// Runs windows and reads facts on a host. Native async fns: there is no
/// `dyn Executor`, and the engine is `!Send` anyway.
#[allow(async_fn_in_trait)]
pub trait Executor {
    /// Answer `queries` (each in its own RunAs), in order, in one round trip.
    async fn read_facts(
        &mut self,
        host: &HostInfo,
        window: usize,
        queries: &[FactQuery],
    ) -> Vec<Value>;

    /// Run the ops of `window` in order, or decline it for its head
    /// ([`Applied::Head`]; only for a window with a [`Split`]), having
    /// sent and reported nothing of it.
    async fn apply(&mut self, host: &HostInfo, window: &Window) -> Result<Applied, String>;

    fn observe(&mut self, _host: &HostInfo, _obs: &Observation) {}

    /// Cross the reboot barrier `at`: every op before it has been applied.
    /// `window` is the number of the last window that ran. The default
    /// crosses nothing (an executor with no host to reboot).
    async fn reboot(
        &mut self,
        _host: &HostInfo,
        _window: usize,
        _at: &RebootAt,
    ) -> Result<Crossed, String> {
        Ok(Crossed::Continue)
    }

    /// The trace ended and every window ran: whatever the host is left with
    /// (a reboot still pending) is the executor's to report or act on.
    async fn end_of_run(&mut self, _host: &HostInfo, _window: usize) -> Result<(), String> {
        Ok(())
    }

    /// A dry run: [`Executor::apply`] only prepares and reports, it never
    /// changes the host. The engine then stops at the first implicit
    /// barrier instead of reading past changes that were not made.
    fn check_only(&self) -> bool {
        false
    }
}

pub struct Engine<E: Executor> {
    pub executor: E,
}

impl<E: Executor> Engine<E> {
    pub fn new(executor: E) -> Engine<E> {
        Engine { executor }
    }

    /// Drive `playbook` on `host` to completion.
    pub async fn run_host<V>(
        &mut self,
        playbook: &Playbook<V>,
        host: Host<V>,
    ) -> Result<(), EngineError> {
        let info = HostInfo {
            name: host.name().to_string(),
            os: host.os.clone(),
        };
        // The name is an owner in cache usage records (T122): a trace error
        // before anything is traced or applied.
        if !crate::ops::fetch::valid_playbook_name(playbook.name) {
            return Err(EngineError::Playbook(crate::host::TraceError {
                message: format!(
                    "playbook name {:?} must not be empty or hold a tab, a comma or a newline: it owns what it caches",
                    playbook.name
                ),
                roles: Vec::new(),
                site: None,
                policy: None,
            }));
        }
        let mut fut = (playbook.run)(host.clone());
        let mut cx = Context::from_waker(Waker::noop());
        let mut window = 1;
        let mut carry = Carry::default();
        loop {
            let poll = fut.as_mut().poll(&mut cx);
            // A trace error is a playbook bug: fail before applying anything.
            let first_error = host.first_error();
            if let Some(err) = first_error {
                return Err(EngineError::Playbook(err));
            }
            self.drain_resolved(&info, &host, window);
            // An assert that failed during this poll blocks before any
            // pending op of this window is applied, including the writes a
            // fact read would otherwise flush.
            self.refuse_assertions(&info, &host, window).await?;
            match poll {
                Poll::Ready(Err(e)) => return Err(EngineError::Playbook(e)),
                Poll::Ready(Ok(())) => {
                    host.check_only().map_err(EngineError::Playbook)?;
                    self.cross_reboots(&info, &host, &mut window, &mut carry)
                        .await?;
                    let ids = pending_ids(&host, false);
                    let ran = !ids.is_empty();
                    self.apply(
                        &info,
                        &host,
                        &mut window,
                        WindowKind::Final,
                        ids,
                        &mut carry,
                    )
                    .await?;
                    // `.at_end()` ops: a window of their own, after the last.
                    let late = pending_ids(&host, true);
                    if !late.is_empty() {
                        if ran {
                            window += 1;
                        }
                        self.apply(
                            &info,
                            &host,
                            &mut window,
                            WindowKind::AtEnd,
                            late,
                            &mut carry,
                        )
                        .await?;
                    }
                    self.executor
                        .end_of_run(&info, window)
                        .await
                        .map_err(EngineError::Exec)?;
                    return Ok(());
                }
                Poll::Pending => {
                    let waiting = host.resolver_cell().borrow_mut().take_waiting();
                    if waiting.is_empty() {
                        return Err(EngineError::Stalled);
                    }
                    let mut queries: Vec<FactQuery> = Vec::new();
                    for w in &waiting {
                        if !queries.contains(&w.query) {
                            queries.push(w.query.clone());
                        }
                    }
                    let site = waiting.iter().find_map(|w| w.site);
                    self.block_round(&info, &host, &mut window, &queries, site, &mut carry)
                        .await?;
                    let values = self.executor.read_facts(&info, window, &queries).await;
                    for w in &waiting {
                        let i = queries.iter().position(|q| *q == w.query).unwrap();
                        host.resolver_cell()
                            .borrow_mut()
                            .fulfil(w, values[i].clone());
                    }
                    window += 1;
                }
            }
        }
    }

    /// Cross every reboot node traced so far and not yet crossed, in
    /// order: apply the pending ops before it (their own windows), then
    /// [`Executor::reboot`]. A crossing that rebooted the host makes the
    /// facts confirmed before it stale.
    async fn cross_reboots<V>(
        &mut self,
        info: &HostInfo,
        host: &Host<V>,
        window: &mut usize,
        carry: &mut Carry,
    ) -> Result<(), EngineError> {
        loop {
            let Some(at) = pending_reboots(host).into_iter().next() else {
                return Ok(());
            };
            let ids: Vec<NodeId> = pending_ids(host, false)
                .into_iter()
                .filter(|id| id.0 < at.0)
                .collect();
            let applied = !ids.is_empty();
            self.apply(info, host, window, WindowKind::Final, ids, carry)
                .await?;
            let place = {
                let trace = host.trace_cell().borrow();
                let site = match trace.nodes.get(at.0) {
                    Some(Node::Reboot { site }) => Some(*site),
                    _ => None,
                };
                RebootAt {
                    site,
                    role: node_roles(&trace).get(at.0).cloned().unwrap_or_default(),
                }
            };
            let crossed = self
                .executor
                .reboot(info, *window, &place)
                .await
                .map_err(EngineError::Exec)?;
            if crossed == Crossed::CheckStopped {
                return Err(EngineError::CheckStoppedAtReboot { site: place.site });
            }
            {
                let mut resolver = host.resolver_cell().borrow_mut();
                resolver.mark_applied(&host.trace_cell().borrow(), &[at]);
                if crossed == Crossed::Rebooted {
                    resolver.forget_confirmed();
                }
            }
            if applied {
                *window += 1;
            }
        }
    }

    /// Hand failed assertions to prepare on an empty window and stop.
    /// Taking them here means a later window does not report them again.
    async fn refuse_assertions<V>(
        &mut self,
        info: &HostInfo,
        host: &Host<V>,
        window: usize,
    ) -> Result<(), EngineError> {
        let assertions = {
            let mut t = host.trace_cell().borrow_mut();
            std::mem::take(&mut t.assertions)
        };
        if assertions.is_empty() {
            return Ok(());
        }
        let why = assertions.join("\n");
        let w = Window {
            number: window,
            kind: WindowKind::Final,
            ops: Vec::new(),
            split: None,
            assertions,
        };
        match self.executor.apply(info, &w).await {
            Ok(_) => Err(EngineError::Exec(why)),
            Err(e) => Err(EngineError::Exec(e)),
        }
    }

    fn drain_resolved<V>(&mut self, info: &HostInfo, host: &Host<V>, window: usize) {
        let resolved = host.resolver_cell().borrow_mut().take_resolved();
        for read in resolved {
            self.executor
                .observe(info, &Observation::Resolved { window, read });
        }
    }

    /// Everything that must run before the round trip for `queries`.
    /// `site` is the role's call of the first read, for a check that stops
    /// here.
    async fn block_round<V>(
        &mut self,
        info: &HostInfo,
        host: &Host<V>,
        window: &mut usize,
        queries: &[FactQuery],
        site: Option<Site>,
        carry: &mut Carry,
    ) -> Result<(), EngineError> {
        // A read after a reboot node meets the rebooted host.
        self.cross_reboots(info, host, window, carry).await?;
        let hoist = non_mutating_ready(host);
        self.apply(info, host, window, WindowKind::NonMutating, hoist, carry)
            .await?;

        let mut keys: Vec<Key> = queries.iter().map(|q| q.fact.effect_key()).collect();
        // A read in u's principal needs u to exist, so a pending `user` op
        // for u is applied before the read like any writer it depends on.
        for q in queries {
            if let Principal::User(u) = &q.run_as.principal {
                let k = Key::user(u);
                if !keys.contains(&k) {
                    keys.push(k);
                }
            }
        }
        let upto = {
            let trace = host.trace_cell().borrow();
            let resolver = host.resolver_cell().borrow();
            resolver
                .pending_ops(&trace)
                .iter()
                .filter(|(_, op)| !op.at_end)
                .filter(|(_, op)| op.writes.iter().any(|w| keys.iter().any(|k| w.overlaps(k))))
                .map(|(id, _)| *id)
                .next_back()
        };
        if let Some(last) = upto {
            let ids: Vec<NodeId> = {
                let trace = host.trace_cell().borrow();
                let resolver = host.resolver_cell().borrow();
                resolver
                    .pending_ops(&trace)
                    .iter()
                    .filter(|(_, op)| !op.at_end)
                    .map(|(id, _)| *id)
                    .take_while(|id| *id <= last)
                    .collect()
            };
            self.executor.observe(
                info,
                &Observation::ImplicitBarrier {
                    window: *window,
                    reads: keys,
                },
            );
            self.apply(info, host, window, WindowKind::ImplicitBarrier, ids, carry)
                .await?;
            if self.executor.check_only() {
                // The read's own call; the last change it waits for when
                // the read has none recorded.
                let site = site.or_else(|| match host.trace_cell().borrow().nodes.get(last.0) {
                    Some(Node::Op(op)) => op.site,
                    _ => None,
                });
                let reads = queries.iter().map(|q| q.fact.effect_key()).collect();
                return Err(EngineError::CheckStopped { site, reads });
            }
        }
        Ok(())
    }

    async fn apply<V>(
        &mut self,
        info: &HostInfo,
        host: &Host<V>,
        window: &mut usize,
        kind: WindowKind,
        mut ids: Vec<NodeId>,
        carry: &mut Carry,
    ) -> Result<(), EngineError> {
        // One window per piece: an op addressed to a user the run creates
        // starts the next window, so prepare checks that user after it
        // exists. Without a `user` op the whole of `ids` is one piece.
        while !ids.is_empty() {
            let cut = user_split(host, &ids);
            let rest = ids.split_off(cut);
            // A piece the executor declines for its head (no downloader on
            // the host, and an op of the piece installs one) becomes two
            // windows: the head, then the rest.
            let mut todo = vec![ids];
            while let Some(piece) = todo.pop() {
                if let Some((head, tail)) = self
                    .apply_one(info, host, *window, kind, piece, carry)
                    .await?
                {
                    todo.push(tail);
                    todo.push(head);
                    continue;
                }
                if !todo.is_empty() {
                    *window += 1;
                }
            }
            ids = rest;
            if !ids.is_empty() {
                *window += 1;
            }
        }
        Ok(())
    }

    /// Form the window of `ids` and hand it to the executor. `Some((head,
    /// tail))` when the executor declined it for its head: nothing of it
    /// ran or was noted, and `head` (the trace ids of the head's ops) then
    /// `tail` (the others), each in trace order, are applied in its place.
    async fn apply_one<V>(
        &mut self,
        info: &HostInfo,
        host: &Host<V>,
        window: usize,
        kind: WindowKind,
        ids: Vec<NodeId>,
        carry: &mut Carry,
    ) -> Result<Option<(Vec<NodeId>, Vec<NodeId>)>, EngineError> {
        let (Some(first), Some(last)) = (ids.first(), ids.last()) else {
            return Ok(None);
        };
        // The window's ops plus every explicit barrier traced between them.
        let items: Vec<Item> = {
            let trace = host.trace_cell().borrow();
            let roles = node_roles(&trace);
            let mut items = Vec::new();
            let mut wanted = ids.iter().peekable();
            for (i, node) in trace
                .nodes
                .iter()
                .enumerate()
                .take(last.0 + 1)
                .skip(first.0)
            {
                match node {
                    Node::Op(op) if wanted.next_if(|id| id.0 == i).is_some() => {
                        // A rustup op only asks for what earlier windows
                        // did not.
                        let mut op = op.clone();
                        if let OpSpec::Pkg(p) = &mut op.spec {
                            carry.rustup.reduce(p);
                        }
                        items.push(Item::Op(WindowOp {
                            id: NodeId(i),
                            members: vec![Member::of(NodeId(i), &roles[i], &op)],
                            node: op,
                        }));
                    }
                    Node::Barrier => items.push(Item::Barrier(NodeId(i))),
                    _ => {}
                }
            }
            items
        };
        // What the window notes is kept only if the executor runs it.
        let mut next = carry.clone();
        let w = prepare_window(window, kind, items, &mut next.apt_stale);
        for o in &w.ops {
            next.note(o);
        }
        let applied = self
            .executor
            .apply(info, &w)
            .await
            .map_err(EngineError::Exec)?;
        if let Applied::Head(n) = applied {
            let head: BTreeSet<NodeId> = w
                .ops
                .iter()
                .take(n)
                .flat_map(|o| o.members.iter().map(|m| m.id))
                .collect();
            let (h, t): (Vec<NodeId>, Vec<NodeId>) = ids.iter().partition(|id| head.contains(id));
            if h.is_empty() || t.is_empty() {
                return Err(EngineError::Exec(format!(
                    "window {window}: declined for a head of {n} of its {} ops",
                    w.ops.len()
                )));
            }
            return Ok(Some((h, t)));
        }
        *carry = next;
        host.resolver_cell()
            .borrow_mut()
            .mark_applied(&host.trace_cell().borrow(), &ids);
        Ok(None)
    }
}

/// Where to end the window that would run `ids`: the index of the first op
/// addressed to `User(u)` that follows an op in `ids` writing `user:u`
/// (`ids.len()` when there is none).
fn user_split<V>(host: &Host<V>, ids: &[NodeId]) -> usize {
    let trace = host.trace_cell().borrow();
    let mut created: BTreeSet<&str> = BTreeSet::new();
    for (i, id) in ids.iter().enumerate() {
        let Some(Node::Op(op)) = trace.nodes.get(id.0) else {
            continue;
        };
        if let Principal::User(u) = &op.run_as.principal
            && created.contains(u.as_str())
        {
            return i;
        }
        for w in &op.writes {
            if let Key::User(n) = w {
                created.insert(n.as_str());
            }
        }
    }
    ids.len()
}

/// The reboot nodes traced and not crossed yet, in trace order.
fn pending_reboots<V>(host: &Host<V>) -> Vec<NodeId> {
    let trace = host.trace_cell().borrow();
    host.resolver_cell().borrow().pending_reboots(&trace)
}

/// The pending ops that are (`at_end`) or are not `.at_end()`.
fn pending_ids<V>(host: &Host<V>, at_end: bool) -> Vec<NodeId> {
    let trace = host.trace_cell().borrow();
    let resolver = host.resolver_cell().borrow();
    resolver
        .pending_ops(&trace)
        .iter()
        .filter(|(_, op)| op.at_end == at_end)
        .map(|(id, _)| *id)
        .collect()
}

/// Pending non-mutating ops that may run now: unguarded, not `at_end`, not
/// reading what an earlier pending mutation writes, and not behind an
/// explicit barrier that has a pending mutation before it. A pending
/// `at_end` op is invisible here: it runs after everything else.
fn non_mutating_ready<V>(host: &Host<V>) -> Vec<NodeId> {
    let trace = host.trace_cell().borrow();
    let resolver = host.resolver_cell().borrow();
    let pending: BTreeSet<NodeId> = resolver
        .pending_ops(&trace)
        .iter()
        .map(|(id, _)| *id)
        .collect();
    let mut mutation_writes: Vec<&Key> = Vec::new();
    let mut fenced = false;
    let mut out = Vec::new();
    let reboots: BTreeSet<NodeId> = resolver.pending_reboots(&trace).into_iter().collect();
    for (i, node) in trace.nodes.iter().enumerate() {
        match node {
            // Nothing moves across a reboot node that has not been crossed.
            Node::Reboot { .. } if reboots.contains(&NodeId(i)) => break,
            Node::Barrier if !mutation_writes.is_empty() => fenced = true,
            Node::Op(op) if pending.contains(&NodeId(i)) && !op.at_end => {
                if op.writes.is_empty() {
                    let blocked = op
                        .reads
                        .iter()
                        .any(|r| mutation_writes.iter().any(|w| w.overlaps(r)))
                        || matches!(&op.run_as.principal, Principal::User(u)
                            if mutation_writes.contains(&&Key::user(u)));
                    if !fenced && !blocked && op.guard.is_none() {
                        out.push(NodeId(i));
                    }
                } else {
                    mutation_writes.extend(op.writes.iter());
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod reboot_tests {
    use std::rc::Rc;

    use super::*;
    use crate::testing::{FakeExecutor, FixtureHost, block_on};

    fn debian() -> FixtureHost<()> {
        FixtureHost::new("web1", Os::fixture("debian12").unwrap(), Rc::new(()))
    }

    fn host() -> Host<()> {
        let f = debian();
        Host::new(f.name.clone(), f.os.clone(), f.vars.clone())
    }

    fn nodes(h: &Host<()>) -> Vec<Node> {
        h.snapshot().nodes
    }

    #[test]
    fn needs_reboot_sets_the_flag_on_every_typed_handle() {
        let h = host();
        h.raw("true").needs_reboot();
        h.file("/etc/x").needs_reboot();
        h.pkg(["linux-generic"]).needs_reboot();
        h.svc("docker").started().needs_reboot();
        h.unit("x.service").needs_reboot();
        h.user("bot").needs_reboot();
        h.group("g").needs_reboot();
        h.repo("r").needs_reboot();
        h.git("https://x/y.git").needs_reboot();
        h.wait_for(crate::Wait::unit("x")).needs_reboot();
        h.raw("untouched");
        let flags: Vec<bool> = nodes(&h)
            .iter()
            .filter_map(|n| match n {
                Node::Op(o) => Some(o.needs_reboot),
                _ => None,
            })
            .collect();
        assert_eq!(flags.len(), 11, "{flags:?}");
        assert!(flags[..10].iter().all(|f| *f), "{flags:?}");
        assert!(!flags[10], "{flags:?}");
    }

    #[test]
    fn a_reboot_node_is_recorded_by_if_pending_only() {
        let h = host();
        let _ = h.reboot();
        assert!(nodes(&h).is_empty(), "h.reboot() alone records nothing");
        h.raw("a");
        let id = h.reboot().if_pending();
        h.raw("b");
        let n = nodes(&h);
        assert_eq!(id, NodeId(1));
        assert!(matches!(n[1], Node::Reboot { .. }), "{n:?}");
        assert!(matches!(n[1], Node::Reboot { site } if site.file.ends_with("engine.rs")));
        let plan = crate::plan::render_host("web1", &host().os, &h.snapshot());
        assert!(plan.contains("  #1 reboot if pending\n"), "{plan}");
    }

    #[test]
    fn a_marked_op_shows_needs_reboot_in_the_plan() {
        let h = host();
        h.file("/etc/x").needs_reboot();
        h.file("/etc/y");
        let plan = crate::plan::render_host("web1", &h.os, &h.snapshot());
        let lines: Vec<&str> = plan.lines().filter(|l| l.starts_with("  #")).collect();
        assert!(lines[0].contains(" needs_reboot"), "{plan}");
        assert!(!lines[1].contains("needs_reboot"), "{plan}");
    }

    fn pkg_barrier_pb(h: Host<()>) -> crate::PlaybookFuture {
        Box::pin(async move {
            h.pkg(["a"]);
            h.reboot().if_pending();
            h.pkg(["b"]);
            Ok(())
        })
    }

    fn run(pb: fn(Host<()>) -> crate::PlaybookFuture) -> FakeExecutor {
        let h = host();
        let pb = Playbook::new("t", pb);
        let mut engine = Engine::new(FakeExecutor::new());
        block_on(engine.run_host(&pb, h)).unwrap();
        engine.executor
    }

    #[test]
    fn the_optimizer_never_merges_pkg_ops_across_a_reboot_node() {
        let fake = run(pkg_barrier_pb);
        let names: Vec<(usize, Vec<usize>)> = fake
            .windows
            .iter()
            .map(|(_, w)| (w.number, w.ops.iter().map(|o| o.id.0).collect()))
            .collect();
        assert_eq!(names, [(1, vec![0]), (2, vec![2])], "{}", fake.render());
        assert_eq!(fake.reboots, [("web1".to_string(), 1)]);
        let text = fake.render();
        let barrier = text.find("-- reboot if pending --").expect(&text);
        let second = text.find("window 2").expect(&text);
        assert!(barrier < second, "{text}");
    }

    #[test]
    fn a_fact_read_after_the_reboot_node_waits_for_the_crossing() {
        fn pb(h: Host<()>) -> crate::PlaybookFuture {
            Box::pin(async move {
                h.raw("echo a").writes([crate::Key::file("/a")]);
                h.reboot().if_pending();
                let _ = h.exists("/elsewhere").await;
                h.raw("echo b").writes([crate::Key::file("/b")]);
                Ok(())
            })
        }
        let fake = run(pb);
        let text = fake.render();
        let apply_a = text.find("echo a").expect(&text);
        let barrier = text.find("-- reboot if pending --").expect(&text);
        let read = text.find("round trip").expect(&text);
        let apply_b = text.find("echo b").expect(&text);
        assert!(
            apply_a < barrier && barrier < read && read < apply_b,
            "{text}"
        );
    }

    #[test]
    fn a_non_mutating_op_after_the_reboot_node_does_not_hoist_across_it() {
        fn pb(h: Host<()>) -> crate::PlaybookFuture {
            Box::pin(async move {
                h.raw("echo before").writes([crate::Key::file("/a")]);
                h.reboot().if_pending();
                h.raw("echo after-ro")
                    .reads([crate::Key::file("/c")])
                    .writes([]);
                let _ = h.exists("/elsewhere").await;
                Ok(())
            })
        }
        let fake = run(pb);
        let text = fake.render();
        let barrier = text.find("-- reboot if pending --").expect(&text);
        let ro = text.find("echo after-ro").expect(&text);
        assert!(barrier < ro, "hoisted across the barrier:\n{text}");
        assert!(text.find("echo before").unwrap() < barrier, "{text}");
    }

    #[test]
    fn a_trailing_reboot_node_is_crossed_after_the_last_window() {
        fn pb(h: Host<()>) -> crate::PlaybookFuture {
            Box::pin(async move {
                h.raw("echo last");
                h.reboot().if_pending();
                Ok(())
            })
        }
        let fake = run(pb);
        assert_eq!(fake.reboots, [("web1".to_string(), 1)], "{}", fake.render());
    }
}
