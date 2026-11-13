//! Guards and change triggers through the real executor: `.when(g)` with
//! `.changed()` / `.ok()` composed with `| & !`, evaluated in the shell for
//! a source in the same run and as a constant otherwise; `.at_end()`.
//!
//! The local transport runs every op in one root shell (uid injected as
//! 0). The cross-principal tests use the stub `ssh` and `sudo` of
//! `tests/executor_ssh.rs` (no network, no real ssh or sudo): a user's ops
//! go to a second shell.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::report::{HostStatus, Outcome, Report};
use telepatch::transport::local::LocalTransport;
use telepatch::transport::ssh::SshTransport;
use telepatch::{Guard, Host, NodeId, Playbook, PlaybookFuture, sh};

// ---- harness: local ------------------------------------------------------

fn local_spec(dir: &Path, path: Option<&str>) -> HostSpec<PathBuf> {
    let mut t = LocalTransport::new()
        .uid(0)
        .sandboxed_at(dir)
        .env("TP_FETCH_STAGE", &dir.join("stage").display().to_string());
    if let Some(p) = path {
        t = t.env("PATH", p);
    }
    HostSpec {
        groups: Vec::new(),
        name: "h1".into(),
        target: Target::Local(t),
        vars: dir.to_path_buf(),
    }
}

fn go<V: 'static>(pb: &Playbook<V>, hosts: Vec<HostSpec<V>>) -> (Report, String) {
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

fn local(pb: &Playbook<PathBuf>, d: &Path) -> (Report, String) {
    go(pb, vec![local_spec(d, None)])
}

fn p(h: &Host<PathBuf>, name: &str) -> String {
    h.vars.join(name).display().to_string()
}

fn read(d: &Path, name: &str) -> String {
    std::fs::read_to_string(d.join(name)).unwrap_or_default()
}

/// The outcome of the op whose description contains `mark`.
fn outcome(r: &Report, mark: &str) -> Outcome {
    let ops = &r.hosts[0].ops;
    ops.iter()
        .find(|o| o.desc.contains(mark))
        .unwrap_or_else(|| panic!("no op {mark} in {ops:#?}"))
        .outcome
}

// ---- the guard type -------------------------------------------------------

#[test]
fn guards_compose_with_or_and_not() {
    let (a, b, c) = (NodeId(0), NodeId(1), NodeId(2));
    let g = (Guard::Changed(a) | Guard::Changed(b)) & !Guard::ExitOk(c);
    assert_eq!(
        g,
        Guard::And(
            Box::new(Guard::Or(
                Box::new(Guard::Changed(a)),
                Box::new(Guard::Changed(b))
            )),
            Box::new(Guard::Not(Box::new(Guard::ExitOk(c))))
        )
    );
    assert_eq!(g.to_string(), "(changed(#0) | changed(#1)) & !exit_ok(#2)");
    assert_eq!(
        (Guard::Changed(a) | (Guard::Changed(b) & Guard::Changed(c))).to_string(),
        "changed(#0) | changed(#1) & changed(#2)"
    );
    assert_eq!(
        (!(Guard::Changed(a) | Guard::Changed(b))).to_string(),
        "!(changed(#0) | changed(#1))"
    );
    assert_eq!(g.sources(), vec![a, b, c]);
}

#[test]
fn every_handle_has_changed_and_ok() {
    fn run_(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            let f = h.file("/etc/x");
            f.content("x");
            let s = sh!(h, "true");
            assert_eq!(f.changed(), Guard::Changed(f.id()));
            assert_eq!(s.ok(), Guard::ExitOk(s.id()));
            Ok(())
        })
    }
    let host = telepatch::testing::FixtureHost::new(
        "h",
        telepatch::facts::os::Os::fixture("debian12").unwrap(),
        Rc::new(()),
    );
    let _ = telepatch::testing::plan(&Playbook::new("t", run_), &host);
}

// ---- one shell: a file op and a guarded sh! -------------------------------

fn file_then_guarded(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let conf = p(&h, "app.conf");
        let log = p(&h, "log");
        let f = h.file(&conf);
        f.content("v=1\n");
        sh!(h, "echo MARK_ON_CHANGE >> {log}").when(f.changed());
        sh!(h, "echo MARK_ON_SETTLED >> {log}").when(!f.changed());
        Ok(())
    })
}

#[test]
fn guarded_sh_runs_when_the_file_changed_then_is_skipped_when_settled() {
    let d = tempfile::tempdir().unwrap();
    let pb = Playbook::new("g", file_then_guarded);
    let (r1, out1) = local(&pb, d.path());
    assert_eq!(r1.exit_code(), 0, "{}\n{out1}", r1.render());
    assert_eq!(read(d.path(), "log"), "MARK_ON_CHANGE\n", "{out1}");
    assert_eq!(outcome(&r1, "MARK_ON_CHANGE"), Outcome::Changed);
    assert_eq!(outcome(&r1, "MARK_ON_SETTLED"), Outcome::Skipped);
    assert!(out1.contains("-> skipped (when !changed(#0))"), "{out1}");

    let (r2, out2) = local(&pb, d.path());
    assert_eq!(r2.hosts[0].status, HostStatus::Changed, "{}", r2.render());
    assert_eq!(
        read(d.path(), "log"),
        "MARK_ON_CHANGE\nMARK_ON_SETTLED\n",
        "{out2}"
    );
    assert_eq!(outcome(&r2, "MARK_ON_CHANGE"), Outcome::Skipped);
    assert!(out2.contains("-> skipped (when changed(#0))"), "{out2}");
    let rec = r2.hosts[0]
        .ops
        .iter()
        .find(|o| o.desc.contains("MARK_ON_CHANGE"))
        .unwrap();
    assert_eq!(rec.when.as_deref(), Some("changed(#0)"));
    assert!(!rec.changed);
}

/// A settled run whose only guarded op is skipped is `ok`, not `changed`;
/// the skipped op counts as OK in the table.
#[test]
fn a_skipped_op_does_not_make_the_host_changed() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            let conf = p(&h, "app.conf");
            let f = h.file(&conf);
            f.content("v=1\n");
            sh!(h, "echo MARK_RESTART").when(f.changed());
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    let pb = Playbook::new("g", run_);
    let _ = local(&pb, d.path());
    let (r2, out2) = local(&pb, d.path());
    assert_eq!(
        r2.hosts[0].status,
        HostStatus::Ok,
        "{}\n{out2}",
        r2.render()
    );
    let row = r2.table().lines().nth(1).unwrap().to_string();
    let cols: Vec<&str> = row.split_whitespace().collect();
    assert_eq!(cols[..6], ["h1", "ok", "2", "0", "0", "0"], "{row}");
}

/// A guarded op whose source is itself guarded: a skipped source is
/// unchanged and ok.
#[test]
fn a_skipped_source_is_unchanged_and_ok() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            let conf = p(&h, "app.conf");
            let log = p(&h, "log");
            let f = h.file(&conf);
            f.content("v=1\n");
            let s = sh!(h, "echo MARK_MID >> {log}");
            s.when(f.changed());
            sh!(h, "echo MARK_C >> {log}").when(s.changed());
            sh!(h, "echo MARK_OK >> {log}").when(s.ok());
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    let pb = Playbook::new("g", run_);
    let (r1, _) = local(&pb, d.path());
    assert_eq!(r1.exit_code(), 0, "{}", r1.render());
    assert_eq!(read(d.path(), "log"), "MARK_MID\nMARK_C\nMARK_OK\n");
    std::fs::remove_file(d.path().join("log")).unwrap();
    let (r2, out2) = local(&pb, d.path());
    assert_eq!(read(d.path(), "log"), "MARK_OK\n", "{out2}");
    assert_eq!(outcome(&r2, "MARK_MID"), Outcome::Skipped);
    assert_eq!(outcome(&r2, "MARK_C"), Outcome::Skipped);
}

// ---- a failed source --------------------------------------------------------

#[test]
fn a_guarded_op_whose_source_failed_is_skipped_not_failed() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            let log = p(&h, "log");
            let s = sh!(h, "echo MARK_SRC; exit 3");
            s.allow_fail();
            sh!(h, "echo MARK_ON_CHANGE >> {log}").when(s.changed());
            sh!(h, "echo MARK_ON_OK >> {log}").when(s.ok());
            sh!(h, "echo MARK_ON_FAIL >> {log}").when(!s.ok());
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    let pb = Playbook::new("g", run_);
    let (r, out) = local(&pb, d.path());
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert_eq!(outcome(&r, "MARK_SRC"), Outcome::AllowedFail);
    assert_eq!(outcome(&r, "MARK_ON_CHANGE"), Outcome::Skipped);
    assert_eq!(outcome(&r, "MARK_ON_OK"), Outcome::Skipped);
    assert_eq!(outcome(&r, "MARK_ON_FAIL"), Outcome::Changed);
    assert_eq!(read(d.path(), "log"), "MARK_ON_FAIL\n");
}

/// A source that fails the host: the guarded op does not run and is not
/// failed; only the source is.
#[test]
fn a_guarded_op_after_a_failed_source_is_not_run() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            let log = p(&h, "log");
            let s = sh!(h, "echo MARK_SRC; exit 3");
            sh!(h, "echo MARK_ON_FAIL >> {log}").when(!s.ok());
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    let pb = Playbook::new("g", run_);
    let (r, out) = local(&pb, d.path());
    assert!(
        matches!(r.hosts[0].status, HostStatus::Failed { .. }),
        "{out}"
    );
    assert_eq!(outcome(&r, "MARK_SRC"), Outcome::Failed);
    assert_eq!(outcome(&r, "MARK_ON_FAIL"), Outcome::NotRun);
    assert_eq!(read(d.path(), "log"), "");
}

// ---- across windows ---------------------------------------------------------

/// The source runs in window 1 (an implicit barrier for the fact read); the
/// guarded op in the final window gets the outcome as a constant.
fn across_windows(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let made = p(&h, "made");
        let log = p(&h, "log");
        let s = sh!(h, "echo MARK_SRC; touch {made}", creates = "{made}");
        let _ = h.bin_exists("tp-no-such-bin-zz").await;
        sh!(h, "echo MARK_ON_CHANGE >> {log}").when(s.changed());
        sh!(h, "echo MARK_SETTLED >> {log}").when(!s.changed() & s.ok());
        Ok(())
    })
}

#[test]
fn a_source_in_an_earlier_window_is_a_constant() {
    let d = tempfile::tempdir().unwrap();
    let pb = Playbook::new("g", across_windows);
    let (r1, out1) = local(&pb, d.path());
    assert_eq!(r1.exit_code(), 0, "{}\n{out1}", r1.render());
    assert_eq!(read(d.path(), "log"), "MARK_ON_CHANGE\n", "{out1}");
    let (r2, out2) = local(&pb, d.path());
    assert_eq!(r2.exit_code(), 0, "{}\n{out2}", r2.render());
    assert_eq!(read(d.path(), "log"), "MARK_ON_CHANGE\nMARK_SETTLED\n");
    assert_eq!(outcome(&r2, "MARK_ON_CHANGE"), Outcome::Skipped);
}

// ---- the pkg member rule ----------------------------------------------------

/// A stub `cargo` whose `install --list` reads `<dir>/state`.
const STUB_CARGO: &str = r##"#!/bin/sh
d='@D@'
case "$1" in
install)
  if [ "$2" = "--list" ]; then
    awk '{print $1 " v" $2 ":"; print "    " $1}' "$d/state"; exit 0
  fi
  shift
  echo "install $*" >> "$d/cargo.log"
  for n in "$@"; do
    grep -v "^$n " "$d/state" > "$d/state.new"
    echo "$n 0.1.0" >> "$d/state.new"
    mv "$d/state.new" "$d/state"
  done ;;
esac
"##;

fn write_exe(p: &Path, text: &str) {
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

fn cargo_stub(d: &Path, installed: &str) -> String {
    std::fs::create_dir_all(d.join("bin")).unwrap();
    std::fs::write(d.join("state"), installed).unwrap();
    write_exe(
        &d.join("bin/cargo"),
        &STUB_CARGO.replace("@D@", d.to_str().unwrap()),
    );
    format!(
        "{}/bin:{}",
        d.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

fn member_rule(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let log = p(&h, "log");
        let have = h.pkg(["have"]).cargo();
        let new = h.pkg(["new"]).cargo();
        sh!(h, "echo MARK_HAVE >> {log}").when(have.changed());
        sh!(h, "echo MARK_NEW >> {log}").when(new.changed());
        Ok(())
    })
}

#[test]
fn a_merged_pkg_member_changed_only_when_its_own_names_were_installed() {
    let d = tempfile::tempdir().unwrap();
    let path = cargo_stub(d.path(), "have 1.0.0\n");
    let pb = Playbook::new("g", member_rule);
    let (r, out) = go(&pb, vec![local_spec(d.path(), Some(&path))]);
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert_eq!(read(d.path(), "cargo.log"), "install new\n");
    // One merged op.
    assert_eq!(
        r.hosts[0]
            .ops
            .iter()
            .filter(|o| o.desc.contains("pkg[cargo]"))
            .count(),
        1,
        "{:#?}",
        r.hosts[0].ops
    );
    assert_eq!(read(d.path(), "log"), "MARK_NEW\n", "{out}");
    assert_eq!(outcome(&r, "MARK_HAVE"), Outcome::Skipped);
}

/// The same rule when the guards are in a later window (constants).
fn member_rule_later(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let log = p(&h, "log");
        let have = h.pkg(["have"]).cargo();
        let new = h.pkg(["new"]).cargo();
        sh!(h, "true");
        let _ = h.bin_exists("tp-no-such-bin-zz").await;
        sh!(h, "echo MARK_HAVE >> {log}").when(have.changed());
        sh!(h, "echo MARK_NEW >> {log}").when(new.changed());
        Ok(())
    })
}

#[test]
fn the_member_rule_holds_across_windows() {
    let d = tempfile::tempdir().unwrap();
    let path = cargo_stub(d.path(), "have 1.0.0\n");
    let pb = Playbook::new("g", member_rule_later);
    let (r, out) = go(&pb, vec![local_spec(d.path(), Some(&path))]);
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert_eq!(read(d.path(), "log"), "MARK_NEW\n", "{out}");
}

// ---- restart and reload (T76) ---------------------------------------------------

/// A stub `systemctl` logging every call to `<dir>/systemctl.log`; a unit
/// is active once `<dir>/active` exists.
const STUB_SYSTEMCTL: &str = r##"#!/bin/sh
d='@D@'
echo "$*" >> "$d/systemctl.log"
case "$1" in
is-enabled) echo enabled ;;
is-active) [ -e "$d/active" ] || exit 3 ;;
show) case "$*" in *NeedDaemonReload*) echo no ;; *) echo "UnitFileState=enabled"; if [ -e "$d/active" ]; then echo ActiveState=active; else echo ActiveState=inactive; fi ;; esac ;;
start|restart) touch "$d/active" ;;
esac
"##;

/// A local host whose PATH has the stub `systemctl` (and `cargo`) and
/// whose systemd counts as running. The cargo state (`have` installed at
/// first) carries over from run to run.
fn svc_spec(d: &Path) -> HostSpec<PathBuf> {
    let state = std::fs::read_to_string(d.join("state")).unwrap_or("have 1.0.0\n".into());
    let path = cargo_stub(d, &state);
    write_exe(
        &d.join("bin/systemctl"),
        &STUB_SYSTEMCTL.replace("@D@", d.to_str().unwrap()),
    );
    let run = d.join("run-systemd");
    std::fs::create_dir_all(&run).unwrap();
    HostSpec {
        groups: Vec::new(),
        name: "h1".into(),
        target: Target::Local(
            LocalTransport::new()
                .uid(0)
                .sandboxed_at(d)
                .env("PATH", &path)
                .env("TP_SVC_RUNDIR", run.to_str().unwrap())
                .env("TP_FETCH_STAGE", &d.join("stage").display().to_string()),
        ),
        vars: d.to_path_buf(),
    }
}

fn restarts(d: &Path) -> usize {
    read(d, "systemctl.log")
        .lines()
        .filter(|l| l.starts_with("restart "))
        .count()
}

/// The start runs in window 1; the restart, in the final window, reads
/// the start's `changed` as a constant.
fn start_then_restart(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        h.svc("app").enabled().started();
        let _ = h.bin_exists("tp-no-such-bin-zz").await;
        let conf = h.file(&p(&h, "app.conf"));
        conf.content("x\n");
        h.svc("app").restart().when(conf.changed());
        Ok(())
    })
}

#[test]
fn a_restart_after_a_start_in_an_earlier_window_is_skipped() {
    let d = tempfile::tempdir().unwrap();
    let pb = Playbook::new("g", start_then_restart);
    let (r1, out1) = go(&pb, vec![svc_spec(d.path())]);
    assert_eq!(r1.exit_code(), 0, "{}\n{out1}", r1.render());
    assert!(
        read(d.path(), "systemctl.log").contains("start -- app\n"),
        "{out1}"
    );
    assert_eq!(restarts(d.path()), 0, "{}", read(d.path(), "systemctl.log"));
    let op = r1.hosts[0]
        .ops
        .iter()
        .find(|o| o.desc.contains("restart"))
        .expect("restart op");
    assert_eq!(op.outcome, Outcome::Skipped, "{out1}");
    assert_eq!(op.when.as_deref(), Some("changed(#2) & !started(#0)"));
    // The start settled; a config change now restarts.
    std::fs::write(d.path().join("app.conf"), "old\n").unwrap();
    let (r2, out2) = go(&pb, vec![svc_spec(d.path())]);
    assert_eq!(r2.exit_code(), 0, "{}\n{out2}", r2.render());
    assert_eq!(restarts(d.path()), 1, "{out2}");
}

/// A restart on a merged pkg op's member: named by its own package.
fn restart_on_member(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let have = h.pkg(["have"]).cargo();
        let new = h.pkg(["new"]).cargo();
        h.svc("app").restart_on(&new);
        h.svc("other").restart_on(&have);
        Ok(())
    })
}

#[test]
fn a_restart_names_its_trigger_and_a_second_run_changes_nothing() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("active"), "").unwrap();
    let pb = Playbook::new("g", restart_on_member);
    let (r1, out1) = go(&pb, vec![svc_spec(d.path())]);
    assert_eq!(r1.exit_code(), 0, "{}\n{out1}", r1.render());
    assert!(
        out1.contains("== h1 [prepare] svc app.service: will restart if pkg[cargo] new changes =="),
        "{out1}"
    );
    assert!(
        out1.contains(
            "== h1 [prepare] svc other.service: will restart if pkg[cargo] have changes =="
        ),
        "{out1}"
    );
    let log = read(d.path(), "systemctl.log");
    assert!(log.contains("restart -- app\n"), "{log}");
    assert!(!log.contains("restart -- other"), "{log}");
    assert_eq!(outcome(&r1, "other restart"), Outcome::Skipped);
    let (r2, out2) = go(&pb, vec![svc_spec(d.path())]);
    assert_eq!(
        r2.hosts[0].status,
        HostStatus::Ok,
        "{}\n{out2}",
        r2.render()
    );
    assert!(
        r2.hosts[0].ops.iter().all(|o| !o.changed),
        "{}",
        r2.render()
    );
    assert_eq!(restarts(d.path()), 1);
}

// ---- the merge ----------------------------------------------------------------

fn merged(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let a = p(&h, "a.conf");
        let b = p(&h, "b.conf");
        let log = p(&h, "restart.log");
        let fa = h.file(&a);
        fa.content("a\n");
        h.raw(format!("echo MARK_RESTART >> {log}"))
            .writes([telepatch::Key::svc("app")])
            .when(fa.changed());
        let fb = h.file(&b);
        fb.content("b\n");
        h.raw(format!("echo MARK_RESTART >> {log}"))
            .writes([telepatch::Key::svc("app")])
            .when(fb.changed());
        Ok(())
    })
}

#[test]
fn two_equal_guarded_ops_merge_with_their_guards_or_joined() {
    let d = tempfile::tempdir().unwrap();
    let pb = Playbook::new("g", merged);
    let (r1, out1) = local(&pb, d.path());
    assert_eq!(r1.exit_code(), 0, "{}\n{out1}", r1.render());
    assert_eq!(read(d.path(), "restart.log"), "MARK_RESTART\n", "{out1}");
    let restarts: Vec<_> = r1.hosts[0]
        .ops
        .iter()
        .filter(|o| o.desc.contains("MARK_RESTART"))
        .collect();
    assert_eq!(restarts.len(), 1, "{restarts:#?}");
    assert_eq!(restarts[0].members.len(), 2);
    assert_eq!(
        restarts[0].when.as_deref(),
        Some("changed(#0) | changed(#2)")
    );

    // Only b changes: the merged op still runs, once.
    std::fs::write(d.path().join("b.conf"), "old\n").unwrap();
    let (r2, out2) = local(&pb, d.path());
    assert_eq!(r2.exit_code(), 0, "{}\n{out2}", r2.render());
    assert_eq!(
        read(d.path(), "restart.log"),
        "MARK_RESTART\nMARK_RESTART\n"
    );

    // Nothing changes: skipped.
    let (r3, out3) = local(&pb, d.path());
    assert_eq!(outcome(&r3, "MARK_RESTART"), Outcome::Skipped, "{out3}");
    assert_eq!(r3.hosts[0].status, HostStatus::Ok);
}

// ---- at_end -------------------------------------------------------------------

fn late(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let log = p(&h, "log");
        let conf = p(&h, "app.conf");
        let f = h.file(&conf);
        f.content("x\n");
        sh!(h, "echo MARK_LAST >> {log}").when(f.changed()).at_end();
        sh!(h, "echo MARK_FIRST >> {log}");
        let _ = h.bin_exists("tp-no-such-bin-zz").await;
        sh!(h, "echo MARK_SECOND >> {log}");
        Ok(())
    })
}

#[test]
fn at_end_moves_an_op_after_the_last_window() {
    let d = tempfile::tempdir().unwrap();
    let pb = Playbook::new("g", late);
    let (r, out) = local(&pb, d.path());
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert_eq!(
        read(d.path(), "log"),
        "MARK_FIRST\nMARK_SECOND\nMARK_LAST\n",
        "{out}"
    );
    let ids: Vec<usize> = r.hosts[0].ops.iter().map(|o| o.id).collect();
    assert_eq!(ids.last(), Some(&1), "{ids:?}");
    let (r2, _) = local(&pb, d.path());
    assert_eq!(outcome(&r2, "MARK_LAST"), Outcome::Skipped);
}

// ---- trace errors ---------------------------------------------------------------

fn trace_error(run_: fn(Host<()>) -> PlaybookFuture) -> String {
    let host = telepatch::testing::FixtureHost::new(
        "h",
        telepatch::facts::os::Os::fixture("debian12").unwrap(),
        Rc::new(()),
    );
    let h = Host::new(host.name.clone(), host.os.clone(), host.vars.clone());
    let mut engine = telepatch::engine::Engine::new(telepatch::testing::FakeExecutor::new());
    let res = telepatch::testing::block_on(engine.run_host(&Playbook::new("t", run_), h));
    match res {
        Err(telepatch::engine::EngineError::Playbook(e)) => e.to_string(),
        other => panic!("expected a trace error, got {other:?}"),
    }
}

#[test]
fn a_guard_on_a_later_op_is_a_trace_error() {
    fn run_(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            let s = sh!(h, "echo restart");
            let f = h.file("/etc/x");
            f.content("x");
            s.when(f.changed());
            Ok(())
        })
    }
    let e = trace_error(run_);
    assert!(
        e.contains("when(..): the guard names #1, which is traced after this op"),
        "{e}"
    );
}

#[test]
fn a_guard_on_an_at_end_op_from_an_op_that_is_not_is_a_trace_error() {
    fn run_(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            let f = h.file("/etc/x");
            f.content("x");
            f.at_end();
            sh!(h, "echo restart").when(f.changed());
            Ok(())
        })
    }
    let e = trace_error(run_);
    assert!(
        e.contains(
            "when(..): the guard names #0, which runs at the end (at_end); this op does not"
        ),
        "{e}"
    );
}

// ---- across principals (stub ssh) -----------------------------------------------

const SSH: &str = r#"#!/bin/sh
d="$TP_STUB_DIR"
master=; cp=; ctl=
while [ $# -gt 0 ]; do
  case "$1" in
    -T) shift ;;
    -N) master=1; shift ;;
    -i) shift 2 ;;
    -O) ctl="$2"; shift 2 ;;
    -o) case "$2" in ControlPath=*) [ -n "$cp" ] || cp="${2#ControlPath=}" ;; esac; shift 2 ;;
    -*) echo "stub ssh: unknown flag $1" >&2; exit 2 ;;
    *) break ;;
  esac
done
shift
if [ -n "$ctl" ]; then
  [ "$ctl" = exit ] || exit 2
  [ -e "$cp.pid" ] && kill $(cat "$cp.pid") 2>/dev/null
  rm -f "$cp" "$cp.pid"
  exit 0
fi
if [ -n "$master" ]; then
  echo $$ > "$cp.pid"
  : > "$cp"
  exec sleep 100000
fi
[ -e "$cp" ] || { echo "stub ssh: no master at $cp" >&2; exit 255; }
exec bash -c "exec $*"
"#;

const ID: &str = r#"#!/bin/sh
if [ "$1" = -u ]; then echo "${TP_STUB_UID:-1000}"; exit 0; fi
exec /usr/bin/id "$@"
"#;

const SUDO: &str = r#"#!/bin/sh
[ "$1" = -n ] || exit 2
shift
if [ "$1" = -H ]; then shift; HOME=/root; export HOME; fi
if [ "$1" = -iu ]; then
  shift 2
  exec "$@"
fi
TP_STUB_UID=0 exec "$@"
"#;

#[derive(Clone)]
struct Vars {
    dir: PathBuf,
    user: String,
}

fn me() -> String {
    std::env::var("USER").unwrap_or_else(|_| "root".into())
}

fn ssh_go(pb: &Playbook<Vars>, stub: &Path) -> (Report, String) {
    let bin = stub.join("bin");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let t = SshTransport::new()
        .sandboxed_at(stub)
        .ssh(&stub.join("ssh").display().to_string())
        .env("TP_STUB_DIR", &stub.display().to_string())
        .env("TP_FETCH_STAGE", &stub.join("stage").display().to_string())
        .env("PATH", &path);
    let work = stub.join("work");
    std::fs::create_dir_all(&work).unwrap();
    go(
        pb,
        vec![HostSpec {
            groups: Vec::new(),
            name: "bot@h".into(),
            target: Target::Ssh(t),
            vars: Vars {
                dir: work,
                user: me(),
            },
        }],
    )
}

fn ssh_stub() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir(d.path().join("bin")).unwrap();
    write_exe(&d.path().join("ssh"), SSH);
    write_exe(&d.path().join("bin/id"), ID);
    write_exe(&d.path().join("bin/sudo"), SUDO);
    d
}

fn principals(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move {
        let w = |n: &str| h.vars.dir.join(n).display().to_string();
        let (conf, ulog, rlog, uconf) = (w("root.conf"), w("user.log"), w("root.log"), w("u.conf"));
        let u = h.as_user(&h.vars.user);
        // Root source, user target: another principal's run (a constant).
        let f = h.file(&conf);
        f.content("r\n");
        sh!(u, "echo MARK_USER_ON_ROOT >> {ulog}").when(f.changed());
        // User source, user target in the same run (a login op as the
        // source: a variable of the user's persistent login shell).
        let uf = u.file(&uconf);
        uf.content("u\n");
        sh!(u, "echo MARK_USER_ON_USER >> {ulog}").when(uf.changed());
        // User source, root target.
        sh!(h, "echo MARK_ROOT_ON_USER >> {rlog}").when(uf.changed());
        Ok(())
    })
}

#[test]
fn guards_cross_principals() {
    let stub = ssh_stub();
    let pb = Playbook::new("g", principals);
    let (r1, out1) = ssh_go(&pb, stub.path());
    assert_eq!(r1.exit_code(), 0, "{}\n{out1}", r1.render());
    let work = stub.path().join("work");
    assert_eq!(
        read(&work, "user.log"),
        "MARK_USER_ON_ROOT\nMARK_USER_ON_USER\n",
        "{out1}"
    );
    assert_eq!(read(&work, "root.log"), "MARK_ROOT_ON_USER\n");

    let (r2, out2) = ssh_go(&pb, stub.path());
    assert_eq!(r2.exit_code(), 0, "{}\n{out2}", r2.render());
    assert_eq!(r2.hosts[0].status, HostStatus::Ok, "{out2}");
    assert_eq!(outcome(&r2, "MARK_USER_ON_ROOT"), Outcome::Skipped);
    assert_eq!(outcome(&r2, "MARK_USER_ON_USER"), Outcome::Skipped);
    assert_eq!(outcome(&r2, "MARK_ROOT_ON_USER"), Outcome::Skipped);
}

// ---- owned handles: the decided API's bindings ----------------------------

/// The op node `id` of `h`'s trace.
fn node(h: &Host<()>, id: NodeId) -> telepatch::OpNode {
    match &h.snapshot().nodes[id.0] {
        telepatch::Node::Op(op) => op.clone(),
        n => panic!("not an op: {n:?}"),
    }
}

#[test]
fn builders_return_an_owned_handle_so_a_chain_binds() {
    use telepatch::ops::repo::AptRepo;
    use telepatch::{OpSpec, Policy};
    let h = Host::new(
        "h",
        telepatch::facts::os::Os::fixture("debian12").unwrap(),
        Rc::new(()),
    );
    // the decided API's shapes, bound as written
    let daemon = h.file("/etc/docker/daemon.json").content("{}\n");
    let unit = h
        .unit("x.service")
        .content("[Service]\nExecStart=/bin/true\n")
        .mode(0o644);
    let restart = h.svc("docker").restart_on(&daemon);
    let tools = h.pkg(["jq"]).update_cache(true);
    let bot = h.user("bot").shell("/bin/bash").groups_add(["docker"]);
    let grp = h.group("docker").system();
    let repo = h.repo("docker").apt(AptRepo {
        uris: vec!["https://download.docker.com/linux/debian".into()],
        suites: vec!["bookworm".into()],
        components: vec!["stable".into()],
        key_url: "https://download.docker.com/linux/debian/gpg".into(),
    });
    // the shared handle methods keep the handle's type
    let late = h
        .svc("late")
        .restart()
        .when(unit.changed())
        .allow(Policy::UnguardedRestart, "why not")
        .at_end();
    let _typed: &telepatch::ops::svc::SvcHandle = &late;

    match node(&h, daemon.id()).spec {
        OpSpec::File(f) => {
            assert_eq!(
                f.content.as_ref().and_then(|c| c.as_bytes()),
                Some(&b"{}\n"[..])
            )
        }
        s => panic!("{s:?}"),
    }
    match node(&h, unit.id()).spec {
        OpSpec::Unit(u) => assert_eq!(u.mode, Some(0o644)),
        s => panic!("{s:?}"),
    }
    let r = node(&h, restart.id());
    assert_eq!(r.guard, Some(daemon.changed()));
    match node(&h, tools.id()).spec {
        OpSpec::Pkg(p) => assert!(p.update_cache),
        s => panic!("{s:?}"),
    }
    match node(&h, bot.id()).spec {
        OpSpec::User(u) => assert_eq!(u.shell.as_deref(), Some("/bin/bash")),
        s => panic!("{s:?}"),
    }
    match node(&h, grp.id()).spec {
        OpSpec::Group(g) => assert!(g.system),
        s => panic!("{s:?}"),
    }
    match node(&h, repo.id()).spec {
        OpSpec::Repo(r) => assert!(matches!(r.kind, telepatch::ops::repo::RepoKind::Apt(_))),
        s => panic!("{s:?}"),
    }
    let l = node(&h, late.id());
    assert!(l.at_end);
    assert_eq!(l.guard, Some(unit.changed()));
    assert_eq!(l.allowed.len(), 1);

    // a builder called on a bound handle as a statement edits the same node
    let s = h.svc("sshd");
    s.reload();
    s.when(daemon.changed());
    let n = node(&h, s.id());
    match &n.spec {
        OpSpec::Svc(v) => assert!(v.reload),
        x => panic!("{x:?}"),
    }
    assert_eq!(n.guard, Some(daemon.changed()));
    let f = h.file("/etc/y");
    f.content("y");
    f.mode(0o600);
    match node(&h, f.id()).spec {
        OpSpec::File(v) => {
            assert_eq!(v.mode, Some(0o600));
            assert_eq!(
                v.content.as_ref().and_then(|c| c.as_bytes()),
                Some(&b"y"[..])
            );
        }
        x => panic!("{x:?}"),
    }
    assert!(h.snapshot().errors.is_empty(), "{:?}", h.snapshot().errors);
}
