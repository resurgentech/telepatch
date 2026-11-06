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

/// Run `playbook` on one host through the engine (fake executor, every fact
/// absent) and render the optimized plan: the windows the executor received,
/// merged ops annotated with their member roles.
pub fn plan<V>(playbook: &Playbook<V>, host: &FixtureHost<V>) -> String {
    plan_with(playbook, host, FakeExecutor::new())
}

/// Like [`plan`] with fixture answers for facts.
pub fn plan_with<V>(playbook: &Playbook<V>, host: &FixtureHost<V>, fake: FakeExecutor) -> String {
    let h = Host::new(host.name.clone(), host.os.clone(), host.vars.clone());
    let mut engine = crate::engine::Engine::new(fake);
    block_on(engine.run_host(playbook, h.clone()))
        .unwrap_or_else(|e| panic!("playbook {} on {}: {e}", playbook.name, host.name));
    let windows: Vec<crate::engine::Window> = engine
        .executor
        .windows
        .iter()
        .map(|(_, w)| w.clone())
        .collect();
    crate::plan::render_windows(
        &host.name,
        &host.os,
        engine.executor.blocking_reads(),
        &windows,
    )
}

/// [`plan`] for several hosts, separated by a blank line.
pub fn plan_all<V>(playbook: &Playbook<V>, hosts: &[FixtureHost<V>]) -> String {
    hosts
        .iter()
        .map(|h| plan(playbook, h))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render the optimized plan of `playbook` for the embedded os-release
/// fixture `distro` (see [`crate::facts::os::FIXTURES`]), every fact
/// answered absent. The host is named after the distro. Panics on an
/// unknown distro or a failing playbook; meant for downstream snapshot tests.
pub fn plan_for<V>(distro: &str, playbook: &Playbook<V>, vars: V) -> String {
    let os = Os::fixture(distro).unwrap_or_else(|| {
        let names: Vec<&str> = crate::facts::os::FIXTURES.iter().map(|(n, _)| *n).collect();
        panic!("unknown distro `{distro}`; fixtures: {}", names.join(", "))
    });
    plan(playbook, &FixtureHost::new(distro, os, Rc::new(vars)))
}

/// [`plan_for`] with vars built by the four layers (see [`crate::vars`])
/// from `vars_toml` (a vars file's text, `""` for none) and the playbook's
/// `defaults`. Panics with the loader's message when the vars are invalid.
pub fn plan_for_layers<V: serde::de::DeserializeOwned>(
    distro: &str,
    playbook: &Playbook<V>,
    vars_toml: &str,
    defaults: &str,
) -> String {
    let vars = crate::vars::load("vars", vars_toml, defaults).unwrap_or_else(|e| panic!("{e}"));
    plan_for(distro, playbook, vars)
}
