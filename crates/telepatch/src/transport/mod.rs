//! Transports open shells on a host. A transport decides only which argv
//! to spawn; the shell itself is [`crate::pool::Shell`].

pub mod docker;
pub mod local;
pub mod ssh;

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

/// Read the host's `os` fact through an open shell: run
/// [`crate::facts::os::READ_COMMAND`] as a raw op and parse its output.
/// T11 calls this when a host opens.
pub async fn read_os(shell: &mut Shell) -> Result<crate::facts::os::Os, String> {
    use crate::facts::os::{Os, READ_COMMAND};
    use crate::trace::Node;
    // Rendering needs an `Os` only for package ops; a raw op ignores it.
    let placeholder = Os::parse("ID=debian\n").map_err(|e| e.to_string())?;
    let h = crate::Host::new("os-probe", placeholder.clone(), std::rc::Rc::new(()));
    h.raw(READ_COMMAND);
    let snap = h.snapshot();
    let Some(Node::Op(op)) = snap.nodes.first() else {
        return Err("probe op was not recorded".into());
    };
    shell
        .send_op(op, 0, &placeholder)
        .await
        .map_err(|e| e.to_string())?;
    let out = shell.wait(0).await.map_err(|e| e.to_string())?;
    Os::parse(&String::from_utf8_lossy(&out.output)).map_err(|e| e.to_string())
}
