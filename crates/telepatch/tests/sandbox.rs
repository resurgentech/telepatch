//! T124: no test touches the real stage, cache, run dir or a real
//! filesystem root. `.cargo/config.toml` sets `TP_SANDBOX_REQUIRED=1` for
//! every process cargo runs; a shell that has it refuses every real path
//! that does not lie under `$TP_SANDBOX`.
//!
//! These tests read the real paths (never write them) to prove that a
//! refused run left them as they were.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::ops::undo::{HELPERS, sweep_script};
use telepatch::testing::sandbox;
use telepatch::transport::local::LocalTransport;
use telepatch::transport::ssh::SshTransport;
use telepatch::{Host, Playbook, PlaybookFuture};

const REFUSAL: &str = "telepatch: sandbox: refusing /var/tmp/telepatch (TP_SANDBOX_REQUIRED is set; tests must use telepatch::testing::sandbox)";

/// Names and mtimes of the real paths, and the `.telepatch` of the real
/// roots: read-only.
fn real_state() -> String {
    let mut s = String::new();
    for d in [
        "/var/tmp/telepatch",
        "/var/cache/telepatch",
        "/run/telepatch",
    ] {
        let o = Command::new("find")
            .args([d, "-printf", "%p %T@\\n"])
            .output()
            .unwrap();
        s.push_str(&String::from_utf8_lossy(&o.stdout));
    }
    for r in ["/", "/tmp", "/home", "/var", "/var/tmp"] {
        s.push_str(&format!(
            "{r}: {}\n",
            Path::new(r).join(".telepatch").exists()
        ));
    }
    s
}

fn pb(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let p = h.vars.join("out.txt").display().to_string();
        h.file(&p).content("hello\n");
        Ok(())
    })
}

fn go(target: Target, dir: &Path) -> (telepatch::report::Report, String) {
    let out = Rc::new(RefCell::new(String::new()));
    let r = run(
        &Playbook::new("sandbox", pb),
        vec![HostSpec {
            groups: Vec::new(),
            name: "h1".into(),
            target,
            vars: dir.to_path_buf(),
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    let text = out.borrow().clone();
    (r, text)
}

/// A stub `ssh` that runs the remote command locally, as `tests/ssh_transport.rs`
/// does, reduced to what one root channel needs.
fn stub_ssh(dir: &Path) -> String {
    let p = dir.join("ssh");
    let body = r#"#!/bin/sh
master=; cp=; ctl=
while [ $# -gt 0 ]; do
  case "$1" in
    -T) shift ;;
    -N) master=1; shift ;;
    -i) shift 2 ;;
    -O) ctl="$2"; shift 2 ;;
    -o) case "$2" in ControlPath=*) [ -n "$cp" ] || cp="${2#ControlPath=}" ;; esac; shift 2 ;;
    -*) exit 2 ;;
    *) break ;;
  esac
done
shift
if [ -n "$ctl" ]; then [ -e "$cp.pid" ] && kill $(cat "$cp.pid") 2>/dev/null; rm -f "$cp" "$cp.pid"; exit 0; fi
if [ -n "$master" ]; then echo $$ > "$cp.pid"; : > "$cp"; exec sleep 100000; fi
[ -e "$cp" ] || exit 255
exec bash -c "exec $*"
"#;
    std::fs::write(&p, body).unwrap();
    let ok = Command::new("chmod").arg("755").arg(&p).status().unwrap();
    assert!(ok.success());
    // `sudo -n -H bash` of a root channel: run it as the current user.
    let sudo = dir.join("sudo");
    std::fs::write(
        &sudo,
        "#!/bin/sh\n[ \"$1\" = -n ] && shift; [ \"$1\" = -H ] && shift; exec \"$@\"\n",
    )
    .unwrap();
    assert!(
        Command::new("chmod")
            .arg("755")
            .arg(&sudo)
            .status()
            .unwrap()
            .success()
    );
    p.display().to_string()
}

fn text(r: &telepatch::report::Report, out: &str) -> String {
    format!("{}\n{out}", r.render())
}

#[test]
fn a_bare_local_transport_is_refused_and_creates_nothing() {
    let before = real_state();
    let d = tempfile::tempdir().unwrap();
    let t = LocalTransport::new().uid(0).env("TP_SANDBOX_REQUIRED", "1");
    let (r, out) = go(Target::Local(t), d.path());
    let all = text(&r, &out);
    assert_eq!(r.exit_code(), 1, "{all}");
    assert!(all.contains("telepatch: sandbox: refusing"), "{all}");
    assert!(
        all.contains("tests must use telepatch::testing::sandbox"),
        "{all}"
    );
    // Q-H2: a local run through `cargo run` is told what to do instead.
    assert!(all.contains("use the built binary"), "{all}");
    assert!(all.contains("unset TP_SANDBOX_REQUIRED"), "{all}");
    assert!(!d.path().join("out.txt").exists(), "{all}");
    assert_eq!(before, real_state());
}

#[test]
fn a_bare_ssh_stub_is_refused_and_creates_nothing() {
    let before = real_state();
    let d = tempfile::tempdir().unwrap();
    let ssh = stub_ssh(d.path());
    let path = format!("{}:{}", d.path().display(), std::env::var("PATH").unwrap());
    let t = SshTransport::new()
        .ssh(&ssh)
        .env("PATH", &path)
        .env("TP_SANDBOX_REQUIRED", "1");
    let (r, out) = go(Target::Ssh(t), d.path());
    let all = text(&r, &out);
    assert_eq!(r.exit_code(), 1, "{all}");
    assert!(all.contains("telepatch: sandbox: refusing"), "{all}");
    assert!(!d.path().join("out.txt").exists(), "{all}");
    assert_eq!(before, real_state());
}

#[test]
fn the_same_file_op_through_sandboxed_passes() {
    let sb = sandbox();
    let t = LocalTransport::new().uid(0).sandboxed(&sb);
    let (r, out) = go(Target::Local(t), sb.path());
    assert_eq!(r.exit_code(), 0, "{}", text(&r, &out));
    assert_eq!(
        std::fs::read_to_string(sb.path().join("out.txt")).unwrap(),
        "hello\n"
    );

    let d = tempfile::tempdir().unwrap();
    let ssh = stub_ssh(d.path());
    let path = format!("{}:{}", d.path().display(), std::env::var("PATH").unwrap());
    let t = SshTransport::new()
        .ssh(&ssh)
        .env("PATH", &path)
        .sandboxed(&sb);
    let (r, out) = go(Target::Ssh(t), sb.path());
    assert_eq!(r.exit_code(), 0, "{}", text(&r, &out));
}

#[test]
fn a_stage_outside_the_sandbox_fails_the_shell_open() {
    let sb = sandbox();
    let other = tempfile::tempdir().unwrap();
    let t = LocalTransport::new()
        .uid(0)
        .sandboxed(&sb)
        .env("TP_FETCH_STAGE", &other.path().display().to_string());
    let (r, out) = go(Target::Local(t), sb.path());
    let all = text(&r, &out);
    assert_eq!(r.exit_code(), 1, "{all}");
    assert!(
        all.contains(&format!("sandbox: refusing {}", other.path().display())),
        "{all}"
    );
    assert!(!sb.path().join("out.txt").exists(), "{all}");
}

#[test]
fn tp_fsroot_at_a_real_root_refuses_to_create_dot_telepatch() {
    let sb = sandbox();
    // A root outside the sandbox, named by the seam so the uid guard lets
    // the chain through; only the switch can stop it.
    let root = tempfile::tempdir().unwrap();
    let script = format!("{HELPERS}tp_id=1\ntp_undo {}/x\n", root.path().display());
    let o = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("TP_SANDBOX_REQUIRED", "1")
        .env("TP_SANDBOX", sb.path())
        .env("TP_FETCH_STAGE", &sb.stage)
        .env("TP_FS_ROOTS", root.path())
        .env("__tp_run", "r1")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success(), "{err}");
    assert!(
        err.contains(&format!(
            "telepatch: sandbox: refusing {}/.telepatch (TP_SANDBOX_REQUIRED is set; tests must use telepatch::testing::sandbox)",
            root.path().display()
        )),
        "{err}"
    );
    assert!(!err.contains("not prepared"), "{err}");
    assert!(!root.path().join(".telepatch").exists());
}

#[test]
fn a_hand_run_tp_undo_without_a_run_still_works_under_the_switch() {
    let script = format!("{HELPERS}tp_id=1\ntp_undo /nonexistent/x && echo ok\n");
    let o = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .env("TP_SANDBOX_REQUIRED", "1")
        .env_remove("__tp_run")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stdout), "ok\n");
}

#[test]
fn the_stale_sweep_under_the_switch_never_lists_the_real_stage() {
    let before = real_state();
    let sb = sandbox();
    let o = Command::new("bash")
        .arg("-c")
        .arg(sweep_script())
        .env("TP_SANDBOX_REQUIRED", "1")
        .env("TP_SANDBOX", sb.path())
        .env_remove("TP_FETCH_STAGE")
        .env("__tp_run", "r1")
        .output()
        .unwrap();
    let (out, err) = (
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr),
    );
    assert!(err.contains(REFUSAL), "{err}");
    assert_eq!(out, "", "{err}");
    assert_eq!(before, real_state());
}
