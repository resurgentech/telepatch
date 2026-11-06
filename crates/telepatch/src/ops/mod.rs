//! Primitive op templates: `pkg`, `file`, `fetch`, `group`, `user`, `repo`, `svc`, `unit`, `sh`. Each module holds the op's
//! spec (data, so T8 can merge it), its builder methods on [`Host`], and the
//! script body it renders. [`crate::trailer`] wraps bodies for execution.
//!
//! [`Host`]: crate::Host

pub mod fetch;
pub mod file;
pub mod group;
pub mod pkg;
pub mod repo;
pub mod shell;
pub mod svc;
pub mod unit;
pub mod user;
