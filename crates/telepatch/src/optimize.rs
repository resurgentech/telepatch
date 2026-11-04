//! The optimizer: three passes over one host's ops about to be flushed.
//!
//! 1. Edges: between two items in trace order, where one writes what the
//!    other reads or writes (or reads what the other writes), or either is
//!    a barrier.
//! 2. Merge: `pkg` ops with the same manager, mode, `RunAs`, guard and
//!    `allow_fail` are folded into the earliest one when
//!    [`Graph::hoist_is_safe`] holds, i.e. no other item between the
//!    earliest and the latest member has an edge to a member.
//! 3. Schedule: the surviving ops in trace order ([`Graph::schedule`]).
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
    hit(&a.writes, &b.reads) || hit(&a.writes, &b.writes) || hit(&a.reads, &b.writes)
}

fn writes_repo(n: &crate::trace::OpNode) -> bool {
    n.writes.iter().any(|k| matches!(k, Key::Repo(_)))
}

fn is_distro_pkg(n: &crate::trace::OpNode) -> bool {
    matches!(&n.spec, OpSpec::Pkg(p) if matches!(p.source, crate::ops::pkg::Source::Distro(_)))
}

/// What must be equal for two pkg ops to be merge candidates.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MergeKey {
    source: crate::ops::pkg::Source,
    wanted: crate::ops::pkg::Wanted,
    mode: crate::ops::pkg::PkgMode,
    run_as: RunAs,
    guard: Option<Guard>,
    allow_fail: bool,
}

fn merge_key(item: &Item) -> Option<MergeKey> {
    let Item::Op(w) = item else { return None };
    match &w.node.spec {
        // Script and Rustup ops and pinned Cargo ops never merge: no key, so no two
        // compare equal.
        OpSpec::Pkg(p) if !p.mergeable() => None,
        OpSpec::Pkg(p) => Some(MergeKey {
            source: p.source.clone(),
            wanted: p.wanted.clone(),
            mode: p.mode,
            run_as: w.node.run_as.clone(),
            guard: w.node.guard.clone(),
            allow_fail: w.node.allow_fail,
        }),
        _ => None,
    }
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
}

/// Run the passes. Callers apply [`Graph::schedule`] for pass 3.
pub fn optimize(items: Vec<Item>) -> Graph {
    let mut g = Graph::from_items(items);
    g.merge_pass();
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

    /// Fold `group[1..]` into `group[0]` (the earliest). Callers check
    /// [`Graph::hoist_is_safe`] and [`merge_key`] equality first.
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
    /// join, else starts its own.
    fn merge_pass(&mut self) {
        let mut groups: Vec<(MergeKey, usize)> = Vec::new();
        for i in 0..self.items.len() {
            let Some(item) = &self.items[i] else { continue };
            let Some(key) = merge_key(item) else { continue };
            let joined = groups
                .iter()
                .find(|(k, head)| *k == key && self.hoist_is_safe(&[*head, i]))
                .map(|(_, head)| *head);
            match joined {
                Some(head) => self.merge(&[head, i]),
                None => groups.push((key, i)),
            }
        }
    }

    /// Pass 3: the ops in trace order (barriers have done their job). A
    /// guard that names a merged-away op now names the merged one.
    pub fn schedule(self) -> Vec<WindowOp> {
        let mut renamed: Vec<(NodeId, NodeId)> = Vec::new();
        let mut ops: Vec<WindowOp> = Vec::new();
        for item in self.items.into_iter().flatten() {
            if let Item::Op(w) = item {
                for m in w.members.iter().skip(1) {
                    renamed.push((m.id, w.id));
                }
                ops.push(w);
            }
        }
        for w in &mut ops {
            if let Some(g) = &mut w.node.guard {
                rename_guard(g, &renamed);
            }
        }
        ops
    }
}

fn rename_guard(g: &mut Guard, renamed: &[(NodeId, NodeId)]) {
    match g {
        Guard::Changed(n) | Guard::ExitOk(n) => {
            if let Some((_, to)) = renamed.iter().find(|(from, _)| from == n) {
                *n = *to;
            }
        }
        Guard::Not(inner) => rename_guard(inner, renamed),
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
                spec,
            },
            members: vec![Member {
                id: NodeId(id),
                role: format!("r{id}"),
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
    fn equal_guards_merge_and_guard_is_renamed() {
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
        assert_eq!(ops[3].node.guard, Some(Guard::Changed(NodeId(1))));
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
