//! The real executor over the local transport (uid injected as 0, so no
//! sudo): idempotent re-runs, failure semantics, prepare failures.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::report::{HostStatus, Report};
use telepatch::transport::local::LocalTransport;
use telepatch::{Host, Playbook, PlaybookFuture, TraceError, role, sh};

fn local() -> Target {
    Target::Local(LocalTransport::new().uid(0))
}

fn spec(name: &str, dir: &std::path::Path) -> HostSpec<PathBuf> {
    HostSpec {
        name: name.into(),
        target: local(),
        vars: dir.to_path_buf(),
    }
}

fn go(pb: &Playbook<PathBuf>, hosts: Vec<HostSpec<PathBuf>>) -> (Report, String) {
    let out = Rc::new(RefCell::new(String::new()));
    let report = run(
        pb,
        hosts,
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    let text = out.borrow().clone();
    (report, text)
}

fn p(h: &Host<PathBuf>, name: &str) -> String {
    h.vars.join(name).to_str().unwrap().to_string()
}

#[role]
async fn files(h: &Host<PathBuf>) -> Result<(), TraceError> {
    let x = p(h, "x.conf");
    let y = p(h, "y.conf");
    let made = p(h, "made");
    h.file(&x).content("hello\n");
    h.file(&y).line("A=1");
    sh!(h, "echo building; touch {made}", creates = "{made}");
    Ok(())
}

fn e2e_run(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move { files(&h).await })
}

fn scrub(s: &str, dir: &std::path::Path) -> String {
    s.replace(dir.to_str().unwrap(), "$TMP")
}

#[test]
fn e2e_local_twice_second_run_unchanged() {
    let d = tempfile::tempdir().unwrap();
    let pb = Playbook {
        name: "e2e",
        run: e2e_run,
    };
    let (r1, out1) = go(&pb, vec![spec("local", d.path())]);
    assert_eq!(r1.exit_code(), 0, "{}", r1.render());
    assert_eq!(r1.hosts[0].status, HostStatus::Changed);
    assert_eq!(
        std::fs::read_to_string(d.path().join("x.conf")).unwrap(),
        "hello\n"
    );
    let (r2, out2) = go(&pb, vec![spec("local", d.path())]);
    assert_eq!(r2.hosts[0].status, HostStatus::Ok, "{}", r2.render());
    assert!(r2.hosts[0].ops.iter().all(|o| !o.changed));
    insta::assert_snapshot!(
        "e2e_local_twice",
        format!(
            "-- run 1 table --\n{}\n-- run 1 stream --\n{}\n-- run 2 table --\n{}\n-- run 2 stream --\n{}",
            scrub(&r1.render(), d.path()),
            scrub(&out1, d.path()),
            scrub(&r2.render(), d.path()),
            scrub(&out2, d.path()),
        )
    );
}

#[role]
async fn first(h: &Host<PathBuf>) -> Result<(), TraceError> {
    let before = p(h, "before");
    let after = p(h, "after");
    sh!(h, "touch {before}");
    sh!(h, "echo one; echo two >&2; echo boom-last; exit 3");
    h.file(&after).content("x");
    Ok(())
}

#[role]
async fn second(h: &Host<PathBuf>) -> Result<(), TraceError> {
    let later = p(h, "later");
    h.file(&later).content("x");
    Ok(())
}

fn failing(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        first(&h).await?;
        second(&h).await
    })
}

#[test]
fn failure_fails_role_skips_rest_and_exits_nonzero() {
    let d = tempfile::tempdir().unwrap();
    let pb = Playbook {
        name: "fail",
        run: failing,
    };
    let (r, out) = go(&pb, vec![spec("h1", d.path())]);
    assert_eq!(r.exit_code(), 1);
    let HostStatus::Failed {
        role, tail, rc, op, ..
    } = &r.hosts[0].status
    else {
        panic!("{:?}", r.hosts[0].status)
    };
    assert_eq!(role, "first");
    assert_eq!(*rc, 3);
    assert!(op.contains("echo one"), "{op}");
    assert!(tail.contains("boom-last") && tail.contains("two"), "{tail}");
    assert_eq!(r.hosts[0].skipped_roles, vec!["second".to_string()]);
    assert!(d.path().join("before").exists());
    assert!(
        !d.path().join("after").exists(),
        "pipelined op must not run"
    );
    assert!(!d.path().join("later").exists());
    assert!(
        r.render().starts_with("HOST"),
        "table first: {}",
        r.render()
    );
    assert!(out.contains("not run"), "{out}");
    insta::assert_snapshot!("failure", scrub(&r.render(), d.path()));
}

#[test]
fn failure_tail_is_last_40_lines() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            sh!(h, "seq 1 100; exit 1");
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    let pb = Playbook {
        name: "tail",
        run: run_,
    };
    let (r, _) = go(&pb, vec![spec("h1", d.path())]);
    let HostStatus::Failed { tail, .. } = &r.hosts[0].status else {
        panic!()
    };
    let lines: Vec<&str> = tail.lines().collect();
    assert_eq!(lines.len(), 40, "{tail}");
    assert_eq!(lines[0], "61");
    assert_eq!(lines[39], "100");
}

#[test]
fn allow_fail_continues() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            let ok = p(&h, "ok");
            sh!(h, "exit 5").allow_fail();
            h.file(&ok).content("y");
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    let pb = Playbook {
        name: "af",
        run: run_,
    };
    let (r, _) = go(&pb, vec![spec("h1", d.path())]);
    assert_eq!(r.exit_code(), 0, "{}", r.render());
    assert!(d.path().join("ok").exists());
}

#[test]
fn failure_spans_windows_and_skips_later_roles() {
    #[role]
    async fn a(h: &Host<PathBuf>) -> Result<(), TraceError> {
        sh!(h, "exit 1");
        // a read forces an implicit barrier: the failing op is applied here
        let _ = h.exists("/nonexistent-tp").await;
        Ok(())
    }
    #[role]
    async fn b(h: &Host<PathBuf>) -> Result<(), TraceError> {
        let f = p(h, "b-ran");
        h.file(&f).content("x");
        Ok(())
    }
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            a(&h).await?;
            b(&h).await
        })
    }
    let d = tempfile::tempdir().unwrap();
    let pb = Playbook {
        name: "span",
        run: run_,
    };
    let (r, _) = go(&pb, vec![spec("h1", d.path())]);
    assert!(matches!(r.hosts[0].status, HostStatus::Failed { .. }));
    assert_eq!(r.hosts[0].skipped_roles, vec!["b".to_string()]);
    assert!(!d.path().join("b-ran").exists());
}

#[test]
fn unknown_user_blocks_before_apply_other_hosts_continue() {
    fn bad(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            let marker = p(&h, "marker");
            h.file(&marker).content("x");
            let u = h.as_user("tp_no_such_user_zz");
            sh!(u, "true");
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    let pb = Playbook {
        name: "blocked",
        run: bad,
    };
    let (r, _) = go(&pb, vec![spec("h1", d.path())]);
    let HostStatus::Blocked(why) = &r.hosts[0].status else {
        panic!("{:?}", r.hosts[0].status)
    };
    assert!(why.contains("tp_no_such_user_zz"), "{why}");
    assert!(!d.path().join("marker").exists(), "nothing applied");
    assert_eq!(r.exit_code(), 1);
}

#[test]
fn blocked_host_does_not_stop_others() {
    fn pbk(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            let marker = p(&h, "m");
            h.file(&marker).content("x");
            if h.name() == "bad" {
                let u = h.as_user("tp_no_such_user_zz");
                sh!(u, "true");
            }
            Ok(())
        })
    }
    let d1 = tempfile::tempdir().unwrap();
    let d2 = tempfile::tempdir().unwrap();
    let pb = Playbook {
        name: "two",
        run: pbk,
    };
    let (r, _) = go(&pb, vec![spec("bad", d1.path()), spec("good", d2.path())]);
    assert!(matches!(r.hosts[0].status, HostStatus::Blocked(_)));
    assert_eq!(r.hosts[1].status, HostStatus::Changed);
    assert!(d2.path().join("m").exists());
    assert!(!d1.path().join("m").exists());
    assert_eq!(r.exit_code(), 1);
}

#[test]
fn jsonl_log_has_events() {
    let d = tempfile::tempdir().unwrap();
    let log = d.path().join("run.jsonl");
    let pb = Playbook {
        name: "log",
        run: e2e_run,
    };
    let r = run(
        &pb,
        vec![spec("local", d.path())],
        RunOpts {
            log: Some(log.clone()),
            stdout: Stdout::Quiet,
        },
    );
    assert_eq!(r.exit_code(), 0);
    let text = std::fs::read_to_string(&log).unwrap();
    let evs: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let kinds: Vec<&str> = evs.iter().map(|e| e["ev"].as_str().unwrap()).collect();
    for want in [
        "host_open",
        "window",
        "op_start",
        "output",
        "op_result",
        "host_status",
    ] {
        assert!(kinds.contains(&want), "{want} in {kinds:?}");
    }
}
