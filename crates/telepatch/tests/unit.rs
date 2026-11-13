//! The `unit` primitive: the trace (keys, posts, trace errors, no
//! `UnitViaFile`), the template run by bash against a stub `systemd-analyze`
//! (and the real one where the machine has it), and the executor over the
//! local transport with a stub `systemctl`: one `daemon-reload` before the
//! next `svc` op after a changed unit, none when nothing changed or the host
//! is not booted, `.restart_on(&unit)`, and the prepare lines.
//!
//! `TP_UNIT_DIR` stands in for `/etc/systemd/system` and `TP_SVC_RUNDIR` for
//! `/run/systemd/system`, so nothing here depends on the machine running it.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::facts::os::Os;
use telepatch::report::{HostStatus, Outcome, Report};
use telepatch::trailer::{FileResult, render_op, split_stream};
use telepatch::transport::local::LocalTransport;
use telepatch::{Host, Key, Node, NodeId, OpSpec, Playbook, PlaybookFuture, Value};

const NONCE: &str = "t0n";

fn os() -> Os {
    Os::parse("ID=debian\nVERSION_ID=12\n").unwrap()
}

fn host() -> Host<()> {
    Host::new("t", os(), Rc::new(()))
}

fn op(h: &Host<()>, id: NodeId) -> telepatch::OpNode {
    match &h.snapshot().nodes[id.0] {
        Node::Op(op) => op.clone(),
        _ => panic!("not an op"),
    }
}

fn errors(h: &Host<()>) -> Vec<String> {
    h.snapshot()
        .errors
        .iter()
        .map(|e| e.message().to_string())
        .collect()
}

// --- the trace ---

#[test]
fn unit_writes_the_file_and_the_svc_key_and_is_no_unit_via_file() {
    let h = host();
    let id = h
        .unit("minikube.service")
        .content("[Service]\nExecStart=/bin/true\n")
        .mode(0o600)
        .id();
    let o = op(&h, id);
    assert_eq!(o.spec.kind(), "unit");
    assert_eq!(
        o.writes.iter().cloned().collect::<Vec<_>>(),
        vec![
            Key::file("/etc/systemd/system/minikube.service"),
            Key::svc("minikube.service"),
        ]
    );
    assert_eq!(
        o.post
            .get(&Key::file("/etc/systemd/system/minikube.service")),
        Some(&Value::Exists(true))
    );
    assert!(errors(&h).is_empty(), "{:?}", errors(&h));
}

#[test]
fn a_name_without_a_suffix_is_a_service_and_a_drop_in_is_a_conf() {
    let h = host();
    let a = h.unit("minikube").content("x").id();
    let b = h
        .unit("systemd-networkd-wait-online.service")
        .drop_in("override", "[Service]\n")
        .id();
    let c = h.unit("foo.service").drop_in("10-limits.conf", "x").id();
    let path = |id| match op(&h, id).spec {
        OpSpec::Unit(u) => u.path(),
        _ => panic!(),
    };
    assert_eq!(path(a), "/etc/systemd/system/minikube.service");
    assert_eq!(
        path(b),
        "/etc/systemd/system/systemd-networkd-wait-online.service.d/override.conf"
    );
    assert_eq!(path(c), "/etc/systemd/system/foo.service.d/10-limits.conf");
    assert!(
        op(&h, b)
            .writes
            .contains(&Key::svc("systemd-networkd-wait-online.service"))
    );
    assert!(errors(&h).is_empty(), "{:?}", errors(&h));
}

#[test]
fn absent_posts_the_file_gone() {
    let h = host();
    let id = h.unit("old.service").absent().id();
    assert_eq!(
        op(&h, id)
            .post
            .get(&Key::file("/etc/systemd/system/old.service")),
        Some(&Value::Exists(false))
    );
    assert!(errors(&h).is_empty(), "{:?}", errors(&h));
}

#[test]
fn trace_errors() {
    let h = host();
    h.unit("a.service");
    assert_eq!(errors(&h), ["unit(a.service): nothing wanted"]);

    let h = host();
    h.unit("a.service").content("x").absent();
    assert_eq!(
        errors(&h),
        ["unit(a.service): .content() and .absent() are exclusive"]
    );

    let h = host();
    h.unit("../a.service").content("x");
    h.unit("").content("x");
    h.unit("a.service").drop_in("x/y", "z");
    h.unit("a.service").drop_in("", "z");
    let e = errors(&h);
    // a refused drop-in sets nothing, so that op also wants nothing
    assert_eq!(
        e,
        [
            "unit(\"../a.service\"): not a unit name",
            "unit(\"\"): not a unit name",
            "unit drop_in(\"x/y\"): not a drop-in name",
            "unit drop_in(\"\"): not a drop-in name",
            "unit(a.service): nothing wanted",
            "unit(a.service): nothing wanted",
        ]
    );

    let h = host();
    h.as_user("bob").unit("a.service").content("x");
    let e = errors(&h);
    assert_eq!(e.len(), 1, "{e:?}");
    assert!(e[0].contains("root only"), "{e:?}");
}

#[test]
fn content_takes_str_string_and_bytes() {
    let h = host();
    let s = String::from("b");
    h.unit("a.service").content("a");
    h.unit("b.service").content(s.clone());
    h.unit("c.service").content(&s);
    h.unit("d.service").content(b"d");
    h.unit("e.service").content(vec![b'e']);
    h.unit("f.service").content(&b"f"[..]);
    h.unit("g.service").drop_in("o", String::from("g"));
    assert!(errors(&h).is_empty());
}

#[test]
fn restart_on_takes_a_unit_handle() {
    let h = host();
    let unit = h.unit("minikube.service");
    unit.content("x");
    let r = h.svc("minikube").restart_on(&unit).id();
    assert_eq!(op(&h, r).guard, Some(telepatch::Guard::Changed(unit.id())));
    assert!(errors(&h).is_empty(), "{:?}", errors(&h));
}

// --- the template, by bash ---

fn write_exe(p: &Path, body: &str) {
    std::fs::write(p, format!("#!/bin/bash\n{body}\n")).unwrap();
    std::fs::set_permissions(p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

/// A `bin` dir with a stub `systemd-analyze`: logs each `verify` call and
/// the files beside the unit it was given (`log`), fails (rc 1, a message)
/// when any of them contains `BAD`, and lists `--recursive-errors` in its
/// help.
fn analyze_stub(d: &Path) -> PathBuf {
    let bin = d.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let dd = d.display();
    write_exe(
        &bin.join("systemd-analyze"),
        &format!(
            r#"D={dd}
if [ "$1" = --help ]; then echo "  --recursive-errors=MODE"; exit 0; fi
f="${{@: -1}}"
dir=$(dirname "$f")
echo "analyze $* | $(basename "$f") | $(cd "$dir" && find . -type f | sort | tr '\n' ' ')" >> $D/log
if grep -rq BAD "$dir"; then echo "$(basename "$f"): Unknown key 'BAD'" >&2; exit 1; fi
exit 0"#
        ),
    );
    bin
}

/// A PATH directory holding every `/usr/bin` entry except
/// `systemd-analyze` and `systemctl`.
fn bare_bin(d: &Path) -> PathBuf {
    let bin = d.join("bare");
    std::fs::create_dir_all(&bin).unwrap();
    for e in std::fs::read_dir("/usr/bin").unwrap().flatten() {
        let n = e.file_name();
        let s = n.to_string_lossy();
        if s.starts_with("systemd-analyze") || s.starts_with("systemctl") {
            continue;
        }
        let _ = std::os::unix::fs::symlink(e.path(), bin.join(&n));
    }
    bin
}

fn with_path(bin: &Path) -> String {
    if bin.ends_with("bare") || bin.ends_with("nocmp") {
        bin.display().to_string()
    } else {
        format!("{}:{}", bin.display(), std::env::var("PATH").unwrap())
    }
}

fn unit_dir(d: &Path) -> PathBuf {
    let p = d.join("etc-systemd-system");
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn run_script(script: &str, d: &Path, bin: &Path) -> (String, FileResult) {
    let out = Command::new("/bin/bash")
        .arg("-c")
        .arg(telepatch::trailer::with_helpers(script))
        .current_dir(d)
        .env("PATH", with_path(bin))
        .env("TP_UNIT_DIR", unit_dir(d))
        .output()
        .unwrap();
    let stream = String::from_utf8(out.stdout).unwrap();
    let mut v = split_stream(&stream, NONCE);
    assert_eq!(v.len(), 1, "stream: {stream:?}");
    let (o, t) = v.remove(0);
    let mut text = o;
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let r = t.typed::<FileResult>().unwrap();
    // The undo fields are undo_capture.rs's; these tests compare rc and changed.
    (
        text,
        FileResult {
            rc: r.rc,
            changed: r.changed,
            ..Default::default()
        },
    )
}

fn wrapped(h: &Host<()>, id: NodeId) -> String {
    render_op(&op(h, id), id.0, NONCE, &h.os)
}

fn log(d: &Path) -> String {
    std::fs::read_to_string(d.join("log")).unwrap_or_default()
}

fn res(rc: i32, changed: bool) -> FileResult {
    FileResult {
        rc,
        changed,
        ..Default::default()
    }
}

fn mode(p: &Path) -> u32 {
    std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(p).unwrap().permissions()) & 0o777
}

/// Nothing but the target (and drop-in dirs) is left in the unit dir.
fn no_leftovers(d: &Path) {
    let mut left = Vec::new();
    for e in walk(&unit_dir(d)) {
        let n = e.file_name().unwrap().to_string_lossy().to_string();
        if n.contains(".tp") {
            left.push(e);
        }
    }
    assert!(left.is_empty(), "candidates left: {left:?}");
}

fn walk(p: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(p).unwrap().flatten() {
        let path = e.path();
        if path.is_dir() {
            out.extend(walk(&path));
        }
        out.push(path);
    }
    out
}

#[test]
fn create_verifies_a_candidate_named_as_the_unit_then_rerun_is_quiet() {
    let d = tempfile::tempdir().unwrap();
    let bin = analyze_stub(d.path());
    let h = host();
    let body = "[Unit]\nDescription=m\n[Service]\nExecStart=/bin/true\n";
    let id = h.unit("minikube.service").content(body).id();
    let (text, r) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert_eq!(r, res(0, true), "{text}");
    let target = unit_dir(d.path()).join("minikube.service");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), body);
    assert_eq!(mode(&target), 0o644);
    let l = log(d.path());
    assert!(
        l.contains("verify --man=no --recursive-errors=no "),
        "flags: {l}"
    );
    assert!(
        l.contains("| minikube.service | ./minikube.service "),
        "candidate named as the unit, alone in its dir: {l}"
    );
    no_leftovers(d.path());

    // rerun: nothing to write, nothing verified
    std::fs::remove_file(d.path().join("log")).unwrap();
    let (text, r) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert_eq!(r, res(0, false), "{text}");
    assert_eq!(log(d.path()), "", "an unchanged unit is not verified");
}

#[test]
fn mode_is_applied_to_the_candidate() {
    let d = tempfile::tempdir().unwrap();
    let bin = analyze_stub(d.path());
    let h = host();
    let id = h.unit("m.service").content("x\n").mode(0o600).id();
    let (text, r) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert_eq!(r, res(0, true), "{text}");
    assert_eq!(mode(&unit_dir(d.path()).join("m.service")), 0o600);
    let (_, r) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert_eq!(r, res(0, false));
}

#[test]
fn a_failed_verify_fails_the_op_and_leaves_the_target_untouched() {
    let d = tempfile::tempdir().unwrap();
    let bin = analyze_stub(d.path());
    let target = unit_dir(d.path()).join("m.service");
    std::fs::write(&target, "old\n").unwrap();
    let h = host();
    let id = h.unit("m.service").content("BAD=1\n").id();
    let (text, r) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert_eq!(r, res(1, false), "{text}");
    assert!(text.contains("Unknown key 'BAD'"), "{text}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "old\n");
    no_leftovers(d.path());
}

#[test]
fn no_systemd_analyze_writes_unverified_and_says_so() {
    let d = tempfile::tempdir().unwrap();
    let bin = bare_bin(d.path());
    let h = host();
    let id = h.unit("m.service").content("BAD but unchecked\n").id();
    let (text, r) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert_eq!(r, res(0, true), "{text}");
    assert!(
        text.contains("telepatch: no systemd-analyze: m.service not verified"),
        "{text}"
    );
    assert!(unit_dir(d.path()).join("m.service").exists());
}

#[test]
fn a_drop_in_creates_its_dir_and_is_verified_with_its_unit() {
    let d = tempfile::tempdir().unwrap();
    let bin = analyze_stub(d.path());
    // the unit the drop-in is for lives in the unit dir (first searched)
    std::fs::write(
        unit_dir(d.path()).join("foo.service"),
        "[Service]\nExecStart=/bin/true\n",
    )
    .unwrap();
    let h = host();
    let id = h
        .unit("foo")
        .drop_in("override", "[Service]\nNice=5\n")
        .id();
    let (text, r) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert_eq!(r, res(0, true), "{text}");
    let conf = unit_dir(d.path()).join("foo.service.d/override.conf");
    assert_eq!(
        std::fs::read_to_string(&conf).unwrap(),
        "[Service]\nNice=5\n"
    );
    let l = log(d.path());
    assert!(
        l.contains("| foo.service | ./foo.service ./foo.service.d/override.conf "),
        "the unit is verified with the candidate as its drop-in: {l}"
    );
    no_leftovers(d.path());
    let (_, r) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert_eq!(r, res(0, false));

    // a bad drop-in fails and leaves the old one
    let h2 = host();
    let id2 = h2.unit("foo").drop_in("override", "BAD\n").id();
    let (text, r) = run_script(&wrapped(&h2, id2), d.path(), &bin);
    assert_eq!(r, res(1, false), "{text}");
    assert_eq!(
        std::fs::read_to_string(&conf).unwrap(),
        "[Service]\nNice=5\n"
    );
}

#[test]
fn a_drop_in_for_a_unit_not_found_is_written_unverified() {
    let d = tempfile::tempdir().unwrap();
    let bin = analyze_stub(d.path());
    let h = host();
    let id = h
        .unit("tp-no-such-unit-here.service")
        .drop_in("override", "[Service]\nNice=5\n")
        .id();
    let (text, r) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert_eq!(r, res(0, true), "{text}");
    assert!(
        text.contains("telepatch: unit tp-no-such-unit-here.service not found: drop-in tp-no-such-unit-here.service.d/override.conf not verified"),
        "{text}"
    );
    assert_eq!(log(d.path()), "");
}

#[test]
fn absent_removes_then_rerun_is_quiet() {
    let d = tempfile::tempdir().unwrap();
    let bin = analyze_stub(d.path());
    let target = unit_dir(d.path()).join("old.service");
    std::fs::write(&target, "x\n").unwrap();
    let h = host();
    let id = h.unit("old.service").absent().id();
    let (text, r) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert_eq!(r, res(0, true), "{text}");
    assert!(!target.exists());
    let (_, r) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert_eq!(r, res(0, false));
}

/// The machine's own `systemd-analyze`, where it has one: a good unit and a
/// drop-in pass, an unknown key fails. Skipped (with a note) without it.
#[test]
fn the_real_systemd_analyze_accepts_good_and_refuses_bad() {
    if Command::new("systemd-analyze")
        .arg("--version")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!("no systemd-analyze on this machine: skipped");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let bin = d.path().join("empty");
    std::fs::create_dir_all(&bin).unwrap();
    let h = host();
    let good = h
        .unit("tp-good.service")
        .content("[Unit]\nDescription=t\n[Service]\nExecStart=/bin/sh -c true\n")
        .id();
    let (text, r) = run_script(&wrapped(&h, good), d.path(), &bin);
    assert_eq!(r, res(0, true), "{text}");
    let drop = h
        .unit("tp-good.service")
        .drop_in("override", "[Service]\nNice=5\n")
        .id();
    let (text, r) = run_script(&wrapped(&h, drop), d.path(), &bin);
    assert_eq!(r, res(0, true), "{text}");
    let bad = h
        .unit("tp-bad.service")
        .content("[Unit]\nDescription=t\nNoSuchKey=1\n[Service]\nExecStart=/bin/sh -c true\n")
        .id();
    let (text, r) = run_script(&wrapped(&h, bad), d.path(), &bin);
    assert_eq!(r, res(1, false), "{text}");
    assert!(!unit_dir(d.path()).join("tp-bad.service").exists());
    let bad_drop = h
        .unit("tp-good.service")
        .drop_in("override", "[Service]\nNoSuchKey=1\n")
        .id();
    let (text, r) = run_script(&wrapped(&h, bad_drop), d.path(), &bin);
    assert_eq!(r, res(1, false), "{text}");
}

// --- through the executor ---

/// `bin` with the stub `systemd-analyze` and a stub `systemctl` that logs
/// every call to `log` and keeps `is-enabled` in `state`.
fn stubs(d: &Path) -> PathBuf {
    let bin = analyze_stub(d);
    let dd = d.display();
    write_exe(
        &bin.join("systemctl"),
        &format!(
            r#"D={dd}
echo "systemctl $*" >> $D/log
u="${{@: -1}}"
s=$(cat "$D/state.$u" 2>/dev/null || echo disabled)
case "$1" in
  is-enabled) echo "$s"; case "$s" in enabled|static) exit 0;; *) exit 1;; esac;;
  enable) echo enabled > "$D/state.$u";;
  is-active) if [ -e "$D/active.$u" ]; then echo active; exit 0; else echo inactive; exit 3; fi;;
  show)
    case "$*" in
      *UnitFileState*) exit 1;;
      *) echo no;;
    esac;;
  daemon-reload)
    if [ -e "$D/reloadfail" ]; then echo "Failed to reload daemon: Access denied" >&2; exit 1; fi;;
  start|restart) touch "$D/active.$u";;
  stop) rm -f "$D/active.$u";;
  disable) echo disabled > "$D/state.$u";;
  *) exit 64;;
esac"#
        ),
    );
    bin
}

fn rundir(d: &Path, booted: bool) -> PathBuf {
    let p = d.join("run-systemd-system");
    if booted {
        std::fs::create_dir_all(&p).unwrap();
    } else {
        let _ = std::fs::remove_dir(&p);
    }
    p
}

fn go(pb: &Playbook<()>, d: &Path, bin: &Path, booted: bool) -> (Report, String) {
    let out = Rc::new(RefCell::new(String::new()));
    let _ = std::fs::remove_file(d.join("log"));
    let report = run(
        pb,
        vec![HostSpec {
            groups: Vec::new(),
            name: "h1".into(),
            target: Target::Local(
                LocalTransport::new()
                    .uid(0)
                    .sandboxed_at(d)
                    .env("PATH", &with_path(bin))
                    .env("TP_SVC_RUNDIR", &rundir(d, booted).display().to_string())
                    .env("TP_UNIT_DIR", &unit_dir(d).display().to_string())
                    .env("TP_FETCH_STAGE", &d.join("stage").display().to_string()),
            ),
            vars: (),
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    let text = out.borrow().clone();
    (report, text)
}

/// The `systemctl` calls the ops made, in order: prepare's probe (`show
/// -p Id,...`) is left out.
fn systemctl_calls(d: &Path) -> Vec<String> {
    log(d)
        .lines()
        .filter(|l| l.starts_with("systemctl ") && !l.contains("-p Id,"))
        .map(str::to_string)
        .collect()
}

/// A `daemon-reload` comes before the first svc op's first call (its
/// state capture, `show -p UnitFileState,ActiveState`).
fn reload_first(calls: &[String]) -> bool {
    let reload = calls.iter().position(|c| c == "systemctl daemon-reload");
    let op = calls
        .iter()
        .position(|c| c.starts_with("systemctl show -p UnitFileState,ActiveState"));
    matches!((reload, op), (Some(r), Some(o)) if r < o)
}

/// The calls that change something.
fn actions(d: &Path) -> Vec<String> {
    const VERBS: [&str; 4] = [
        "systemctl daemon-reload",
        "systemctl enable ",
        "systemctl start ",
        "systemctl restart ",
    ];
    systemctl_calls(d)
        .into_iter()
        .filter(|c| VERBS.iter().any(|v| c.starts_with(v)))
        .collect()
}

fn pb_two_units() -> Playbook<()> {
    fn run_(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            h.unit("a.service")
                .content("[Service]\nExecStart=/bin/true\n");
            h.unit("b.service")
                .content("[Service]\nExecStart=/bin/true\n");
            h.svc("a").enabled().started();
            h.svc("b").enabled();
            Ok(())
        })
    }
    Playbook::new("units", run_)
}

#[test]
fn executor_one_daemon_reload_before_the_first_svc_op_then_none() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let pb = pb_two_units();
    let (r1, out1) = go(&pb, d.path(), &bin, true);
    assert_eq!(
        r1.hosts[0].status,
        HostStatus::Changed,
        "{}\n{out1}",
        r1.render()
    );
    let calls = systemctl_calls(d.path());
    assert_eq!(
        calls
            .iter()
            .filter(|c| *c == "systemctl daemon-reload")
            .count(),
        1,
        "exactly one reload: {calls:#?}"
    );
    assert!(
        reload_first(&calls),
        "the reload comes before the svc ops: {calls:#?}"
    );
    assert!(
        out1.contains("== h1 [prepare] unit a.service: will create =="),
        "{out1}"
    );
    assert!(
        out1.contains("== h1 [prepare] unit b.service: will create =="),
        "{out1}"
    );

    // run 2: the units are as wanted, so no reload
    let (r2, out2) = go(&pb, d.path(), &bin, true);
    assert_eq!(
        r2.hosts[0].status,
        HostStatus::Ok,
        "{}\n{out2}",
        r2.render()
    );
    let calls = systemctl_calls(d.path());
    assert!(
        !calls.iter().any(|c| c == "systemctl daemon-reload"),
        "{calls:#?}"
    );
    assert!(!out2.contains("[prepare] unit"), "{out2}");
}

#[test]
fn executor_not_booted_writes_and_enables_with_no_reload() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let pb = pb_two_units();
    let (r1, out1) = go(&pb, d.path(), &bin, false);
    assert_eq!(r1.exit_code(), 0, "{}\n{out1}", r1.render());
    let calls = systemctl_calls(d.path());
    assert!(
        !calls.iter().any(|c| c == "systemctl daemon-reload"),
        "{calls:#?}"
    );
    assert!(
        calls.iter().any(|c| c == "systemctl enable -- a"),
        "{calls:#?}"
    );
    assert!(unit_dir(d.path()).join("a.service").exists());
    assert!(
        r1.hosts[0].daemon_reloads.is_empty(),
        "not booted: nothing reported\n{}",
        r1.render()
    );
    assert!(!out1.contains("systemd daemon-reload:"), "{out1}");
}

fn pb_unit_with_restart(body: &'static str) -> Playbook<()> {
    // a fn pointer cannot capture; pick the body by the playbook name
    fn run_a(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            let unit = h.unit("m.service");
            unit.content("[Service]\nExecStart=/bin/true\n");
            h.svc("m").restart_on(&unit);
            Ok(())
        })
    }
    fn run_b(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            let unit = h.unit("m.service");
            unit.content("[Service]\nExecStart=/bin/false\n");
            h.svc("m").restart_on(&unit);
            Ok(())
        })
    }
    Playbook::new("restart", if body == "a" { run_a } else { run_b })
}

#[test]
fn executor_restart_on_a_unit_reloads_then_restarts_only_when_it_changed() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let (r1, out1) = go(&pb_unit_with_restart("a"), d.path(), &bin, true);
    assert_eq!(
        r1.hosts[0].status,
        HostStatus::Changed,
        "{}\n{out1}",
        r1.render()
    );
    let calls = systemctl_calls(d.path());
    let reload = calls.iter().position(|c| c == "systemctl daemon-reload");
    let restart = calls.iter().position(|c| c == "systemctl restart -- m");
    assert!(
        reload.is_some() && restart.is_some() && reload < restart,
        "{calls:#?}"
    );
    assert_eq!(
        calls
            .iter()
            .filter(|c| *c == "systemctl daemon-reload")
            .count(),
        1,
        "{calls:#?}"
    );
    assert!(
        out1.contains("svc m.service: will restart if unit m.service changes"),
        "{out1}"
    );

    // unchanged: the restart is skipped, nothing reloads
    let (r2, out2) = go(&pb_unit_with_restart("a"), d.path(), &bin, true);
    assert_eq!(
        r2.hosts[0].status,
        HostStatus::Ok,
        "{}\n{out2}",
        r2.render()
    );
    assert!(
        systemctl_calls(d.path()).is_empty(),
        "{:#?}",
        systemctl_calls(d.path())
    );
    assert!(
        r2.hosts[0]
            .ops
            .iter()
            .any(|o| o.outcome == Outcome::Skipped && o.desc == "svc m restart"),
        "{}",
        r2.render()
    );

    // changed again: one reload, one restart
    let (r3, _) = go(&pb_unit_with_restart("b"), d.path(), &bin, true);
    assert_eq!(r3.hosts[0].status, HostStatus::Changed);
    let calls = actions(d.path());
    assert_eq!(
        calls,
        ["systemctl daemon-reload", "systemctl restart -- m"],
        "{calls:#?}"
    );
}

#[test]
fn executor_a_unit_in_an_earlier_window_reloads_before_a_later_svc_op() {
    fn run_(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            h.unit("a.service")
                .content("[Service]\nExecStart=/bin/true\n");
            // a blocking read ends the window
            let _ = h.bin_exists("tp-no-such-bin-zz").await;
            h.svc("a").enabled();
            Ok(())
        })
    }
    let pb = Playbook::new("windows", run_);
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let (r1, out1) = go(&pb, d.path(), &bin, true);
    assert_eq!(r1.exit_code(), 0, "{}\n{out1}", r1.render());
    let calls = systemctl_calls(d.path());
    assert!(reload_first(&calls), "{calls:#?}");
    assert_eq!(
        calls
            .iter()
            .filter(|c| *c == "systemctl daemon-reload")
            .count(),
        1,
        "{calls:#?}"
    );
}

#[test]
fn executor_prepare_says_not_verified_without_systemd_analyze() {
    fn run_(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            h.unit("a.service")
                .content("[Service]\nExecStart=/bin/true\n");
            Ok(())
        })
    }
    let pb = Playbook::new("noanalyze", run_);
    let d = tempfile::tempdir().unwrap();
    let bin = bare_bin(d.path());
    let (r1, out1) = go(&pb, d.path(), &bin, false);
    assert_eq!(
        r1.hosts[0].status,
        HostStatus::Changed,
        "{}\n{out1}",
        r1.render()
    );
    assert!(
        out1.contains(
            "== h1 [prepare] unit a.service: will create; no systemd-analyze: not verified =="
        ),
        "{out1}"
    );
    // a changed unit: will change; then absent: will remove
    std::fs::write(unit_dir(d.path()).join("a.service"), "other\n").unwrap();
    let (_, out2) = go(&pb, d.path(), &bin, false);
    assert!(
        out2.contains("unit a.service: will change; no systemd-analyze: not verified"),
        "{out2}"
    );
}

// --- the flush: a changed unit with no svc op after it in its window ---

const FLUSH: &str = "systemd daemon-reload, end of window ";

fn reloads(calls: &[String]) -> usize {
    calls
        .iter()
        .filter(|c| *c == "systemctl daemon-reload")
        .count()
}

fn pb_stop_then_remove() -> Playbook<()> {
    fn run_(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            h.svc("a").disabled().stopped();
            h.unit("a.service").absent();
            Ok(())
        })
    }
    Playbook::new("remove", run_)
}

#[test]
fn executor_a_removed_unit_with_no_svc_op_after_it_reloads_once_after_the_removal() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    std::fs::create_dir_all(unit_dir(d.path())).unwrap();
    std::fs::write(
        unit_dir(d.path()).join("a.service"),
        "[Service]\nExecStart=/bin/true\n",
    )
    .unwrap();
    std::fs::write(d.path().join("state.a"), "enabled\n").unwrap();
    std::fs::write(d.path().join("active.a"), "").unwrap();
    let pb = pb_stop_then_remove();
    let (r1, out1) = go(&pb, d.path(), &bin, true);
    assert_eq!(
        r1.hosts[0].status,
        HostStatus::Changed,
        "{}\n{out1}",
        r1.render()
    );
    assert!(!unit_dir(d.path()).join("a.service").exists());
    let calls: Vec<String> = systemctl_calls(d.path())
        .into_iter()
        .filter(|c| {
            c.starts_with("systemctl stop ")
                || c.starts_with("systemctl disable ")
                || c == "systemctl daemon-reload"
        })
        .collect();
    assert_eq!(
        calls,
        [
            "systemctl stop -- a",
            "systemctl disable -- a",
            "systemctl daemon-reload"
        ],
        "one reload, after the removal\n{out1}"
    );
    // the prepare line, the stream line, the report line
    assert!(
        out1.contains(
            "== h1 [prepare] systemd daemon-reload: at the end of the window, after unit a.service =="
        ),
        "{out1}"
    );
    let flush = out1
        .find("   systemd daemon-reload: end of window")
        .expect(&out1);
    let unit_op = out1.find("unit a.service absent ==").expect(&out1);
    assert!(unit_op < flush, "{out1}");
    let rendered = r1.render();
    assert!(
        rendered.contains("h1: window 1: systemd daemon-reload: end of window"),
        "{rendered}"
    );
    assert_eq!(r1.hosts[0].daemon_reloads.len(), 1, "{rendered}");

    // run 2: nothing changes, nothing reloads
    let (r2, out2) = go(&pb, d.path(), &bin, true);
    assert_eq!(
        r2.hosts[0].status,
        HostStatus::Ok,
        "{}\n{out2}",
        r2.render()
    );
    assert_eq!(reloads(&systemctl_calls(d.path())), 0, "{out2}");
    assert!(!out2.contains("daemon-reload"), "{out2}");
    let table = r2.render();
    let row = table.lines().find(|l| l.starts_with("h1")).expect(&table);
    assert_eq!(row.split_whitespace().nth(3), Some("0"), "{table}");
}

#[test]
fn executor_a_unit_changed_in_one_window_reloads_at_its_end_not_in_the_next() {
    fn run_(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            h.unit("a.service")
                .content("[Service]\nExecStart=/bin/true\n");
            // a blocking read ends the window
            let _ = h.bin_exists("tp-no-such-bin-zz").await;
            h.svc("a").enabled();
            Ok(())
        })
    }
    let pb = Playbook::new("windows", run_);
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let (r1, out1) = go(&pb, d.path(), &bin, true);
    assert_eq!(r1.exit_code(), 0, "{}\n{out1}", r1.render());
    let calls = systemctl_calls(d.path());
    assert_eq!(reloads(&calls), 1, "{calls:#?}");
    let flush = out1
        .find("   systemd daemon-reload: end of window")
        .expect(&out1);
    let svc_op = out1.find("svc a ==").expect(&out1);
    assert!(flush < svc_op, "the reload ends window 1: {out1}");
    assert!(
        r1.render().contains("h1: window 1: systemd daemon-reload"),
        "{}",
        r1.render()
    );
}

#[test]
fn executor_two_units_then_a_svc_op_reload_once_before_it_and_do_not_flush() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let logp = d.path().join("events.jsonl");
    let out = Rc::new(RefCell::new(String::new()));
    let pb = pb_two_units();
    let r1 = run(
        &pb,
        vec![HostSpec {
            groups: Vec::new(),
            name: "h1".into(),
            target: Target::Local(
                LocalTransport::new()
                    .uid(0)
                    .sandboxed_at(d.path())
                    .env("PATH", &with_path(&bin))
                    .env(
                        "TP_SVC_RUNDIR",
                        &rundir(d.path(), true).display().to_string(),
                    )
                    .env("TP_UNIT_DIR", &unit_dir(d.path()).display().to_string())
                    .env(
                        "TP_FETCH_STAGE",
                        &d.path().join("stage").display().to_string(),
                    ),
            ),
            vars: (),
        }],
        RunOpts {
            log: Some(logp.clone()),
            stdout: Stdout::Capture(out.clone()),
        },
    );
    let out1 = out.borrow().clone();
    let events = std::fs::read_to_string(&logp).unwrap_or_default();
    assert_eq!(r1.exit_code(), 0, "{}\n{out1}", r1.render());
    assert_eq!(reloads(&systemctl_calls(d.path())), 1);
    assert!(!out1.contains(FLUSH), "{out1}");
    assert!(!out1.contains("[prepare] systemd daemon-reload"), "{out1}");
    // The reload ran inside the svc op. It is one report entry and one
    // stream line, and it is not an op, so CHANGED stays the ops' count.
    assert_eq!(
        r1.hosts[0].daemon_reloads.len(),
        1,
        "carried reload is one report entry\n{}\n{out1}",
        r1.render()
    );
    let reason = "before #2 svc a (unit a.service, b.service changed)";
    assert_eq!(r1.hosts[0].daemon_reloads[0].reason, reason);
    assert_eq!(r1.hosts[0].daemon_reloads[0].rc, 0);
    assert_eq!(r1.hosts[0].daemon_reloads[0].window, 1);
    let line = format!("   systemd daemon-reload: {reason}");
    assert!(out1.contains(&line), "{out1}");
    let svc = out1.find("== h1 [(no role)] #2 svc a ==").expect(&out1);
    let reported = out1.find(&line).expect(&out1);
    let result = out1[svc..].find("-> changed").expect(&out1);
    assert!(reported > svc && reported < svc + result, "{out1}");
    assert!(
        events.contains("\"ev\":\"daemon_reload\"")
            && events.contains(&format!("\"reason\":\"{reason}\""))
            && events.contains("\"op\":2"),
        "one daemon_reload event\n{events}"
    );
    let changed_ops = r1.hosts[0]
        .ops
        .iter()
        .filter(|o| o.outcome == Outcome::Changed)
        .count();
    let row = r1
        .table()
        .lines()
        .find(|l| l.starts_with("h1"))
        .unwrap()
        .to_string();
    assert_eq!(
        row.split_whitespace().nth(3),
        Some(changed_ops.to_string()).as_deref(),
        "CHANGED counts ops, not the reload: {row}"
    );
}

#[test]
fn executor_a_failed_flush_fails_the_host_with_systemctls_message() {
    fn run_(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            h.unit("a.service")
                .content("[Service]\nExecStart=/bin/true\n");
            Ok(())
        })
    }
    let pb = Playbook::new("fail", run_);
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    std::fs::write(d.path().join("reloadfail"), "").unwrap();
    let (r1, out1) = go(&pb, d.path(), &bin, true);
    let rendered = r1.render();
    match &r1.hosts[0].status {
        HostStatus::Failed { op, rc, tail, .. } => {
            assert!(op.starts_with(FLUSH), "{op}");
            assert_eq!(*rc, 1);
            assert!(
                tail.contains("Failed to reload daemon: Access denied"),
                "{tail}"
            );
        }
        s => panic!("{s:?}\n{rendered}\n{out1}"),
    }
    assert_eq!(r1.exit_code(), 1);
    assert!(
        rendered.contains("  | Failed to reload daemon: Access denied"),
        "{rendered}"
    );
    assert!(out1.contains("-> FAILED rc=1"), "{out1}");
}

#[test]
fn executor_not_booted_a_changed_unit_alone_does_not_reload() {
    fn run_(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            h.unit("a.service")
                .content("[Service]\nExecStart=/bin/true\n");
            Ok(())
        })
    }
    let pb = Playbook::new("offline", run_);
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let (r1, out1) = go(&pb, d.path(), &bin, false);
    assert_eq!(r1.exit_code(), 0, "{}\n{out1}", r1.render());
    assert_eq!(reloads(&systemctl_calls(d.path())), 0);
    assert!(!out1.contains("daemon-reload"), "{out1}");
    assert!(!r1.render().contains("daemon-reload"), "{}", r1.render());
}

// --- no cmp, no diff on the host ---

/// Every `/usr/bin` entry except `cmp` and `diff` (Rocky's minimal image has
/// no diffutils), plus the stub `systemd-analyze` and `systemctl`.
fn nocmp_bin(d: &Path) -> PathBuf {
    let stub = stubs(d);
    let bin = d.join("nocmp");
    std::fs::create_dir_all(&bin).unwrap();
    for e in std::fs::read_dir("/usr/bin").unwrap().flatten() {
        let n = e.file_name();
        let s = n.to_string_lossy();
        if matches!(&*s, "cmp" | "diff" | "systemd-analyze" | "systemctl") {
            continue;
        }
        let _ = std::os::unix::fs::symlink(e.path(), bin.join(&n));
    }
    for n in ["systemd-analyze", "systemctl"] {
        std::os::unix::fs::symlink(stub.join(n), bin.join(n)).unwrap();
    }
    bin
}

#[test]
fn unit_op_and_probe_need_no_cmp_or_diff_on_the_host() {
    fn run_(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            h.unit("a.service")
                .content("[Service]\nExecStart=/bin/true\n")
                .mode(0o644);
            Ok(())
        })
    }
    let pb = Playbook::new("nocmp", run_);
    let d = tempfile::tempdir().unwrap();
    let bin = nocmp_bin(d.path());
    let target = unit_dir(d.path()).join("a.service");
    let (r1, out1) = go(&pb, d.path(), &bin, false);
    assert_eq!(
        r1.hosts[0].status,
        HostStatus::Changed,
        "{}\n{out1}",
        r1.render()
    );
    assert!(!out1.contains("command not found"), "{out1}");
    assert!(out1.contains("unit a.service: will create"), "{out1}");
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "[Service]\nExecStart=/bin/true\n"
    );
    // settled: the probe finds it the same, the op changes nothing
    let (r2, out2) = go(&pb, d.path(), &bin, false);
    assert_eq!(
        r2.hosts[0].status,
        HostStatus::Ok,
        "{}\n{out2}",
        r2.render()
    );
    assert!(!out2.contains("command not found"), "{out2}");
    assert!(!out2.contains("will change"), "{out2}");
    // edited: the probe says will change, the op rewrites it
    std::fs::write(&target, "other\n").unwrap();
    let (r3, out3) = go(&pb, d.path(), &bin, false);
    assert_eq!(r3.hosts[0].status, HostStatus::Changed, "{out3}");
    assert!(out3.contains("unit a.service: will change"), "{out3}");
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "[Service]\nExecStart=/bin/true\n"
    );
}
