//! Trace errors (bad package names, unknown os family, build_dep off apt)
//! are recorded, not returned: `testing::trace` and the engine fail with them.

use std::rc::Rc;

use telepatch::engine::{Engine, EngineError};
use telepatch::facts::os::Os;
use telepatch::testing::{FakeExecutor, FixtureHost, block_on, trace};
use telepatch::{Host, Playbook, PlaybookFuture};

fn fixture(name: &str) -> FixtureHost<()> {
    let p = format!(
        "{}/tests/fixtures/os-release/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let os = Os::parse(&std::fs::read_to_string(p).unwrap()).unwrap();
    FixtureHost::new("h", os, Rc::new(()))
}

fn empty_name(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        h.raw("echo first");
        h.pkg(["ok", ""]);
        Ok(())
    })
}
fn control_name(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        h.pip(["bad\nname"]);
        Ok(())
    })
}
fn build_dep_on_dnf(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        h.raw("echo first");
        h.pkg(["git"]).build_dep();
        Ok(())
    })
}
fn unknown_family(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        h.raw("echo first");
        h.pkg(["git"]);
        Ok(())
    })
}

type Run = fn(Host<()>) -> PlaybookFuture;

const CASES: [(&str, Run, &str, &str); 4] = [
    ("empty_name", empty_name, "debian12", "invalid package name"),
    (
        "control_name",
        control_name,
        "debian12",
        "invalid package name",
    ),
    (
        "build_dep_non_apt",
        build_dep_on_dnf,
        "rockylinux9",
        "build_dep() is only available on apt hosts",
    ),
    (
        "unknown_family",
        unknown_family,
        "omarchy-host",
        "no package manager known",
    ),
];

fn host_for(fx: &str) -> FixtureHost<()> {
    if fx == "omarchy-host" {
        // A family the manager table does not know.
        FixtureHost::new(
            "h",
            Os::parse("ID=nixos\nVERSION_ID=24\n").unwrap(),
            Rc::new(()),
        )
    } else {
        fixture(fx)
    }
}

#[test]
fn engine_returns_the_recorded_error_before_applying() {
    for (name, run, fx, msg) in CASES {
        let host = host_for(fx);
        let h = Host::new("h", host.os.clone(), host.vars.clone());
        let mut engine = Engine::new(FakeExecutor::new());
        let r = block_on(engine.run_host(&Playbook { name, run }, h));
        match r {
            Err(EngineError::Playbook(e)) => {
                assert!(e.to_string().contains(msg), "{name}: {e}")
            }
            other => panic!("{name}: expected a playbook error, got {other:?}"),
        }
        assert!(
            engine
                .executor
                .calls
                .iter()
                .all(|(_, c)| !matches!(c, telepatch::testing::fake::Call::Apply { .. })),
            "{name}: something was applied"
        );
    }
}

#[test]
fn testing_trace_fails_with_the_recorded_error() {
    for (name, run, fx, msg) in CASES {
        let host = host_for(fx);
        let r = std::panic::catch_unwind(|| trace(&Playbook { name, run }, &host));
        let payload = r.expect_err(name);
        let text = payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap();
        assert!(text.contains(msg), "{name}: {text}");
    }
}
