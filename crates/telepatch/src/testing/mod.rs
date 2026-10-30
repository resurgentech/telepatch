//! Snapshot harness: trace a playbook against fixture hosts and render the
//! plan text. Later engine steps extend this module.

use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use crate::facts::os::Os;
use crate::host::Host;
use crate::plan::render_host;
use crate::role::Playbook;

pub mod fake;

pub use fake::{FakeExecutor, run_engine};

/// A host description for tests: a name, an `Os` and variables.
pub struct FixtureHost<V> {
    pub name: String,
    pub os: Os,
    pub vars: Rc<V>,
}

impl<V> FixtureHost<V> {
    pub fn new(name: impl Into<String>, os: Os, vars: Rc<V>) -> FixtureHost<V> {
        FixtureHost {
            name: name.into(),
            os,
            vars,
        }
    }
}

/// Trace `playbook` on one host and return the plan text. Panics if the
/// playbook errors or ever returns `Pending` (nothing can wake it yet).
pub fn trace<V>(playbook: &Playbook<V>, host: &FixtureHost<V>) -> String {
    let h = Host::new(host.name.clone(), host.os.clone(), host.vars.clone());
    let mut fut = (playbook.run)(h.clone());
    let mut cx = Context::from_waker(Waker::noop());
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(Ok(())) => {}
        Poll::Ready(Err(e)) => panic!("playbook {} failed on {}: {e}", playbook.name, host.name),
        Poll::Pending => panic!(
            "playbook {} returned Pending on {}; the harness has no resolver yet",
            playbook.name, host.name
        ),
    }
    drop(fut);
    if let Some(e) = h.snapshot().errors.first() {
        panic!("playbook {} failed on {}: {e}", playbook.name, host.name);
    }
    render_host(&host.name, &host.os, &h.snapshot())
}

/// Trace on several hosts, in order, separated by a blank line.
pub fn trace_all<V>(playbook: &Playbook<V>, hosts: &[FixtureHost<V>]) -> String {
    hosts
        .iter()
        .map(|h| trace(playbook, h))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Poll `fut` to completion on this thread. The futures the fake executor
/// produces are always ready, so a `Pending` means a bug and panics.
pub fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    let mut fut = std::pin::pin!(fut);
    let mut cx = Context::from_waker(Waker::noop());
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("block_on: future returned Pending with nothing to wake it"),
    }
}
