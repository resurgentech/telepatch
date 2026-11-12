//! Policy errors: a trace error the project forbids but an op can permit
//! with `.allow(Policy::Name, "reason")`. `UnitViaFile` is the one checked
//! so far: `h.file` under `/etc/systemd/system/`.

use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

use telepatch::facts::os::Os;
use telepatch::testing::{FixtureHost, plan};
use telepatch::{Host, Playbook, PlaybookFuture, Policy, TraceError, role};

fn os() -> Os {
    Os::parse("ID=debian\nVERSION_ID=12\nVERSION_CODENAME=bookworm\n").unwrap()
}

fn host() -> Host<()> {
    Host::new("h", os(), Rc::new(()))
}

fn errors(h: &Host<()>) -> Vec<TraceError> {
    h.snapshot().errors
}

/// The error text with this file's `file:line` replaced by `<site>`, after
/// checking it is exactly `site`.
fn text(e: &TraceError, line: u32) -> String {
    let site = format!("{}:{line}", file!());
    let s = e.to_string();
    assert!(s.contains(&site), "expected {site} in:\n{s}");
    s.replace(&site, "<site>")
}

#[role]
async fn vendor_agent(h: &Host<()>) -> Result<(), TraceError> {
    VENDOR_FILE_LINE.store(line!() + 1, Relaxed);
    h.file("/etc/systemd/system/legacy.service")
        .content("[Service]\nExecStart=/opt/vendor/agent\n");
    Ok(())
}
static VENDOR_FILE_LINE: AtomicU32 = AtomicU32::new(0);

#[test]
fn policy_error_prints_its_name_and_the_permitting_call() {
    let h = host();
    let _ = telepatch::testing::block_on(vendor_agent(&h));
    let e = errors(&h);
    assert_eq!(e.len(), 1, "{e:?}");
    assert_eq!(e[0].policy, Some(Policy::UnitViaFile));
    insta::assert_snapshot!(text(&e[0], VENDOR_FILE_LINE.load(Relaxed)), @r#"
    trace error: file /etc/systemd/system/legacy.service: unit files go through h.unit (policy: unit-via-file)
      in role vendor_agent, <site>
      to permit: .allow(Policy::UnitViaFile, "<reason>")
    "#);
}

#[test]
fn an_allowance_permits_the_op() {
    let h = host();
    h.file("/etc/systemd/system/legacy.service")
        .content("x")
        .allow(Policy::UnitViaFile, "vendor ships this unit");
    assert!(errors(&h).is_empty(), "{:?}", errors(&h));
}

#[test]
fn an_allowance_may_come_later_on_the_handle() {
    let h = host();
    let f = h.file("/etc/systemd/system/a.service");
    f.content("x");
    assert_eq!(errors(&h).len(), 1);
    f.allow(Policy::UnitViaFile, "kept by the vendor");
    assert!(errors(&h).is_empty(), "{:?}", errors(&h));
}

#[test]
fn every_file_form_under_etc_systemd_system_trips() {
    let h = host();
    h.file("/etc/systemd/system/a.service").content("x");
    h.file("/etc/systemd/system/b.service").line("X=1");
    h.file("/etc/systemd/system/c.service.d").dir();
    h.file("/etc/systemd/system/d.service").absent();
    h.file("/etc/systemd/system/e.service");
    h.file("/etc/systemd//system/f.service").content("x");
    h.file("/etc/systemd/./system/g.service").content("x");
    h.file("/etc/systemd/system/h.service")
        .line_re("^X=", "X=1");
    let e = errors(&h);
    assert_eq!(e.len(), 8, "{e:?}");
    assert!(e.iter().all(|e| e.policy == Some(Policy::UnitViaFile)));
}

#[test]
fn package_owned_unit_dirs_do_not_trip() {
    let h = host();
    h.file("/lib/systemd/system/a.service").content("x");
    h.file("/usr/lib/systemd/system/b.service").content("x");
    h.file("/etc/systemd/system").dir();
    h.file("/etc/systemd/system.conf").line("X=1");
    h.file("/etc/systemd/systemx/a").content("x");
    assert!(errors(&h).is_empty(), "{:?}", errors(&h));
}

#[test]
fn an_empty_reason_is_a_hard_error() {
    for reason in ["", "   "] {
        let h = host();
        let line = line!() + 3;
        h.file("/etc/systemd/system/a.service")
            .content("x")
            .allow(Policy::UnitViaFile, reason);
        let e = errors(&h);
        assert!(!e.is_empty());
        assert_eq!(e[0].policy, None, "{e:?}");
        let t = text(&e[0], line);
        assert!(!t.contains("to permit"), "{t}");
        insta::assert_snapshot!("empty_reason", t);
    }
}

#[test]
fn policy_names_are_kebab() {
    assert_eq!(Policy::UnitViaFile.to_string(), "unit-via-file");
    assert_eq!(Policy::UnguardedRestart.to_string(), "unguarded-restart");
    assert_eq!(Policy::Unverified.to_string(), "unverified");
}

#[role]
async fn legacy(h: &Host<()>) -> Result<(), TraceError> {
    h.file("/etc/systemd/system/legacy.service")
        .content("[Service]\nExecStart=/opt/vendor/agent\n")
        .allow(
            Policy::UnitViaFile,
            "vendor ships this unit and rewrites it on upgrade",
        );
    Ok(())
}

fn legacy_run(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move { legacy(&h).await })
}

#[test]
fn the_plan_lists_every_allowance() {
    let pb = Playbook::new("legacy", legacy_run);
    let fx = FixtureHost::new("ws1", os(), Rc::new(()));
    let p = plan(&pb, &fx);
    assert!(
        p.contains("allowed unit-via-file: vendor ships this unit and rewrites it on upgrade"),
        "{p}"
    );
    let t = telepatch::testing::trace(&pb, &fx);
    assert!(
        t.contains("allowed unit-via-file: vendor ships this unit and rewrites it on upgrade"),
        "{t}"
    );
}

#[role]
async fn allowed_tmp(h: &Host<std::path::PathBuf>) -> Result<(), TraceError> {
    let p = h.vars.join("x.conf");
    h.file(p.to_str().unwrap())
        .content("x\n")
        .allow(Policy::UnitViaFile, "a reason the report shows");
    Ok(())
}

fn allowed_tmp_run(h: Host<std::path::PathBuf>) -> PlaybookFuture {
    Box::pin(async move { allowed_tmp(&h).await })
}

#[test]
fn the_run_report_lists_every_allowance() {
    use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
    use telepatch::transport::local::LocalTransport;
    let d = tempfile::tempdir().unwrap();
    let out = Rc::new(std::cell::RefCell::new(String::new()));
    let report = run(
        &Playbook::new("allowed", allowed_tmp_run),
        vec![HostSpec {
            groups: Vec::new(),
            name: "local".into(),
            target: Target::Local(LocalTransport::new().uid(0)),
            vars: d.path().to_path_buf(),
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    assert_eq!(report.exit_code(), 0, "{}", report.render());
    let r = report.render();
    assert!(
        r.contains("allowed unit-via-file: a reason the report shows"),
        "{r}"
    );
}
