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

// ---- pkg[script] (T32): a fake installer in the temp dir, no network ----

/// Write an executable via a child `sh` so no write fd lives in this test
/// process (a sibling thread's fork could inherit it; exec then fails with
/// ETXTBSY).
fn write_exe(p: &std::path::Path, text: &str) {
    use std::io::Write;
    let mut c = std::process::Command::new("sh")
        .arg("-c")
        .arg("cat > \"$1\" && chmod 755 \"$1\"")
        .arg("sh")
        .arg(p)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
    assert!(c.wait().unwrap().success());
}

/// A shell command that (re)writes `<dir>/tool` to print `tool <v>`, where
/// `<v>` is `$TP_WANT` when set, else `default`.
fn installer(dir: &str, default: &str) -> String {
    let tool = format!("{dir}/tool");
    format!(
        "v=${{TP_WANT:-{default}}}; printf '#!/bin/sh\\necho tool %s\\n' \"$v\" > {tool}.new && chmod 755 {tool}.new && mv {tool}.new {tool}"
    )
}

fn tool_spec(h: &Host<PathBuf>) -> telepatch::ops::pkg::ScriptSpec {
    let dir = h.vars.to_str().unwrap().to_string();
    telepatch::ops::pkg::ScriptSpec::new()
        .bin(format!("{dir}/tool"))
        .version(format!("{dir}/tool --version"))
        .install(installer(&dir, "1.2.3"))
}

fn tool_out(d: &std::path::Path) -> String {
    let o = std::process::Command::new(d.join("tool")).output().unwrap();
    String::from_utf8(o.stdout).unwrap()
}

#[test]
fn script_present_installs_once() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.script("tool", tool_spec(&h));
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    let pb = Playbook {
        name: "script",
        run: run_,
    };
    let (r1, out1) = go(&pb, vec![spec("h1", d.path())]);
    assert_eq!(r1.hosts[0].status, HostStatus::Changed, "{}", r1.render());
    let line = "h1: window 1 pkg[script] root tool: absent -> present";
    assert!(r1.render().contains(line), "{}", r1.render());
    assert!(
        out1.contains("== h1 [prepare] window 1 pkg[script] root tool: absent -> present =="),
        "{out1}"
    );
    assert!(out1.contains("pkg[script] install tool"), "{out1}");
    assert_eq!(tool_out(d.path()), "tool 1.2.3\n");
    let w = &r1.hosts[0].will_change[0];
    assert_eq!(
        (w.source.as_str(), w.user.as_str(), w.name.as_str()),
        ("script", "root", "tool")
    );
    assert_eq!(
        (w.installed.as_deref(), w.wanted.as_str()),
        (None, "present")
    );

    let (r2, out2) = go(&pb, vec![spec("h1", d.path())]);
    assert_eq!(r2.hosts[0].status, HostStatus::Ok, "{}", r2.render());
    assert!(r2.hosts[0].ops.iter().all(|o| !o.changed));
    assert!(r2.hosts[0].will_change.is_empty());
    assert!(!out2.contains("[prepare]"), "{out2}");
    assert!(!r2.render().contains("pkg[script]"), "{}", r2.render());
}

#[test]
fn script_will_change_is_a_json_event() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.script("tool", tool_spec(&h)).pin("2.0.0");
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    write_exe(&d.path().join("tool"), "#!/bin/sh\necho tool 1.0.0\n");
    let log = d.path().join("run.jsonl");
    let pb = Playbook {
        name: "script",
        run: run_,
    };
    let r = run(
        &pb,
        vec![spec("h1", d.path())],
        RunOpts {
            log: Some(log.clone()),
            stdout: Stdout::Quiet,
        },
    );
    assert_eq!(r.exit_code(), 0, "{}", r.render());
    let text = std::fs::read_to_string(&log).unwrap();
    let ev: serde_json::Value = text
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|e| e["ev"] == "will_change")
        .expect("a will_change event");
    assert_eq!(ev["host"], "h1");
    assert_eq!(ev["window"], 1);
    assert_eq!(ev["source"], "script");
    assert_eq!(ev["user"], "root");
    assert_eq!(ev["name"], "tool");
    assert_eq!(ev["installed"], "1.0.0");
    assert_eq!(ev["wanted"], "2.0.0");
}

#[test]
fn script_pin_mismatch_runs_update() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            let dir = h.vars.to_str().unwrap().to_string();
            h.script(
                "tool",
                tool_spec(&h)
                    .install("echo install-must-not-run; exit 9")
                    .update(installer(&dir, "never")),
            )
            .pin("2.0.0");
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    write_exe(&d.path().join("tool"), "#!/bin/sh\necho tool 1.0.0\n");
    let pb = Playbook {
        name: "pin",
        run: run_,
    };
    let (r, out) = go(&pb, vec![spec("h1", d.path())]);
    assert_eq!(
        r.hosts[0].status,
        HostStatus::Changed,
        "{}\n{out}",
        r.render()
    );
    assert!(
        r.render()
            .contains("h1: window 1 pkg[script] root tool: 1.0.0 -> 2.0.0"),
        "{}",
        r.render()
    );
    assert!(out.contains("pkg[script] pin 2.0.0 tool"), "{out}");
    assert!(!out.contains("install-must-not-run"), "{out}");
    assert_eq!(tool_out(d.path()), "tool 2.0.0\n");
    // Pinned and met: nothing to do.
    let (r2, _) = go(&pb, vec![spec("h1", d.path())]);
    assert_eq!(r2.hosts[0].status, HostStatus::Ok, "{}", r2.render());
    assert!(r2.hosts[0].will_change.is_empty());
}

#[test]
fn script_installer_that_creates_nothing_fails() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.script("tool", tool_spec(&h).install("echo pretending"));
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    let pb = Playbook {
        name: "noop",
        run: run_,
    };
    let (r, _) = go(&pb, vec![spec("h1", d.path())]);
    let HostStatus::Failed { tail, rc, .. } = &r.hosts[0].status else {
        panic!("{:?}", r.hosts[0].status)
    };
    assert_eq!(*rc, 1);
    assert!(tail.contains("installed absent, wanted present"), "{tail}");
}

#[test]
fn script_latest_command_drives_an_update() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            let dir = h.vars.to_str().unwrap().to_string();
            h.script(
                "tool",
                tool_spec(&h)
                    .install("exit 9")
                    .update(installer(&dir, "never"))
                    .latest(telepatch::ops::pkg::Latest::command("echo 1.2.3")),
            )
            .latest();
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    write_exe(&d.path().join("tool"), "#!/bin/sh\necho tool 1.0.0\n");
    let pb = Playbook {
        name: "latest",
        run: run_,
    };
    let (r, out) = go(&pb, vec![spec("h1", d.path())]);
    assert_eq!(
        r.hosts[0].status,
        HostStatus::Changed,
        "{}\n{out}",
        r.render()
    );
    assert!(
        r.render()
            .contains("h1: window 1 pkg[script] root tool: 1.0.0 -> 1.2.3 (latest)"),
        "{}",
        r.render()
    );
    assert!(out.contains("pkg[script] latest tool"), "{out}");
    assert_eq!(tool_out(d.path()), "tool 1.2.3\n");
    let (r2, _) = go(&pb, vec![spec("h1", d.path())]);
    assert_eq!(r2.hosts[0].status, HostStatus::Ok, "{}", r2.render());
}

#[test]
fn script_unresolvable_latest_fails_the_op() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.script(
                "tool",
                tool_spec(&h).latest(telepatch::ops::pkg::Latest::command("exit 1")),
            )
            .latest();
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    let pb = Playbook {
        name: "nolatest",
        run: run_,
    };
    let (r, _) = go(&pb, vec![spec("h1", d.path())]);
    assert!(
        r.render()
            .contains("h1: window 1 pkg[script] root tool: absent -> unknown (latest)"),
        "{}",
        r.render()
    );
    let HostStatus::Failed { tail, .. } = &r.hosts[0].status else {
        panic!("{:?}", r.hosts[0].status)
    };
    assert!(
        tail.contains("could not resolve the latest version"),
        "{tail}"
    );
    assert!(!d.path().join("tool").exists());
}
