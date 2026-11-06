//! The `svc` primitive: the template run against a fake `systemctl` on
//! PATH (every state of it), key normalisation, wrapped-script and plan
//! snapshots, trace errors, and the prepare lines through the real executor
//! over the local transport.
//!
//! "Booted" is `[ -d "${TP_SVC_RUNDIR:-/run/systemd/system}" ]`; these tests
//! point `TP_SVC_RUNDIR` at a directory that exists or not, so the answer
//! does not depend on the machine running them.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::facts::os::Os;
use telepatch::plan::render_host;
use telepatch::report::{HostStatus, Report};
use telepatch::trailer::{SvcResult, render_op, split_stream};
use telepatch::transport::local::LocalTransport;
use telepatch::{Host, Key, Node, NodeId, Playbook, PlaybookFuture, Value};

const NONCE: &str = "t0n";

fn os() -> Os {
    Os::parse("ID=debian\nVERSION_ID=12\n").unwrap()
}

fn host() -> Host<()> {
    Host::new("t", os(), Rc::new(()))
}

fn wrapped(h: &Host<()>, id: NodeId) -> String {
    match &h.snapshot().nodes[id.0] {
        Node::Op(op) => render_op(op, id.0, NONCE, &h.os),
        _ => panic!("not an op"),
    }
}

fn write_exe(p: &Path, body: &str) {
    std::fs::write(p, format!("#!/bin/bash\n{body}\n")).unwrap();
    std::fs::set_permissions(p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

/// A `bin` dir with a fake `systemctl`. State files in `d`: `state` (the
/// `is-enabled` word, default `disabled`), `active` (exists: active),
/// `reload` (exists: NeedDaemonReload=yes), `log` (every call).
fn stubs(d: &Path) -> PathBuf {
    let bin = d.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let dd = d.display();
    write_exe(
        &bin.join("systemctl"),
        &format!(
            r#"D={dd}
echo "systemctl $*" >> $D/log
s=$(cat $D/state 2>/dev/null || echo disabled)
case "$1" in
  is-enabled) echo "$s"; case "$s" in enabled|enabled-runtime|static|alias) exit 0;; *) exit 1;; esac;;
  enable)
    case "$s" in
      masked) echo "Failed to enable unit: Unit ${{@: -1}} is masked" >&2; exit 1;;
      not-found) echo "Failed to enable unit: Unit file ${{@: -1}} does not exist." >&2; exit 1;;
    esac
    echo enabled > $D/state;;
  is-active) if [ -e $D/active ]; then echo active; exit 0; else echo inactive; exit 3; fi;;
  show) if [ -e $D/reload ]; then echo yes; else echo no; fi;;
  daemon-reload) rm -f $D/reload;;
  start) touch $D/active;;
  *) exit 64;;
esac"#
        ),
    );
    bin
}

/// A PATH directory holding every `/usr/bin` entry except `systemctl`: a
/// host with no systemctl (the machine running the tests has one).
fn no_systemctl_bin(d: &Path) -> PathBuf {
    let bin = d.join("nosys");
    std::fs::create_dir_all(&bin).unwrap();
    for e in std::fs::read_dir("/usr/bin").unwrap().flatten() {
        let n = e.file_name();
        if n.to_string_lossy().starts_with("systemctl") {
            continue;
        }
        let _ = std::os::unix::fs::symlink(e.path(), bin.join(&n));
    }
    bin
}

/// A directory that exists (booted) or one that does not.
fn rundir(d: &Path, booted: bool) -> PathBuf {
    let p = d.join("run-systemd-system");
    if booted {
        std::fs::create_dir_all(&p).unwrap();
    }
    p
}

fn set(d: &Path, file: &str, v: &str) {
    std::fs::write(d.join(file), v).unwrap();
}

/// `bin` first, then the real PATH; the no-systemctl dir stands alone.
fn with_path(bin: &Path) -> String {
    if bin.ends_with("nosys") {
        bin.display().to_string()
    } else {
        format!("{}:{}", bin.display(), std::env::var("PATH").unwrap())
    }
}

fn run_script(script: &str, d: &Path, path: &Path, booted: bool) -> (String, SvcResult) {
    let out = Command::new("/bin/bash")
        .arg("-c")
        .arg(script)
        .current_dir(d)
        .env("PATH", with_path(path))
        .env("TP_SVC_RUNDIR", rundir(d, booted))
        .output()
        .unwrap();
    let stream = String::from_utf8(out.stdout).unwrap();
    let mut v = split_stream(&stream, NONCE);
    assert_eq!(v.len(), 1, "stream: {stream:?}");
    let (o, t) = v.remove(0);
    let mut text = o;
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (text, t.typed::<SvcResult>().unwrap())
}

fn log(d: &Path) -> String {
    std::fs::read_to_string(d.join("log")).unwrap_or_default()
}

fn r(rc: i32, changed: bool, skipped: bool) -> SvcResult {
    SvcResult {
        rc,
        changed,
        skipped,
    }
}

#[test]
fn enabled_disabled_is_enabled_then_rerun_changes_nothing() {
    for booted in [true, false] {
        let d = tempfile::tempdir().unwrap();
        let bin = stubs(d.path());
        let h = host();
        let id = h.svc("docker").enabled().id();
        let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, booted);
        assert_eq!(res, r(0, true, false), "booted={booted}");
        assert_eq!(
            log(d.path()),
            "systemctl is-enabled -- docker\nsystemctl enable -- docker\n"
        );
        let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, booted);
        assert_eq!(res, r(0, false, false), "booted={booted}");
        assert_eq!(log(d.path()).matches("enable --").count(), 1);
    }
}

#[test]
fn already_enabled_states_need_no_action() {
    for st in ["enabled", "enabled-runtime", "static", "alias"] {
        let d = tempfile::tempdir().unwrap();
        let bin = stubs(d.path());
        set(d.path(), "state", st);
        let h = host();
        let id = h.svc("docker").enabled().id();
        let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, true);
        assert_eq!(res, r(0, false, false), "{st}");
        assert_eq!(log(d.path()), "systemctl is-enabled -- docker\n", "{st}");
    }
}

#[test]
fn masked_unit_fails_with_systemctls_message_and_rc() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    set(d.path(), "state", "masked");
    let h = host();
    let id = h.svc("docker").enabled().started().id();
    let (out, res) = run_script(&wrapped(&h, id), d.path(), &bin, true);
    assert_eq!(res, r(1, false, false));
    assert!(out.contains("Unit docker is masked"), "{out}");
    assert!(!log(d.path()).contains("start"), "{}", log(d.path()));
}

#[test]
fn unit_that_does_not_exist_fails_on_a_booted_host() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    set(d.path(), "state", "not-found");
    let h = host();
    let id = h.svc("nosuch").enabled().id();
    let (out, res) = run_script(&wrapped(&h, id), d.path(), &bin, true);
    assert_eq!(res, r(1, false, false));
    assert!(out.contains("does not exist"), "{out}");
}

#[test]
fn started_inactive_starts_then_active_changes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let h = host();
    let id = h.svc("docker").started().id();
    let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, true);
    assert_eq!(res, r(0, true, false));
    assert_eq!(
        log(d.path()),
        "systemctl is-active --quiet -- docker\nsystemctl show -p NeedDaemonReload --value -- docker\nsystemctl start -- docker\n"
    );
    let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, true);
    assert_eq!(res, r(0, false, false));
    assert_eq!(log(d.path()).matches("start --").count(), 1);
}

#[test]
fn enabled_and_started_do_both_then_rerun_is_quiet() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let h = host();
    let id = h.svc("docker").enabled().started().id();
    let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, true);
    assert_eq!(res, r(0, true, false));
    assert!(log(d.path()).contains("enable -- docker"));
    assert!(log(d.path()).contains("start -- docker"));
    let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, true);
    assert_eq!(res, r(0, false, false));
}

#[test]
fn daemon_reload_runs_before_start_when_systemd_says_so() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    set(d.path(), "reload", "");
    let h = host();
    let id = h.svc("docker").started().id();
    let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, true);
    assert_eq!(res, r(0, true, false));
    let l = log(d.path());
    let reload = l.find("daemon-reload").expect(&l);
    let start = l.find("start -- docker").expect(&l);
    assert!(reload < start, "{l}");
    // Already active: no reload is asked about, nothing runs.
    let d2 = tempfile::tempdir().unwrap();
    let bin2 = stubs(d2.path());
    set(d2.path(), "reload", "");
    set(d2.path(), "active", "");
    let (_, res) = run_script(&wrapped(&h, id), d2.path(), &bin2, true);
    assert_eq!(res, r(0, false, false));
    assert!(!log(d2.path()).contains("daemon-reload"));
}

#[test]
fn not_booted_applies_enable_and_skips_start() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let h = host();
    let id = h.svc("docker").enabled().started().id();
    let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, false);
    assert_eq!(res, r(0, true, false), "enable was applied");
    let l = log(d.path());
    assert!(l.contains("enable -- docker"), "{l}");
    for never in ["is-active", "start", "show", "daemon-reload"] {
        assert!(!l.contains(never), "{never} offline: {l}");
    }
    // Rerun: enable settled, start skipped: rc 0, skipped, never changed.
    let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, false);
    assert_eq!(res, r(0, false, true));
    // Start alone, not booted: skipped, no systemctl call at all.
    let d2 = tempfile::tempdir().unwrap();
    let bin2 = stubs(d2.path());
    let id = h.svc("docker").started().id();
    let (_, res) = run_script(&wrapped(&h, id), d2.path(), &bin2, false);
    assert_eq!(res, r(0, false, true));
    assert_eq!(log(d2.path()), "");
}

#[test]
fn no_systemctl_skips_the_whole_op() {
    let d = tempfile::tempdir().unwrap();
    let bin = no_systemctl_bin(d.path());
    let h = host();
    let id = h.svc("docker").enabled().started().id();
    for booted in [true, false] {
        let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, booted);
        assert_eq!(res, r(0, false, true), "booted={booted}");
    }
}

#[test]
fn unit_name_is_quoted() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let h = host();
    let id = h.svc("we ird;x").enabled().id();
    let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, true);
    assert!(res.changed);
    assert!(
        log(d.path()).contains("enable -- we ird;x\n"),
        "{}",
        log(d.path())
    );
}

#[test]
fn svc_template_snapshots() {
    let h = host();
    let a = h.svc("docker").enabled().started().id();
    let b = h.svc("cron.service").enabled().id();
    let c = h.svc("ssh.socket").started().id();
    insta::assert_snapshot!(format!(
        "{}\n{}\n{}",
        wrapped(&h, a),
        wrapped(&h, b),
        wrapped(&h, c)
    ));
}

#[test]
fn svc_plan_shows_effects() {
    let h = host();
    h.svc("docker").enabled().started();
    h.svc("docker.service").enabled();
    h.svc("ssh.socket").started();
    h.as_user("root").svc("cron").enabled();
    insta::assert_snapshot!(render_host("h", &h.os, &h.snapshot()));
}

#[test]
fn svc_key_adds_service_when_the_name_has_no_unit_suffix() {
    let h = host();
    let a = h.svc("docker").enabled().id();
    let b = h.svc("docker.service").enabled().id();
    let c = h.svc("ssh.socket").enabled().id();
    let e = h.svc("getty@tty1").enabled().id();
    let snap = h.snapshot();
    let writes = |id: NodeId| {
        let Node::Op(op) = &snap.nodes[id.0] else {
            panic!()
        };
        assert_eq!(op.spec.kind(), "svc");
        op.writes.clone()
    };
    assert_eq!(writes(a), [Key::svc("docker.service")].into());
    assert_eq!(writes(b), writes(a), "one key");
    assert_eq!(writes(c), [Key::svc("ssh.socket")].into());
    assert_eq!(writes(e), [Key::svc("getty@tty1.service")].into());
    let Node::Op(op) = &snap.nodes[a.0] else {
        panic!()
    };
    assert_eq!(
        op.post,
        [(Key::svc("docker.service"), Value::Exists(true))].into()
    );
    assert_eq!(Key::svc("docker.service").to_string(), "svc:docker.service");
    assert!(snap.errors.is_empty());
}

#[test]
fn svc_is_root_only() {
    let h = host();
    h.as_user("deploy").svc("docker").enabled();
    let errs = h.snapshot().errors;
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(
        errs[0].to_string().contains("svc") && errs[0].to_string().contains("root"),
        "{}",
        errs[0]
    );
    let ok = host();
    ok.as_user("root").svc("docker").enabled();
    ok.svc("docker").started();
    assert!(ok.snapshot().errors.is_empty());
}

#[test]
fn svc_that_wants_nothing_is_a_trace_error() {
    let h = host();
    h.svc("docker");
    let errs = h.snapshot().errors;
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!(errs[0].message(), "svc(docker): nothing wanted");
    let ok = host();
    ok.svc("docker").started();
    ok.svc("cron").enabled();
    assert!(ok.snapshot().errors.is_empty());
}

// --- through the executor ---

fn go(pb: &Playbook<PathBuf>, d: &Path, path: &Path, booted: bool) -> (Report, String) {
    go_log(pb, d, path, booted, None)
}

fn go_log(
    pb: &Playbook<PathBuf>,
    d: &Path,
    path: &Path,
    booted: bool,
    log: Option<PathBuf>,
) -> (Report, String) {
    let path_s = with_path(path);
    let out = Rc::new(RefCell::new(String::new()));
    let report = run(
        pb,
        vec![HostSpec {
            name: "h1".into(),
            target: Target::Local(
                LocalTransport::new()
                    .uid(0)
                    .env("PATH", &path_s)
                    .env("TP_SVC_RUNDIR", &rundir(d, booted).display().to_string()),
            ),
            vars: d.to_path_buf(),
        }],
        RunOpts {
            log,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    let text = out.borrow().clone();
    (report, text)
}

fn pb_docker() -> Playbook<PathBuf> {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.svc("docker").enabled().started();
            Ok(())
        })
    }
    Playbook {
        name: "svc",
        run: run_,
    }
}

#[test]
fn executor_booted_prepare_line_then_changed_0() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let pb = pb_docker();
    let (r1, out1) = go(&pb, d.path(), &bin, true);
    assert_eq!(
        r1.hosts[0].status,
        HostStatus::Changed,
        "{}\n{out1}",
        r1.render()
    );
    assert!(
        out1.contains("== h1 [prepare] svc docker.service: will enable, will start =="),
        "{out1}"
    );
    assert!(out1.contains("svc docker"), "executor label: {out1}");
    assert_eq!(r1.hosts[0].will_create.len(), 1);
    let (r2, out2) = go(&pb, d.path(), &bin, true);
    assert_eq!(r2.hosts[0].status, HostStatus::Ok, "{}", r2.render());
    assert!(r2.hosts[0].ops.iter().all(|o| !o.changed));
    assert!(r2.hosts[0].will_create.is_empty());
    assert!(
        !out2.contains("[prepare]"),
        "settled prints nothing: {out2}"
    );
}

#[test]
fn executor_not_booted_enables_and_says_start_is_skipped() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let pb = pb_docker();
    let (r1, out1) = go(&pb, d.path(), &bin, false);
    assert_eq!(r1.exit_code(), 0, "{}\n{out1}", r1.render());
    assert!(
        out1.contains(
            "== h1 [prepare] svc docker.service: will enable; no running systemd: start skipped =="
        ),
        "{out1}"
    );
    assert_eq!(
        std::fs::read_to_string(d.path().join("state"))
            .unwrap()
            .trim(),
        "enabled"
    );
    let (r2, out2) = go(&pb, d.path(), &bin, false);
    assert_eq!(r2.hosts[0].status, HostStatus::Ok, "{}", r2.render());
    assert!(
        r2.hosts[0].ops.iter().all(|o| !o.changed),
        "{}",
        r2.render()
    );
    assert!(!out2.contains("[prepare]"), "{out2}");
}

#[test]
fn executor_no_systemctl_is_skipped_and_never_a_change() {
    let d = tempfile::tempdir().unwrap();
    let bin = no_systemctl_bin(d.path());
    let pb = pb_docker();
    for _ in 0..2 {
        let (r1, out1) = go(&pb, d.path(), &bin, false);
        assert_eq!(
            r1.hosts[0].status,
            HostStatus::Ok,
            "{}\n{out1}",
            r1.render()
        );
        assert!(
            out1.contains("== h1 [prepare] svc docker.service: no systemctl: skipped =="),
            "{out1}"
        );
        assert!(r1.hosts[0].ops.iter().all(|o| !o.changed));
    }
}

#[test]
fn executor_merges_wants_of_one_unit_and_names_it_once() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.svc("docker").enabled();
            h.svc("docker.service").started();
            Ok(())
        })
    }
    let pb = Playbook {
        name: "svc",
        run: run_,
    };
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let (_, out) = go(&pb, d.path(), &bin, true);
    assert_eq!(
        out.matches("svc docker.service: will enable, will start")
            .count(),
        1,
        "{out}"
    );
}

#[test]
fn executor_failed_probe_reports_nothing() {
    // A systemctl whose queries all fail: the probe's output is empty of
    // facts, so nothing is claimed.
    let d = tempfile::tempdir().unwrap();
    let bin = d.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    write_exe(&bin.join("systemctl"), "exit 1");
    let pb = pb_docker();
    let (_, out) = go(&pb, d.path(), &bin, true);
    assert!(!out.contains("[prepare] svc"), "{out}");
}

#[test]
fn svc_will_create_is_a_json_event() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let log_path = d.path().join("events.jsonl");
    let pb = pb_docker();
    go_log(&pb, d.path(), &bin, true, Some(log_path.clone()));
    let text = std::fs::read_to_string(&log_path).unwrap();
    let ev = text
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|e| e["ev"] == "will_create")
        .unwrap_or_else(|| panic!("no will_create event:\n{text}"));
    assert_eq!(ev["kind"], "svc");
    assert_eq!(ev["name"], "docker.service");
    assert_eq!(ev["detail"], "will enable, will start");
    assert_eq!(ev["host"], "h1");
}

/// No systemctl when prepare looks, but an earlier op of the same window
/// can bring it (a package, a `sh`, a file): prepare must not promise a
/// skip, since the script decides at run time. Here a file op writes the
/// stub `systemctl` into the PATH dir, and the svc op then enables.
#[test]
fn executor_no_systemctl_yet_is_decided_at_run_time() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            let stub = std::fs::read(h.vars.join("bin/systemctl")).unwrap();
            let target = h.vars.join("nosys/systemctl");
            h.file(&target.display().to_string())
                .content(stub)
                .mode(0o755);
            h.svc("docker").enabled().started();
            Ok(())
        })
    }
    let pb = Playbook {
        name: "svc",
        run: run_,
    };
    let d = tempfile::tempdir().unwrap();
    stubs(d.path());
    let bin = no_systemctl_bin(d.path());
    let (r1, out1) = go(&pb, d.path(), &bin, false);
    assert_eq!(r1.exit_code(), 0, "{}\n{out1}", r1.render());
    assert!(
        out1.contains(
            "== h1 [prepare] svc docker.service: no systemctl yet: decided at run time, after this window's earlier ops =="
        ),
        "{out1}"
    );
    assert!(!out1.contains("no systemctl: skipped"), "{out1}");
    assert_eq!(
        std::fs::read_to_string(d.path().join("state"))
            .unwrap()
            .trim(),
        "enabled"
    );
}
