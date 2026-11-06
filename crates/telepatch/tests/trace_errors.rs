//! Trace errors (bad package names, unknown os family, build_dep off apt)
//! are recorded, not returned: `testing::trace` and the engine fail with them.

use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

use telepatch::engine::{Engine, EngineError};
use telepatch::facts::os::Os;
use telepatch::testing::{FakeExecutor, FixtureHost, block_on, trace};
use telepatch::{Host, Playbook, PlaybookFuture, TraceError, role};

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
        h.pkg(["bad\nname"]).pip();
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

/// The error text with this file's `file:line` replaced by `<site>`, after
/// checking it is exactly that line.
fn located(e: &TraceError, line: u32) -> String {
    let site = format!("{}:{line}", file!());
    let s = e.to_string();
    assert!(s.contains(&site), "expected {site} in:\n{s}");
    s.replace(&site, "<site>")
}

fn debian() -> Host<()> {
    let fx = fixture("debian12");
    Host::new("h", fx.os.clone(), fx.vars.clone())
}

#[role]
async fn inner(h: &Host<()>) -> Result<(), TraceError> {
    INNER_PKG_LINE.store(line!() + 1, Relaxed);
    h.pkg(["git", ""]);
    Ok(())
}
static INNER_PKG_LINE: AtomicU32 = AtomicU32::new(0);

#[role]
async fn outer(h: &Host<()>) -> Result<(), TraceError> {
    inner(h).await
}

#[test]
fn a_hard_error_prints_role_chain_and_site_and_no_permit() {
    let h = debian();
    let _ = block_on(outer(&h));
    let e = h.snapshot().errors;
    assert_eq!(e.len(), 1, "{e:?}");
    assert_eq!(e[0].roles, ["outer", "inner"]);
    let t = located(&e[0], INNER_PKG_LINE.load(Relaxed));
    assert!(!t.contains("to permit"), "{t}");
    insta::assert_snapshot!(t, @r#"
    trace error: invalid package name ""
      in role outer > inner, <site>
    "#);
}

#[test]
fn the_site_is_the_callers_line_through_every_layer() {
    let h = debian();
    // A host builder through pkg -> pkg_op -> pkg_spec_op.
    let l1 = line!() + 1;
    h.pkg([""]);
    // A handle builder after a line break in the chain.
    let l2 = line!() + 4;
    h.file("/etc/sudoers.d/tp")
        .content("a")
        .mode(0o440)
        .validate("visudo -c");
    // A handle builder through a private helper (want).
    let l3 = line!() + 1;
    h.pkg(["git", "tig"]).pin("1");
    // A source method on a user scope (pkg_spec_op directly).
    let l4 = line!() + 1;
    h.as_user("bob").pkg(["node"]).mise();
    // Recorded on the handle by the host builder (svc is root only).
    let l5 = line!() + 1;
    h.as_user("bob").svc("x").enabled();
    // A repo builder through its private `fail`.
    let l6 = line!() + 1;
    h.repo("bad name");
    // Derived once the op is complete (svc wants nothing): the op's site.
    let l7 = line!() + 1;
    h.svc("y");
    let e = h.snapshot().errors;
    let sites: Vec<String> = e
        .iter()
        .map(|e| e.site.map(|s| s.to_string()).unwrap_or_default())
        .collect();
    let want: Vec<String> = [l1, l2, l3, l4, l5, l6, l7]
        .iter()
        .map(|l| format!("{}:{l}", file!()))
        .collect();
    assert_eq!(sites, want, "{e:?}");
    assert!(e.iter().all(|e| e.roles.is_empty()));
    assert_eq!(
        located(&e[0], l1),
        "trace error: invalid package name \"\"\n  at <site>"
    );
}

#[role]
async fn returns_err(h: &Host<()>) -> Result<(), TraceError> {
    let _ = h;
    RETURNS_ERR_LINE.store(line!() + 1, Relaxed);
    Err(TraceError::new("boom"))
}
static RETURNS_ERR_LINE: AtomicU32 = AtomicU32::new(0);

#[test]
fn a_returned_error_carries_its_construction_site() {
    let h = debian();
    let e = block_on(returns_err(&h)).unwrap_err();
    assert_eq!(e.message, "boom");
    assert_eq!(
        located(&e, RETURNS_ERR_LINE.load(Relaxed)),
        "trace error: boom\n  at <site>"
    );
}
