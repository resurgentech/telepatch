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

// ---- pkg[cargo] (T33): a stub `cargo` first on PATH, keeping its install
// list in a temp file ----

/// `name version` per line: what the stub's `cargo install --list` shows.
const STUB_CARGO: &str = r##"#!/bin/sh
d='@D@'
case "$1" in
install)
  if [ "$2" = "--list" ]; then
    awk '{print $1 " v" $2 ":"; print "    " $1}' "$d/state"; exit 0
  fi
  shift
  echo "install $*" >> "$d/log"
  [ -e "$d/refuse" ] && exit 0
  names=; ver=
  while [ $# -gt 0 ]; do
    case "$1" in --version) ver=$2; shift 2;; *) names="$names $1"; shift;; esac
  done
  for n in $names; do
    v=$ver
    [ -n "$v" ] || v=$(awk -v n="$n" '$1==n {print $2}' "$d/latest")
    [ -n "$v" ] || v=0.1.0
    grep -v "^$n " "$d/state" > "$d/state.new"
    echo "$n $v" >> "$d/state.new"
    mv "$d/state.new" "$d/state"
  done ;;
search)
  echo "$2-extra = \"9.9.9\"    # a decoy"
  awk -v n="$2" '$1==n {print n " = \"" $2 "\"    # a crate"}' "$d/latest" ;;
esac
"##;

/// A fresh stub in `<dir>/bin`, with the installed list and the search
/// table (`name version` lines).
fn cargo_stub(d: &std::path::Path, installed: &str, latest: &str) {
    std::fs::create_dir_all(d.join("bin")).unwrap();
    std::fs::write(d.join("state"), installed).unwrap();
    std::fs::write(d.join("latest"), latest).unwrap();
    write_exe(
        &d.join("bin/cargo"),
        &STUB_CARGO.replace("@D@", d.to_str().unwrap()),
    );
}

fn cargo_log(d: &std::path::Path) -> String {
    std::fs::read_to_string(d.join("log")).unwrap_or_default()
}

fn cargo_host(name: &str, dir: &std::path::Path, path: &str) -> HostSpec<PathBuf> {
    HostSpec {
        name: name.into(),
        target: Target::Local(LocalTransport::new().uid(0).env("PATH", path)),
        vars: dir.to_path_buf(),
    }
}

/// The stub's `bin` first, then the real PATH.
fn stub_path(d: &std::path::Path) -> String {
    format!(
        "{}/bin:{}",
        d.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

#[test]
fn cargo_present_installs_missing_in_one_command_then_changed_0() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.cargo(["cargo-deb", "cargo-generate-rpm", "have"]);
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    cargo_stub(d.path(), "have 1.0.0\n", "");
    let path = stub_path(d.path());
    let pb = Playbook {
        name: "cargo",
        run: run_,
    };
    let (r1, out1) = go(&pb, vec![cargo_host("h1", d.path(), &path)]);
    assert_eq!(
        r1.hosts[0].status,
        HostStatus::Changed,
        "{}\n{out1}",
        r1.render()
    );
    assert_eq!(
        cargo_log(d.path()),
        "install cargo-deb cargo-generate-rpm\n"
    );
    let line = "window 1 pkg[cargo] root cargo-deb, cargo-generate-rpm: absent -> present";
    assert!(
        r1.render().contains(&format!("h1: {line}")),
        "{}",
        r1.render()
    );
    assert!(
        out1.contains(&format!("== h1 [prepare] {line} ==")),
        "{out1}"
    );
    assert!(
        out1.contains("pkg[cargo] install cargo-deb cargo-generate-rpm have"),
        "{out1}"
    );
    // The report keeps one item per name.
    assert_eq!(r1.hosts[0].will_change.len(), 2);
    assert_eq!(r1.hosts[0].will_change[0].source, "cargo");

    let (r2, out2) = go(&pb, vec![cargo_host("h1", d.path(), &path)]);
    assert_eq!(r2.hosts[0].status, HostStatus::Ok, "{}", r2.render());
    assert!(r2.hosts[0].ops.iter().all(|o| !o.changed));
    assert!(r2.hosts[0].will_change.is_empty());
    assert!(!out2.contains("[prepare]"), "{out2}");
    assert_eq!(cargo_log(d.path()).lines().count(), 1, "no second install");
}

#[test]
fn cargo_will_change_json_event_is_one_per_name() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.cargo(["a", "b"]);
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    cargo_stub(d.path(), "", "");
    let log = d.path().join("run.jsonl");
    let pb = Playbook {
        name: "cargo",
        run: run_,
    };
    let r = run(
        &pb,
        vec![cargo_host("h1", d.path(), &stub_path(d.path()))],
        RunOpts {
            log: Some(log.clone()),
            stdout: Stdout::Quiet,
        },
    );
    assert_eq!(r.exit_code(), 0, "{}", r.render());
    let text = std::fs::read_to_string(&log).unwrap();
    let evs: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .filter(|e| e["ev"] == "will_change")
        .collect();
    let names: Vec<&str> = evs.iter().map(|e| e["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["a", "b"]);
    assert_eq!(evs[0]["source"], "cargo");
    assert_eq!(evs[0]["installed"], serde_json::Value::Null);
    assert_eq!(evs[0]["wanted"], "present");
}

#[test]
fn cargo_pinned_installs_with_version_when_different() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.cargo(["cargo-deb"]).pin("0.2.0");
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    cargo_stub(d.path(), "cargo-deb 0.1.0\n", "");
    let path = stub_path(d.path());
    let pb = Playbook {
        name: "cargo",
        run: run_,
    };
    let (r1, out1) = go(&pb, vec![cargo_host("h1", d.path(), &path)]);
    assert_eq!(
        r1.hosts[0].status,
        HostStatus::Changed,
        "{}\n{out1}",
        r1.render()
    );
    assert_eq!(cargo_log(d.path()), "install cargo-deb --version 0.2.0\n");
    assert!(
        r1.render()
            .contains("h1: window 1 pkg[cargo] root cargo-deb: 0.1.0 -> 0.2.0"),
        "{}",
        r1.render()
    );
    assert!(out1.contains("pkg[cargo] pin 0.2.0 cargo-deb"), "{out1}");
    let (r2, _) = go(&pb, vec![cargo_host("h1", d.path(), &path)]);
    assert_eq!(r2.hosts[0].status, HostStatus::Ok, "{}", r2.render());
    assert!(r2.hosts[0].will_change.is_empty());
    assert_eq!(cargo_log(d.path()).lines().count(), 1);
}

#[test]
fn cargo_latest_asks_cargo_search_on_the_host() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.cargo(["cargo-deb", "cargo-generate-rpm"]).latest();
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    cargo_stub(
        d.path(),
        "cargo-deb 0.1.0\ncargo-generate-rpm 0.5.0\n",
        "cargo-deb 0.3.0\ncargo-generate-rpm 0.5.0\n",
    );
    let path = stub_path(d.path());
    let pb = Playbook {
        name: "cargo",
        run: run_,
    };
    let (r1, out1) = go(&pb, vec![cargo_host("h1", d.path(), &path)]);
    assert_eq!(
        r1.hosts[0].status,
        HostStatus::Changed,
        "{}\n{out1}",
        r1.render()
    );
    // Only the outdated name is installed.
    assert_eq!(cargo_log(d.path()), "install cargo-deb\n");
    assert!(
        r1.render()
            .contains("h1: window 1 pkg[cargo] root cargo-deb: 0.1.0 -> 0.3.0 (latest)"),
        "{}",
        r1.render()
    );
    assert!(!r1.render().contains("generate-rpm"), "{}", r1.render());
    let (r2, _) = go(&pb, vec![cargo_host("h1", d.path(), &path)]);
    assert_eq!(r2.hosts[0].status, HostStatus::Ok, "{}", r2.render());
    assert_eq!(cargo_log(d.path()).lines().count(), 1);
}

#[test]
fn cargo_latest_lookup_failure_fails_the_op() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.cargo(["nowhere"]).latest();
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    cargo_stub(d.path(), "", "");
    let pb = Playbook {
        name: "cargo",
        run: run_,
    };
    let (r, out) = go(&pb, vec![cargo_host("h1", d.path(), &stub_path(d.path()))]);
    assert!(
        matches!(r.hosts[0].status, HostStatus::Failed { .. }),
        "{}",
        r.render()
    );
    assert!(
        r.render()
            .contains("cargo nowhere: could not resolve the latest version"),
        "{}\n{out}",
        r.render()
    );
    assert!(r.render().contains("unknown (latest)"), "{}", r.render());
    assert_eq!(cargo_log(d.path()), "", "nothing installed");
}

#[test]
fn cargo_install_that_installs_nothing_fails() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.cargo(["a"]);
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    cargo_stub(d.path(), "", "");
    std::fs::write(d.path().join("refuse"), "").unwrap();
    let pb = Playbook {
        name: "cargo",
        run: run_,
    };
    let (r, _) = go(&pb, vec![cargo_host("h1", d.path(), &stub_path(d.path()))]);
    assert!(
        matches!(r.hosts[0].status, HostStatus::Failed { .. }),
        "{}",
        r.render()
    );
    assert!(
        r.render()
            .contains("cargo a: installed absent, wanted present"),
        "{}",
        r.render()
    );
}

#[test]
fn cargo_missing_fails_and_prepare_says_unknown() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.cargo(["a"]);
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    // A PATH with the shell's tools but no cargo.
    let bin = d.path().join("nocargo");
    std::fs::create_dir_all(&bin).unwrap();
    let real = std::env::var("PATH").unwrap_or_default();
    for t in [
        "bash", "sh", "cat", "sed", "awk", "grep", "mv", "rm", "dirname", "head", "env", "id",
        "mktemp", "date", "sleep", "tr", "cut", "stty",
    ] {
        if let Some(p) = std::env::split_paths(&real)
            .map(|dir| dir.join(t))
            .find(|p| p.exists())
        {
            std::os::unix::fs::symlink(p, bin.join(t)).unwrap();
        }
    }
    let pb = Playbook {
        name: "cargo",
        run: run_,
    };
    let (r, out) = go(&pb, vec![cargo_host("h1", d.path(), bin.to_str().unwrap())]);
    assert!(
        matches!(r.hosts[0].status, HostStatus::Failed { .. }),
        "{}\n{out}",
        r.render()
    );
    assert!(r.render().contains("cargo not found"), "{}", r.render());
    assert!(
        r.render()
            .contains("window 1 pkg[cargo] root a: unknown -> present"),
        "{}",
        r.render()
    );
}

#[test]
fn cargo_adjacent_ops_merge_into_one_install() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.cargo(["a"]);
            h.cargo(["b"]);
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    cargo_stub(d.path(), "", "");
    let pb = Playbook {
        name: "cargo",
        run: run_,
    };
    let (r, out) = go(&pb, vec![cargo_host("h1", d.path(), &stub_path(d.path()))]);
    assert_eq!(
        r.hosts[0].status,
        HostStatus::Changed,
        "{}\n{out}",
        r.render()
    );
    assert_eq!(cargo_log(d.path()), "install a b\n");
    assert_eq!(r.hosts[0].ops.len(), 1, "{:?}", r.hosts[0].ops);
    assert!(
        r.render()
            .contains("pkg[cargo] root a, b: absent -> present"),
        "{}",
        r.render()
    );
}

#[test]
fn cargo_ops_do_not_merge_across_a_star_writer() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.cargo(["a"]);
            sh!(h, "true");
            h.cargo(["b"]);
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    cargo_stub(d.path(), "", "");
    let pb = Playbook {
        name: "cargo",
        run: run_,
    };
    let (r, out) = go(&pb, vec![cargo_host("h1", d.path(), &stub_path(d.path()))]);
    assert_eq!(
        r.hosts[0].status,
        HostStatus::Changed,
        "{}\n{out}",
        r.render()
    );
    assert_eq!(cargo_log(d.path()), "install a\ninstall b\n");
}

#[test]
fn pinned_cargo_ops_never_merge() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.cargo(["a"]).pin("1.0.0");
            h.cargo(["b"]).pin("1.0.0");
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    cargo_stub(d.path(), "", "");
    let pb = Playbook {
        name: "cargo",
        run: run_,
    };
    let (r, out) = go(&pb, vec![cargo_host("h1", d.path(), &stub_path(d.path()))]);
    assert_eq!(
        r.hosts[0].status,
        HostStatus::Changed,
        "{}\n{out}",
        r.render()
    );
    assert_eq!(
        cargo_log(d.path()),
        "install a --version 1.0.0\ninstall b --version 1.0.0\n"
    );
}

// ---- pkg[rustup] (T34): a stub `rustup` keeping its state in temp files,
// and a stub installer that drops the stub into `$HOME/.cargo/bin` ----

/// State lives in `$d`: `toolchains` (`<tc> <version>`), `default` (a
/// toolchain name), `targets` and `components` (`<tc> <name>`), `latest`
/// (`<tc> <version>` an install or update lands on), `check` and `checkrc`
/// (what `rustup check` prints and exits with). `log` has every call.
const STUB_RUSTUP: &str = r##"#!/bin/sh
d='@D@'
echo "$*" >> "$d/log"
dflt() { cat "$d/default" 2>/dev/null; }
lat() { v=$(awk -v t="$1" '$1==t {print $2}' "$d/latest"); echo "${v:-1.0.0}"; }
setver() {
  grep -v "^$1 " "$d/toolchains" > "$d/t.new"
  echo "$1 $2" >> "$d/t.new"
  mv "$d/t.new" "$d/toolchains"
}
case "$1" in
run) v=$(awk -v t="$2" '$1==t {print $2}' "$d/toolchains"); [ -n "$v" ] || exit 1
  echo "rustc $v (abc 2026-01-01)" ;;
toolchain)
  case "$2" in
  list)
    if [ -s "$d/toolchains" ]; then
      dd=$(dflt)
      awk -v dd="$dd" '{print $1 "-x86_64-unknown-linux-gnu" ($1==dd ? " (default)" : "")}' "$d/toolchains"
    else echo "no installed toolchains"; fi ;;
  install) setver "$3" "$(lat "$3")" ;;
  esac ;;
default)
  if [ $# -eq 1 ]; then
    dd=$(dflt)
    if [ -n "$dd" ]; then echo "$dd-x86_64-unknown-linux-gnu (default)"
    else echo "no default toolchain configured" >&2; exit 1; fi
  else echo "$2" > "$d/default"; fi ;;
target)
  case "$2" in
  list) awk -v t="$5" '$1==t {print $2}' "$d/targets" ;;
  add) tc=$4; shift 4; for t in "$@"; do echo "$tc $t" >> "$d/targets"; done ;;
  esac ;;
component)
  case "$2" in
  list) awk -v t="$5" '$1==t {print $2}' "$d/components" ;;
  add) tc=$4; shift 4
    for c in "$@"; do
      case "$c" in rust-src) n=$c;; *) n=$c-x86_64-unknown-linux-gnu;; esac
      echo "$tc $n" >> "$d/components"
    done ;;
  esac ;;
check) cat "$d/check"; rc=$(cat "$d/checkrc" 2>/dev/null); exit "${rc:-0}" ;;
update) v=$(lat "$2"); setver "$2" "$v"
  echo "$2-x86_64-unknown-linux-gnu - Up to date : $v (abc 2026-01-01)" > "$d/check"
  echo "$2 updated" ;;
esac
"##;

/// Called as `installer.sh --default-toolchain <tc> --profile <p>`.
const STUB_INSTALLER: &str = r##"d='@D@'
echo "installer $*" >> "$d/inst.log"
tc=$2
v=$(awk -v t="$tc" '$1==t {print $2}' "$d/latest")
echo "$tc ${v:-1.0.0}" >> "$d/toolchains"
echo "$tc" > "$d/default"
ch=${CARGO_HOME:-$HOME/.cargo}
mkdir -p "$ch/bin"
cp "$d/bin/rustup" "$ch/bin/rustup"
"##;

#[derive(Default)]
struct RustupState<'a> {
    toolchains: &'a str,
    default: &'a str,
    targets: &'a str,
    components: &'a str,
    latest: &'a str,
    check: &'a str,
    checkrc: &'a str,
}

fn rustup_stub(d: &std::path::Path, st: RustupState) {
    std::fs::create_dir_all(d.join("bin")).unwrap();
    std::fs::create_dir_all(d.join("home")).unwrap();
    for (f, v) in [
        ("toolchains", st.toolchains),
        ("targets", st.targets),
        ("components", st.components),
        ("latest", st.latest),
        ("check", st.check),
        ("checkrc", st.checkrc),
        ("default", st.default),
    ] {
        std::fs::write(d.join(f), v).unwrap();
    }
    let dir = d.to_str().unwrap();
    write_exe(&d.join("bin/rustup"), &STUB_RUSTUP.replace("@D@", dir));
    std::fs::write(d.join("installer.sh"), STUB_INSTALLER.replace("@D@", dir)).unwrap();
}

/// What the stub saw, minus the read-only queries.
fn rustup_mutations(d: &std::path::Path) -> String {
    std::fs::read_to_string(d.join("log"))
        .unwrap_or_default()
        .lines()
        .filter(|l| {
            !(l.starts_with("run ")
                || l.starts_with("toolchain list")
                || *l == "default"
                || l.starts_with("target list")
                || l.starts_with("component list")
                || *l == "check")
        })
        .map(|l| format!("{l}\n"))
        .collect()
}

fn rustup_calls(d: &std::path::Path, prefix: &str) -> usize {
    std::fs::read_to_string(d.join("log"))
        .unwrap_or_default()
        .lines()
        .filter(|l| l.starts_with(prefix))
        .count()
}

fn inst_log(d: &std::path::Path) -> String {
    std::fs::read_to_string(d.join("inst.log")).unwrap_or_default()
}

/// A host whose HOME is `<d>/home` (so the installer's `~/.cargo/bin` is
/// private to the test) and whose CARGO_HOME is unset (empty).
fn rustup_host(name: &str, dir: &std::path::Path, path: &str) -> HostSpec<PathBuf> {
    rustup_host_with(name, dir, path, "")
}

fn rustup_host_with(
    name: &str,
    dir: &std::path::Path,
    path: &str,
    cargo_home: &str,
) -> HostSpec<PathBuf> {
    HostSpec {
        name: name.into(),
        target: Target::Local(
            LocalTransport::new()
                .uid(0)
                .env("PATH", path)
                .env("HOME", dir.join("home").to_str().unwrap())
                .env("CARGO_HOME", cargo_home),
        ),
        vars: dir.to_path_buf(),
    }
}

/// The shell's tools only: no `rustup` (the machine may have a real one).
fn tools_path(d: &std::path::Path) -> String {
    let bin = d.join("tools");
    std::fs::create_dir_all(&bin).unwrap();
    let real = std::env::var("PATH").unwrap_or_default();
    for t in [
        "bash", "sh", "cat", "sed", "awk", "grep", "mv", "rm", "dirname", "head", "env", "id",
        "mktemp", "date", "sleep", "tr", "cut", "stty", "mkdir", "cp", "chmod", "tee", "sort",
    ] {
        if let Some(p) = std::env::split_paths(&real)
            .map(|dir| dir.join(t))
            .find(|p| p.exists())
        {
            std::os::unix::fs::symlink(p, bin.join(t)).unwrap();
        }
    }
    bin.to_str().unwrap().to_string()
}

const CHECK_UPDATE: &str = "stable-x86_64-unknown-linux-gnu - update available: 1.99.1 (abc 2026-01-01) -> 1.100.0 (def 2026-02-01)\nrustup - Up to date : 1.28.0\n";
const CHECK_UPTODATE: &str = "stable-x86_64-unknown-linux-gnu - Up to date : 1.99.1 (abc 2026-01-01)\nrustup - Up to date : 1.28.0\n";

#[test]
fn rustup_fresh_install_with_target_then_changed_0() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.rustup("stable")
                .targets(["x86_64-unknown-linux-musl"])
                .installer(format!("sh {}", p(&h, "installer.sh")));
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    rustup_stub(
        d.path(),
        RustupState {
            latest: "stable 1.99.1\n",
            ..Default::default()
        },
    );
    let path = tools_path(d.path());
    let pb = Playbook {
        name: "rustup",
        run: run_,
    };
    let (r1, out1) = go(&pb, vec![rustup_host("h1", d.path(), &path)]);
    assert_eq!(
        r1.hosts[0].status,
        HostStatus::Changed,
        "{}\n{out1}",
        r1.render()
    );
    assert_eq!(
        inst_log(d.path()),
        "installer --default-toolchain stable --profile default\n"
    );
    // The installer's rustup is only in $HOME/.cargo/bin: the op found it.
    assert_eq!(
        rustup_mutations(d.path()),
        "target add --toolchain stable x86_64-unknown-linux-musl\n"
    );
    let line =
        "window 1 pkg[rustup] root stable: absent -> present (+target x86_64-unknown-linux-musl)";
    assert!(
        r1.render().contains(&format!("h1: {line}")),
        "{}",
        r1.render()
    );
    assert!(out1.contains("pkg[rustup] install stable"), "{out1}");
    assert_eq!(r1.hosts[0].will_change.len(), 1);
    assert_eq!(r1.hosts[0].will_change[0].source, "rustup");

    let (r2, out2) = go(&pb, vec![rustup_host("h1", d.path(), &path)]);
    assert_eq!(
        r2.hosts[0].status,
        HostStatus::Ok,
        "{}\n{out2}",
        r2.render()
    );
    assert!(r2.hosts[0].ops.iter().all(|o| !o.changed));
    assert!(r2.hosts[0].will_change.is_empty());
    assert_eq!(inst_log(d.path()).lines().count(), 1, "no second install");
    assert_eq!(rustup_calls(d.path(), "target add"), 1);
}

#[test]
fn rustup_toolchain_missing_with_rustup_present_installs_it() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.rustup("stable")
                .profile("minimal")
                .components(["rust-src", "rustfmt"])
                .installer("exit 99");
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    // The YAML's repair state: the proxy exists, no toolchain behind it.
    rustup_stub(
        d.path(),
        RustupState {
            latest: "stable 1.99.1\n",
            ..Default::default()
        },
    );
    let path = stub_path(d.path());
    let pb = Playbook {
        name: "rustup",
        run: run_,
    };
    let (r1, out1) = go(&pb, vec![rustup_host("h1", d.path(), &path)]);
    assert_eq!(
        r1.hosts[0].status,
        HostStatus::Changed,
        "{}\n{out1}",
        r1.render()
    );
    assert_eq!(
        rustup_mutations(d.path()),
        "toolchain install stable --profile minimal\ncomponent add --toolchain stable rust-src rustfmt\n"
    );
    assert!(
        r1.render().contains(
            "h1: window 1 pkg[rustup] root stable: absent -> present (+component rust-src, rustfmt)"
        ),
        "{}",
        r1.render()
    );
    assert_eq!(inst_log(d.path()), "", "the installer never ran");
    let (r2, _) = go(&pb, vec![rustup_host("h1", d.path(), &path)]);
    assert_eq!(r2.hosts[0].status, HostStatus::Ok, "{}", r2.render());
    assert_eq!(rustup_calls(d.path(), "toolchain install"), 1);
    assert_eq!(rustup_calls(d.path(), "component add"), 1);
}

#[test]
fn rustup_latest_updates_when_check_says_so() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.rustup("stable").latest();
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    rustup_stub(
        d.path(),
        RustupState {
            toolchains: "stable 1.99.1\n",
            default: "stable\n",
            latest: "stable 1.100.0\n",
            check: CHECK_UPDATE,
            // rustup check exits 100 when an update is available.
            checkrc: "100",
            ..Default::default()
        },
    );
    let path = stub_path(d.path());
    let pb = Playbook {
        name: "rustup",
        run: run_,
    };
    let (r1, out1) = go(&pb, vec![rustup_host("h1", d.path(), &path)]);
    assert_eq!(
        r1.hosts[0].status,
        HostStatus::Changed,
        "{}\n{out1}",
        r1.render()
    );
    assert_eq!(rustup_mutations(d.path()), "update stable\n");
    assert!(
        r1.render()
            .contains("h1: window 1 pkg[rustup] root stable: 1.99.1 -> 1.100.0 (latest)"),
        "{}",
        r1.render()
    );
    assert!(out1.contains("pkg[rustup] latest stable"), "{out1}");
    let (r2, _) = go(&pb, vec![rustup_host("h1", d.path(), &path)]);
    assert_eq!(r2.hosts[0].status, HostStatus::Ok, "{}", r2.render());
    assert!(r2.hosts[0].will_change.is_empty());
    assert_eq!(rustup_calls(d.path(), "update"), 1);
}

#[test]
fn rustup_latest_without_an_update_changes_nothing() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.rustup("stable").latest();
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    rustup_stub(
        d.path(),
        RustupState {
            toolchains: "stable 1.99.1\n",
            default: "stable\n",
            check: CHECK_UPTODATE,
            ..Default::default()
        },
    );
    let pb = Playbook {
        name: "rustup",
        run: run_,
    };
    let (r, out) = go(&pb, vec![rustup_host("h1", d.path(), &stub_path(d.path()))]);
    assert_eq!(r.hosts[0].status, HostStatus::Ok, "{}\n{out}", r.render());
    assert!(r.hosts[0].will_change.is_empty());
    assert_eq!(rustup_mutations(d.path()), "");
    assert_eq!(rustup_calls(d.path(), "check"), 2, "prepare and the op");
}

#[test]
fn rustup_check_failure_fails_the_op() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.rustup("stable").latest();
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    rustup_stub(
        d.path(),
        RustupState {
            toolchains: "stable 1.99.1\n",
            default: "stable\n",
            checkrc: "1",
            ..Default::default()
        },
    );
    let pb = Playbook {
        name: "rustup",
        run: run_,
    };
    let (r, _) = go(&pb, vec![rustup_host("h1", d.path(), &stub_path(d.path()))]);
    let HostStatus::Failed { tail, .. } = &r.hosts[0].status else {
        panic!("{:?}\n{}", r.hosts[0].status, r.render())
    };
    assert!(tail.contains("rustup check failed"), "{tail}");
    assert!(
        r.render()
            .contains("window 1 pkg[rustup] root stable: 1.99.1 -> unknown (latest)"),
        "{}",
        r.render()
    );
    assert_eq!(rustup_mutations(d.path()), "");
}

#[test]
fn rustup_default_is_set_only_when_needed() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.rustup("stable").set_default();
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    // Installed, but another toolchain is the default.
    rustup_stub(
        d.path(),
        RustupState {
            toolchains: "stable 1.99.1\nnightly 1.101.0\n",
            default: "nightly\n",
            ..Default::default()
        },
    );
    let path = stub_path(d.path());
    let pb = Playbook {
        name: "rustup",
        run: run_,
    };
    let (r1, out1) = go(&pb, vec![rustup_host("h1", d.path(), &path)]);
    assert_eq!(
        r1.hosts[0].status,
        HostStatus::Changed,
        "{}\n{out1}",
        r1.render()
    );
    assert_eq!(rustup_mutations(d.path()), "default stable\n");
    assert!(
        r1.render()
            .contains("h1: window 1 pkg[rustup] root stable: 1.99.1 -> present (+default)"),
        "{}",
        r1.render()
    );
    let (r2, _) = go(&pb, vec![rustup_host("h1", d.path(), &path)]);
    assert_eq!(r2.hosts[0].status, HostStatus::Ok, "{}", r2.render());
    assert_eq!(rustup_calls(d.path(), "default stable"), 1);

    // The default is unset (rustup default fails): set it too.
    let d2 = tempfile::tempdir().unwrap();
    rustup_stub(
        d2.path(),
        RustupState {
            toolchains: "stable 1.99.1\n",
            ..Default::default()
        },
    );
    let (r3, _) = go(
        &pb,
        vec![rustup_host("h1", d2.path(), &stub_path(d2.path()))],
    );
    assert_eq!(r3.hosts[0].status, HostStatus::Changed, "{}", r3.render());
    assert_eq!(rustup_mutations(d2.path()), "default stable\n");
}

/// The installer honours CARGO_HOME, and so does the op's PATH fix.
#[test]
fn rustup_finds_the_installer_s_rustup_under_cargo_home() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.rustup("stable")
                .installer(format!("sh {}", p(&h, "installer.sh")));
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    rustup_stub(
        d.path(),
        RustupState {
            latest: "stable 1.99.1\n",
            ..Default::default()
        },
    );
    let path = tools_path(d.path());
    let ch = d.path().join("elsewhere");
    let pb = Playbook {
        name: "rustup",
        run: run_,
    };
    let host = || rustup_host_with("h1", d.path(), &path, ch.to_str().unwrap());
    let (r1, out1) = go(&pb, vec![host()]);
    assert_eq!(
        r1.hosts[0].status,
        HostStatus::Changed,
        "{}\n{out1}",
        r1.render()
    );
    assert!(ch.join("bin/rustup").exists());
    assert!(!d.path().join("home/.cargo").exists());
    let (r2, _) = go(&pb, vec![host()]);
    assert_eq!(r2.hosts[0].status, HostStatus::Ok, "{}", r2.render());
    assert_eq!(inst_log(d.path()).lines().count(), 1);
}
