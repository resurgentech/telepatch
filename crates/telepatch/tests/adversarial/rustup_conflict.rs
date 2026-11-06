//! Two rustup ops for one user and toolchain that disagree on `profile` (or
//! `installer`) are a trace error naming both call sites.

use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

use telepatch::engine::{Engine, EngineError};
use telepatch::testing::{FakeExecutor, block_on};
use telepatch::{Host, Playbook, PlaybookFuture, TraceError};

static FIRST: AtomicU32 = AtomicU32::new(0);
static SECOND: AtomicU32 = AtomicU32::new(0);

fn run(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        {
            let _r = h.role_enter("a");
            FIRST.store(line!() + 1, Relaxed);
            h.pkg(["stable"]).rustup().profile("minimal");
        }
        {
            let _r = h.role_enter("b");
            SECOND.store(line!() + 1, Relaxed);
            h.pkg(["stable"]).rustup().components(["rust-src"]);
        }
        Ok::<(), TraceError>(())
    })
}

fn error_of(run: fn(Host<()>) -> PlaybookFuture) -> (String, bool) {
    let host = super::common::ubuntu();
    let h = Host::new("web1", host.os.clone(), host.vars.clone());
    let mut engine = Engine::new(FakeExecutor::new());
    let r = block_on(engine.run_host(&Playbook::new("t", run), h));
    let applied = engine
        .executor
        .calls
        .iter()
        .any(|(_, c)| matches!(c, telepatch::testing::fake::Call::Apply { .. }));
    match r {
        Err(EngineError::Playbook(e)) => (e.to_string(), applied),
        other => panic!("expected a playbook error, got {other:?}"),
    }
}

#[test]
fn conflicting_profile_is_a_trace_error_naming_both_sites() {
    let (msg, applied) = error_of(run);
    assert!(!applied, "something was applied");
    assert!(msg.contains("profile"), "{msg}");
    assert!(msg.contains("stable"), "{msg}");
    for l in [FIRST.load(Relaxed), SECOND.load(Relaxed)] {
        assert!(
            msg.contains(&format!("rustup_conflict.rs:{l}")),
            "no site line {l} in: {msg}"
        );
    }
}

fn installer_run(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        h.pkg(["stable"]).rustup().installer("sh ./a.sh -y");
        h.pkg(["stable"]).rustup();
        Ok::<(), TraceError>(())
    })
}

#[test]
fn conflicting_installer_is_a_trace_error() {
    let (msg, _) = error_of(installer_run);
    assert!(msg.contains("installer"), "{msg}");
}

fn same_run(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        // `default` spelled out equals the default; other users and
        // toolchains are free to differ.
        h.pkg(["stable"]).rustup().profile("default");
        h.pkg(["stable"]).rustup();
        h.pkg(["nightly"]).rustup().profile("minimal");
        h.as_user("deploy")
            .pkg(["stable"])
            .rustup()
            .profile("minimal");
        Ok::<(), TraceError>(())
    })
}

#[test]
fn equal_effective_options_are_no_conflict() {
    let host = super::common::ubuntu();
    let h = Host::new("web1", host.os.clone(), host.vars.clone());
    let mut engine = Engine::new(FakeExecutor::new());
    block_on(engine.run_host(&Playbook::new("t", same_run), h)).unwrap();
}
