//! `--serial`, `--max-fail` and `--only` (T100): the host limiter in the
//! executor, the selection check `#[role]` makes at entry, and the CLI
//! wiring. Hosts are this machine through the local transport with the uid
//! injected as 0 (no sudo), logging into a temp file.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use telepatch::cli::run_with_args;
use telepatch::executor::{CheckOpts, HostSpec, RunControls, RunOpts, Stdout, Target, run_with};
use telepatch::facts::os::Os;
use telepatch::report::{HostStatus, Report};
use telepatch::transport::local::LocalTransport;
use telepatch::transport::ssh::SshTransport;
use telepatch::{Host, Node, Playbook, PlaybookFuture, TraceError, role, sh};

#[derive(Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
struct V {
    log: PathBuf,
    fail: bool,
}

// ---- playbooks ----

/// Logs `s <host>`, works for 300 ms, logs `e <host>`; a `fail` host exits 1
/// after its start line.
#[role]
async fn work(h: &Host<V>) -> Result<(), TraceError> {
    let (log, name) = (h.vars.log.to_str().unwrap().to_string(), h.name());
    if h.vars.fail {
        sh!(h, "echo s {name} >> {log}; false");
    } else {
        sh!(
            h,
            "echo s {name} >> {log}; sleep 0.3; echo e {name} >> {log}"
        );
    }
    Ok(())
}

fn work_entry(h: Host<V>) -> PlaybookFuture {
    Box::pin(async move { work(&h).await })
}

fn work_pb() -> Playbook<V> {
    Playbook::new("work", work_entry)
}

/// A trace error on a `fail` host, so a `--check` can have a failed host.
#[role]
async fn picky(h: &Host<V>) -> Result<(), TraceError> {
    if h.vars.fail {
        return Err(TraceError::new("picky host"));
    }
    Ok(())
}

fn picky_entry(h: Host<V>) -> PlaybookFuture {
    Box::pin(async move { picky(&h).await })
}

fn mark(h: &Host<V>, name: &str) {
    let f = h.vars.log.to_str().unwrap().to_string();
    sh!(h, "echo {name} >> {f}");
}

#[role]
async fn alpha(h: &Host<V>) -> Result<(), TraceError> {
    mark(h, "alpha");
    Ok(())
}
#[role]
async fn beta(h: &Host<V>) -> Result<(), TraceError> {
    mark(h, "beta");
    Ok(())
}
#[role]
async fn inner(h: &Host<V>) {
    mark(h, "inner");
}
#[role]
async fn outer(h: &Host<V>) {
    mark(h, "outer");
    inner(h).await;
}

fn roles_entry(h: Host<V>) -> PlaybookFuture {
    Box::pin(async move {
        alpha(&h).await?;
        beta(&h).await?;
        outer(&h).await;
        Ok(())
    })
}

fn roles_pb() -> Playbook<V> {
    Playbook::new("roles", roles_entry)
}

// ---- helpers ----

fn hosts(n: usize, log: &std::path::Path, fail: &[usize]) -> Vec<HostSpec<V>> {
    (1..=n)
        .map(|i| HostSpec {
            groups: Vec::new(),
            name: format!("h{i}"),
            target: Target::Local(
                LocalTransport::new()
                    .uid(0)
                    .sandboxed_at(log.parent().unwrap()),
            ),
            vars: V {
                log: log.to_path_buf(),
                fail: fail.contains(&i),
            },
        })
        .collect()
}

fn go(
    pb: &Playbook<V>,
    hs: Vec<HostSpec<V>>,
    check: bool,
    controls: RunControls,
) -> (Report, String) {
    let out = Rc::new(RefCell::new(String::new()));
    let report = run_with(
        pb,
        hs,
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
        check.then(CheckOpts::default),
        controls,
    );
    let text = format!("{}{}", out.borrow(), report.render());
    (report, text)
}

fn lines(log: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

fn max_in_flight(ls: &[String]) -> usize {
    let (mut now, mut max) = (0usize, 0usize);
    for l in ls {
        if l.starts_with("s ") {
            now += 1;
            max = max.max(now);
        } else if l.starts_with("e ") {
            now -= 1;
        }
    }
    max
}

fn status<'a>(r: &'a Report, name: &str) -> &'a HostStatus {
    &r.hosts.iter().find(|h| h.name == name).unwrap().status
}

// ---- --serial ----

#[test]
fn serial_runs_n_hosts_at_a_time() {
    let d = tempfile::tempdir().unwrap();
    let log = d.path().join("log");
    let (r, text) = go(
        &work_pb(),
        hosts(4, &log, &[]),
        false,
        RunControls {
            serial: Some(2),
            ..Default::default()
        },
    );
    assert_eq!(r.exit_code(), 0, "{text}");
    let ls = lines(&log);
    assert_eq!(
        ls.iter().filter(|l| l.starts_with("e ")).count(),
        4,
        "{ls:?}"
    );
    assert_eq!(max_in_flight(&ls), 2, "{ls:?}");
    // The report keeps the hosts in the order given.
    let names: Vec<&str> = r.hosts.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(names, ["h1", "h2", "h3", "h4"]);
}

#[test]
fn serial_one_is_one_host_at_a_time() {
    let d = tempfile::tempdir().unwrap();
    let log = d.path().join("log");
    let (r, text) = go(
        &work_pb(),
        hosts(3, &log, &[]),
        false,
        RunControls {
            serial: Some(1),
            ..Default::default()
        },
    );
    assert_eq!(r.exit_code(), 0, "{text}");
    assert_eq!(max_in_flight(&lines(&log)), 1);
}

// ---- --max-fail ----

#[test]
fn max_fail_zero_stops_starting_hosts_after_the_first_failure() {
    let d = tempfile::tempdir().unwrap();
    let log = d.path().join("log");
    let (r, text) = go(
        &work_pb(),
        hosts(4, &log, &[1]),
        false,
        RunControls {
            serial: Some(1),
            max_fail: Some(0),
            ..Default::default()
        },
    );
    assert!(
        matches!(status(&r, "h1"), HostStatus::Failed { .. }),
        "{text}"
    );
    for h in ["h2", "h3", "h4"] {
        assert_eq!(
            status(&r, h),
            &HostStatus::NotStarted("max-fail 0 reached".into()),
            "{text}"
        );
    }
    assert_eq!(lines(&log), ["s h1"], "no later host ran");
    assert_eq!(r.exit_code(), 1);
    assert!(
        text.contains("h2: not started: max-fail 0 reached"),
        "{text}"
    );
    let row = text.lines().find(|l| l.starts_with("h3 ")).unwrap();
    assert!(
        row.contains("skipped") && row.contains("max-fail 0 reached"),
        "{row}"
    );
}

#[test]
fn max_fail_tolerates_n_failures() {
    let d = tempfile::tempdir().unwrap();
    let log = d.path().join("log");
    let (r, text) = go(
        &work_pb(),
        hosts(4, &log, &[1, 2]),
        false,
        RunControls {
            serial: Some(1),
            max_fail: Some(1),
            ..Default::default()
        },
    );
    // One failure is not more than 1: h2 starts, and its failure trips it.
    assert!(
        matches!(status(&r, "h1"), HostStatus::Failed { .. }),
        "{text}"
    );
    assert!(
        matches!(status(&r, "h2"), HostStatus::Failed { .. }),
        "{text}"
    );
    assert_eq!(
        status(&r, "h3"),
        &HostStatus::NotStarted("max-fail 1 reached".into())
    );
    assert_eq!(lines(&log), ["s h1", "s h2"]);
}

#[test]
fn max_fail_not_reached_runs_every_host() {
    let d = tempfile::tempdir().unwrap();
    let log = d.path().join("log");
    let (r, text) = go(
        &work_pb(),
        hosts(3, &log, &[2]),
        false,
        RunControls {
            serial: Some(1),
            max_fail: Some(1),
            ..Default::default()
        },
    );
    assert!(
        matches!(status(&r, "h3"), HostStatus::Ok | HostStatus::Changed),
        "{text}"
    );
    assert_eq!(r.exit_code(), 1, "the failed host still fails the run");
}

#[test]
fn serial_and_max_fail_work_under_check() {
    let d = tempfile::tempdir().unwrap();
    let log = d.path().join("log");
    let pb = Playbook::new("picky", picky_entry);
    let (r, text) = go(
        &pb,
        hosts(3, &log, &[1]),
        true,
        RunControls {
            serial: Some(1),
            max_fail: Some(0),
            ..Default::default()
        },
    );
    assert!(matches!(status(&r, "h1"), HostStatus::Error(_)), "{text}");
    assert!(
        matches!(status(&r, "h2"), HostStatus::NotStarted(_)),
        "{text}"
    );
    assert!(
        matches!(status(&r, "h3"), HostStatus::NotStarted(_)),
        "{text}"
    );
    assert_eq!(r.exit_code(), 1);
    assert!(
        text.contains("h2: not started: max-fail 0 reached"),
        "{text}"
    );
}

// ---- --only ----

fn only(names: &[&str]) -> RunControls {
    RunControls {
        only: names.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

#[test]
fn only_runs_the_named_roles_and_skips_the_rest() {
    let d = tempfile::tempdir().unwrap();
    let log = d.path().join("log");
    let (r, text) = go(&roles_pb(), hosts(1, &log, &[]), false, only(&["beta"]));
    assert_eq!(r.exit_code(), 0, "{text}");
    assert_eq!(lines(&log), ["beta"]);
    assert!(
        text.contains("h1: skipped by --only: alpha, outer"),
        "{text}"
    );
}

#[test]
fn a_nested_role_runs_whole_inside_a_selected_role() {
    let d = tempfile::tempdir().unwrap();
    let log = d.path().join("log");
    let (r, text) = go(&roles_pb(), hosts(1, &log, &[]), false, only(&["outer"]));
    assert_eq!(r.exit_code(), 0, "{text}");
    assert_eq!(lines(&log), ["outer", "inner"]);
}

#[test]
fn a_role_only_reached_through_a_skipped_one_is_unknown() {
    let d = tempfile::tempdir().unwrap();
    let log = d.path().join("log");
    let (r, text) = go(&roles_pb(), hosts(1, &log, &[]), false, only(&["inner"]));
    assert_eq!(r.exit_code(), 1, "{text}");
    assert!(
        text.contains("--only: unknown role `inner`; the playbook traced: alpha, beta, outer"),
        "{text}"
    );
    assert!(lines(&log).is_empty());
}

#[test]
fn only_with_an_unknown_name_lists_the_traced_roles() {
    let d = tempfile::tempdir().unwrap();
    let log = d.path().join("log");
    let (r, text) = go(
        &roles_pb(),
        hosts(1, &log, &[]),
        false,
        only(&["alpha", "nope", "nada"]),
    );
    assert!(matches!(status(&r, "h1"), HostStatus::Error(_)), "{text}");
    assert_eq!(r.exit_code(), 1);
    assert!(
        text.contains(
            "--only: unknown roles `nada`, `nope`; the playbook traced: alpha, beta, outer"
        ),
        "{text}"
    );
}

#[test]
fn only_applies_under_check() {
    let d = tempfile::tempdir().unwrap();
    let log = d.path().join("log");
    let (r, text) = go(&roles_pb(), hosts(1, &log, &[]), true, only(&["alpha"]));
    assert_eq!(r.exit_code(), 2, "{text}");
    assert!(lines(&log).is_empty(), "check applies nothing");
    assert!(text.contains("skipped by --only: beta, outer"), "{text}");
}

// ---- the macro ----

#[test]
fn a_role_not_selected_records_its_marks_and_runs_no_body() {
    let h = Host::new(
        "t",
        Os::parse("ID=debian\nVERSION_ID=12\n").unwrap(),
        Rc::new(V::default()),
    )
    .with_only(["beta"]);
    let mut f = Box::pin(alpha(&h));
    let poll = f
        .as_mut()
        .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()));
    assert!(matches!(poll, std::task::Poll::Ready(Ok(()))));
    let marks: Vec<String> = h
        .snapshot()
        .nodes
        .iter()
        .map(|n| match n {
            Node::RoleBegin(s) => format!("begin {s}"),
            Node::RoleEnd(s) => format!("end {s}"),
            _ => "other".into(),
        })
        .collect();
    assert_eq!(marks, ["begin alpha", "end alpha"]);
}

// ---- the CLI ----

fn cli(a: &[&str]) -> (String, i32) {
    let args: Vec<String> = std::iter::once("pb")
        .chain(a.iter().copied())
        .map(String::from)
        .collect();
    let sb = telepatch::testing::sandbox();
    run_with_args(
        &args,
        &[Playbook::new("roles", roles_entry)],
        || LocalTransport::new().uid(0).sandboxed(&sb),
        SshTransport::new,
    )
}

fn vars_file(d: &std::path::Path) -> String {
    let p = d.join("vars.toml");
    std::fs::write(&p, format!("log = {:?}\n", d.join("log").to_str().unwrap())).unwrap();
    p.to_str().unwrap().to_string()
}

#[test]
fn cli_only_runs_the_roles_named_and_serial_max_fail_parse() {
    let d = tempfile::tempdir().unwrap();
    let v = vars_file(d.path());
    let (out, code) = cli(&[
        "run",
        "roles",
        "--local",
        "--vars",
        &v,
        "--only",
        "alpha,beta",
        "--serial",
        "1",
        "--max-fail",
        "0",
    ]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(lines(&d.path().join("log")), ["alpha", "beta"]);
}

#[test]
fn cli_only_is_repeatable_and_unknown_is_listed() {
    let d = tempfile::tempdir().unwrap();
    let v = vars_file(d.path());
    let (out, code) = cli(&[
        "run", "roles", "--local", "--vars", &v, "--only", "alpha", "--only", "zzz",
    ]);
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("unknown role `zzz`"), "{out}");
    assert!(out.contains("alpha, beta, outer"), "{out}");
}

#[test]
fn cli_serial_zero_is_a_usage_error() {
    let (out, code) = cli(&["run", "roles", "--local", "--serial", "0"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("--serial"), "{out}");
}

#[test]
fn cli_max_fail_needs_a_number() {
    let (out, code) = cli(&["run", "roles", "--local", "--max-fail", "x"]);
    assert_eq!(code, 2, "{out}");
}
