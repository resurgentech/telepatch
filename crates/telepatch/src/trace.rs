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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    Root,
    User(String),
}

/// Where an op runs. Ops issued on `h` run in the plain root shell
/// (`login: false`). Ops issued through `h.as_user(u)` run in a fresh login
/// environment of `u` (`login: true`), including `as_user("root")`: no
/// privilege change, but the login environment.
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

impl OpSpec {
    /// Short kind name used in plans.
    pub fn kind(&self) -> &'static str {
        match self {
            OpSpec::Raw { .. } => "sh",
        }
    }

    /// The shell script this op runs on a host with the given `os`.
    pub fn script(&self, _os: &Os) -> String {
        match self {
            OpSpec::Raw { script } => script.clone(),
        }
    }

    /// Effects of an op of this kind before the caller narrows them.
    pub fn default_writes(&self) -> BTreeSet<Key> {
        match self {
            OpSpec::Raw { .. } => BTreeSet::from([Key::Any]),
        }
    }
}

/// Run an op only if an earlier op's output says so. The type exists; no
/// builder sets one yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Guard {
    Changed(NodeId),
    ExitOk(NodeId),
    Not(Box<Guard>),
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
}

impl Trace {
    pub fn push(&mut self, node: Node) -> NodeId {
        self.nodes.push(node);
        NodeId(self.nodes.len() - 1)
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
