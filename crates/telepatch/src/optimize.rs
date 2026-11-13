//! The optimizer: three passes over one host's ops about to be flushed.
//!
//! 1. Edges: between two items in trace order, where one writes what the
//!    other reads or writes (or reads what the other writes), or either is
//!    a barrier. A repo op and a cargo op each also conflict with every
//!    distro `pkg` op, both ways.
//! 2. Merge: `pkg` ops with the same manager, mode, wanted, `RunAs`, guard
//!    `allow_fail`, `retry` and `timeout` (a limit is part of equality) are folded
//!    into the earliest one when
//!    [`Graph::hoist_is_safe`] holds, i.e. no other item between the
//!    earliest and the latest member has an edge to a member. Rustup ops
//!    (same user, toolchain, profile, installer, `RunAs`, guard and
//!    `allow_fail`, `retry` and `timeout`; any wanted, the merged op wanting the stronger) use
//!    [`Graph::rustup_hoist_is_safe`] instead: only the moving op's
//!    effects are checked, and of those only what it adds to the earlier
//!    op. Then guarded
//!    ops with an equal spec (any kind except a resource, which never
//!    merges), `RunAs`, `allow_fail`, `retry` and `timeout` are folded
//!    (svc units compared by key, so a restart of `docker` and one of
//!    `docker.service` are equal; a restart and a reload are not)
//!    into the latest one, their guards or-joined, when
//!    [`Graph::sink_is_safe`] holds: the earlier op moves forward, so no
//!    item it passes may overlap its effects or be guarded on it. The
//!    later position keeps every guard's sources before the merged op.
//! 3. Schedule: the surviving ops in trace order ([`Graph::schedule`]).
//!
//! A merged op carries every member's allowances, each tagged with the
//! member it came from (see [`crate::policy::unpermitted`]).
//!
//! Merging is per call: the engine calls this once per window, so ops
//! applied in an earlier window never merge with later ones.

use std::collections::BTreeSet;

use crate::engine::WindowOp;
use crate::key::Key;
use crate::trace::{Guard, NodeId, OpSpec, RunAs};

/// One trace item of a window: an op, or an explicit barrier.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Item {
    Op(WindowOp),
    Barrier(NodeId),
}

fn is_barrier(i: &Item) -> bool {
    matches!(i, Item::Barrier(_))
}

fn conflicts(a: &Item, b: &Item) -> bool {
    let (Item::Op(a), Item::Op(b)) = (a, b) else {
        return true;
    };
    let hit =
        |x: &BTreeSet<Key>, y: &BTreeSet<Key>| x.iter().any(|k| y.iter().any(|l| k.overlaps(l)));
    let (a, b) = (&a.node, &b.node);
    // A repo op changes what a distro install resolves, so no distro `pkg`
    // op moves across it, in either direction.
    if (writes_repo(a) && is_distro_pkg(b)) || (writes_repo(b) && is_distro_pkg(a)) {
        return true;
    }
    // A distro install may be a cargo op's build dependency (`libssl-dev`),
    // so no cargo op moves across a distro `pkg` op, in either direction.
    if (is_cargo(a) && is_distro_pkg(b)) || (is_cargo(b) && is_distro_pkg(a)) {
        return true;
    }
    // An absent op and an install op keep trace order: neither hoists
    // across the other, even when their package names do not overlap.
    if (is_pkg_absent(a) && is_pkg_install(b)) || (is_pkg_install(a) && is_pkg_absent(b)) {
        return true;
    }
    hit(&a.writes, &b.reads) || hit(&a.writes, &b.writes) || hit(&a.reads, &b.writes)
}

fn writes_repo(n: &crate::trace::OpNode) -> bool {
    n.writes.iter().any(|k| matches!(k, Key::Repo(_)))
}

fn is_distro_pkg(n: &crate::trace::OpNode) -> bool {
    matches!(&n.spec, OpSpec::Pkg(p) if matches!(p.source, crate::ops::pkg::Source::Distro(_)))
}

fn is_cargo(n: &crate::trace::OpNode) -> bool {
    matches!(&n.spec, OpSpec::Pkg(p) if matches!(p.source, crate::ops::pkg::Source::Cargo { .. }))
}

fn is_pkg_absent(n: &crate::trace::OpNode) -> bool {
    matches!(&n.spec, OpSpec::Pkg(p) if matches!(p.wanted, crate::ops::pkg::Wanted::Absent))
}

fn is_pkg_install(n: &crate::trace::OpNode) -> bool {
    matches!(&n.spec, OpSpec::Pkg(p) if !matches!(p.wanted, crate::ops::pkg::Wanted::Absent))
}

fn is_rustup(item: &Item) -> bool {
    matches!(item, Item::Op(w) if matches!(&w.node.spec,
        OpSpec::Pkg(p) if matches!(p.source, crate::ops::pkg::Source::Rustup(_))))
}

/// What must be equal for two pkg ops to be merge candidates.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MergeKey {
    source: crate::ops::pkg::Source,
    /// Rustup only: user, toolchain, profile, installer.
    scope: Option<(String, String, String, String)>,
    /// `None` for rustup ([`crate::ops::pkg::PkgSpec::merge_wanted`]).
    wanted: Option<crate::ops::pkg::Wanted>,
    mode: crate::ops::pkg::PkgMode,
    purge: bool,
    run_as: RunAs,
    guard: Option<Guard>,
    allow_fail: bool,
    timeout: Option<std::time::Duration>,
    retry: u32,
}

fn merge_key(item: &Item) -> Option<MergeKey> {
    let Item::Op(w) = item else { return None };
    match &w.node.spec {
        // Script ops and pinned ops never merge: no key, so no two compare
        // equal.
        OpSpec::Pkg(p) if !p.mergeable() => None,
        OpSpec::Pkg(p) => Some(MergeKey {
            source: p.merge_source(),
            scope: p.merge_scope(),
            wanted: p.merge_wanted(),
            mode: p.mode,
            purge: p.purge,
            run_as: w.node.run_as.clone(),
            guard: w.node.guard.clone(),
            allow_fail: w.node.allow_fail,
            timeout: w.node.timeout,
            retry: w.node.retry,
        }),
        _ => None,
    }
}

/// What must be equal for two guarded ops to merge with their guards
/// or-joined.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SameKey {
    spec: OpSpec,
    run_as: RunAs,
    allow_fail: bool,
    timeout: Option<std::time::Duration>,
    retry: u32,
}

fn same_key(item: &Item) -> Option<SameKey> {
    let Item::Op(w) = item else { return None };
    // A resource keeps its own probe and plan. Equal guarded resources
    // stay two ops.
    if matches!(w.node.spec, OpSpec::Resource(_)) {
        return None;
    }
    w.node.guard.as_ref()?;
    // `svc("docker")` and `svc("docker.service")` are one unit: compare
    // svc specs by key name (the merged op keeps the later spelling).
    let spec = match &w.node.spec {
        OpSpec::Svc(s) => {
            let mut s = s.clone();
            s.unit = s.key_name();
            OpSpec::Svc(s)
        }
        other => other.clone(),
    };
    Some(SameKey {
        spec,
        run_as: w.node.run_as.clone(),
        allow_fail: w.node.allow_fail,
        timeout: w.node.timeout,
        retry: w.node.retry,
    })
}

/// The per-host graph. Indices are positions in the item list given to
/// [`Graph::from_items`]; a merged-away item leaves a hole.
#[derive(Debug, Clone)]
pub struct Graph {
    items: Vec<Option<Item>>,
    /// Original indices folded into each live item (itself included).
    cluster: Vec<Vec<usize>>,
    /// Pass 1 edges `(a, b)`, `a < b`, over original indices.
    edges: BTreeSet<(usize, usize)>,
    /// Trace ids merged away by the guarded-op merge, and the op they now
    /// are; applied to guards by [`Graph::schedule`].
    renamed: Vec<(NodeId, NodeId)>,
}

/// Run the passes. Callers apply [`Graph::schedule`] for pass 3.
pub fn optimize(items: Vec<Item>) -> Graph {
    let mut g = Graph::from_items(items);
    g.merge_pass();
    g.merge_guarded_pass();
    g
}

impl Graph {
    /// Pass 1.
    pub fn from_items(items: Vec<Item>) -> Graph {
        let mut edges = BTreeSet::new();
        for b in 0..items.len() {
            for a in 0..b {
                if is_barrier(&items[a]) || is_barrier(&items[b]) || conflicts(&items[a], &items[b])
                {
                    edges.insert((a, b));
                }
            }
        }
        Graph {
            cluster: (0..items.len()).map(|i| vec![i]).collect(),
            items: items.into_iter().map(Some).collect(),
            edges,
            renamed: Vec::new(),
        }
    }

    pub fn edges(&self) -> &BTreeSet<(usize, usize)> {
        &self.edges
    }

    fn has_edge(&self, a: usize, b: usize) -> bool {
        self.edges.contains(&(a.min(b), a.max(b)))
    }

    /// May the live items in `group` become one op at the earliest one's
    /// position? Not if some other live item strictly between the earliest
    /// and the latest has an edge to any original member of any of them
    /// (an effect overlap, or a barrier).
    pub fn hoist_is_safe(&self, group: &[usize]) -> bool {
        let (Some(&lo), Some(&hi)) = (group.iter().min(), group.iter().max()) else {
            return true;
        };
        (lo + 1..hi)
            .filter(|x| self.items[*x].is_some() && !group.contains(x))
            .all(|x| {
                !self.cluster[x].iter().any(|&p| {
                    group
                        .iter()
                        .any(|&g| self.cluster[g].iter().any(|&q| self.has_edge(p, q)))
                })
            })
    }

    /// May rustup op `m` (a single trace op) join the earlier rustup head
    /// `head` of an equal [`merge_key`]? The rustup rule, in place of
    /// [`Graph::hoist_is_safe`]: only `m` moves, so only what `m` would
    /// change is checked, and rustup keeps writing `*` for every other
    /// purpose (edges, the apt cache pass). Not if a live item strictly
    /// between is a barrier, writes `*` or any `file:` key (the toolchain
    /// lives in files under the user's home), or reads or writes a key
    /// that overlaps one `m` adds (`pkg:rustup:<u>:<tc>/target/<t>` or
    /// `.../component/<c>` the head lacks) or one `m` declared with
    /// `.reads(..)`/`.writes(..)`. When `m` adds a `set_default` or a
    /// Latest the head lacks, it is checked as writing `*`: it merges only
    /// across items with no effects (in practice, when adjacent).
    pub fn rustup_hoist_is_safe(&self, head: usize, m: usize) -> bool {
        use crate::ops::pkg::{Source, Wanted};
        let (Some(Item::Op(h)), Some(Item::Op(w))) = (&self.items[head], &self.items[m]) else {
            return false;
        };
        let (OpSpec::Pkg(hp), OpSpec::Pkg(mp)) = (&h.node.spec, &w.node.spec) else {
            return false;
        };
        let (Source::Rustup(hr), Source::Rustup(mr), Some(tc)) =
            (&hp.source, &mp.source, mp.names.first())
        else {
            return false;
        };
        let mut keys: BTreeSet<Key> = BTreeSet::new();
        if (mr.set_default && !hr.set_default)
            || (mp.wanted == Wanted::Latest && hp.wanted != Wanted::Latest)
        {
            keys.insert(Key::Any);
        }
        for t in mr.targets.iter().filter(|t| !hr.targets.contains(t)) {
            keys.insert(mp.key_of(&format!("{tc}/target/{t}")));
        }
        for c in mr.components.iter().filter(|c| !hr.components.contains(c)) {
            keys.insert(mp.key_of(&format!("{tc}/component/{c}")));
        }
        keys.extend(w.node.reads.iter().cloned());
        if w.node.writes != w.node.spec.default_writes() {
            keys.extend(w.node.writes.iter().cloned());
        }
        (head + 1..m)
            .filter_map(|x| self.items[x].as_ref())
            .all(|item| match item {
                Item::Barrier(_) => false,
                Item::Op(o) => {
                    let n = &o.node;
                    !n.writes
                        .iter()
                        .any(|k| matches!(k, Key::Any | Key::File(_)))
                        && !n
                            .reads
                            .iter()
                            .chain(n.writes.iter())
                            .any(|e| keys.iter().any(|k| k.overlaps(e)))
                }
            })
    }

    /// Fold `group[1..]` into `group[0]` (the earliest). Callers check
    /// [`Graph::hoist_is_safe`] (or [`Graph::rustup_hoist_is_safe`]) and
    /// [`merge_key`] equality first.
    fn merge(&mut self, group: &[usize]) {
        let head = group[0];
        for &o in &group[1..] {
            let Some(Item::Op(other)) = self.items[o].take() else {
                panic!("merge: not an op");
            };
            let moved = std::mem::take(&mut self.cluster[o]);
            self.cluster[head].extend(moved);
            let Some(Item::Op(h)) = self.items[head].as_mut() else {
                panic!("merge: not an op");
            };
            union_allowed(h, &other);
            h.members.extend(other.members);
            if let (OpSpec::Pkg(a), OpSpec::Pkg(b)) = (&mut h.node.spec, &other.node.spec) {
                a.merge(b);
            }
            h.node.reads.extend(other.node.reads);
            h.node.writes.extend(other.node.writes);
            h.node.post.extend(other.node.post);
        }
    }

    /// Pass 2. Each candidate joins the first earlier group it can safely
    /// join, else starts its own. Rustup candidates use
    /// [`Graph::rustup_hoist_is_safe`], every other source
    /// [`Graph::hoist_is_safe`].
    fn merge_pass(&mut self) {
        let mut groups: Vec<(MergeKey, usize)> = Vec::new();
        for i in 0..self.items.len() {
            let Some(item) = &self.items[i] else { continue };
            let Some(key) = merge_key(item) else { continue };
            let rustup = is_rustup(item);
            let joined = groups
                .iter()
                .find(|(k, head)| {
                    *k == key
                        && if rustup {
                            self.rustup_hoist_is_safe(*head, i)
                        } else {
                            self.hoist_is_safe(&[*head, i])
                        }
                })
                .map(|(_, head)| *head);
            match joined {
                Some(head) => self.merge(&[head, i]),
                None => groups.push((key, i)),
            }
        }
    }

    /// The trace ids of the members of live item `x`.
    fn member_ids(&self, x: usize) -> Vec<NodeId> {
        match &self.items[x] {
            Some(Item::Op(w)) => w.members.iter().map(|m| m.id).collect(),
            _ => Vec::new(),
        }
    }

    fn guard_sources(&self, x: usize) -> Vec<NodeId> {
        match &self.items[x] {
            Some(Item::Op(w)) => w.node.guard.as_ref().map_or(Vec::new(), |g| g.sources()),
            _ => Vec::new(),
        }
    }

    /// May live item `from` move forward to `to` and become one op with it?
    /// Not if either guard names the other's members (the merged op would
    /// be guarded on itself), and not if a live item strictly between has
    /// an edge to any original member of `from` or is guarded on one of
    /// `from`'s members (it would read the outcome before the op ran).
    pub fn sink_is_safe(&self, from: usize, to: usize) -> bool {
        let moving = self.member_ids(from);
        let staying = self.member_ids(to);
        if self.guard_sources(from).iter().any(|s| staying.contains(s))
            || self.guard_sources(to).iter().any(|s| moving.contains(s))
        {
            return false;
        }
        (from + 1..to)
            .filter(|x| self.items[*x].is_some())
            .all(|x| {
                let overlap = self.cluster[x]
                    .iter()
                    .any(|&p| self.cluster[from].iter().any(|&q| self.has_edge(p, q)));
                let reads = self.guard_sources(x).iter().any(|s| moving.contains(s));
                !overlap && !reads
            })
    }

    /// Fold `from` into the later `to` (callers check [`Graph::sink_is_safe`]
    /// and [`same_key`] equality): members join in trace order, the guards
    /// are or-joined (kept once when equal), effects and postconditions
    /// are unioned. A guard naming a member of `from` is renamed to `to`,
    /// except for a `pkg` op, whose members keep their ids (the member rule
    /// of `changed`).
    fn sink(&mut self, from: usize, to: usize) {
        let Some(Item::Op(other)) = self.items[from].take() else {
            panic!("sink: not an op");
        };
        let moved = std::mem::take(&mut self.cluster[from]);
        self.cluster[to].extend(moved);
        let Some(Item::Op(h)) = self.items[to].as_mut() else {
            panic!("sink: not an op");
        };
        if !matches!(h.node.spec, OpSpec::Pkg(_)) {
            for m in &other.members {
                self.renamed.push((m.id, h.id));
            }
        }
        union_allowed(h, &other);
        let mut members = other.members;
        members.append(&mut h.members);
        members.sort_by_key(|m| m.id);
        h.members = members;
        h.node.guard = match (other.node.guard, h.node.guard.take()) {
            (Some(a), Some(b)) if a == b => Some(a),
            (Some(a), Some(b)) => Some(a | b),
            (a, b) => a.or(b),
        };
        h.node.reads.extend(other.node.reads);
        h.node.writes.extend(other.node.writes);
        for (k, v) in other.node.post {
            h.node.post.entry(k).or_insert(v);
        }
    }

    /// Pass 2, guarded ops: each joins the latest earlier op with an equal
    /// [`SameKey`] it can safely sink into it, else starts its own group.
    fn merge_guarded_pass(&mut self) {
        let mut groups: Vec<(SameKey, usize)> = Vec::new();
        for i in 0..self.items.len() {
            let Some(item) = &self.items[i] else { continue };
            let Some(key) = same_key(item) else { continue };
            let joined = groups
                .iter()
                .position(|(k, head)| *k == key && self.sink_is_safe(*head, i));
            match joined {
                Some(g) => {
                    let head = groups[g].1;
                    self.sink(head, i);
                    groups[g].1 = i;
                }
                None => groups.push((key, i)),
            }
        }
    }

    /// Pass 3: the ops in trace order (barriers have done their job). A
    /// guard that names an op merged away by the guarded-op merge now names
    /// the merged one. A merged `pkg` op's members keep their own ids.
    pub fn schedule(self) -> Vec<WindowOp> {
        let mut ops: Vec<WindowOp> = Vec::new();
        for item in self.items.into_iter().flatten() {
            if let Item::Op(w) = item {
                ops.push(w);
            }
        }
        for w in &mut ops {
            if let Some(g) = &mut w.node.guard {
                rename_guard(g, &self.renamed);
            }
        }
        ops
    }
}

/// Fold `other`'s allowances into `h`'s, each tagged with the trace op it
/// came from ([`crate::policy::AllowedFor`]): an untagged allowance is its
/// op's own, so `h`'s are tagged with `h`'s id and site and `other`'s with
/// `other`'s; one already tagged (from an earlier merge) keeps its tag.
fn union_allowed(h: &mut WindowOp, other: &WindowOp) {
    fn tag(a: &mut crate::policy::Allowance, w: &WindowOp) {
        a.member.get_or_insert(crate::policy::AllowedFor {
            id: w.id,
            site: w.node.site,
        });
    }
    let mut own = std::mem::take(&mut h.node.allowed);
    for a in &mut own {
        tag(a, h);
    }
    for a in &other.node.allowed {
        let mut a = a.clone();
        tag(&mut a, other);
        own.push(a);
    }
    h.node.allowed = own;
}

/// Rename the ops `g` names along `renamed` (`from -> to`), following
/// chains (`a -> b`, `b -> c` renames `a` to `c`).
fn rename_guard(g: &mut Guard, renamed: &[(NodeId, NodeId)]) {
    match g {
        Guard::Changed(n) | Guard::ExitOk(n) => {
            while let Some((_, to)) = renamed.iter().find(|(from, _)| from == n) {
                *n = *to;
            }
        }
        Guard::Not(inner) => rename_guard(inner, renamed),
        Guard::Or(a, b) | Guard::And(a, b) => {
            rename_guard(a, renamed);
            rename_guard(b, renamed);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::engine::Member;
    use crate::ops::pkg::{Manager, PkgSpec};
    use crate::trace::{OpNode, RunAs};

    fn op(id: usize, spec: OpSpec, reads: &[Key], writes: &[Key]) -> Item {
        Item::Op(WindowOp {
            id: NodeId(id),
            node: OpNode {
                run_as: RunAs::root_shell(),
                reads: reads.iter().cloned().collect(),
                writes: writes.iter().cloned().collect(),
                guard: None,
                post: BTreeMap::new(),
                allow_fail: false,
                site: None,
                allowed: Vec::new(),
                at_end: false,
                timeout: None,
                retry: 0,
                spec,
            },
            members: vec![Member {
                id: NodeId(id),
                role: format!("r{id}"),
                pkg_names: Vec::new(),
            }],
        })
    }

    fn pkg(id: usize, name: &str) -> Item {
        let spec = PkgSpec::new(Manager::Apt, vec![name.into()]);
        let w: Vec<Key> = spec.keys().into_iter().collect();
        op(id, OpSpec::Pkg(spec), &[], &w)
    }

    fn file(id: usize, path: &str) -> Item {
        let spec = OpSpec::File(crate::ops::file::FileSpec::new(path));
        op(id, spec.clone(), &[], &[Key::file(path)])
    }

    fn repo(id: usize, name: &str) -> Item {
        let spec = crate::ops::repo::RepoSpec::new(name);
        let w: Vec<Key> = spec.default_writes().into_iter().collect();
        op(id, OpSpec::Repo(spec), &[], &w)
    }

    fn with(mut item: Item, f: impl FnOnce(&mut OpNode)) -> Item {
        if let Item::Op(w) = &mut item {
            f(&mut w.node);
        }
        item
    }

    fn ids(g: Graph) -> Vec<Vec<usize>> {
        g.schedule()
            .iter()
            .map(|w| w.members.iter().map(|m| m.id.0).collect())
            .collect()
    }

    #[test]
    fn unrelated_file_between_allows_merge() {
        let g = optimize(vec![pkg(0, "a"), file(1, "/etc/x"), pkg(2, "b")]);
        assert_eq!(ids(g), [vec![0, 2], vec![1]]);
    }

    #[test]
    fn star_writer_between_blocks_merge() {
        let sh = op(1, OpSpec::Raw { script: "x".into() }, &[], &[Key::Any]);
        let g = optimize(vec![pkg(0, "a"), sh, pkg(2, "b")]);
        assert_eq!(ids(g), [vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn reader_of_member_key_between_blocks_merge() {
        let r = with(file(1, "/etc/x"), |n| {
            n.reads.insert(Key::pkg("b"));
        });
        let g = optimize(vec![pkg(0, "a"), r, pkg(2, "b")]);
        assert_eq!(ids(g), [vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn barrier_between_blocks_merge() {
        let g = optimize(vec![pkg(0, "a"), Item::Barrier(NodeId(1)), pkg(2, "b")]);
        assert_eq!(ids(g), [vec![0], vec![2]]);
    }

    #[test]
    fn guard_mismatch_blocks_merge() {
        let guarded = with(pkg(2, "b"), |n| n.guard = Some(Guard::Changed(NodeId(1))));
        let g = optimize(vec![pkg(0, "a"), file(1, "/x"), guarded]);
        assert_eq!(ids(g), [vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn equal_guards_merge_and_a_pkg_member_keeps_its_id() {
        let g1 = |n: &mut OpNode| n.guard = Some(Guard::Changed(NodeId(0)));
        let items = vec![
            file(0, "/x"),
            with(pkg(1, "a"), g1),
            file(2, "/y"),
            with(pkg(3, "b"), g1),
            with(file(4, "/z"), |n| n.guard = Some(Guard::Changed(NodeId(3)))),
        ];
        let ops = optimize(items).schedule();
        assert_eq!(ops.len(), 4);
        assert_eq!(ops[1].members.len(), 2);
        // A merged pkg member keeps its own id in a guard: its `changed` is
        // its own names' (the member rule), not the merged op's.
        assert_eq!(ops[3].node.guard, Some(Guard::Changed(NodeId(3))));
    }

    #[test]
    fn a_different_timeout_blocks_a_pkg_merge_and_an_equal_one_merges() {
        use std::time::Duration;
        let t = |n: u64| move |o: &mut OpNode| o.timeout = Some(Duration::from_secs(n));
        let g = optimize(vec![with(pkg(0, "a"), t(5)), with(pkg(1, "b"), t(9))]);
        assert_eq!(ids(g), [vec![0], vec![1]]);
        let g = optimize(vec![with(pkg(0, "a"), t(5)), pkg(1, "b")]);
        assert_eq!(ids(g), [vec![0], vec![1]]);
        let g = optimize(vec![with(pkg(0, "a"), t(5)), with(pkg(1, "b"), t(5))]);
        assert_eq!(ids(g), [vec![0, 1]]);
    }

    #[test]
    fn a_different_retry_blocks_a_pkg_merge_and_an_equal_one_merges() {
        let r = |n: u32| move |o: &mut OpNode| o.retry = n;
        let g = optimize(vec![with(pkg(0, "a"), r(1)), with(pkg(1, "b"), r(2))]);
        assert_eq!(ids(g), [vec![0], vec![1]]);
        let g = optimize(vec![with(pkg(0, "a"), r(1)), pkg(1, "b")]);
        assert_eq!(ids(g), [vec![0], vec![1]]);
        let g = optimize(vec![with(pkg(0, "a"), r(2)), with(pkg(1, "b"), r(2))]);
        assert_eq!(ids(g), [vec![0, 1]]);
    }

    #[test]
    fn a_different_timeout_blocks_the_guarded_merge() {
        use std::time::Duration;
        let mk = |id: usize, secs: Option<u64>| {
            with(restart(id, Guard::Changed(NodeId(0))), move |n| {
                n.timeout = secs.map(Duration::from_secs)
            })
        };
        let g = optimize(vec![file(0, "/x"), mk(1, Some(5)), mk(2, Some(9))]);
        assert_eq!(ids(g), [vec![0], vec![1], vec![2]]);
        let g = optimize(vec![file(0, "/x"), mk(1, Some(5)), mk(2, Some(5))]);
        assert_eq!(ids(g), [vec![0], vec![1, 2]]);
    }

    fn restart(id: usize, guard: Guard) -> Item {
        let spec = OpSpec::Raw {
            script: "systemctl restart docker".into(),
        };
        with(op(id, spec, &[], &[Key::svc("docker")]), |n| {
            n.guard = Some(guard)
        })
    }

    #[test]
    fn equal_guarded_ops_merge_at_the_later_position_or_joined() {
        let items = vec![
            pkg(0, "toolkit"),
            restart(1, Guard::Changed(NodeId(0))),
            file(2, "/etc/docker/daemon.json"),
            restart(3, Guard::Changed(NodeId(2))),
            with(file(4, "/z"), |n| n.guard = Some(Guard::Changed(NodeId(1)))),
        ];
        let ops = optimize(items).schedule();
        let at: Vec<usize> = ops.iter().map(|w| w.id.0).collect();
        assert_eq!(at, [0, 2, 3, 4]);
        assert_eq!(
            ops[2].members.iter().map(|m| m.id.0).collect::<Vec<_>>(),
            [1, 3]
        );
        assert_eq!(
            ops[2].node.guard,
            Some(Guard::Changed(NodeId(0)) | Guard::Changed(NodeId(2)))
        );
        // A guard on the merged-away op names the merged one.
        assert_eq!(ops[3].node.guard, Some(Guard::Changed(NodeId(3))));
    }

    #[test]
    fn equal_guards_on_equal_ops_are_not_doubled() {
        let g = Guard::Changed(NodeId(0));
        let ops = optimize(vec![
            file(0, "/x"),
            restart(1, g.clone()),
            restart(2, g.clone()),
        ])
        .schedule();
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[1].node.guard, Some(g));
    }

    #[test]
    fn an_op_guarded_on_the_earlier_member_blocks_the_move() {
        let items = vec![
            file(0, "/a"),
            restart(1, Guard::Changed(NodeId(0))),
            with(file(2, "/b"), |n| n.guard = Some(Guard::ExitOk(NodeId(1)))),
            restart(3, Guard::Changed(NodeId(0))),
        ];
        assert_eq!(ids(optimize(items)), [vec![0], vec![1], vec![2], vec![3]]);
    }

    #[test]
    fn an_effect_overlap_between_blocks_the_move() {
        let items = vec![
            file(0, "/a"),
            restart(1, Guard::Changed(NodeId(0))),
            with(file(2, "/b"), |n| {
                n.reads.insert(Key::svc("docker"));
            }),
            restart(3, Guard::Changed(NodeId(2))),
        ];
        assert_eq!(ids(optimize(items)), [vec![0], vec![1], vec![2], vec![3]]);
    }

    #[test]
    fn a_merge_never_makes_a_guard_name_its_own_op() {
        let items = vec![
            file(0, "/a"),
            restart(1, Guard::Changed(NodeId(0))),
            restart(2, Guard::Changed(NodeId(1))),
        ];
        assert_eq!(ids(optimize(items)), [vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn unguarded_equal_ops_do_not_merge() {
        let spec = OpSpec::Raw { script: "x".into() };
        let a = op(0, spec.clone(), &[], &[Key::svc("d")]);
        let b = op(1, spec, &[], &[Key::svc("d")]);
        assert_eq!(ids(optimize(vec![a, b])), [vec![0], vec![1]]);
    }

    fn svc_action(id: usize, unit: &str, restart: bool, guard: Guard) -> Item {
        let mut spec = crate::ops::svc::SvcSpec::new(unit);
        spec.restart = restart;
        spec.reload = !restart;
        let spec = OpSpec::Svc(spec);
        let w: Vec<Key> = spec.default_writes().into_iter().collect();
        with(op(id, spec, &[], &w), |n| n.guard = Some(guard))
    }

    #[test]
    fn restarts_of_one_unit_merge_whatever_the_spelling() {
        let items = vec![
            file(0, "/a"),
            svc_action(1, "docker", true, Guard::Changed(NodeId(0))),
            file(2, "/b"),
            svc_action(3, "docker.service", true, Guard::Changed(NodeId(2))),
        ];
        let ops = optimize(items).schedule();
        assert_eq!(ops.len(), 3);
        assert_eq!(
            ops[2].members.iter().map(|m| m.id.0).collect::<Vec<_>>(),
            [1, 3]
        );
        assert_eq!(
            ops[2].node.guard,
            Some(Guard::Changed(NodeId(0)) | Guard::Changed(NodeId(2)))
        );
    }

    #[test]
    fn a_restart_and_a_reload_of_one_unit_stay_apart() {
        let items = vec![
            file(0, "/a"),
            svc_action(1, "docker", true, Guard::Changed(NodeId(0))),
            svc_action(2, "docker", false, Guard::Changed(NodeId(0))),
        ];
        assert_eq!(ids(optimize(items)), [vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn rename_guard_handles_or_and_not() {
        let mut g =
            (Guard::Changed(NodeId(1)) | Guard::ExitOk(NodeId(5))) & !Guard::Changed(NodeId(1));
        rename_guard(&mut g, &[(NodeId(1), NodeId(3)), (NodeId(3), NodeId(4))]);
        assert_eq!(
            g,
            (Guard::Changed(NodeId(4)) | Guard::ExitOk(NodeId(5))) & !Guard::Changed(NodeId(4))
        );
    }

    #[test]
    fn different_run_as_blocks_merge() {
        let user = with(pkg(2, "b"), |n| n.run_as = RunAs::login("deploy"));
        let g = optimize(vec![pkg(0, "a"), file(1, "/x"), user]);
        assert_eq!(ids(g), [vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn names_union_in_first_seen_order_without_duplicates() {
        let ops = optimize(vec![pkg(0, "b"), pkg(1, "a"), pkg(2, "b")]).schedule();
        assert_eq!(ops.len(), 1);
        let OpSpec::Pkg(p) = &ops[0].node.spec else {
            panic!()
        };
        assert_eq!(p.names, ["b", "a"]);
        assert_eq!(ops[0].node.writes.len(), 2);
    }

    #[test]
    fn later_group_merges_when_earlier_is_blocked() {
        let sh = op(1, OpSpec::Raw { script: "x".into() }, &[], &[Key::Any]);
        let g = optimize(vec![pkg(0, "a"), sh, pkg(2, "b"), pkg(3, "c")]);
        assert_eq!(ids(g), [vec![0], vec![1], vec![2, 3]]);
    }

    #[test]
    fn edges_include_barriers_and_war() {
        let r = with(file(0, "/x"), |n| {
            n.reads.insert(Key::pkg("a"));
        });
        let g = Graph::from_items(vec![r, pkg(1, "a"), Item::Barrier(NodeId(2))]);
        assert!(g.edges().contains(&(0, 1)));
        assert!(g.edges().contains(&(0, 2)) && g.edges().contains(&(1, 2)));
    }

    #[test]
    fn repo_op_blocks_merging_distro_pkgs_across_it() {
        let g = optimize(vec![pkg(0, "a"), repo(1, "docker"), pkg(2, "b")]);
        assert_eq!(ids(g), [vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn repo_op_conflicts_with_every_distro_pkg_either_way() {
        let g = Graph::from_items(vec![pkg(0, "a"), repo(1, "r"), pkg(2, "b")]);
        assert!(g.edges().contains(&(0, 1)) && g.edges().contains(&(1, 2)));
        // Not with other kinds of op.
        let g = Graph::from_items(vec![repo(0, "r"), file(1, "/etc/x")]);
        assert!(!g.edges().contains(&(0, 1)));
    }

    fn absent(id: usize, name: &str) -> Item {
        let mut spec = PkgSpec::new(Manager::Apt, vec![name.into()]);
        spec.wanted = crate::ops::pkg::Wanted::Absent;
        let w: Vec<Key> = spec.keys().into_iter().collect();
        op(id, OpSpec::Pkg(spec), &[], &w)
    }

    #[test]
    fn an_absent_op_does_not_cross_an_install() {
        // install, remove, install: the later install does not hoist over
        // the removal, and the two removals do not merge across an install.
        let g = optimize(vec![pkg(0, "curl"), absent(1, "cmake"), pkg(2, "wget")]);
        assert_eq!(ids(g), [vec![0], vec![1], vec![2]]);
        let g = optimize(vec![absent(0, "cmake"), pkg(1, "git"), absent(2, "nano")]);
        assert_eq!(ids(g), [vec![0], vec![1], vec![2]]);
        let ops = optimize(vec![absent(0, "cmake"), absent(1, "nano")]).schedule();
        assert_eq!(ops.len(), 1);
        let OpSpec::Pkg(p) = &ops[0].node.spec else {
            panic!()
        };
        assert_eq!(p.names, ["cmake", "nano"]);
        assert_eq!(p.wanted, crate::ops::pkg::Wanted::Absent);
    }

    #[test]
    fn pkgs_after_a_repo_still_merge_with_each_other() {
        let g = optimize(vec![
            pkg(0, "a"),
            repo(1, "docker"),
            pkg(2, "b"),
            pkg(3, "c"),
        ]);
        assert_eq!(ids(g), [vec![0], vec![1], vec![2, 3]]);
    }
}
