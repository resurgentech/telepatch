//! The fact resolver: confirmed map, postconditions of pending ops, or
//! block. One per host, shared by the host handle and the engine.

use std::collections::{BTreeMap, BTreeSet};

use crate::facts::query::{Fact, FactQuery};
use crate::key::Value;
use crate::trace::{Node, NodeId, OpNode, RunAs, Source, Trace};

/// How the resolver answered a read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Now(Value, Source),
    Block,
}

/// A registered read the engine must satisfy with a round trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waiting {
    pub ticket: u64,
    pub query: FactQuery,
    /// The role's call of the read, when known.
    pub site: Option<crate::trace::Site>,
}

/// A read answered without a round trip, for the engine's observers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub query: FactQuery,
    pub value: Value,
    pub source: Source,
}

#[derive(Debug, Default)]
pub struct Resolver {
    confirmed: BTreeMap<(RunAs, Fact), Value>,
    applied: BTreeSet<NodeId>,
    waiting: Vec<Waiting>,
    answers: BTreeMap<u64, Value>,
    next_ticket: u64,
    resolved: Vec<Resolved>,
}

impl Resolver {
    /// Op nodes the executor has not run yet, in trace order.
    pub fn pending_ops<'a>(&self, trace: &'a Trace) -> Vec<(NodeId, &'a OpNode)> {
        trace
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(i, n)| match n {
                Node::Op(op) if !self.applied.contains(&NodeId(i)) => Some((NodeId(i), op)),
                _ => None,
            })
            .collect()
    }

    /// Resolve in order: confirmed, postcondition, block.
    ///
    /// A confirmed value is only trusted while no pending op writes the
    /// key: any such op was traced after the read was confirmed (an earlier
    /// one would have been applied to answer it), so the value may be stale.
    /// A postcondition comes from the last pending writer only, and not from
    /// a guarded op, which may be skipped. A pending `.at_end()` op is no
    /// writer here: it runs after every read.
    pub fn resolve(&self, trace: &Trace, q: &FactQuery) -> Answer {
        let key = q.fact.effect_key();
        let pending = self.pending_ops(trace);
        let writer = pending
            .iter()
            .rev()
            .filter(|(_, op)| !op.at_end)
            .find(|(_, op)| op.writes.iter().any(|w| w.overlaps(&key)));
        if writer.is_none()
            && let Some(v) = self.confirmed.get(&(q.run_as.clone(), q.fact.clone()))
        {
            return Answer::Now(v.clone(), Source::Confirmed);
        }
        if let Some((id, op)) = writer
            && op.guard.is_none()
            && let Some(v) = op.post.get(&key).and_then(|p| q.fact.from_post(p))
        {
            return Answer::Now(v, Source::Postcondition(*id));
        }
        Answer::Block
    }

    /// Record an answer given without a round trip.
    pub(crate) fn note_resolved(&mut self, query: &FactQuery, value: &Value, source: &Source) {
        self.resolved.push(Resolved {
            query: query.clone(),
            value: value.clone(),
            source: source.clone(),
        });
    }

    /// Drain the no-round-trip answers given since the last call.
    pub fn take_resolved(&mut self) -> Vec<Resolved> {
        std::mem::take(&mut self.resolved)
    }

    /// Register a read that needs a round trip; returns its ticket.
    pub(crate) fn register(&mut self, query: FactQuery, site: crate::trace::Site) -> u64 {
        let ticket = self.next_ticket;
        self.next_ticket += 1;
        self.waiting.push(Waiting {
            ticket,
            query,
            site: Some(site),
        });
        ticket
    }

    /// Hand the registered reads to the engine.
    pub fn take_waiting(&mut self) -> Vec<Waiting> {
        std::mem::take(&mut self.waiting)
    }

    /// The engine read `ticket`'s fact: store it as confirmed and make it
    /// available to the waiting future.
    pub fn fulfil(&mut self, w: &Waiting, value: Value) {
        self.confirmed.insert(
            (w.query.run_as.clone(), w.query.fact.clone()),
            value.clone(),
        );
        self.answers.insert(w.ticket, value);
    }

    pub(crate) fn take_answer(&mut self, ticket: u64) -> Option<Value> {
        self.answers.remove(&ticket)
    }

    /// The executor ran these ops: they are no longer pending, and their
    /// writes invalidate overlapping confirmed values.
    pub fn mark_applied(&mut self, trace: &Trace, ids: &[NodeId]) {
        for id in ids {
            self.applied.insert(*id);
            if let Node::Op(op) = &trace.nodes[id.0] {
                self.confirmed.retain(|(_, fact), _| {
                    let key = fact.effect_key();
                    !op.writes.iter().any(|w| w.overlaps(&key))
                });
            }
        }
    }

    pub fn is_confirmed(&self, q: &FactQuery) -> bool {
        self.confirmed
            .contains_key(&(q.run_as.clone(), q.fact.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::Key;
    use crate::trace::OpSpec;

    fn op(writes: Vec<Key>) -> Node {
        Node::Op(OpNode {
            spec: OpSpec::Raw { script: "x".into() },
            run_as: RunAs::root_shell(),
            reads: Default::default(),
            writes: writes.into_iter().collect(),
            guard: None,
            post: Default::default(),
            allow_fail: false,
            site: None,
            allowed: Vec::new(),
            at_end: false,
            timeout: None,
            retry: 0,
        })
    }

    fn q(f: Fact) -> FactQuery {
        FactQuery {
            run_as: RunAs::root_shell(),
            fact: f,
        }
    }

    #[test]
    fn executed_write_invalidates_confirmed() {
        let mut t = Trace::default();
        let mut r = Resolver::default();
        let query = q(Fact::exists("/p"));
        let w = Waiting {
            ticket: 0,
            query: query.clone(),
            site: None,
        };
        r.fulfil(&w, Value::Exists(false));
        assert!(matches!(
            r.resolve(&t, &query),
            Answer::Now(_, Source::Confirmed)
        ));
        let id = t.push(op(vec![Key::file("/p")]));
        // pending writer: not trusted
        assert_eq!(r.resolve(&t, &query), Answer::Block);
        r.mark_applied(&t, &[id]);
        assert!(!r.is_confirmed(&query));
        assert_eq!(r.resolve(&t, &query), Answer::Block);
    }

    #[test]
    fn unrelated_write_keeps_confirmed() {
        let mut t = Trace::default();
        let mut r = Resolver::default();
        let query = q(Fact::exists("/p"));
        r.fulfil(
            &Waiting {
                ticket: 0,
                query: query.clone(),
                site: None,
            },
            Value::Exists(true),
        );
        let id = t.push(op(vec![Key::file("/other")]));
        r.mark_applied(&t, &[id]);
        assert!(r.is_confirmed(&query));
    }
}
