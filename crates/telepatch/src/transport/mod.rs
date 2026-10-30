//! Transports open shells on a host. A transport decides only which argv
//! to spawn; the shell itself is [`crate::pool::Shell`].

pub mod local;

use crate::pool::Shell;
use crate::trace::Principal;

/// Why a host gave fewer shells than asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Limit {
    MaxSessions,
    ConnectRefused,
    AuthFailed,
    SudoRefused,
    Other(String),
}

/// The answer to `open`: `want` is a hint, `got.len()` is the fact.
pub struct Shells {
    pub got: Vec<Shell>,
    pub want: usize,
    pub limit: Option<Limit>,
}

/// Opens shells for a host. The futures are `!Send` by design (the engine
/// runs on a current-thread runtime).
#[allow(async_fn_in_trait)]
pub trait Transport {
    async fn open(&self, host: &str, principal: &Principal, want: usize) -> Shells;
}
