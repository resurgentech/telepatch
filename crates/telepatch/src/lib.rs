//! telepatch: trace imperative Rust roles into a per-host graph and stream
//! it into persistent shells. See `docs/execution-model.html`.
//!
//! Tracing model: a playbook entry takes an owned [`Host`] and returns a
//! `'static`, `!Send` future ([`PlaybookFuture`]). Every op called on the
//! host appends a node to that host's [`Trace`] and returns an [`OpHandle`]
//! whose builder methods edit the node in place. Nothing executes.

pub mod cli;
pub mod engine;
pub mod executor;
pub mod facts;
pub mod host;
pub mod key;
pub mod ops;
pub mod optimize;
pub mod plan;
pub mod pool;
pub mod prepare;
pub mod report;
pub mod resolver;
pub mod role;
pub mod sh;
pub mod testing;
pub mod trace;
pub mod trailer;
pub mod transport;

pub use host::{Host, OpHandle, Scope, TraceError};
pub use key::{Key, Value};
pub use role::{Playbook, PlaybookFuture, RoleGuard};
pub use telepatch_macros::*;
pub use trace::{Guard, Node, NodeId, OpNode, OpSpec, Principal, RunAs, Source, Trace};
