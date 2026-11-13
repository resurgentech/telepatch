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
  show)
    case "$*" in
      *UnitFileState*)
        if [ -e $D/showfail ]; then exit 1; fi
        # one block per unit, in argument order, blank-line separated
        first=1
        for u in "${{@:5}}"; do
          if [ $first = 0 ]; then echo; fi
          first=0
          echo "Id=$u.service"
          echo "UnitFileState=$s"
          if [ -e $D/active ]; then echo ActiveState=active; else echo ActiveState=inactive; fi
          if [ "$s" = masked ]; then echo LoadState=masked; else echo LoadState=loaded; fi
        done;;
      *) if [ -e $D/reload ]; then echo yes; else echo no; fi;;
    esac;;
  daemon-reload) rm -f $D/reload;;
  start) touch $D/active;;
  stop) rm -f $D/active;;
  disable) echo disabled > $D/state;;
  mask) echo masked > $D/state;;
  restart) touch $D/active;;
  reload) if [ -e $D/active ]; then :; else echo "Job for unit failed: not active" >&2; exit 1; fi;;
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

/// As [`run_script_full`], with the inverse data cleared so a result
/// compares against `r(rc, changed, skipped)`.
fn run_script(script: &str, d: &Path, path: &Path, booted: bool) -> (String, SvcResult) {
    let (text, mut res) = run_script_full(script, d, path, booted);
    res.was_enabled = None;
    res.was_active = None;
    res.was_masked = None;
    (text, res)
}

fn run_script_full(script: &str, d: &Path, path: &Path, booted: bool) -> (String, SvcResult) {
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

/// `res` from a start that ran (the trailer's `started`).
fn started(res: SvcResult) -> SvcResult {
    SvcResult {
        started: true,
        ..res
    }
}

fn r(rc: i32, changed: bool, skipped: bool) -> SvcResult {
    SvcResult {
        rc,
        changed,
        skipped,
        ..SvcResult::default()
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
        // the capture of the inverse data comes first
        let cap = if booted {
            "systemctl show -p UnitFileState,ActiveState -- docker\n"
        } else {
            "systemctl is-enabled -- docker\n"
        };
        assert_eq!(
            log(d.path()),
            format!("{cap}systemctl is-enabled -- docker\nsystemctl enable -- docker\n")
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
        assert_eq!(
            log(d.path()),
            "systemctl show -p UnitFileState,ActiveState -- docker\nsystemctl is-enabled -- docker\n",
            "{st}"
        );
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
    assert_eq!(res, started(r(0, true, false)));
    assert_eq!(
        log(d.path()),
        "systemctl show -p UnitFileState,ActiveState -- docker\nsystemctl is-active --quiet -- docker\nsystemctl show -p NeedDaemonReload --value -- docker\nsystemctl start -- docker\n"
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
    assert_eq!(res, started(r(0, true, false)));
    assert!(log(d.path()).contains("enable -- docker"));
    assert!(log(d.path()).contains("start -- docker"));
    let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, true);
    assert_eq!(res, r(0, false, false));
}

#[test]
fn enabling_a_running_unit_is_changed_but_not_started() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    set(d.path(), "active", "");
    let h = host();
    let id = h.svc("docker").enabled().started().id();
    let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, true);
    assert_eq!(res, r(0, true, false), "enabled, already active");
    assert!(!log(d.path()).contains("start -- docker"));
}

#[test]
fn daemon_reload_runs_before_start_when_systemd_says_so() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    set(d.path(), "reload", "");
    let h = host();
    let id = h.svc("docker").started().id();
    let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, true);
    let mut want = started(r(0, true, false));
    want.reloads = "need".into();
    assert_eq!(res, want);
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
    assert_eq!(
        log(d2.path()),
        "systemctl is-enabled -- docker\n",
        "capture only"
    );
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
            groups: Vec::new(),
            name: "h1".into(),
            target: Target::Local(
                LocalTransport::new()
                    .uid(0)
                    .sandboxed_at(d)
                    .env("PATH", &path_s)
                    .env("TP_SVC_RUNDIR", &rundir(d, booted).display().to_string())
                    .env("TP_FETCH_STAGE", &d.join("stage").display().to_string()),
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
    Playbook::new("svc", run_)
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
    let pb = Playbook::new("svc", run_);
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
    let pb = Playbook::new("svc", run_);
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

// --- T75: disabled, masked, stopped ---

fn state_of(d: &Path) -> String {
    std::fs::read_to_string(d.join("state"))
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[test]
fn disabled_disables_an_enabled_unit_once() {
    for booted in [true, false] {
        let d = tempfile::tempdir().unwrap();
        let bin = stubs(d.path());
        set(d.path(), "state", "enabled");
        let h = host();
        let id = h.svc("docker").disabled().id();
        let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, booted);
        assert_eq!(res, r(0, true, false), "booted={booted}");
        assert_eq!(state_of(d.path()), "disabled");
        let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, booted);
        assert_eq!(res, r(0, false, false), "booted={booted}");
        assert_eq!(log(d.path()).matches("disable --").count(), 1);
    }
}

#[test]
fn disabled_leaves_masked_static_and_missing_units_alone() {
    for st in ["masked", "static", "not-found", "disabled"] {
        let d = tempfile::tempdir().unwrap();
        let bin = stubs(d.path());
        set(d.path(), "state", st);
        let h = host();
        let id = h.svc("docker").disabled().id();
        let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, false);
        assert_eq!(res, r(0, false, false), "{st}");
        assert!(!log(d.path()).contains("disable --"), "{st}");
    }
}

#[test]
fn masked_masks_once_offline_and_booted() {
    for booted in [true, false] {
        let d = tempfile::tempdir().unwrap();
        let bin = stubs(d.path());
        let h = host();
        let id = h.svc("docker").masked().id();
        let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, booted);
        assert_eq!(res, r(0, true, false), "booted={booted}");
        assert_eq!(state_of(d.path()), "masked");
        let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, booted);
        assert_eq!(res, r(0, false, false), "booted={booted}");
        assert_eq!(log(d.path()).matches("mask --").count(), 1);
    }
}

#[test]
fn stopped_stops_an_active_unit_then_changes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    set(d.path(), "active", "");
    let h = host();
    let id = h.svc("docker").stopped().id();
    let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, true);
    assert_eq!(res, r(0, true, false));
    assert!(!d.path().join("active").exists());
    let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, true);
    assert_eq!(res, r(0, false, false));
    assert_eq!(log(d.path()).matches("stop --").count(), 1);
}

#[test]
fn not_booted_stopped_is_skipped_but_mask_is_applied() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let h = host();
    let id = h.svc("docker").stopped().id();
    let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, false);
    assert_eq!(res, r(0, false, true));
    let id = h.svc("docker").masked().stopped().id();
    let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, false);
    assert_eq!(res, r(0, true, false), "mask applied");
    let l = log(d.path());
    assert!(l.contains("mask -- docker"), "{l}");
    for never in ["is-active", "stop --", "show"] {
        assert!(!l.contains(never), "{never} offline: {l}");
    }
}

#[test]
fn no_systemctl_skips_new_states_too() {
    let d = tempfile::tempdir().unwrap();
    let bin = no_systemctl_bin(d.path());
    let h = host();
    let id = h.svc("docker").masked().stopped().id();
    for booted in [true, false] {
        let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, booted);
        assert_eq!(res, r(0, false, true), "booted={booted}");
    }
}

#[test]
fn masked_and_stopped_stops_before_it_masks() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    set(d.path(), "active", "");
    let h = host();
    let id = h.svc("docker").masked().stopped().id();
    let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, true);
    assert_eq!(res, r(0, true, false));
    let l = log(d.path());
    assert!(
        l.find("stop --").unwrap() < l.find("mask --").unwrap(),
        "{l}"
    );
}

#[test]
fn inverse_data_is_captured_before_the_change() {
    // booted: from one `systemctl show`
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    set(d.path(), "state", "enabled");
    set(d.path(), "active", "");
    let h = host();
    let id = h.svc("docker").masked().stopped().id();
    let (_, res) = run_script_full(&wrapped(&h, id), d.path(), &bin, true);
    assert_eq!(
        (res.was_enabled, res.was_active, res.was_masked),
        (Some(true), Some(true), Some(false))
    );
    // masked now: the next run's capture says so
    let (_, res) = run_script_full(&wrapped(&h, id), d.path(), &bin, true);
    assert_eq!(
        (res.was_enabled, res.was_active, res.was_masked),
        (Some(false), Some(false), Some(true))
    );
    // not booted: enabled and masked from is-enabled, active unknown
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    set(d.path(), "state", "enabled");
    let id = h.svc("docker").disabled().id();
    let (_, res) = run_script_full(&wrapped(&h, id), d.path(), &bin, false);
    assert_eq!(
        (res.was_enabled, res.was_active, res.was_masked),
        (Some(true), None, Some(false))
    );
    // no systemctl: nothing known
    let bin = no_systemctl_bin(d.path());
    let (_, res) = run_script_full(&wrapped(&h, id), d.path(), &bin, false);
    assert_eq!(
        (res.was_enabled, res.was_active, res.was_masked),
        (None, None, None)
    );
}

#[test]
fn exclusive_states_are_trace_errors() {
    let h = host();
    h.svc("a").enabled().disabled();
    h.svc("b").enabled().masked();
    h.svc("c").disabled().masked().stopped();
    h.svc("d").started().stopped();
    let errs: Vec<String> = h
        .snapshot()
        .errors
        .iter()
        .map(|e| e.message().to_string())
        .collect();
    assert_eq!(
        errs,
        [
            "svc(a): .enabled() and .disabled() are exclusive",
            "svc(b): .enabled() and .masked() are exclusive",
            "svc(c): .disabled() and .masked() are exclusive",
            "svc(d): .started() and .stopped() are exclusive",
        ]
    );
    let ok = host();
    ok.svc("a").enabled().started();
    ok.svc("b").disabled().stopped();
    ok.svc("c").masked().stopped();
    assert!(ok.snapshot().errors.is_empty());
}

#[test]
fn stopped_or_masked_alone_is_something_wanted() {
    let h = host();
    h.svc("a").stopped();
    h.svc("b").masked();
    h.svc("c").disabled();
    assert!(h.snapshot().errors.is_empty());
}

#[test]
fn template_instance_keeps_its_name_and_key() {
    let h = host();
    let id = h.svc("tigervncserver@:1").enabled().id();
    let snap = h.snapshot();
    let Node::Op(op) = &snap.nodes[id.0] else {
        panic!()
    };
    assert_eq!(
        op.writes,
        [Key::svc("tigervncserver@:1.service")]
            .into_iter()
            .collect()
    );
    assert_eq!(
        Key::svc("tigervncserver@:1.service").to_string(),
        "svc:tigervncserver@:1.service"
    );
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, false);
    assert_eq!(res, r(0, true, false));
    assert!(
        log(d.path()).contains("enable -- tigervncserver@:1\n"),
        "{}",
        log(d.path())
    );
}

#[test]
fn svc_new_states_template_snapshots() {
    let h = host();
    let a = h.svc("bluetooth").disabled().stopped().id();
    let b = h
        .svc("systemd-networkd-wait-online")
        .masked()
        .stopped()
        .id();
    let c = h.svc("tigervncserver@:1").enabled().started().id();
    insta::assert_snapshot!(format!(
        "{}\n{}\n{}",
        wrapped(&h, a),
        wrapped(&h, b),
        wrapped(&h, c)
    ));
}

#[test]
fn svc_new_states_plan_shows_effects() {
    let h = host();
    h.svc("bluetooth").disabled().stopped();
    h.svc("wait-online").masked();
    insta::assert_snapshot!(render_host("h", &h.os, &h.snapshot()));
}

fn pb_off() -> Playbook<PathBuf> {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.svc("wait-online").masked().stopped();
            h.svc("bluetooth").disabled().stopped();
            Ok(())
        })
    }
    Playbook::new("svc", run_)
}

#[test]
fn executor_booted_says_will_stop_will_mask_with_one_show() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    set(d.path(), "state", "enabled");
    set(d.path(), "active", "");
    let (r1, out1) = go(&pb_off(), d.path(), &bin, true);
    assert_eq!(r1.exit_code(), 0, "{}\n{out1}", r1.render());
    assert!(
        out1.contains("== h1 [prepare] svc wait-online.service: will stop, will mask =="),
        "{out1}"
    );
    assert!(
        out1.contains("== h1 [prepare] svc bluetooth.service: will stop, will disable =="),
        "{out1}"
    );
    // the probe: one systemctl show for both units, no per-unit queries
    let l = log(d.path());
    let probe =
        "systemctl show -p Id,UnitFileState,ActiveState,LoadState -- wait-online bluetooth\n";
    assert_eq!(l.matches(probe).count(), 1, "{l}");
    let before_run = l.find(probe).unwrap();
    assert!(
        !l[..before_run].contains("is-enabled") && !l[..before_run].contains("is-active"),
        "{l}"
    );
    // second run: the unit is masked, so nothing is left to say
    let (r2, out2) = go(&pb_off(), d.path(), &bin, true);
    assert_eq!(
        r2.hosts[0].status,
        HostStatus::Ok,
        "{}\n{out2}",
        r2.render()
    );
    assert!(r2.hosts[0].will_create.is_empty(), "{out2}");
}

#[test]
fn executor_offline_applies_mask_and_says_stop_is_skipped() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let (r1, out1) = go(&pb_off(), d.path(), &bin, false);
    assert_eq!(r1.exit_code(), 0, "{}\n{out1}", r1.render());
    assert!(
        out1.contains(
            "== h1 [prepare] svc wait-online.service: will mask; no running systemd: stop skipped =="
        ),
        "{out1}"
    );
    assert!(!log(d.path()).contains("show -p Id"), "{}", log(d.path()));
    assert_eq!(state_of(d.path()), "masked");
}

#[test]
fn executor_falls_back_to_is_enabled_when_show_fails() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    set(d.path(), "showfail", "");
    let (_, out) = go(&pb_docker(), d.path(), &bin, true);
    assert!(
        out.contains("== h1 [prepare] svc docker.service: will enable, will start =="),
        "{out}"
    );
}

// --- restart and reload (T76) ---

fn errors_of(h: &Host<()>) -> Vec<String> {
    h.snapshot().errors.iter().map(|e| e.to_string()).collect()
}

#[test]
fn restart_template_restarts_booted_and_skips_offline() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let h = host();
    let f = h.file("/etc/x");
    f.content("x");
    let id = h.svc("docker").restart().when(f.changed()).id();
    let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, true);
    assert_eq!(res, r(0, true, false));
    assert!(
        log(d.path()).contains("systemctl restart -- docker\n"),
        "{}",
        log(d.path())
    );
    // no running systemd: skipped, never changed
    let d2 = tempfile::tempdir().unwrap();
    let bin2 = stubs(d2.path());
    let (_, res) = run_script(&wrapped(&h, id), d2.path(), &bin2, false);
    assert_eq!(res, r(0, false, true));
    assert!(!log(d2.path()).contains("restart"), "{}", log(d2.path()));
    // no systemctl: skipped
    let d3 = tempfile::tempdir().unwrap();
    let nos = no_systemctl_bin(d3.path());
    let (_, res) = run_script(&wrapped(&h, id), d3.path(), &nos, true);
    assert_eq!(res, r(0, false, true));
}

#[test]
fn restart_runs_daemon_reload_first_when_systemd_says_so() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    set(d.path(), "reload", "");
    let h = host();
    let f = h.file("/etc/x");
    f.content("x");
    let id = h.svc("docker").restart().when(f.changed()).id();
    let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, true);
    let mut want = r(0, true, false);
    want.reloads = "need".into();
    assert_eq!(res, want);
    let l = log(d.path());
    let dr = l.find("systemctl daemon-reload\n").expect(&l);
    let rs = l.find("systemctl restart -- docker\n").expect(&l);
    assert!(dr < rs, "{l}");
}

#[test]
fn reload_reloads_an_active_unit_and_starts_an_inactive_one() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    set(d.path(), "active", "");
    let h = host();
    let f = h.file("/etc/ssh/sshd_config");
    f.content("x");
    let id = h.svc("ssh").reload().when(f.changed()).id();
    let (_, res) = run_script(&wrapped(&h, id), d.path(), &bin, true);
    assert_eq!(res, r(0, true, false));
    assert!(
        log(d.path()).contains("systemctl reload -- ssh\n"),
        "{}",
        log(d.path())
    );
    let d2 = tempfile::tempdir().unwrap();
    let bin2 = stubs(d2.path());
    let (_, res) = run_script(&wrapped(&h, id), d2.path(), &bin2, true);
    assert_eq!(res, r(0, true, false));
    let l = log(d2.path());
    assert!(
        l.contains("systemctl start -- ssh\n") && !l.contains("reload -- ssh"),
        "{l}"
    );
}

#[test]
fn restart_template_snapshots() {
    let h = host();
    let f = h.file("/etc/x");
    f.content("x");
    let a = h.svc("docker").restart().when(f.changed()).id();
    let b = h.svc("ssh").reload().when(f.changed()).id();
    insta::assert_snapshot!(format!("{}\n{}", wrapped(&h, a), wrapped(&h, b)));
}

#[test]
fn an_unguarded_restart_is_the_policy_error_unguarded_restart() {
    fn run_(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            h.svc("docker").restart();
            Ok(())
        })
    }
    let e = trace_error(run_);
    assert!(
        e.contains("svc docker.service: .restart() has no guard (policy: unguarded-restart)"),
        "{e}"
    );
    assert!(
        e.contains("to permit: .allow(Policy::UnguardedRestart, \"<reason>\")"),
        "{e}"
    );
    fn reload_(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            h.svc("ssh").reload();
            Ok(())
        })
    }
    let e = trace_error(reload_);
    assert!(
        e.contains("svc ssh.service: .reload() has no guard (policy: unguarded-restart)"),
        "{e}"
    );
}

#[test]
fn a_guard_or_an_allowance_after_restart_satisfies_the_policy() {
    fn run_(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            let toolkit = h.pkg(["nvidia-container-toolkit"]);
            h.svc("docker").restart().when(toolkit.changed());
            let conf = h.file("/etc/ssh/sshd_config");
            conf.content("x");
            let s = h.svc("ssh");
            s.reload();
            s.when(conf.changed());
            h.svc("flaky-agent").restart().allow(
                telepatch::Policy::UnguardedRestart,
                "leaks memory; the vendor says restart daily",
            );
            h.svc("containerd").restart_on(&toolkit);
            Ok(())
        })
    }
    let host = telepatch::testing::FixtureHost::new("h", os(), Rc::new(()));
    let h = Host::new(host.name.clone(), host.os.clone(), host.vars.clone());
    let mut engine = telepatch::engine::Engine::new(telepatch::testing::FakeExecutor::new());
    let res = telepatch::testing::block_on(engine.run_host(&Playbook::new("t", run_), h));
    assert!(res.is_ok(), "{res:?}");
}

#[test]
fn restart_on_is_restart_when_changed() {
    let h = host();
    let toolkit = h.pkg(["nvidia-container-toolkit"]);
    let a = h.svc("docker").restart_on(&toolkit).id();
    let b = h.svc("docker").restart().when(toolkit.changed()).id();
    let snap = h.snapshot();
    let (Node::Op(x), Node::Op(y)) = (&snap.nodes[a.0], &snap.nodes[b.0]) else {
        panic!()
    };
    assert_eq!(x.spec, y.spec);
    assert_eq!(x.guard, Some(toolkit.changed()));
    assert_eq!(x.guard, y.guard);
    assert!(errors_of(&h).is_empty(), "{:?}", errors_of(&h));
}

#[test]
fn restart_shares_an_op_with_no_state() {
    let h = host();
    let f = h.file("/etc/x");
    f.content("x");
    h.svc("docker").restart().when(f.changed()).started();
    h.svc("ssh").restart().reload().when(f.changed());
    let errs: Vec<String> = h
        .snapshot()
        .errors
        .iter()
        .map(|e| e.message().to_string())
        .collect();
    assert_eq!(
        errs,
        [
            "svc(docker): .started() after .restart(): a chain names its states before the action",
            "svc(ssh): .restart() and .reload() are exclusive",
        ]
    );
}

fn trace_error(run_: fn(Host<()>) -> PlaybookFuture) -> String {
    let host = telepatch::testing::FixtureHost::new("h", os(), Rc::new(()));
    let h = Host::new(host.name.clone(), host.os.clone(), host.vars.clone());
    let mut engine = telepatch::engine::Engine::new(telepatch::testing::FakeExecutor::new());
    let res = telepatch::testing::block_on(engine.run_host(&Playbook::new("t", run_), h));
    match res {
        Err(telepatch::engine::EngineError::Playbook(e)) => e.to_string(),
        other => panic!("expected a trace error, got {other:?}"),
    }
}

fn conf(h: &Host<PathBuf>, name: &str) -> String {
    h.vars.join(name).display().to_string()
}

/// docker started by one role; a restart on a config file by another.
fn pb_restart() -> Playbook<PathBuf> {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            let daemon = h.file(&conf(&h, "daemon.json"));
            daemon.content("{}\n");
            h.svc("docker").restart().when(daemon.changed());
            Ok(())
        })
    }
    Playbook::new("svc", run_)
}

fn restarts(d: &Path) -> usize {
    log(d)
        .lines()
        .filter(|l| {
            *l == "systemctl restart -- docker" || *l == "systemctl restart -- docker.service"
        })
        .count()
}

#[test]
fn executor_guarded_restart_says_why_then_a_second_run_is_changed_0() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    set(d.path(), "state", "enabled");
    set(d.path(), "active", "");
    let (r1, out1) = go(&pb_restart(), d.path(), &bin, true);
    assert_eq!(
        r1.hosts[0].status,
        HostStatus::Changed,
        "{}\n{out1}",
        r1.render()
    );
    let path = d.path().join("daemon.json").display().to_string();
    assert!(
        out1.contains(&format!(
            "== h1 [prepare] svc docker.service: will restart if file {path} changes =="
        )),
        "{out1}"
    );
    assert_eq!(restarts(d.path()), 1, "{}", log(d.path()));
    let (r2, out2) = go(&pb_restart(), d.path(), &bin, true);
    assert_eq!(
        r2.hosts[0].status,
        HostStatus::Ok,
        "{}\n{out2}",
        r2.render()
    );
    assert!(r2.hosts[0].ops.iter().all(|o| !o.changed));
    let table = r2.render();
    let row = table.lines().find(|l| l.starts_with("h1")).expect(&table);
    let cols: Vec<&str> = row.split_whitespace().collect();
    assert_eq!(cols[3], "0", "CHANGED column: {table}");
    assert_eq!(restarts(d.path()), 1, "{}", log(d.path()));
    let restart = r2.hosts[0]
        .ops
        .iter()
        .find(|o| o.desc.contains("restart"))
        .expect("the restart op");
    assert_eq!(restart.outcome, telepatch::report::Outcome::Skipped);
}

#[test]
fn executor_restart_of_a_unit_the_run_starts_is_skipped() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            {
                let _r = h.role_enter("docker");
                h.svc("docker").enabled().started();
            }
            let _r = h.role_enter("nvidia");
            let daemon = h.file(&conf(&h, "daemon.json"));
            daemon.content("{}\n");
            h.svc("docker").restart().when(daemon.changed());
            Ok(())
        })
    }
    let pb = Playbook::new("svc", run_);
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let (r1, out1) = go(&pb, d.path(), &bin, true);
    assert_eq!(r1.exit_code(), 0, "{}\n{out1}", r1.render());
    let l = log(d.path());
    assert!(l.contains("systemctl start -- docker\n"), "{l}");
    assert_eq!(
        restarts(d.path()),
        0,
        "the start already runs the new config: {l}"
    );
    let restart = r1.hosts[0]
        .ops
        .iter()
        .find(|o| o.desc.contains("restart"))
        .expect("the restart op");
    assert_eq!(
        restart.outcome,
        telepatch::report::Outcome::Skipped,
        "{out1}"
    );
    // The unit runs now; the config changes: this time it restarts.
    std::fs::write(d.path().join("daemon.json"), "old\n").unwrap();
    let (r2, out2) = go(&pb, d.path(), &bin, true);
    assert_eq!(r2.exit_code(), 0, "{}\n{out2}", r2.render());
    assert_eq!(restarts(d.path()), 1, "{}", log(d.path()));
}

/// A file changes; `enabled().started()` then `restart_on(&file)`.
fn pb_enable_start_then_restart() -> Playbook<PathBuf> {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            let daemon = h.file(&conf(&h, "daemon.json")).content("{}\n");
            h.svc("docker").enabled().started();
            h.svc("docker").restart_on(&daemon);
            Ok(())
        })
    }
    Playbook::new("svc", run_)
}

fn changed_column(r: &Report) -> String {
    let table = r.render();
    let row = table.lines().find(|l| l.starts_with("h1")).expect(&table);
    row.split_whitespace().nth(3).unwrap().to_string()
}

#[test]
fn executor_a_start_that_only_enabled_a_running_unit_does_not_drop_the_restart() {
    // active and disabled: the start op enables (changed) but starts nothing
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    set(d.path(), "active", "");
    let pb = pb_enable_start_then_restart();
    let (r1, out1) = go(&pb, d.path(), &bin, true);
    assert_eq!(r1.exit_code(), 0, "{}\n{out1}", r1.render());
    let l = log(d.path());
    assert!(l.contains("systemctl enable -- docker\n"), "{l}");
    assert!(!l.contains("systemctl start "), "{l}");
    assert_eq!(restarts(d.path()), 1, "the config changed: {l}\n{out1}");
    let restart = r1.hosts[0]
        .ops
        .iter()
        .find(|o| o.desc.contains("restart"))
        .expect("the restart op");
    assert_eq!(restart.outcome, telepatch::report::Outcome::Changed);
    let (r2, out2) = go(&pb, d.path(), &bin, true);
    assert_eq!(
        r2.hosts[0].status,
        HostStatus::Ok,
        "{}\n{out2}",
        r2.render()
    );
    assert_eq!(changed_column(&r2), "0", "{}", r2.render());
    assert_eq!(restarts(d.path()), 1, "{}", log(d.path()));
}

#[test]
fn executor_a_start_that_started_the_unit_drops_the_restart() {
    // inactive before: the start runs the new config; no restart
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let pb = pb_enable_start_then_restart();
    let (r1, out1) = go(&pb, d.path(), &bin, true);
    assert_eq!(r1.exit_code(), 0, "{}\n{out1}", r1.render());
    let l = log(d.path());
    assert!(l.contains("systemctl start -- docker\n"), "{l}");
    assert_eq!(restarts(d.path()), 0, "{l}");
    let restart = r1.hosts[0]
        .ops
        .iter()
        .find(|o| o.desc.contains("restart"))
        .expect("the restart op");
    assert_eq!(restart.outcome, telepatch::report::Outcome::Skipped);
    let (r2, out2) = go(&pb, d.path(), &bin, true);
    assert_eq!(
        r2.hosts[0].status,
        HostStatus::Ok,
        "{}\n{out2}",
        r2.render()
    );
    assert_eq!(changed_column(&r2), "0", "{}", r2.render());
    assert_eq!(restarts(d.path()), 0, "{}", log(d.path()));
}

#[test]
fn executor_guarded_restarts_of_one_unit_merge_across_roles() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            {
                let _r = h.role_enter("a");
                let f = h.file(&conf(&h, "a.conf"));
                f.content("a\n");
                h.svc("docker").restart().when(f.changed());
            }
            let _r = h.role_enter("b");
            let f = h.file(&conf(&h, "b.conf"));
            f.content("b\n");
            h.svc("docker.service").restart_on(&f);
            Ok(())
        })
    }
    let pb = Playbook::new("svc", run_);
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    set(d.path(), "active", "");
    let (r1, out1) = go(&pb, d.path(), &bin, true);
    assert_eq!(r1.exit_code(), 0, "{}\n{out1}", r1.render());
    assert_eq!(restarts(d.path()), 1, "{}", log(d.path()));
    let a = d.path().join("a.conf").display().to_string();
    let b = d.path().join("b.conf").display().to_string();
    assert_eq!(
        out1.matches(&format!(
            "== h1 [prepare] svc docker.service: will restart if file {a} changes or file {b} changes =="
        ))
        .count(),
        1,
        "{out1}"
    );
    let ops: Vec<_> = r1.hosts[0]
        .ops
        .iter()
        .filter(|o| o.desc.contains("restart"))
        .collect();
    assert_eq!(ops.len(), 1, "{ops:#?}");
    assert_eq!(ops[0].members.len(), 2);
}

#[test]
fn executor_allowed_unguarded_restart_runs_every_run_and_says_so() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.svc("flaky-agent").restart().allow(
                telepatch::Policy::UnguardedRestart,
                "leaks memory; the vendor says restart daily",
            );
            Ok(())
        })
    }
    let pb = Playbook::new("svc", run_);
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    for n in 1..=2 {
        let (r, out) = go(&pb, d.path(), &bin, true);
        assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
        assert!(
            out.contains(
                "== h1 [prepare] svc flaky-agent.service: will restart, every run; allowed unguarded-restart: leaks memory; the vendor says restart daily =="
            ),
            "{out}"
        );
        assert_eq!(
            log(d.path())
                .matches("systemctl restart -- flaky-agent\n")
                .count(),
            n
        );
    }
}

// --- state and restart on one chain (T77b) ---

/// The state ops and the action ops of a host's trace, as (wants state,
/// action, guard) per svc node in trace order.
fn svc_nodes(h: &Host<()>) -> Vec<(bool, Option<&'static str>, Option<telepatch::Guard>)> {
    h.snapshot()
        .nodes
        .iter()
        .filter_map(|n| match n {
            Node::Op(op) => match &op.spec {
                telepatch::OpSpec::Svc(s) => Some((
                    s.enabled || s.started || s.disabled || s.masked || s.stopped,
                    s.action().map(|a| a.name()),
                    op.guard.clone(),
                )),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[test]
fn the_decided_chain_traces_as_two_ops() {
    let h = host();
    let unit = h
        .unit("minikube.service")
        .content("[Service]\nExecStart=/bin/true\n");
    let r = h.svc("minikube").enabled().started().restart_on(&unit);
    assert!(errors_of(&h).is_empty(), "{:?}", errors_of(&h));
    assert_eq!(
        svc_nodes(&h),
        [
            (true, None, None),
            (false, Some("restart"), Some(unit.changed())),
        ]
    );
    // the chain's handle names the action op from the action on
    let last = h.snapshot().nodes.len() - 1;
    assert_eq!(r.id().0, last);
    assert_eq!(r.changed(), telepatch::Guard::Changed(r.id()));
}

#[test]
fn restart_and_reload_when_on_a_chain_guard_only_the_action() {
    let h = host();
    let f = h.file("/etc/x").content("x");
    h.svc("docker")
        .enabled()
        .started()
        .restart()
        .when(f.changed());
    h.svc("ssh").started().reload().when(f.ok());
    assert!(errors_of(&h).is_empty(), "{:?}", errors_of(&h));
    assert_eq!(
        svc_nodes(&h),
        [
            (true, None, None),
            (false, Some("restart"), Some(f.changed())),
            (true, None, None),
            (false, Some("reload"), Some(f.ok())),
        ]
    );
}

/// The chained form and the two-call form give the same plan.
#[test]
fn the_chain_and_the_two_calls_give_the_same_plan() {
    let chain = host();
    let u = chain
        .unit("minikube.service")
        .content("[Service]\nExecStart=/bin/true\n");
    chain.svc("minikube").enabled().started().restart_on(&u);
    let f = chain.file("/etc/x").content("x");
    chain.svc("sshd").started().reload().when(f.changed());

    let two = host();
    let u = two
        .unit("minikube.service")
        .content("[Service]\nExecStart=/bin/true\n");
    two.svc("minikube").enabled().started();
    two.svc("minikube").restart_on(&u);
    let f = two.file("/etc/x").content("x");
    two.svc("sshd").started();
    two.svc("sshd").reload().when(f.changed());

    let a = render_host("t", &chain.os, &chain.snapshot());
    let b = render_host("t", &two.os, &two.snapshot());
    assert_eq!(a, b);
    assert!(a.contains("restart"), "{a}");
}

#[test]
fn a_bare_restart_on_a_chain_is_still_unguarded_restart() {
    fn run_(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            h.svc("docker").enabled().started().restart();
            Ok(())
        })
    }
    let e = trace_error(run_);
    assert!(
        e.contains("svc docker.service: .restart() has no guard (policy: unguarded-restart)"),
        "{e}"
    );
    let h = host();
    h.svc("flaky-agent").enabled().started().restart().allow(
        telepatch::Policy::UnguardedRestart,
        "leaks memory; the vendor says restart daily",
    );
    assert!(errors_of(&h).is_empty(), "{:?}", errors_of(&h));
}

#[test]
fn restart_with_reload_on_one_chain_stays_a_trace_error() {
    let h = host();
    let f = h.file("/etc/x").content("x");
    h.svc("ssh").started().restart().reload().when(f.changed());
    let errs: Vec<String> = h
        .snapshot()
        .errors
        .iter()
        .map(|e| e.message().to_string())
        .collect();
    assert_eq!(errs, ["svc(ssh): .restart() and .reload() are exclusive"]);
}

/// `.when(g)` guards the op the handle names when it is called: before the
/// action that is the state op, and the action that follows has no guard.
#[test]
fn when_before_the_action_guards_the_state_op() {
    let h = host();
    let f = h.file("/etc/x").content("x");
    h.svc("docker").started().when(f.ok()).restart_on(&f);
    assert!(errors_of(&h).is_empty(), "{:?}", errors_of(&h));
    assert_eq!(
        svc_nodes(&h),
        [
            (true, None, Some(f.ok())),
            (false, Some("restart"), Some(f.changed())),
        ]
    );
    let h = host();
    let f = h.file("/etc/x").content("x");
    h.svc("docker").started().when(f.changed()).restart();
    let errs = errors_of(&h);
    assert!(
        errs.iter()
            .any(|e| e.contains(".restart() has no guard (policy: unguarded-restart)")),
        "{errs:?}"
    );
}

#[test]
fn a_state_after_the_action_is_a_trace_error() {
    let h = host();
    let f = h.file("/etc/x").content("x");
    h.svc("docker").restart_on(&f).started();
    let errs: Vec<String> = h
        .snapshot()
        .errors
        .iter()
        .map(|e| e.message().to_string())
        .collect();
    assert_eq!(
        errs,
        ["svc(docker): .started() after .restart(): a chain names its states before the action"]
    );
}

/// `enabled().started().restart_on(&daemon)` in one chain.
fn pb_chain() -> Playbook<PathBuf> {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            let daemon = h.file(&conf(&h, "daemon.json")).content("{}\n");
            h.svc("docker").enabled().started().restart_on(&daemon);
            Ok(())
        })
    }
    Playbook::new("svc", run_)
}

#[test]
fn executor_chain_inactive_unit_is_started_and_not_restarted() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let (r1, out1) = go(&pb_chain(), d.path(), &bin, true);
    assert_eq!(r1.exit_code(), 0, "{}\n{out1}", r1.render());
    let l = log(d.path());
    assert!(l.contains("systemctl start -- docker\n"), "{l}");
    assert_eq!(restarts(d.path()), 0, "{l}");
    let (r2, out2) = go(&pb_chain(), d.path(), &bin, true);
    assert_eq!(
        r2.hosts[0].status,
        HostStatus::Ok,
        "{}\n{out2}",
        r2.render()
    );
    assert_eq!(changed_column(&r2), "0", "{}", r2.render());
    assert_eq!(restarts(d.path()), 0, "{}", log(d.path()));
}

#[test]
fn executor_chain_active_unit_with_a_changed_file_restarts_once() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    set(d.path(), "state", "enabled");
    set(d.path(), "active", "");
    let (r1, out1) = go(&pb_chain(), d.path(), &bin, true);
    assert_eq!(r1.exit_code(), 0, "{}\n{out1}", r1.render());
    assert_eq!(restarts(d.path()), 1, "{}\n{out1}", log(d.path()));
    let (r2, out2) = go(&pb_chain(), d.path(), &bin, true);
    assert_eq!(
        r2.hosts[0].status,
        HostStatus::Ok,
        "{}\n{out2}",
        r2.render()
    );
    assert_eq!(changed_column(&r2), "0", "{}", r2.render());
    assert_eq!(restarts(d.path()), 1, "{}", log(d.path()));
}

/// The chain and the two calls print the same prepare lines and report.
#[test]
fn executor_chain_and_two_calls_print_the_same_run() {
    let run_both = |pb: Playbook<PathBuf>| {
        let d = tempfile::tempdir().unwrap();
        let bin = stubs(d.path());
        set(d.path(), "active", "");
        let (r, out) = go(&pb, d.path(), &bin, true);
        let dir = d.path().display().to_string();
        (r.render().replace(&dir, "D"), out.replace(&dir, "D"))
    };
    let a = run_both(pb_chain());
    let b = run_both(pb_enable_start_then_restart());
    assert_eq!(a.1, b.1);
    assert_eq!(a.0, b.0);
}

/// A chained restart in one role and a plain restart of the same unit in
/// another merge into one restart, as two plain ones do.
#[test]
fn executor_a_chained_restart_merges_with_another_roles_restart() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            {
                let _r = h.role_enter("a");
                let f = h.file(&conf(&h, "a.conf")).content("a\n");
                h.svc("docker").enabled().started().restart_on(&f);
            }
            let _r = h.role_enter("b");
            let f = h.file(&conf(&h, "b.conf")).content("b\n");
            h.svc("docker.service").restart_on(&f);
            Ok(())
        })
    }
    let pb = Playbook::new("svc", run_);
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    set(d.path(), "state", "enabled");
    set(d.path(), "active", "");
    let (r1, out1) = go(&pb, d.path(), &bin, true);
    assert_eq!(r1.exit_code(), 0, "{}\n{out1}", r1.render());
    assert_eq!(restarts(d.path()), 1, "{}\n{out1}", log(d.path()));
    let n = r1.hosts[0]
        .ops
        .iter()
        .filter(|o| o.desc.contains("restart"))
        .count();
    assert_eq!(n, 1, "{}", r1.render());
}

/// A bound handle keeps naming the op it was bound to.
#[test]
fn a_bound_state_handle_keeps_naming_the_state_op() {
    let h = host();
    let f = h.file("/etc/x").content("x");
    let s = h.svc("docker").enabled().started();
    let r = s.restart_on(&f);
    assert_ne!(s.id(), r.id());
    assert_eq!(s.changed(), telepatch::Guard::Changed(s.id()));
    assert_eq!(r.ok(), telepatch::Guard::ExitOk(r.id()));
    assert!(errors_of(&h).is_empty(), "{:?}", errors_of(&h));
}
