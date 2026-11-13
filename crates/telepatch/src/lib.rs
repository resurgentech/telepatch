//! telepatch: trace imperative Rust roles into a per-host graph and stream
//! it into persistent shells. See `docs/execution-model.html`.
//!
//! Tracing model: a playbook entry takes an owned [`Host`] and returns a
//! `'static`, `!Send` future ([`PlaybookFuture`]). Every op called on the
//! host appends a node to that host's [`Trace`] and returns an [`OpHandle`]
//! whose builder methods edit the node in place. Nothing executes.

extern crate self as telepatch;

pub mod cli;
pub mod engine;
pub mod executor;
pub mod facts;
pub mod host;
pub mod inventory;
pub mod key;
pub mod local;
pub mod ops;
pub mod optimize;
pub mod plan;
pub mod policy;
pub mod pool;
pub mod prepare;
pub mod report;
pub mod resolver;
pub mod resource;
pub mod role;
pub mod secret;
pub mod sh;
pub mod testing;
pub mod trace;
pub mod trailer;
pub mod transport;
pub mod vars;

pub use host::{Host, OpHandle, RebootBarrier, Scope, TraceError};
pub use key::{Key, Value};
pub use local::{local, local_dir, local_file};
pub use ops::wait::{MIN, Port, SEC, Wait};
pub use policy::{Allowance, Policy};
pub use resource::{Change, Probe, Probed, Resource, Script, Terminfo, plan_against};
pub use role::{Playbook, PlaybookFuture, RoleGuard};
pub use secret::{Secret, SecretBackend, SecretError, SecretRef, SecretValue};
pub use telepatch_macros::*;
pub use trace::{Guard, Node, NodeId, OpNode, OpSpec, Principal, RunAs, Site, Source, Trace};
