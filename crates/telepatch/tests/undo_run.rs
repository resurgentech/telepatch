//! Inverse capture at the run level (T106, slice C): the root shell's
//! preparation op before a user's run, the stale-run sweep at a host's
//! start, and cleanup on every way a host can end. A filesystem of its own
//! is the `TP_FS_ROOTS` seam; the stage is `TP_FETCH_STAGE` inside a temp
//! dir. Nothing here mounts, and nothing is written outside a temp dir.
//!
//! Real user principals need the stub `ssh` of `tests/executor_ssh.rs`
//! (copied below); the fake-root local transport of `tests/login_shell.rs`
//! covers `as_user("root")`.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

use telepatch::executor::{
    CheckOpts, HostSpec, RebootWait, RunControls, RunOpts, Stdout, Target, run_with,
};
use telepatch::report::{HostStatus, Report};
use telepatch::transport::local::LocalTransport;
use telepatch::transport::ssh::SshTransport;
use telepatch::{Host, Playbook, PlaybookFuture, sh};

fn write_exe(p: &Path, body: &str) {
    use std::io::Write;
    let mut c = Command::new("sh")
        .arg("-c")
        .arg("cat > \"$1\" && chmod 755 \"$1\"")
        .arg("sh")
        .arg(p)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin
        .take()
        .unwrap()
        .write_all(format!("#!/bin/sh\n{body}\n").as_bytes())
        .unwrap();
    assert!(c.wait().unwrap().success());
}

const SSH: &str = r#"
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
echo $$ >> "$d/pids"
if [ -n "$master" ]; then
  # A host that is down: the next TP_STUB_DOWN master starts are refused.
  if [ -e "$d/down" ] && [ "$(cat "$d/down")" -gt 0 ]; then
    echo $(( $(cat "$d/down") - 1 )) > "$d/down"
    echo "ssh: connect to host h port 22: Connection refused" >&2
    exit 255
  fi
  echo $$ > "$cp.pid"
  : > "$cp"
  exec sleep 100000
fi
[ -e "$cp" ] || { echo "stub ssh: no master at $cp" >&2; exit 255; }
if [ -n "$TP_STUB_MAX_CHAN" ]; then
  echo $$ >> "$d/nchan"
  [ "$(wc -l < "$d/nchan")" -le "$TP_STUB_MAX_CHAN" ] || { echo "stub ssh: session request failed" >&2; exit 255; }
fi
export TP_STUB_CHAN=$$
log="$d/chan-$$.in"
printf '# remote: %s\n' "$*" > "$log"
mkfifo "$log.fifo"
tee -a "$log" <&0 >"$log.fifo" 2>/dev/null &
exec bash -c "exec $*" <"$log.fifo"
"#;

/// The first `id -u` of a channel (the transport's, to decide on sudo)
/// answers `TP_STUB_UID`; later ones tell the truth, so the files the
/// channel makes are owned by the uid it reports, as on a real host.
const ID: &str = r#"
if [ "$1" = -u ]; then
  c="$TP_STUB_DIR/idchan.${TP_STUB_CHAN:-0}"
  if [ ! -e "$c" ]; then : > "$c"; echo "${TP_STUB_UID:-1000}"; exit 0; fi
  exec /usr/bin/id -u
fi
exec /usr/bin/id "$@"
"#;

const SUDO: &str = r#"
printf '%s\n' "$*" >> "$TP_STUB_DIR/sudo.log"
[ "$1" = -n ] || exit 2
shift
if [ "$1" = -H ]; then shift; HOME=/root; export HOME; fi
if [ "$1" = -iu ]; then
  [ -z "$TP_STUB_NO_USER" ] || { echo "sudo: unknown user $2" >&2; exit 1; }
  shift 2
  exec "$@"
fi
TP_STUB_UID=0 exec "$@"
"#;

/// `systemctl reboot` of a fake host: records the call, takes a new boot id
/// (the file `TP_BOOT_ID` names, unless `TP_STUB_SAME_BOOT`), loses `/run`
/// (`TP_RUN_DIR`, the mark copied to `mark.at-reboot` first), goes down for
/// `TP_STUB_DOWN` connection attempts, and ends every ssh process the stub
/// has started, the one running this command's shell included. Any other
/// `systemctl` call is recorded and does nothing.
const SYSTEMCTL: &str = r#"
printf '%s\n' "$*" >> "$TP_STUB_DIR/systemctl.log"
[ "$1" = reboot ] || exit 0
[ -z "$TP_STUB_REBOOT_FAIL" ] || { echo "Failed to reboot: Access denied" >&2; exit 1; }
[ ! -e "$TP_RUN_DIR/reboot-pending" ] || cp "$TP_RUN_DIR/reboot-pending" "$TP_STUB_DIR/mark.at-reboot"
[ -n "$TP_STUB_SAME_BOOT" ] || printf 'boot-%s\n' "$$" > "$TP_BOOT_ID"
rm -r -f "$TP_RUN_DIR"
echo "${TP_STUB_DOWN:-2}" > "$TP_STUB_DIR/down"
cat "$TP_STUB_DIR/pids" >> "$TP_STUB_DIR/pids.dead"
dying=$(cat "$TP_STUB_DIR/pids")
: > "$TP_STUB_DIR/pids"
kill -9 $dying 2>/dev/null
exit 0
"#;

struct Stub {
    dir: tempfile::TempDir,
    ssh: String,
    path: String,
}

impl Stub {
    fn new() -> Stub {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let ssh = dir.path().join("ssh");
        write_exe(&ssh, SSH);
        write_exe(&bin.join("id"), ID);
        write_exe(&bin.join("sudo"), SUDO);
        write_exe(&bin.join("systemctl"), SYSTEMCTL);
        std::fs::write(dir.path().join("boot_id"), "boot-1\n").unwrap();
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
        Stub {
            dir,
            ssh: ssh.display().to_string(),
            path,
        }
    }

    /// The stub host whose stage and filesystem seam are inside `d`.
    fn target(&self, d: &Path, extra: &[(&str, &str)]) -> Target {
        let mut t = SshTransport::new()
            .sandboxed_at(d)
            .ssh(&self.ssh)
            .env("TP_STUB_DIR", &self.dir.path().display().to_string())
            .env("PATH", &self.path)
            .env(
                "TP_BOOT_ID",
                &self.dir.path().join("boot_id").display().to_string(),
            )
            .env("TP_RUN_DIR", &d.join("run").display().to_string())
            .env("TP_FETCH_STAGE", &d.join("stage").display().to_string())
            .env("TP_FS_ROOTS", &d.join("fsB").display().to_string());
        for (k, v) in extra {
            t = t.env(k, v);
        }
        Target::Ssh(t)
    }
}

#[derive(Clone)]
struct Vars {
    d: PathBuf,
    user: String,
}

fn me() -> String {
    let o = Command::new("id").arg("-un").output().unwrap();
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

fn dp(h: &Host<Vars>, n: &str) -> String {
    h.vars.d.join(n).display().to_string()
}

fn vars(d: &Path) -> Vars {
    Vars {
        d: d.to_path_buf(),
        user: me(),
    }
}

/// `<d>/fsB` is the other filesystem, holding two files to overwrite.
fn world(d: &Path) {
    std::fs::create_dir_all(d.join("fsB/etc")).unwrap();
    std::fs::write(d.join("fsB/etc/f.conf"), "old\n").unwrap();
    std::fs::write(d.join("fsB/etc/g.conf"), "oldg\n").unwrap();
}

fn local(d: &Path) -> Target {
    Target::Local(
        LocalTransport::new()
            .uid(0)
            .sandboxed_at(d)
            .env("TP_FETCH_STAGE", &d.join("stage").display().to_string())
            .env("TP_FS_ROOTS", &d.join("fsB").display().to_string()),
    )
}

fn host(name: &str, target: Target, v: Vars) -> HostSpec<Vars> {
    HostSpec {
        groups: Vec::new(),
        name: name.into(),
        target,
        vars: v,
    }
}

fn go(
    pb: &Playbook<Vars>,
    hosts: Vec<HostSpec<Vars>>,
    check: bool,
    ctl: RunControls,
    log: Option<&Path>,
) -> (Report, String) {
    let out = Rc::new(RefCell::new(String::new()));
    let r = run_with(
        pb,
        hosts,
        RunOpts {
            log: log.map(Path::to_path_buf),
            stdout: Stdout::Capture(out.clone()),
        },
        check.then(CheckOpts::default),
        ctl,
    );
    let s = out.borrow().clone();
    (r, s)
}

fn go1(pb: &Playbook<Vars>, target: Target, d: &Path) -> (Report, String) {
    go(
        pb,
        vec![host("h1", target, vars(d))],
        false,
        RunControls::default(),
        None,
    )
}

fn events(log: &Path, ev: &str) -> usize {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains(&format!("\"ev\":\"{ev}\"")))
        .count()
}

fn names(p: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(p)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn read(d: &Path, n: &str) -> String {
    std::fs::read_to_string(d.join(n)).unwrap_or_default()
}

/// Nothing of the run is left: no run directory in the stage, no
/// `.telepatch` under the other filesystem.
fn nothing_left(d: &Path) {
    assert_eq!(names(&d.join("stage")), Vec::<String>::new(), "stage");
    assert!(!d.join("fsB/.telepatch").exists(), "fsB/.telepatch left");
}

// ---- the root shell's preparation op ----------------------------------------

fn prep_pb(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move {
        let u = h.as_user(&h.vars.user);
        let (pre, post) = (dp(&h, "pre"), dp(&h, "post"));
        let (new, old) = (dp(&h, "fsB/etc/new.conf"), dp(&h, "fsB/etc/f.conf"));
        // A new file saves nothing: whatever is under `fsB/.telepatch`
        // afterwards is the preparation op's.
        u.file(&new).content("x\n");
        sh!(
            u,
            "S=\"$TP_FETCH_STAGE/$__tp_run\"; R=\"$TP_FS_ROOTS/.telepatch/tmp/$__tp_run\"; stat -c '%a %U' \"$S/undo\" \"$S/undo/$(id -un)\" \"$R/undo\" \"$R/undo/$(id -un)\" > {pre}; cat \"$S/fsroots\" >> {pre} 2>&1; true"
        );
        u.file(&old).content("new\n");
        sh!(
            u,
            "find -L \"$TP_FETCH_STAGE/$__tp_run/undo\" -name old > {post}"
        );
        Ok(())
    })
}

#[test]
fn the_root_shell_prepares_the_per_filesystem_dirs_before_a_users_run() {
    let d = tempfile::tempdir().unwrap();
    world(d.path());
    let stub = Stub::new();
    let log = d.path().join("log.jsonl");
    let pb = Playbook::new("prep", prep_pb);
    let (r, out) = go(
        &pb,
        vec![host("h1", stub.target(d.path(), &[]), vars(d.path()))],
        false,
        RunControls::default(),
        Some(&log),
    );
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    let (u, fsb) = (me(), d.path().join("fsB").display().to_string());
    // The user's own run finds the dirs the root shell made: modes, owner,
    // and the index of roots.
    assert_eq!(
        read(d.path(), "pre"),
        format!("711 {u}\n700 {u}\n711 {u}\n700 {u}\n{fsb}\n"),
        "{out}"
    );
    // One preparation op per run of that user (the raw op ends a window).
    assert!(events(&log, "undo_prepare") >= 1, "{out}");
    // The op then captured, and the stage link points at the backup.
    let post = read(d.path(), "post");
    assert_eq!(post.lines().count(), 1, "{post}");
    assert_eq!(
        std::fs::read_to_string(d.path().join("fsB/etc/f.conf")).unwrap(),
        "new\n"
    );
    nothing_left(d.path());
}

fn raw_only_pb(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move {
        let u = h.as_user(&h.vars.user);
        let o = dp(&h, "o");
        sh!(u, "echo hi > {o}");
        Ok(())
    })
}

#[test]
fn a_user_run_with_no_capturing_op_gets_no_preparation_op() {
    let d = tempfile::tempdir().unwrap();
    world(d.path());
    let stub = Stub::new();
    let log = d.path().join("log.jsonl");
    let pb = Playbook::new("raw", raw_only_pb);
    let (r, out) = go(
        &pb,
        vec![host("h1", stub.target(d.path(), &[]), vars(d.path()))],
        false,
        RunControls::default(),
        Some(&log),
    );
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert_eq!(events(&log, "undo_prepare"), 0, "{out}");
    assert!(!d.path().join("stage").exists());
}

fn twice_pb(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move {
        let u = h.as_user(&h.vars.user);
        let (f, g, roots) = (
            dp(&h, "fsB/etc/f.conf"),
            dp(&h, "fsB/etc/g.conf"),
            dp(&h, "roots"),
        );
        u.file(&f).content("new\n");
        // A root op between: the user's second run is a run of its own.
        sh!(h, "cat \"$TP_FETCH_STAGE/$__tp_run/fsroots\" > {roots}");
        u.file(&g).content("newg\n");
        Ok(())
    })
}

#[test]
fn preparation_is_idempotent_one_root_one_index_line() {
    let d = tempfile::tempdir().unwrap();
    world(d.path());
    let stub = Stub::new();
    let log = d.path().join("log.jsonl");
    let pb = Playbook::new("twice", twice_pb);
    let (r, out) = go(
        &pb,
        vec![host("h1", stub.target(d.path(), &[]), vars(d.path()))],
        false,
        RunControls::default(),
        Some(&log),
    );
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert_eq!(events(&log, "undo_prepare"), 2, "{out}");
    let fsb = d.path().join("fsB").display().to_string();
    assert_eq!(read(d.path(), "roots"), format!("{fsb}\n"));
    nothing_left(d.path());
}

// ---- as_user("root") and a root op, the fake-root local transport ------------

fn write_login_rig(d: &Path) -> String {
    let (home, bin) = (d.join("home"), d.join("bin"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    write_exe(
        &bin.join("getent"),
        &format!(
            "[ \"$1 $2\" = \"passwd root\" ] || exit 2\necho 'root:x:0:0::{}:/bin/bash'",
            home.display()
        ),
    );
    // `env -i` leaves the login shell none of the root shell's variables.
    std::fs::write(
        home.join(".bash_profile"),
        format!(
            "export TP_FETCH_STAGE={}\nexport TP_FS_ROOTS={}\n",
            d.join("stage").display(),
            d.join("fsB").display()
        ),
    )
    .unwrap();
    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap())
}

fn login_pb(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move {
        let u = h.as_user("root");
        let (f, g, seen) = (
            dp(&h, "fsB/etc/f.conf"),
            dp(&h, "fsB/etc/g.conf"),
            dp(&h, "seen"),
        );
        h.file(&f).content("new\n");
        u.file(&g).content("newg\n");
        sh!(
            h,
            "ls \"$TP_FETCH_STAGE/$__tp_run/undo\" > {seen}; find -L \"$TP_FETCH_STAGE/$__tp_run/undo\" -name old >> {seen}"
        );
        Ok(())
    })
}

#[test]
fn an_as_user_root_op_and_a_root_op_both_capture_under_the_seam() {
    let d = tempfile::tempdir().unwrap();
    world(d.path());
    let path = write_login_rig(d.path());
    let log = d.path().join("log.jsonl");
    let target = Target::Local(
        LocalTransport::new()
            .uid(0)
            .sandboxed_at(d.path())
            .env("PATH", &path)
            .env(
                "TP_FETCH_STAGE",
                &d.path().join("stage").display().to_string(),
            )
            .env("TP_FS_ROOTS", &d.path().join("fsB").display().to_string()),
    );
    let pb = Playbook::new("login", login_pb);
    let (r, out) = go(
        &pb,
        vec![host("h1", target, vars(d.path()))],
        false,
        RunControls::default(),
        Some(&log),
    );
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    let seen = read(d.path(), "seen");
    // The preparation op made `undo/root`, the login user's directory.
    assert!(seen.lines().any(|l| l == "root"), "{seen}");
    assert_eq!(seen.matches("/old").count(), 2, "{seen}");
    assert_eq!(events(&log, "undo_prepare"), 1, "{out}");
    for (f, v) in [("f.conf", "new\n"), ("g.conf", "newg\n")] {
        assert_eq!(
            std::fs::read_to_string(d.path().join("fsB/etc").join(f)).unwrap(),
            v
        );
    }
    nothing_left(d.path());
}

// ---- the stale sweep --------------------------------------------------------

/// A run directory a dead controller left: an undo dir, the index and the
/// per-filesystem directory it names, with one backup linked from the stage.
fn dead_run(d: &Path, name: &str) {
    let (stage, fsb) = (d.join("stage"), d.join("fsB"));
    let t = fsb.join(".telepatch/tmp").join(name).join("undo/x/1");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(t.join("old"), "OLD-SECRET\n").unwrap();
    let s = stage.join(name);
    std::fs::create_dir_all(s.join("undo/x")).unwrap();
    std::os::unix::fs::symlink(&t, s.join("undo/x/1")).unwrap();
    std::fs::write(s.join("fsroots"), format!("{}\n", fsb.display())).unwrap();
}

fn age(p: &Path, spec: &str) {
    let ok = Command::new("touch")
        .arg("-d")
        .arg(spec)
        .arg(p)
        .status()
        .unwrap()
        .success();
    assert!(ok);
}

fn overwrite_pb(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move {
        h.file(&dp(&h, "fsB/etc/f.conf")).content("new\n");
        Ok(())
    })
}

/// Four directories in the stage: a dead run (48 h), a younger one (1 h),
/// a directory holding neither `undo` nor `fsroots` (48 h) and one like
/// the real stage's `testrun` (5 days).
fn stale_world(d: &Path) {
    world(d);
    let stage = d.join("stage");
    dead_run(d, "oldrun");
    dead_run(d, "youngrun");
    for n in ["foreign", "testrun"] {
        std::fs::create_dir_all(stage.join(n)).unwrap();
        std::fs::write(stage.join(n).join("data.txt"), "keep\n").unwrap();
    }
    age(&stage.join("oldrun"), "48 hours ago");
    age(&stage.join("youngrun"), "1 hour ago");
    age(&stage.join("foreign"), "48 hours ago");
    age(&stage.join("testrun"), "5 days ago");
}

#[test]
fn a_run_older_than_a_day_is_swept_a_younger_one_warned_and_foreign_data_kept() {
    let d = tempfile::tempdir().unwrap();
    stale_world(d.path());
    let pb = Playbook::new("stale", overwrite_pb);
    let (r, out) = go1(&pb, local(d.path()), d.path());
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    let (stage, fsb) = (d.path().join("stage"), d.path().join("fsB"));
    // The old one: stage dir, link and the per-fs dir it indexed are gone.
    assert!(!stage.join("oldrun").exists(), "{out}");
    assert!(!fsb.join(".telepatch/tmp/oldrun").exists(), "{out}");
    assert!(
        out.contains("undo data from run oldrun removed (older than 24 h)"),
        "{out}"
    );
    // The younger: left, with its data, and a warning.
    assert!(stage.join("youngrun/fsroots").exists(), "{out}");
    assert!(fsb.join(".telepatch/tmp/youngrun/undo/x/1/old").exists());
    assert!(
        out.contains(
            "undo data from run youngrun, less than 24 h old, left: another controller may be running"
        ),
        "{out}"
    );
    // Not telepatch runs: untouched, and never named.
    for n in ["foreign", "testrun"] {
        assert_eq!(
            std::fs::read_to_string(stage.join(n).join("data.txt")).unwrap(),
            "keep\n",
            "{n}"
        );
        assert!(!out.contains(&format!("run {n}")), "{out}");
    }
    // This run's own directory is gone as before; the rest stays.
    assert_eq!(names(&stage), ["foreign", "testrun", "youngrun"]);
}

fn raw_local_pb(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move {
        let o = dp(&h, "o");
        sh!(h, "echo hi > {o}");
        Ok(())
    })
}

#[test]
fn no_sweep_without_a_capturing_op_and_none_under_check() {
    let d = tempfile::tempdir().unwrap();
    stale_world(d.path());
    // No capturing op: nothing swept.
    let pb = Playbook::new("raw", raw_local_pb);
    let (r, out) = go1(&pb, local(d.path()), d.path());
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert!(d.path().join("stage/oldrun/fsroots").exists(), "{out}");
    // A check applies nothing and sweeps nothing.
    let pb = Playbook::new("stale", overwrite_pb);
    let (r, out) = go(
        &pb,
        vec![host("h1", local(d.path()), vars(d.path()))],
        true,
        RunControls::default(),
        None,
    );
    assert!(r.exit_code() <= 2, "{}\n{out}", r.render());
    assert!(d.path().join("stage/oldrun/fsroots").exists(), "{out}");
    assert!(d.path().join("fsB/.telepatch/tmp/oldrun").exists(), "{out}");
    assert!(!out.contains("undo data from run"), "{out}");
}

// ---- --check captures nothing -----------------------------------------------

#[test]
fn a_check_makes_no_undo_dir_and_leaves_the_file() {
    let d = tempfile::tempdir().unwrap();
    world(d.path());
    let pb = Playbook::new("chk", overwrite_pb);
    let (r, out) = go(
        &pb,
        vec![host("h1", local(d.path()), vars(d.path()))],
        true,
        RunControls::default(),
        None,
    );
    assert_eq!(r.exit_code(), 2, "{}\n{out}", r.render());
    assert!(!d.path().join("stage").exists(), "{out}");
    assert!(!d.path().join("fsB/.telepatch").exists(), "{out}");
    assert_eq!(read(d.path(), "fsB/etc/f.conf"), "old\n");
}

// ---- cleanup on every way a host ends ---------------------------------------

fn failing_pb(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move {
        h.file(&dp(&h, "fsB/etc/f.conf")).content("new\n");
        sh!(h, "exit 3");
        h.file(&dp(&h, "fsB/etc/g.conf")).content("newg\n");
        Ok(())
    })
}

#[test]
fn a_failed_host_is_cleaned_like_a_good_one() {
    let d = tempfile::tempdir().unwrap();
    world(d.path());
    let pb = Playbook::new("fail", failing_pb);
    let (r, out) = go1(&pb, local(d.path()), d.path());
    assert!(
        matches!(r.hosts[0].status, HostStatus::Failed { .. }),
        "{out}"
    );
    assert_eq!(read(d.path(), "fsB/etc/g.conf"), "oldg\n");
    nothing_left(d.path());
}

#[test]
fn max_fail_leaves_the_host_never_started_untouched_and_the_failed_one_cleaned() {
    let d = tempfile::tempdir().unwrap();
    world(d.path());
    let pb = Playbook::new("fail", failing_pb);
    let (r, out) = go(
        &pb,
        vec![
            host("h1", local(d.path()), vars(d.path())),
            host("h2", local(d.path()), vars(d.path())),
        ],
        false,
        RunControls {
            serial: Some(1),
            max_fail: Some(0),
            ..Default::default()
        },
        None,
    );
    assert!(
        matches!(r.hosts[0].status, HostStatus::Failed { .. }),
        "{out}"
    );
    assert!(
        matches!(r.hosts[1].status, HostStatus::NotStarted(_)),
        "{out}"
    );
    nothing_left(d.path());
}

fn dies_pb(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move {
        h.file(&dp(&h, "fsB/etc/f.conf")).content("new\n");
        h.raw("kill -9 $$ $BASHPID");
        h.raw("true");
        Ok(())
    })
}

#[test]
fn a_dropped_root_shell_is_replaced_for_cleanup() {
    let d = tempfile::tempdir().unwrap();
    world(d.path());
    let pb = Playbook::new("dies", dies_pb);
    let (r, out) = go1(&pb, local(d.path()), d.path());
    assert!(
        matches!(r.hosts[0].status, HostStatus::Failed { .. }),
        "{}\n{out}",
        r.render()
    );
    nothing_left(d.path());
}

fn goes_down_pb(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move {
        h.file(&dp(&h, "fsB/etc/f.conf")).content("new\n");
        // The stub ends every ssh process and refuses new connections.
        sh!(h, "systemctl reboot");
        Ok(())
    })
}

#[test]
fn cleanup_that_cannot_open_a_root_shell_warns() {
    let d = tempfile::tempdir().unwrap();
    world(d.path());
    let stub = Stub::new();
    let pb = Playbook::new("down", goes_down_pb);
    let (_, out) = go(
        &pb,
        vec![host(
            "h1",
            stub.target(d.path(), &[("TP_STUB_DOWN", "1000")]),
            vars(d.path()),
        )],
        false,
        RunControls::default(),
        None,
    );
    assert!(
        out.contains("cleanup: no root shell to remove the run's undo data"),
        "{out}"
    );
}

fn barrier_local_pb(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move {
        h.file(&dp(&h, "fsB/etc/f.conf")).content("new\n");
        h.file(&dp(&h, "marked")).content("1\n").needs_reboot();
        h.reboot().if_pending();
        sh!(h, "true");
        Ok(())
    })
}

#[test]
fn a_reboot_pending_stop_is_cleaned() {
    let d = tempfile::tempdir().unwrap();
    world(d.path());
    let target = Target::Local(
        LocalTransport::new()
            .uid(0)
            .sandboxed_at(d.path())
            .env("TP_RUN_DIR", &d.path().join("run").display().to_string())
            .env(
                "TP_FETCH_STAGE",
                &d.path().join("stage").display().to_string(),
            )
            .env("TP_FS_ROOTS", &d.path().join("fsB").display().to_string()),
    );
    let pb = Playbook::new("pending", barrier_local_pb);
    let (r, out) = go1(&pb, target, d.path());
    assert!(
        matches!(r.hosts[0].status, HostStatus::RebootPending(_)),
        "{}\n{out}",
        r.render()
    );
    nothing_left(d.path());
}

fn fast() -> RunControls {
    RunControls {
        reboot_wait: RebootWait {
            deadline: Duration::from_secs(20),
            poll: Duration::from_millis(50),
            down: Duration::from_secs(5),
        },
        ..Default::default()
    }
}

fn barrier_pb(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move {
        h.file(&dp(&h, "fsB/etc/f.conf")).content("new\n");
        h.file(&dp(&h, "marked")).content("1\n").needs_reboot();
        h.reboot().if_pending();
        let seen = dp(&h, "seen");
        sh!(
            h,
            "find \"$TP_FS_ROOTS/.telepatch/tmp/$__tp_run\" -name old > {seen}; ls \"$TP_FETCH_STAGE/$__tp_run\" >> {seen}"
        );
        Ok(())
    })
}

#[test]
fn a_reboot_barrier_does_not_clean_and_the_hosts_end_does() {
    let d = tempfile::tempdir().unwrap();
    world(d.path());
    let stub = Stub::new();
    let pb = Playbook::new("barrier", barrier_pb);
    let (r, out) = go(
        &pb,
        vec![host("h1", stub.target(d.path(), &[]), vars(d.path()))],
        false,
        fast(),
        None,
    );
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert_eq!(r.hosts[0].reboots.len(), 1, "{out}");
    // After the barrier the backup and the index were still there.
    let seen = read(d.path(), "seen");
    assert!(seen.contains("/undo/"), "{seen}");
    assert!(seen.contains("fsroots"), "{seen}");
    nothing_left(d.path());
}

fn barrier_lost_pb(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move {
        h.file(&dp(&h, "fsB/etc/f.conf")).content("new\n");
        h.file(&dp(&h, "marked")).content("1\n").needs_reboot();
        h.reboot().if_pending();
        // A backup on a tmpfs is gone after the reboot.
        sh!(h, "rm -rf \"$TP_FS_ROOTS/.telepatch/tmp/$__tp_run\"");
        Ok(())
    })
}

#[test]
fn cleanup_tolerates_a_per_filesystem_dir_the_reboot_took() {
    let d = tempfile::tempdir().unwrap();
    world(d.path());
    let stub = Stub::new();
    let pb = Playbook::new("lost", barrier_lost_pb);
    let (r, out) = go(
        &pb,
        vec![host("h1", stub.target(d.path(), &[]), vars(d.path()))],
        false,
        fast(),
        None,
    );
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert!(!out.contains("cleanup:"), "{out}");
    nothing_left(d.path());
}
