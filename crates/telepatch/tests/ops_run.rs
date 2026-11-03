//! Run the wrapped `file` and `sh` templates with plain `bash -c` in a
//! tempdir: results, idempotency, subshell isolation, trailer on `exit`.

use std::process::Command;
use std::rc::Rc;

use telepatch::facts::os::Os;
use telepatch::trailer::{FileResult, PkgResult, ShellResult, Trailer, render_op, split_stream};
use telepatch::{Host, Node, NodeId, sh};

const NONCE: &str = "t0n";

fn host() -> Host<()> {
    let os = Os::parse("ID=debian\nVERSION_ID=12\n").unwrap();
    Host::new("t", os, Rc::new(()))
}

fn wrapped(h: &Host<()>, id: NodeId) -> String {
    match &h.snapshot().nodes[id.0] {
        Node::Op(op) => render_op(op, id.0, NONCE, &h.os),
        _ => panic!("not an op"),
    }
}

/// Run scripts in one bash (like a persistent shell) in `dir`; returns the
/// raw stdout.
fn bash(script: &str, dir: &std::path::Path, path_prefix: Option<&std::path::Path>) -> String {
    let mut c = Command::new("bash");
    c.arg("-c").arg(script).current_dir(dir).env("HOME", dir);
    if let Some(p) = path_prefix {
        let old = std::env::var("PATH").unwrap();
        c.env("PATH", format!("{}:{old}", p.display()));
    }
    let out = c.output().unwrap();
    String::from_utf8(out.stdout).unwrap()
}

fn one<R: telepatch::trailer::OpResult>(stream: &str) -> (String, R) {
    let mut v = split_stream(stream, NONCE);
    assert_eq!(v.len(), 1, "stream: {stream:?}");
    let (o, t) = v.remove(0);
    (o, t.typed::<R>().unwrap())
}

fn file_run(h: &Host<()>, id: NodeId, dir: &std::path::Path) -> FileResult {
    one::<FileResult>(&bash(&wrapped(h, id), dir, None)).1
}

#[test]
fn file_content_idempotent_and_default_mode() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("a b.conf");
    let h = host();
    let id = h.file(p.to_str().unwrap()).content("hello\nworld").id();
    let r = file_run(&h, id, d.path());
    assert_eq!((r.rc, r.changed), (0, true));
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "hello\nworld");
    let m = std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&p).unwrap().permissions());
    assert_eq!(m & 0o777, 0o644);
    let r = file_run(&h, id, d.path());
    assert_eq!((r.rc, r.changed), (0, false));
    // changed content: replaced, mode of the existing file kept
    std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    std::fs::write(&p, "other").unwrap();
    assert!(file_run(&h, id, d.path()).changed);
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "hello\nworld");
    let m = std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&p).unwrap().permissions());
    assert_eq!(m & 0o777, 0o600);
}

#[test]
fn file_line_and_line_re() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("rc");
    std::fs::write(&p, "A=1\nB=2\nA=3").unwrap(); // no trailing newline
    let h = host();
    let s = p.to_str().unwrap();
    let add = h.file(s).line("C=4").id();
    let re = h.file(s).line_re("^A=", "A=9").id();
    assert!(file_run(&h, add, d.path()).changed);
    assert!(file_run(&h, re, d.path()).changed);
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "A=1\nB=2\nA=9\nC=4\n");
    assert!(!file_run(&h, add, d.path()).changed);
    assert!(!file_run(&h, re, d.path()).changed);
    // no match: append
    let none = h.file(s).line_re("^Z=", "Z=0").id();
    assert!(file_run(&h, none, d.path()).changed);
    assert!(std::fs::read_to_string(&p).unwrap().ends_with("C=4\nZ=0\n"));
    assert!(!file_run(&h, none, d.path()).changed);
    // a line op creates a missing file
    let q = d.path().join("new");
    let id = h.file(q.to_str().unwrap()).line("x").id();
    assert!(file_run(&h, id, d.path()).changed);
    assert_eq!(std::fs::read_to_string(&q).unwrap(), "x\n");
}

fn mode_of(p: &std::path::Path) -> u32 {
    std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(p).unwrap().permissions()) & 0o7777
}

fn leftovers(d: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(d)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    v.sort();
    v
}

#[test]
fn file_validate_pass_writes_with_final_mode_checked_on_the_candidate() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("ops");
    let seen = d.path().join("seen");
    std::fs::write(&p, "old\n").unwrap();
    std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
    let h = host();
    // the validator records the candidate's mode and name, and its content
    let cmd = format!(
        "stat -c %a %s > {s}; cat %s >> {s}; grep -q good %s",
        s = seen.display()
    );
    let id = h
        .file(p.to_str().unwrap())
        .content("good\n")
        .mode(0o440)
        .validate(&cmd)
        .id();
    let r = file_run(&h, id, d.path());
    assert_eq!((r.rc, r.changed), (0, true));
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "good\n");
    assert_eq!(mode_of(&p), 0o440);
    assert_eq!(std::fs::read_to_string(&seen).unwrap(), "440\ngood\n");
    assert_eq!(leftovers(d.path()), ["ops", "seen"]);
    // idempotent: nothing to write, validator not needed
    let r = file_run(&h, id, d.path());
    assert_eq!((r.rc, r.changed), (0, false));
    assert_eq!(leftovers(d.path()), ["ops", "seen"]);
}

#[test]
fn file_validate_creates_new_file_at_mode_with_no_transient_mode() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("new");
    let h = host();
    let id = h
        .file(p.to_str().unwrap())
        .content("good\n")
        .mode(0o440)
        .validate("grep -q good %s")
        .id();
    let r = file_run(&h, id, d.path());
    assert_eq!((r.rc, r.changed), (0, true));
    assert_eq!(mode_of(&p), 0o440);
}

#[test]
fn file_validate_fail_leaves_original_bytes_and_mtime_and_removes_temp() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("ops");
    std::fs::write(&p, "old\n").unwrap();
    let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    std::fs::File::options()
        .write(true)
        .open(&p)
        .unwrap()
        .set_modified(t)
        .unwrap();
    let before = std::fs::metadata(&p).unwrap().modified().unwrap();
    let h = host();
    let id = h
        .file(p.to_str().unwrap())
        .content("bad\n")
        .validate("grep -q good %s || exit 7")
        .id();
    let r = file_run(&h, id, d.path());
    assert_eq!((r.rc, r.changed), (7, false));
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "old\n");
    assert_eq!(std::fs::metadata(&p).unwrap().modified().unwrap(), before);
    assert_eq!(leftovers(d.path()), ["ops"]);
    // a failing validator on a file that does not exist creates nothing
    let q = d.path().join("nope");
    let id = h
        .file(q.to_str().unwrap())
        .content("bad\n")
        .validate("false %s")
        .id();
    let r = file_run(&h, id, d.path());
    assert_eq!((r.rc, r.changed), (1, false));
    assert_eq!(leftovers(d.path()), ["ops"]);
}

#[test]
fn file_validate_line_re_on_existing_file() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("sshd_config");
    std::fs::write(&p, "A yes\nPermitRootLogin yes\nB no\n").unwrap();
    std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    let h = host();
    let s = p.to_str().unwrap();
    let ok = h
        .file(s)
        .line_re("^PermitRootLogin", "PermitRootLogin no")
        .line("UseDNS no")
        .validate("! grep -q 'yes' %s || grep -q 'PermitRootLogin no' %s")
        .id();
    // candidate still has "A yes", so `! grep yes` is false and the second
    // test decides: it passes, the edit lands
    let r = file_run(&h, ok, d.path());
    assert_eq!((r.rc, r.changed), (0, true));
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "A yes\nPermitRootLogin no\nB no\nUseDNS no\n"
    );
    assert_eq!(mode_of(&p), 0o600);
    assert!(!file_run(&h, ok, d.path()).changed);
    // a rejecting validator leaves the file as it was
    let bad = h
        .file(s)
        .line_re("^B ", "B yes")
        .validate("! grep -q '^B yes' %s")
        .id();
    let r = file_run(&h, bad, d.path());
    assert_eq!((r.rc, r.changed), (1, false));
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "A yes\nPermitRootLogin no\nB no\nUseDNS no\n"
    );
    assert_eq!(leftovers(d.path()), ["sshd_config"]);
}

#[test]
fn file_validate_path_with_spaces_and_quotes_in_command() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("a b's.conf");
    let h = host();
    let id = h
        .file(p.to_str().unwrap())
        .content("good\n")
        .validate("grep -q 'good' %s")
        .id();
    let r = file_run(&h, id, d.path());
    assert_eq!((r.rc, r.changed), (0, true));
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "good\n");
}

#[test]
fn file_dir_absent_mode_owner() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("x/y");
    let h = host();
    let mk = h.file(dir.to_str().unwrap()).dir().mode(0o750).id();
    assert!(file_run(&h, mk, d.path()).changed);
    assert!(dir.is_dir());
    let m =
        std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&dir).unwrap().permissions());
    assert_eq!(m & 0o7777, 0o750);
    assert!(!file_run(&h, mk, d.path()).changed);
    // owner equal to the current owner: no change, no failure
    let me = String::from_utf8(Command::new("id").arg("-un").output().unwrap().stdout).unwrap();
    let grp = String::from_utf8(Command::new("id").arg("-gn").output().unwrap().stdout).unwrap();
    let ow = h
        .file(dir.to_str().unwrap())
        .dir()
        .owner(&format!("{}:{}", me.trim(), grp.trim()))
        .id();
    let r = file_run(&h, ow, d.path());
    assert_eq!((r.rc, r.changed), (0, false));
    let ab = h.file(d.path().join("x").to_str().unwrap()).absent().id();
    assert!(file_run(&h, ab, d.path()).changed);
    assert!(!d.path().join("x").exists());
    assert!(!file_run(&h, ab, d.path()).changed);
}

#[test]
fn file_tilde_expands_on_host() {
    let d = tempfile::tempdir().unwrap();
    let h = host();
    let id = h.file("~/.my rc").line("hi").id();
    assert!(file_run(&h, id, d.path()).changed);
    assert_eq!(
        std::fs::read_to_string(d.path().join(".my rc")).unwrap(),
        "hi\n"
    );
}

#[test]
fn sh_ops_idempotent_and_trailer_shape() {
    let d = tempfile::tempdir().unwrap();
    let h = host();
    let id = sh!(h, "echo hi; touch made", creates = "made").id();
    let (out, r) = one::<ShellResult>(&bash(&wrapped(&h, id), d.path(), None));
    assert_eq!(out, "hi\n");
    assert_eq!((r.rc, r.changed, r.skipped), (0, true, false));
    let (out, r) = one::<ShellResult>(&bash(&wrapped(&h, id), d.path(), None));
    assert_eq!(out, "");
    assert_eq!((r.rc, r.changed, r.skipped), (0, false, true));
}

#[test]
fn output_without_trailing_newline_still_splits() {
    let d = tempfile::tempdir().unwrap();
    let h = host();
    let a = sh!(h, "printf abc").id();
    let b = sh!(h, "echo two").id();
    let s = format!("{}\n{}\n", wrapped(&h, a), wrapped(&h, b));
    let v = split_stream(&bash(&s, d.path(), None), NONCE);
    assert_eq!(v.len(), 2);
    assert_eq!(v[0].0, "abc");
    assert_eq!(v[0].1.id, a.0);
    assert_eq!(v[1].0, "two\n");
    assert_eq!(v[1].1.id, b.0);
}

#[test]
fn exit_in_body_still_emits_trailer() {
    let d = tempfile::tempdir().unwrap();
    let h = host();
    let id = sh!(h, "echo before; exit 3").id();
    let (out, r) = one::<ShellResult>(&bash(&wrapped(&h, id), d.path(), None));
    assert_eq!(out, "before\n");
    assert_eq!((r.rc, r.changed), (3, false));
    // a failing command (not exit) too
    let id = sh!(h, "false").id();
    let (_, r) = one::<ShellResult>(&bash(&wrapped(&h, id), d.path(), None));
    assert_eq!(r.rc, 1);
}

#[test]
fn cd_and_export_do_not_leak_to_next_op() {
    let d = tempfile::tempdir().unwrap();
    let h = host();
    let a = sh!(h, "cd / && export TP_LEAK=1").id();
    let b = sh!(h, "pwd; echo \"[${{TP_LEAK:-}}]\"").id();
    let s = format!("{}\n{}\n", wrapped(&h, a), wrapped(&h, b));
    let v = split_stream(&bash(&s, d.path(), None), NONCE);
    assert_eq!(v.len(), 2);
    assert_eq!(
        v[1].0,
        format!("{}\n[]\n", d.path().canonicalize().unwrap().display())
    );
}

/// Stub package tools on PATH to exercise the apt template's logic.
fn stub_dir(d: &std::path::Path, installed: &str) -> std::path::PathBuf {
    let bin = d.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let w = |n: &str, s: &str| {
        let p = bin.join(n);
        write_exe(&p, &format!("#!/bin/bash\n{s}\n"));
    };
    w(
        "dpkg-query",
        &format!(
            "for a; do :; done; case \" {installed} \" in *\" $a \"*) echo installed;; *) exit 1;; esac"
        ),
    );
    w(
        "apt-get",
        &format!("echo \"apt-get $*\" >> {}/calls", d.display()),
    );
    bin
}

#[test]
fn apt_installs_only_missing_and_reports_them() {
    let d = tempfile::tempdir().unwrap();
    let bin = stub_dir(d.path(), "curl");
    let h = host();
    let id = h.pkg(["curl", "jq", "git"]).id();
    let (_, r) = one::<PkgResult>(&bash(&wrapped(&h, id), d.path(), Some(&bin)));
    assert_eq!((r.rc, r.changed), (0, true));
    assert_eq!(r.installed, ["jq", "git"]);
    let calls = std::fs::read_to_string(d.path().join("calls")).unwrap();
    assert!(calls.contains("install jq git"), "{calls}");
    assert!(calls.contains("--force-confold"));
    // nothing missing: no apt-get install, changed false
    let all = host();
    let id = all.pkg(["curl"]).id();
    let (_, r) = one::<PkgResult>(&bash(&wrapped(&all, id), d.path(), Some(&bin)));
    assert_eq!((r.rc, r.changed, r.installed.len()), (0, false, 0));
}

#[test]
fn apt_update_flag_runs_update_first() {
    let d = tempfile::tempdir().unwrap();
    let bin = stub_dir(d.path(), "curl");
    let h = host();
    let p = h.pkg(["curl"]);
    p.update_cache(true);
    let id = p.id();
    let (_, r) = one::<PkgResult>(&bash(&wrapped(&h, id), d.path(), Some(&bin)));
    assert!(!r.changed);
    let calls = std::fs::read_to_string(d.path().join("calls")).unwrap();
    assert!(calls.contains("update"), "{calls}");
}

#[test]
fn trailer_parse_and_types() {
    let t = Trailer::parse_line("__tp_n 7 {\"rc\":0,\"changed\":true}").unwrap();
    assert_eq!((t.nonce.as_str(), t.id), ("n", 7));
    assert!(t.typed::<FileResult>().unwrap().changed);
    // a template that forgets a field does not parse into the type
    assert!(t.typed::<ShellResult>().is_err());
    assert!(Trailer::parse_line("hello").is_none());
}

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
