//! Inverse capture for the plain `.content(..)` form of `file` (T106, slice
//! A): metadata in the trailer, the backup where the target's filesystem
//! says, and cleanup. A filesystem of its own is the `TP_FS_ROOTS` seam;
//! nothing here mounts, and nothing is written outside a temp dir.

use std::cell::RefCell;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::facts::os::Os;
use telepatch::trailer::{FileResult, render_op, split_stream, with_helpers};
use telepatch::transport::local::LocalTransport;
use telepatch::{Host, Node, NodeId, Playbook, PlaybookFuture, TraceError, role, sh};

const NONCE: &str = "t0n";
const RUN: &str = "r1";
const ID: usize = 7;

fn host() -> Host<()> {
    let os = Os::parse("ID=debian\nVERSION_ID=12\n").unwrap();
    Host::new("t", os, Rc::new(()))
}

fn user() -> String {
    let o = Command::new("id").arg("-un").output().unwrap();
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

struct Env {
    stage: PathBuf,
    roots: Option<PathBuf>,
    run: Option<&'static str>,
    extra: Vec<(&'static str, String)>,
}

fn run_op(h: &Host<()>, id: NodeId, e: &Env) -> (String, FileResult) {
    let script = match &h.snapshot().nodes[id.0] {
        Node::Op(op) => render_op(op, ID, NONCE, &h.os),
        _ => panic!("not an op"),
    };
    let mut c = Command::new("/bin/bash");
    c.arg("-c")
        .arg(with_helpers(&script))
        .env_remove("TP_FS_ROOTS")
        .env_remove("__tp_run")
        .env("TP_SANDBOX", e.stage.parent().unwrap())
        .env("TP_FETCH_STAGE", &e.stage);
    for (k, v) in &e.extra {
        c.env(k, v);
    }
    if let Some(r) = &e.roots {
        c.env("TP_FS_ROOTS", r);
    }
    if let Some(r) = e.run {
        c.env("__tp_run", r);
    }
    let out = c.output().unwrap();
    let stream = String::from_utf8(out.stdout).unwrap();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    let mut v = split_stream(&stream, NONCE);
    assert_eq!(v.len(), 1, "stream: {stream:?}\n{err}");
    (err, v.remove(0).1.typed::<FileResult>().unwrap())
}

fn env(d: &Path, roots: Option<PathBuf>) -> Env {
    Env {
        stage: d.join("stage"),
        roots,
        run: Some(RUN),
        extra: Vec::new(),
    }
}

fn find_telepatch(d: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
        let p = e.path();
        if e.file_name() == ".telepatch" {
            out.push(p.clone());
        }
        if e.file_type().unwrap().is_dir() {
            find_telepatch(&p, out);
        }
    }
}

fn stage_id(e: &Env) -> PathBuf {
    e.stage
        .join(RUN)
        .join("undo")
        .join(user())
        .join(ID.to_string())
}

#[test]
fn content_over_an_existing_file_backs_it_up() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("f.conf");
    std::fs::write(&p, "old\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o640)).unwrap();
    let old = std::fs::metadata(&p).unwrap();
    let h = host();
    let id = h.file(p.to_str().unwrap()).content("new\n").id();
    let e = env(d.path(), None);
    let (err, r) = run_op(&h, id, &e);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert!(r.existed);
    assert_eq!(r.was_type, "file");
    assert_eq!(r.was_mode, "640");
    assert!(r.was_owner.starts_with(&format!("{}:", user())), "{r:?}");
    assert_eq!(r.backup, stage_id(&e).to_str().unwrap());
    let b = stage_id(&e).join("old");
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "old\n");
    let bm = std::fs::metadata(&b).unwrap();
    assert_eq!(bm.permissions().mode() & 0o7777, 0o640);
    assert_eq!((bm.uid(), bm.gid()), (old.uid(), old.gid()));
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "new\n");
    // The directories are the owner's alone.
    let m = |x: PathBuf| std::fs::metadata(x).unwrap().permissions().mode() & 0o777;
    assert_eq!(m(e.stage.join(RUN).join("undo")), 0o711);
    assert_eq!(m(e.stage.join(RUN).join("undo").join(user())), 0o700);
}

#[test]
fn a_new_file_records_that_it_did_not_exist_and_saves_nothing() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("new.conf");
    let h = host();
    let id = h.file(p.to_str().unwrap()).content("x\n").id();
    let e = env(d.path(), None);
    let (err, r) = run_op(&h, id, &e);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert!(!r.existed);
    assert_eq!(r.was_type, "");
    assert_eq!(r.backup, "");
    assert!(!stage_id(&e).exists());
}

#[test]
fn an_unchanged_content_op_saves_nothing() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("same.conf");
    std::fs::write(&p, "same\n").unwrap();
    let h = host();
    let id = h.file(p.to_str().unwrap()).content("same\n").id();
    let e = env(d.path(), None);
    let (err, r) = run_op(&h, id, &e);
    assert_eq!((r.rc, r.changed), (0, false), "{err}");
    assert_eq!(r.backup, "");
    assert!(!e.stage.exists(), "no directory is made for an equal op");
}

#[test]
fn without_a_run_nothing_is_captured() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("f");
    std::fs::write(&p, "old\n").unwrap();
    let h = host();
    let id = h.file(p.to_str().unwrap()).content("new\n").id();
    let mut e = env(d.path(), None);
    e.run = None;
    let (err, r) = run_op(&h, id, &e);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert!(r.existed);
    assert_eq!(r.backup, "");
    assert!(!e.stage.exists());
}

#[test]
fn a_target_on_another_filesystem_is_backed_up_there_and_linked() {
    let d = tempfile::tempdir().unwrap();
    let fsb = d.path().join("fsB");
    std::fs::create_dir_all(fsb.join("etc")).unwrap();
    let p = fsb.join("etc/f.conf");
    std::fs::write(&p, "old\n").unwrap();
    let h = host();
    let id = h.file(p.to_str().unwrap()).content("new\n").id();
    let e = env(d.path(), Some(fsb.clone()));
    let (err, r) = run_op(&h, id, &e);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    let real = fsb
        .join(".telepatch/tmp")
        .join(RUN)
        .join("undo")
        .join(user())
        .join(ID.to_string());
    assert_eq!(std::fs::read_to_string(real.join("old")).unwrap(), "old\n");
    let link = stage_id(&e);
    assert_eq!(std::fs::read_link(&link).unwrap(), real);
    assert_eq!(r.backup, link.to_str().unwrap());
    let idx = e.stage.join(RUN).join("fsroots");
    assert_eq!(
        std::fs::read_to_string(&idx).unwrap(),
        format!("{}\n", fsb.display())
    );
    assert_eq!(
        std::fs::metadata(&idx).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let m = |x: PathBuf| std::fs::metadata(x).unwrap().permissions().mode() & 0o777;
    assert_eq!(m(fsb.join(".telepatch")), 0o711);
    assert_eq!(m(fsb.join(".telepatch/tmp").join(RUN).join("undo")), 0o711);
    assert_eq!(
        m(fsb
            .join(".telepatch/tmp")
            .join(RUN)
            .join("undo")
            .join(user())),
        0o700
    );
}

#[test]
fn a_target_on_the_stage_filesystem_has_no_link_and_no_telepatch_dir() {
    let d = tempfile::tempdir().unwrap();
    let fsb = d.path().join("fsB");
    std::fs::create_dir_all(&fsb).unwrap();
    let p = d.path().join("data.conf");
    std::fs::write(&p, "old\n").unwrap();
    let h = host();
    let id = h.file(p.to_str().unwrap()).content("new\n").id();
    // A seam root exists, but the target is not under it.
    let e = env(d.path(), Some(fsb));
    let (err, r) = run_op(&h, id, &e);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    let meta = std::fs::symlink_metadata(stage_id(&e)).unwrap();
    assert!(meta.is_dir() && !meta.file_type().is_symlink());
    let mut found = Vec::new();
    find_telepatch(d.path(), &mut found);
    assert!(found.is_empty(), "{found:?}");
    assert!(!e.stage.join(RUN).join("fsroots").exists());
}

#[test]
fn a_new_target_under_another_filesystem_uses_its_nearest_ancestor() {
    let d = tempfile::tempdir().unwrap();
    let fsb = d.path().join("fsB");
    std::fs::create_dir_all(&fsb).unwrap();
    // tp_fsroot of a path that does not exist yet.
    let script = format!(
        "{}\ntp_fsroot {}/a/b/c\n",
        telepatch::ops::undo::HELPERS,
        fsb.display()
    );
    let out = Command::new("/bin/bash")
        .arg("-c")
        .arg(script)
        .env("TP_FS_ROOTS", &fsb)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        fsb.display().to_string()
    );
    // And a new file there makes no backup and does not fail.
    let p = fsb.join("n.conf");
    let h = host();
    let id = h.file(p.to_str().unwrap()).content("x\n").id();
    let e = env(d.path(), Some(fsb.clone()));
    let (err, r) = run_op(&h, id, &e);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert!(!r.existed);
    assert_eq!(r.backup, "");
    assert!(!fsb.join(".telepatch").exists());
}

#[test]
fn a_read_only_filesystem_fails_the_op_before_the_target_is_touched() {
    let d = tempfile::tempdir().unwrap();
    let fsb = d.path().join("fsB");
    std::fs::create_dir_all(&fsb).unwrap();
    let p = fsb.join("f.conf");
    std::fs::write(&p, "old\n").unwrap();
    std::fs::set_permissions(&fsb, std::fs::Permissions::from_mode(0o555)).unwrap();
    let h = host();
    let id = h.file(p.to_str().unwrap()).content("new\n").id();
    let e = env(d.path(), Some(fsb.clone()));
    let (err, r) = run_op(&h, id, &e);
    std::fs::set_permissions(&fsb, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(r.rc, 1, "{err}");
    assert!(!r.changed);
    assert!(err.contains("cannot save undo data"), "{err}");
    assert!(err.contains(&format!("file {}", p.display())), "{err}");
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "old\n");
    assert_eq!(r.backup, "");
    let left: Vec<_> = std::fs::read_dir(&fsb)
        .unwrap()
        .flatten()
        .map(|x| x.file_name())
        .collect();
    assert_eq!(left.len(), 1, "{left:?}");
}

#[test]
fn a_user_shell_does_not_create_a_root_on_a_real_filesystem() {
    // No seam: /tmp-style roots are found by the device walk. A shell that
    // is not uid 0 must not create `.telepatch` there.
    if Command::new("id").arg("-u").output().unwrap().stdout == b"0\n" {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("f");
    std::fs::write(&p, "old\n").unwrap();
    let h = host();
    let id = h.file(p.to_str().unwrap()).content("new\n").id();
    // The stage is on the root filesystem's other side only if the temp dir
    // is on its own device; force the difference with a stage under a seam
    // root that is not the target's: the target walks to its real root.
    let stage_fs = d.path().join("stagefs");
    std::fs::create_dir_all(&stage_fs).unwrap();
    let e = Env {
        stage: stage_fs.join("stage"),
        roots: Some(stage_fs),
        run: Some(RUN),
        extra: Vec::new(),
    };
    let (err, r) = run_op(&h, id, &e);
    assert_eq!(r.rc, 1, "{err}");
    assert!(err.contains("not prepared"), "{err}");
    // A refused capture writes nothing on the stage, not even the run dir.
    assert!(!e.stage.exists(), "refused capture left {:?}", e.stage);
    assert_eq!(r.backup, "");
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "old\n");
}

// ---- cleanup, through the executor ----

type V = (PathBuf, PathBuf);

#[role]
async fn put(h: &Host<V>) -> Result<(), TraceError> {
    let t = h.vars.0.join("fsB/etc/f.conf");
    let out = h.vars.1.join("seen");
    let t = t.to_str().unwrap().to_string();
    let out = out.to_str().unwrap().to_string();
    h.file(&t).content("new\n");
    sh!(
        h,
        "for f in \"$TP_FETCH_STAGE\"/\"$__tp_run\"/undo/*/*; do printf '%s %s %s\\n' \"$(cat \"$f/old\")\" \"$(readlink \"$f\")\" \"$f\"; done > {out}"
    );
    Ok(())
}

fn put_run(h: Host<V>) -> PlaybookFuture {
    Box::pin(async move { put(&h).await })
}

#[test]
fn cleanup_removes_this_runs_data_and_leaves_other_runs() {
    let d = tempfile::tempdir().unwrap();
    let fsb = d.path().join("fsB");
    std::fs::create_dir_all(fsb.join("etc")).unwrap();
    std::fs::write(fsb.join("etc/f.conf"), "old\n").unwrap();
    // Another run's data under the same root must stay.
    let other = fsb.join(".telepatch/tmp/other-run/undo");
    std::fs::create_dir_all(&other).unwrap();
    let stage = d.path().join("stage");
    let pb = Playbook::new("undo", put_run);
    let out = Rc::new(RefCell::new(String::new()));
    let report = run(
        &pb,
        vec![HostSpec {
            groups: Vec::new(),
            name: "h1".into(),
            target: Target::Local(
                LocalTransport::new()
                    .uid(0)
                    .sandboxed_at(d.path())
                    .env("TP_FETCH_STAGE", stage.to_str().unwrap())
                    .env("TP_FS_ROOTS", fsb.to_str().unwrap()),
            ),
            vars: (d.path().to_path_buf(), d.path().to_path_buf()),
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    assert_eq!(
        report.exit_code(),
        0,
        "{}\n{}",
        report.render(),
        out.borrow()
    );
    // Mid-run, the shell pool saw the backup through the stage link.
    let seen = std::fs::read_to_string(d.path().join("seen")).unwrap();
    assert!(seen.starts_with("old "), "{seen}");
    assert!(seen.contains("/fsB/.telepatch/tmp/"), "{seen}");
    assert_eq!(
        std::fs::read_to_string(fsb.join("etc/f.conf")).unwrap(),
        "new\n"
    );
    // After the host ends: this run is gone, the other stays.
    assert!(other.exists(), "another run's data was removed");
    let runs: Vec<_> = std::fs::read_dir(fsb.join(".telepatch/tmp"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(runs, vec!["other-run".to_string()]);
    let left: Vec<_> = std::fs::read_dir(&stage)
        .map(|r| r.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "stage run dir left: {left:?}");
}

#[test]
fn cleanup_removes_telepatch_when_it_held_only_this_run() {
    let d = tempfile::tempdir().unwrap();
    let fsb = d.path().join("fsB");
    std::fs::create_dir_all(fsb.join("etc")).unwrap();
    std::fs::write(fsb.join("etc/f.conf"), "old\n").unwrap();
    let stage = d.path().join("stage");
    let pb = Playbook::new("undo", put_run);
    let out = Rc::new(RefCell::new(String::new()));
    let report = run(
        &pb,
        vec![HostSpec {
            groups: Vec::new(),
            name: "h1".into(),
            target: Target::Local(
                LocalTransport::new()
                    .uid(0)
                    .sandboxed_at(d.path())
                    .env("TP_FETCH_STAGE", stage.to_str().unwrap())
                    .env("TP_FS_ROOTS", fsb.to_str().unwrap()),
            ),
            vars: (d.path().to_path_buf(), d.path().to_path_buf()),
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    assert_eq!(report.exit_code(), 0, "{}", report.render());
    assert!(!fsb.join(".telepatch").exists());
    let mut found = Vec::new();
    find_telepatch(d.path(), &mut found);
    assert!(found.is_empty(), "{found:?}");
}

// ---- the other forms (slice B) ----

fn ino(p: &Path) -> u64 {
    std::fs::symlink_metadata(p).unwrap().ino()
}

fn sha_of(bytes: &[u8]) -> String {
    use std::io::Write;
    let mut c = Command::new("sha256sum")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(bytes).unwrap();
    let o = c.wait_with_output().unwrap();
    String::from_utf8(o.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

/// The cache entry for `bytes`, in `d/cache`; the sum names it.
fn cached(d: &Path, bytes: &[u8]) -> String {
    let sum = sha_of(bytes);
    std::fs::create_dir_all(d.join("cache")).unwrap();
    std::fs::write(d.join("cache").join(&sum), bytes).unwrap();
    sum
}

fn with_cache(d: &Path, mut e: Env) -> Env {
    e.extra
        .push(("TP_FETCH_CACHE", d.join("cache").display().to_string()));
    e
}

fn nothing_saved(e: &Env, r: &FileResult) {
    assert_eq!(r.backup, "", "{r:?}");
    assert!(
        !e.stage.exists(),
        "a directory was made for an op that changed nothing"
    );
}

#[test]
fn absent_on_a_directory_moves_the_tree_whole() {
    let d = tempfile::tempdir().unwrap();
    let t = d.path().join("tree");
    std::fs::create_dir_all(t.join("sub")).unwrap();
    std::fs::write(t.join("a"), "a\n").unwrap();
    std::fs::write(t.join("sub/b"), "b\n").unwrap();
    let before = ino(&t);
    let inner = ino(&t.join("sub"));
    let h = host();
    let id = h.file(t.to_str().unwrap()).absent().id();
    let e = env(d.path(), None);
    let (err, r) = run_op(&h, id, &e);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert!(!t.exists());
    assert!(r.existed);
    assert_eq!(r.was_type, "dir");
    assert_eq!(r.backup, stage_id(&e).to_str().unwrap());
    let old = stage_id(&e).join("old");
    assert_eq!(ino(&old), before, "the tree was copied, not moved");
    assert_eq!(ino(&old.join("sub")), inner);
    assert_eq!(std::fs::read_to_string(old.join("sub/b")).unwrap(), "b\n");
}

#[test]
fn absent_on_another_filesystem_keeps_the_inode_there() {
    let d = tempfile::tempdir().unwrap();
    let fsb = d.path().join("fsB");
    let t = fsb.join("data");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(t.join("a"), "a\n").unwrap();
    let before = ino(&t);
    let h = host();
    let id = h.file(t.to_str().unwrap()).absent().id();
    let e = env(d.path(), Some(fsb.clone()));
    let (err, r) = run_op(&h, id, &e);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    let real = fsb
        .join(".telepatch/tmp")
        .join(RUN)
        .join("undo")
        .join(user())
        .join(ID.to_string());
    assert_eq!(ino(&real.join("old")), before);
    assert_eq!(std::fs::read_link(stage_id(&e)).unwrap(), real);
    assert!(!t.exists());
}

#[test]
fn absent_on_a_file_and_a_missing_path() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("f");
    std::fs::write(&f, "keep\n").unwrap();
    let h = host();
    let id = h.file(f.to_str().unwrap()).absent().id();
    let e = env(d.path(), None);
    let (err, r) = run_op(&h, id, &e);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert_eq!(
        std::fs::read_to_string(stage_id(&e).join("old")).unwrap(),
        "keep\n"
    );
    // The path is gone now: a second op changes nothing and saves nothing.
    let e2 = Env {
        stage: d.path().join("stage2"),
        ..env(d.path(), None)
    };
    let (err, r) = run_op(&h, id, &e2);
    assert_eq!((r.rc, r.changed), (0, false), "{err}");
    nothing_saved(&e2, &r);
    assert!(!r.existed);
}

fn tarball(d: &Path, name: &str, files: &[(&str, &str)]) -> Vec<u8> {
    let src = d.join(format!("src-{name}"));
    std::fs::create_dir_all(&src).unwrap();
    for (n, c) in files {
        std::fs::write(src.join(n), c).unwrap();
    }
    let out = d.join(format!("{name}.tgz"));
    let st = Command::new("tar")
        .arg("-czf")
        .arg(&out)
        .arg("-C")
        .arg(&src)
        .arg(".")
        .status()
        .unwrap();
    assert!(st.success());
    std::fs::read(out).unwrap()
}

#[test]
fn unpack_over_an_old_tree_keeps_the_old_tree() {
    let d = tempfile::tempdir().unwrap();
    let app = d.path().join("app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join("old.txt"), "old\n").unwrap();
    let before = ino(&app);
    let sum = cached(d.path(), &tarball(d.path(), "v2", &[("new.txt", "new\n")]));
    let h = host();
    let f = h.fetch("https://example.com/v2.tgz").sha256(&sum);
    let id = h.file(app.to_str().unwrap()).unpack(&f).id();
    let e = with_cache(d.path(), env(d.path(), None));
    let (err, r) = run_op(&h, id, &e);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert_eq!(
        std::fs::read_to_string(app.join("new.txt")).unwrap(),
        "new\n"
    );
    assert!(!app.join("old.txt").exists());
    assert_eq!(r.was_type, "dir");
    let old = stage_id(&e).join("old");
    assert_eq!(ino(&old), before, "the old tree was not moved whole");
    assert_eq!(
        std::fs::read_to_string(old.join("old.txt")).unwrap(),
        "old\n"
    );
    // The same sum again: unchanged, nothing saved.
    let e2 = Env {
        stage: d.path().join("stage2"),
        ..with_cache(d.path(), env(d.path(), None))
    };
    let (err, r) = run_op(&h, id, &e2);
    assert_eq!((r.rc, r.changed), (0, false), "{err}");
    nothing_saved(&e2, &r);
}

#[test]
fn unpack_into_a_new_path_saves_nothing() {
    let d = tempfile::tempdir().unwrap();
    let app = d.path().join("fresh");
    let sum = cached(d.path(), &tarball(d.path(), "v1", &[("x", "x\n")]));
    let h = host();
    let f = h.fetch("https://example.com/v1.tgz").sha256(&sum);
    let id = h.file(app.to_str().unwrap()).unpack(&f).id();
    let e = with_cache(d.path(), env(d.path(), None));
    let (err, r) = run_op(&h, id, &e);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert!(!r.existed);
    nothing_saved(&e, &r);
}

#[test]
fn a_line_edit_makes_a_backup() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("hosts");
    std::fs::write(&f, "a\nb\n").unwrap();
    let h = host();
    let id = h.file(f.to_str().unwrap()).line("c").id();
    let e = env(d.path(), None);
    let (err, r) = run_op(&h, id, &e);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "a\nb\nc\n");
    assert!(r.existed);
    assert_eq!(r.backup, stage_id(&e).to_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(stage_id(&e).join("old")).unwrap(),
        "a\nb\n"
    );
    // Present already: unchanged, nothing saved.
    let e2 = Env {
        stage: d.path().join("stage2"),
        ..env(d.path(), None)
    };
    let (err, r) = run_op(&h, id, &e2);
    assert_eq!((r.rc, r.changed), (0, false), "{err}");
    nothing_saved(&e2, &r);
}

#[test]
fn two_line_edits_save_the_file_as_it_was_once() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("c");
    std::fs::write(&f, "a\n").unwrap();
    let h = host();
    let id = h.file(f.to_str().unwrap()).line("b").line("c").id();
    let e = env(d.path(), None);
    let (err, r) = run_op(&h, id, &e);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "a\nb\nc\n");
    assert_eq!(
        std::fs::read_to_string(stage_id(&e).join("old")).unwrap(),
        "a\n"
    );
}

#[test]
fn a_line_edit_that_creates_the_file_saves_nothing() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("new");
    let h = host();
    let id = h.file(f.to_str().unwrap()).line("x").line("y").id();
    let e = env(d.path(), None);
    let (err, r) = run_op(&h, id, &e);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert!(!r.existed);
    nothing_saved(&e, &r);
}

#[test]
fn line_re_replacing_and_appending_back_up() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("sshd");
    std::fs::write(&f, "Port 22\nUse yes\n").unwrap();
    let h = host();
    let id = h
        .file(f.to_str().unwrap())
        .line_re("^Port ", "Port 2222")
        .id();
    let e = env(d.path(), None);
    let (err, r) = run_op(&h, id, &e);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "Port 2222\nUse yes\n");
    assert_eq!(
        std::fs::read_to_string(stage_id(&e).join("old")).unwrap(),
        "Port 22\nUse yes\n"
    );
    // No match: appended, and backed up.
    let id2 = h
        .file(f.to_str().unwrap())
        .line_re("^Banner ", "Banner x")
        .id();
    let e2 = Env {
        stage: d.path().join("stage2"),
        ..env(d.path(), None)
    };
    let (err, r) = run_op(&h, id2, &e2);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert_eq!(
        std::fs::read_to_string(stage_id(&e2).join("old")).unwrap(),
        "Port 2222\nUse yes\n"
    );
}

#[test]
fn a_validated_write_backs_up_only_when_it_replaces() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("sudoers");
    std::fs::write(&f, "old\n").unwrap();
    let h = host();
    let id = h
        .file(f.to_str().unwrap())
        .content("new\n")
        .validate("test -f %s")
        .id();
    let e = env(d.path(), None);
    let (err, r) = run_op(&h, id, &e);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "new\n");
    assert_eq!(
        std::fs::read_to_string(stage_id(&e).join("old")).unwrap(),
        "old\n"
    );
    // A failed check leaves the target and saves nothing.
    let bad = h
        .file(f.to_str().unwrap())
        .content("worse\n")
        .validate("false %s")
        .id();
    let e2 = Env {
        stage: d.path().join("stage2"),
        ..env(d.path(), None)
    };
    let (_, r) = run_op(&h, bad, &e2);
    assert_ne!(r.rc, 0);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "new\n");
    nothing_saved(&e2, &r);
    // Equal content: unchanged.
    let same = h
        .file(f.to_str().unwrap())
        .content("new\n")
        .validate("test -f %s")
        .id();
    let e3 = Env {
        stage: d.path().join("stage3"),
        ..env(d.path(), None)
    };
    let (err, r) = run_op(&h, same, &e3);
    assert_eq!((r.rc, r.changed), (0, false), "{err}");
    nothing_saved(&e3, &r);
}

#[test]
fn a_fetched_file_over_an_existing_one_backs_it_up() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("key.gpg");
    std::fs::write(&f, "old\n").unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();
    let sum = cached(d.path(), b"fresh\n");
    let h = host();
    let fe = h.fetch("https://example.com/k").sha256(&sum);
    let id = h.file(f.to_str().unwrap()).from(&fe).id();
    let e = with_cache(d.path(), env(d.path(), None));
    let (err, r) = run_op(&h, id, &e);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "fresh\n");
    let b = stage_id(&e).join("old");
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "old\n");
    assert_eq!(
        std::fs::metadata(&b).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // Placed already: unchanged.
    let e2 = Env {
        stage: d.path().join("stage2"),
        ..with_cache(d.path(), env(d.path(), None))
    };
    let (err, r) = run_op(&h, id, &e2);
    assert_eq!((r.rc, r.changed), (0, false), "{err}");
    nothing_saved(&e2, &r);
}

#[test]
fn dir_records_the_topmost_directory_it_created() {
    let d = tempfile::tempdir().unwrap();
    let t = d.path().join("a/b/c");
    let h = host();
    let id = h.file(t.to_str().unwrap()).dir().id();
    let e = env(d.path(), None);
    let (err, r) = run_op(&h, id, &e);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert!(t.is_dir());
    assert!(!r.existed);
    assert_eq!(r.created_top, d.path().join("a").to_str().unwrap());
    nothing_saved(&e, &r);
    // Part of it exists: the top is the first missing level.
    let t2 = d.path().join("a/b/x/y");
    let id2 = h.file(t2.to_str().unwrap()).dir().id();
    let (err, r) = run_op(&h, id2, &e);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert_eq!(r.created_top, d.path().join("a/b/x").to_str().unwrap());
    // Present: nothing created.
    let (err, r) = run_op(&h, id, &e);
    assert_eq!((r.rc, r.changed), (0, false), "{err}");
    assert_eq!(r.created_top, "");
    assert!(r.existed);
    assert_eq!(r.was_type, "dir");
}

#[test]
fn a_mode_only_change_records_the_old_mode_and_saves_no_bytes() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("m");
    std::fs::write(&f, "x\n").unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o640)).unwrap();
    let h = host();
    let id = h.file(f.to_str().unwrap()).mode(0o600).id();
    let e = env(d.path(), None);
    let (err, r) = run_op(&h, id, &e);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert_eq!(
        std::fs::metadata(&f).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        (r.existed, r.was_type.as_str(), r.was_mode.as_str()),
        (true, "file", "640")
    );
    assert!(r.was_owner.starts_with(&format!("{}:", user())), "{r:?}");
    nothing_saved(&e, &r);
    // An owner-only change records the owner the same way.
    let me = format!("{}:", user());
    let id2 = h.file(f.to_str().unwrap()).owner(&me).id();
    let (err, r) = run_op(&h, id2, &e);
    assert_eq!((r.rc, r.changed), (0, false), "{err}");
    assert!(r.existed);
    nothing_saved(&e, &r);
}

fn stub_bin(d: &Path) -> String {
    let bin = d.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("systemd-analyze"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        bin.join("systemd-analyze"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    format!("{}:/usr/bin:/bin", bin.display())
}

fn unit_env(d: &Path) -> Env {
    let mut e = env(d, None);
    std::fs::create_dir_all(d.join("units")).unwrap();
    e.extra
        .push(("TP_UNIT_DIR", d.join("units").display().to_string()));
    e.extra.push(("PATH", stub_bin(d)));
    e
}

#[test]
fn a_unit_captures_like_a_file() {
    let d = tempfile::tempdir().unwrap();
    let e = unit_env(d.path());
    let u = d.path().join("units/x.service");
    std::fs::write(&u, "[Service]\nold=1\n").unwrap();
    std::fs::set_permissions(&u, std::fs::Permissions::from_mode(0o640)).unwrap();
    let h = host();
    let id = h.unit("x.service").content("[Service]\nnew=1\n").id();
    let (err, r) = run_op(&h, id, &e);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert!(r.existed);
    assert_eq!(r.was_mode, "640");
    assert_eq!(
        std::fs::read_to_string(stage_id(&e).join("old")).unwrap(),
        "[Service]\nold=1\n"
    );
    // Unchanged: nothing saved.
    let e2 = Env {
        stage: d.path().join("stage2"),
        ..unit_env(d.path())
    };
    let (err, r) = run_op(&h, id, &e2);
    assert_eq!((r.rc, r.changed), (0, false), "{err}");
    nothing_saved(&e2, &r);
    // Absent moves it.
    let gone = h.unit("x.service").absent().id();
    let e3 = Env {
        stage: d.path().join("stage3"),
        ..unit_env(d.path())
    };
    let (err, r) = run_op(&h, gone, &e3);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert!(!u.exists());
    assert_eq!(
        std::fs::read_to_string(stage_id(&e3).join("old")).unwrap(),
        "[Service]\nnew=1\n"
    );
}

#[test]
fn a_drop_in_records_the_directory_it_created() {
    let d = tempfile::tempdir().unwrap();
    let h = host();
    let id = h.unit("x.service").drop_in("o", "[Service]\nA=1\n").id();
    let e = unit_env(d.path());
    let (err, r) = run_op(&h, id, &e);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    let dd = d.path().join("units/x.service.d");
    assert!(dd.join("o.conf").is_file());
    assert!(!r.existed);
    assert_eq!(r.created_top, dd.to_str().unwrap());
    nothing_saved(&e, &r);
    // The directory is there now, so a changed drop-in creates nothing, and
    // is backed up like a file.
    let id2 = h.unit("x.service").drop_in("o", "[Service]\nA=2\n").id();
    let e2 = Env {
        stage: d.path().join("stage2"),
        ..unit_env(d.path())
    };
    let (err, r) = run_op(&h, id2, &e2);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert_eq!(r.created_top, "");
    assert_eq!(
        std::fs::read_to_string(stage_id(&e2).join("old")).unwrap(),
        "[Service]\nA=1\n"
    );
}

#[test]
fn a_capture_failure_leaves_every_form_s_target_untouched() {
    let d = tempfile::tempdir().unwrap();
    let fsb = d.path().join("fsB");
    std::fs::create_dir_all(fsb.join("tree")).unwrap();
    std::fs::write(fsb.join("tree/a"), "a\n").unwrap();
    std::fs::write(fsb.join("f"), "old\n").unwrap();
    let sum = cached(d.path(), b"fresh\n");
    // The target's directory is writable (so a candidate can be built) but
    // the undo location is not: only the capture can fail.
    let lock = fsb.join(".telepatch");
    std::fs::create_dir(&lock).unwrap();
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o555)).unwrap();
    let h = host();
    let tree = fsb.join("tree");
    let f = fsb.join("f");
    let fe = h.fetch("https://example.com/k").sha256(&sum);
    let ops = [
        h.file(tree.to_str().unwrap()).absent().id(),
        h.file(f.to_str().unwrap()).line("x").id(),
        h.file(f.to_str().unwrap()).line_re("^o", "x").id(),
        h.file(f.to_str().unwrap())
            .content("n\n")
            .validate("test -f %s")
            .id(),
        h.file(f.to_str().unwrap()).from(&fe).id(),
    ];
    let e = with_cache(d.path(), env(d.path(), Some(fsb.clone())));
    let mut got = Vec::new();
    for id in ops {
        let (err, r) = run_op(&h, id, &e);
        got.push((
            r.rc,
            r.changed,
            err.contains("cannot save undo data"),
            r.backup,
        ));
    }
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o755)).unwrap();
    for g in &got {
        assert_eq!((g.0, g.1, g.2), (1, false, true), "{got:?}");
        assert_eq!(g.3, "");
    }
    assert_eq!(std::fs::read_to_string(tree.join("a")).unwrap(), "a\n");
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "old\n");
}

#[test]
fn a_mount_point_is_not_moved() {
    let d = tempfile::tempdir().unwrap();
    let fsb = d.path().join("fsB");
    let m = fsb.join("mnt");
    std::fs::create_dir_all(&m).unwrap();
    std::fs::write(m.join("keep"), "k\n").unwrap();
    // The seam names the target as a filesystem root: it is a mount point.
    let h = host();
    let id = h.file(m.to_str().unwrap()).absent().id();
    let e = env(d.path(), Some(m.clone()));
    let (err, r) = run_op(&h, id, &e);
    assert_eq!((r.rc, r.changed), (1, false), "{err}");
    assert!(
        err.contains(&format!(
            "telepatch: file {}: is a mount point",
            m.display()
        )),
        "{err}"
    );
    assert!(m.join("keep").is_file());
    nothing_saved(&e, &r);
    // `.unpack` over it fails the same way, before the swap.
    let sum = cached(d.path(), &tarball(d.path(), "m", &[("n", "n\n")]));
    let f = h.fetch("https://example.com/m").sha256(&sum);
    let up = h.file(m.to_str().unwrap()).unpack(&f).id();
    let e = with_cache(d.path(), env(d.path(), Some(m.clone())));
    let (err, r) = run_op(&h, up, &e);
    assert_eq!((r.rc, r.changed), (1, false), "{err}");
    assert!(err.contains("is a mount point"), "{err}");
    assert!(m.join("keep").is_file() && !m.join("n").exists());
    // A file whose root is not a directory cannot be backed up.
    let file = fsb.join("bound");
    std::fs::write(&file, "old\n").unwrap();
    let c = h.file(file.to_str().unwrap()).content("new\n").id();
    let e = env(d.path(), Some(file.clone()));
    let (err, r) = run_op(&h, c, &e);
    assert_eq!((r.rc, r.changed), (1, false), "{err}");
    assert!(
        err.contains(&format!(
            "telepatch: file {}: is a mount point",
            file.display()
        )),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "old\n");
}

#[test]
fn a_tree_holding_a_nested_mount_is_not_moved() {
    let d = tempfile::tempdir().unwrap();
    let t = d.path().join("tree");
    let sub = t.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("inside"), "i\n").unwrap();
    let h = host();
    let id = h.file(t.to_str().unwrap()).absent().id();
    let e = env(d.path(), Some(sub.clone()));
    let (err, r) = run_op(&h, id, &e);
    assert_eq!((r.rc, r.changed), (1, false), "{err}");
    assert!(
        err.contains(&format!(
            "telepatch: file {}: a filesystem is mounted under it ({})",
            t.display(),
            sub.display()
        )),
        "{err}"
    );
    assert!(sub.join("inside").is_file());
    nothing_saved(&e, &r);
    // `.unpack` over the tree refuses the same way.
    let sum = cached(d.path(), &tarball(d.path(), "n", &[("n", "n\n")]));
    let f = h.fetch("https://example.com/n").sha256(&sum);
    let up = h.file(t.to_str().unwrap()).unpack(&f).id();
    let e = with_cache(d.path(), env(d.path(), Some(sub.clone())));
    let (err, r) = run_op(&h, up, &e);
    assert_eq!((r.rc, r.changed), (1, false), "{err}");
    assert!(err.contains("a filesystem is mounted under it"), "{err}");
    assert!(sub.join("inside").is_file() && !t.join("n").exists());
}

// ---- controller placement and secrets, through the executor ----

use askama::Template;
use telepatch::Secret;
use telepatch::local::{local_dir, local_file};

#[derive(Clone)]
struct W {
    d: PathBuf,
    src: PathBuf,
    dest: PathBuf,
    marker_file: PathBuf,
}

/// Copies the run's whole undo tree to `<d>/seen` (a mid-run look, through
/// the shell pool), and its modes to `<d>/modes`.
const DUMP: &str = "#!/bin/bash\ncp -a \"$S/$R/undo\" \"$D/seen\" && stat -c '%a %n' \"$S/$R/undo\"/* \"$S/$R/undo\"/*/* > \"$D/modes\"; grep -c -f \"$M\" \"$S/$R/undo\"/*/*/old > \"$D/count\"; true\n";

#[role]
async fn place_file(h: &Host<W>) -> Result<(), TraceError> {
    let (dest, src) = (
        h.vars.dest.display().to_string(),
        h.vars.src.display().to_string(),
    );
    h.file(&dest).from(local_file(&src));
    let (d, m) = (
        h.vars.d.display().to_string(),
        h.vars.marker_file.display().to_string(),
    );
    let x = h.vars.d.join("dump.sh").display().to_string();
    sh!(
        h,
        "D={d} M={m} S=\"$TP_FETCH_STAGE\" R=\"$__tp_run\" bash {x}"
    );
    Ok(())
}

#[role]
async fn place_dir(h: &Host<W>) -> Result<(), TraceError> {
    let (dest, src) = (
        h.vars.dest.display().to_string(),
        h.vars.src.display().to_string(),
    );
    h.file(&dest).from(local_dir(&src));
    let (d, m) = (
        h.vars.d.display().to_string(),
        h.vars.marker_file.display().to_string(),
    );
    let x = h.vars.d.join("dump.sh").display().to_string();
    sh!(
        h,
        "D={d} M={m} S=\"$TP_FETCH_STAGE\" R=\"$__tp_run\" bash {x}"
    );
    Ok(())
}

fn place_file_run(h: Host<W>) -> PlaybookFuture {
    Box::pin(async move { place_file(&h).await })
}
fn place_dir_run(h: Host<W>) -> PlaybookFuture {
    Box::pin(async move { place_dir(&h).await })
}

fn exec(pb: &Playbook<W>, w: W, log: Option<PathBuf>) -> (telepatch::report::Report, String) {
    let out = Rc::new(RefCell::new(String::new()));
    let report = run(
        pb,
        vec![HostSpec {
            groups: Vec::new(),
            name: "h1".into(),
            target: Target::Local(
                LocalTransport::new()
                    .uid(0)
                    .sandboxed_at(&w.d)
                    .env("TP_FETCH_STAGE", w.d.join("stage").to_str().unwrap()),
            ),
            vars: w,
        }],
        RunOpts {
            log,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    let s = out.borrow().clone();
    (report, s)
}

fn world(d: &Path) -> W {
    std::fs::write(d.join("dump.sh"), DUMP).unwrap();
    std::fs::write(d.join("marker"), "OLD-BODY-MARKER\n").unwrap();
    W {
        d: d.to_path_buf(),
        src: d.join("src"),
        dest: d.join("dest"),
        marker_file: d.join("marker"),
    }
}

#[test]
fn a_controller_file_over_an_existing_one_backs_it_up() {
    let d = tempfile::tempdir().unwrap();
    let w = world(d.path());
    std::fs::write(&w.src, "from the controller\n").unwrap();
    std::fs::write(&w.dest, "old on the host\n").unwrap();
    let (report, out) = exec(&Playbook::new("pf", place_file_run), w.clone(), None);
    assert_eq!(report.exit_code(), 0, "{}\n{out}", report.render());
    assert_eq!(
        std::fs::read_to_string(&w.dest).unwrap(),
        "from the controller\n"
    );
    let seen = d.path().join("seen");
    let old = std::fs::read_dir(&seen)
        .unwrap()
        .flatten()
        .next()
        .expect("a user dir")
        .path();
    let b = std::fs::read_dir(&old)
        .unwrap()
        .flatten()
        .next()
        .expect("an op dir")
        .path()
        .join("old");
    assert_eq!(std::fs::read_to_string(b).unwrap(), "old on the host\n");
}

#[test]
fn a_controller_dir_copies_replaced_files_and_lists_what_it_created() {
    let d = tempfile::tempdir().unwrap();
    let w = world(d.path());
    std::fs::create_dir_all(w.src.join("sub")).unwrap();
    std::fs::write(w.src.join("a.txt"), "new a\n").unwrap();
    std::fs::write(w.src.join("keep.txt"), "same\n").unwrap();
    std::fs::write(w.src.join("sub/b.txt"), "new b\n").unwrap();
    std::fs::create_dir_all(&w.dest).unwrap();
    std::fs::write(w.dest.join("a.txt"), "old a\n").unwrap();
    std::fs::write(w.dest.join("keep.txt"), "same\n").unwrap();
    std::fs::write(w.dest.join("extra.txt"), "not ours\n").unwrap();
    let (report, out) = exec(&Playbook::new("pd", place_dir_run), w.clone(), None);
    assert_eq!(report.exit_code(), 0, "{}\n{out}", report.render());
    assert_eq!(
        std::fs::read_to_string(w.dest.join("a.txt")).unwrap(),
        "new a\n"
    );
    let seen = d.path().join("seen");
    let user_dir = std::fs::read_dir(&seen)
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    let op = std::fs::read_dir(&user_dir)
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    // The replaced file is copied; the unchanged one and the extra are not.
    assert_eq!(
        std::fs::read_to_string(op.join("old/a.txt")).unwrap(),
        "old a\n"
    );
    assert!(!op.join("old/keep.txt").exists());
    assert!(!op.join("old/extra.txt").exists());
    // The manifest lists what the op created, and nothing else.
    let m = std::fs::read_to_string(op.join("created")).unwrap();
    let lines: Vec<&str> = m.lines().collect();
    assert!(
        lines.contains(&format!("d {}", w.dest.join("sub").display()).as_str()),
        "{m}"
    );
    assert!(
        lines.contains(&format!("f {}", w.dest.join("sub/b.txt").display()).as_str()),
        "{m}"
    );
    assert_eq!(lines.len(), 2, "{m}");
}

#[test]
fn a_controller_dir_into_a_new_top_lists_the_top() {
    let d = tempfile::tempdir().unwrap();
    let w = W {
        dest: d.path().join("p/q/dest"),
        ..world(d.path())
    };
    std::fs::create_dir_all(&w.src).unwrap();
    std::fs::write(w.src.join("a.txt"), "a\n").unwrap();
    let (report, out) = exec(&Playbook::new("pd", place_dir_run), w.clone(), None);
    assert_eq!(report.exit_code(), 0, "{}\n{out}", report.render());
    let seen = d.path().join("seen");
    let user_dir = std::fs::read_dir(&seen)
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    let op = std::fs::read_dir(&user_dir)
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    let m = std::fs::read_to_string(op.join("created")).unwrap();
    assert!(
        m.lines()
            .any(|l| l == format!("d {}", d.path().join("p").display())),
        "{m}"
    );
    assert!(
        m.lines()
            .any(|l| l == format!("f {}", w.dest.join("a.txt").display())),
        "{m}"
    );
}

#[derive(Template, Clone)]
#[template(source = "tok={{ token }}\n", ext = "txt")]
struct Conf {
    token: Secret,
}

#[role]
async fn secret_over(h: &Host<W>) -> Result<(), TraceError> {
    let dest = h.vars.dest.display().to_string();
    h.file(&dest).content(Conf {
        token: Secret::literal("NEW-SECRET-VALUE"),
    });
    let (d, m) = (
        h.vars.d.display().to_string(),
        h.vars.marker_file.display().to_string(),
    );
    let x = h.vars.d.join("dump.sh").display().to_string();
    sh!(
        h,
        "D={d} M={m} S=\"$TP_FETCH_STAGE\" R=\"$__tp_run\" bash {x}"
    );
    Ok(())
}

fn secret_run(h: Host<W>) -> PlaybookFuture {
    Box::pin(async move { secret_over(&h).await })
}

fn all_files(d: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
        let p = e.path();
        if e.file_type().unwrap().is_dir() {
            all_files(&p, out);
        } else {
            out.push(p);
        }
    }
}

#[test]
fn a_secret_file_over_an_old_body_leaks_nothing_and_the_backup_goes_with_the_run() {
    let d = tempfile::tempdir().unwrap();
    let w = world(d.path());
    let old_body = "OLD-BODY-MARKER body\n";
    std::fs::write(&w.dest, old_body).unwrap();
    let log = d.path().join("events.jsonl");
    let pb = Playbook::new("sec", secret_run);
    let (report, out) = exec(&pb, w.clone(), Some(log.clone()));
    assert_eq!(report.exit_code(), 0, "{}\n{out}", report.render());
    assert_eq!(
        std::fs::read_to_string(&w.dest).unwrap(),
        "tok=NEW-SECRET-VALUE\n"
    );
    // Mid-run, through the shell pool: the backup held the old body, and
    // every undo directory was 0700.
    let count = std::fs::read_to_string(d.path().join("count")).unwrap();
    assert_eq!(count.trim(), "1", "backup did not hold the marker: {count}");
    let modes = std::fs::read_to_string(d.path().join("modes")).unwrap();
    assert!(!modes.is_empty(), "no modes");
    for l in modes.lines() {
        assert!(l.starts_with("700 "), "undo directory not 0700: {l}");
    }
    // Nowhere it must not be: output, report, log, plan.
    let events = std::fs::read_to_string(&log).unwrap();
    let rendered = report.render();
    let os = telepatch::facts::os::Os::fixture("ubuntu2404").unwrap();
    let fx = telepatch::testing::FixtureHost::new("h", os, Rc::new(w.clone()));
    let plan = telepatch::testing::plan(&pb, &fx);
    for (what, text) in [
        ("stdout", &out),
        ("report", &rendered),
        ("log", &events),
        ("plan", &plan),
    ] {
        for needle in [
            "OLD-BODY-MARKER".to_string(),
            "NEW-SECRET-VALUE".to_string(),
            sha_of(old_body.as_bytes()),
            sha_of(b"tok=NEW-SECRET-VALUE\n"),
        ] {
            assert!(!text.contains(&needle), "{what} holds {needle}:\n{text}");
        }
    }
    // After the host ends the backup is gone, everywhere under the stage.
    let mut left = Vec::new();
    all_files(&d.path().join("stage"), &mut left);
    assert!(left.is_empty(), "left behind: {left:?}");
    let runs: Vec<_> = std::fs::read_dir(d.path().join("stage"))
        .map(|r| r.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(runs.is_empty(), "stage run dir left: {runs:?}");
}

#[test]
fn a_secret_write_prints_no_body_in_its_trailer_or_stream() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("s.conf");
    std::fs::write(&p, "OLD-BODY-MARKER\n").unwrap();
    let h = host();
    let id = h
        .file(p.to_str().unwrap())
        .content(Conf {
            token: Secret::literal("NEW-SECRET-VALUE"),
        })
        .id();
    let e = env(d.path(), None);
    let script = match &h.snapshot().nodes[id.0] {
        Node::Op(op) => render_op(op, ID, NONCE, &h.os),
        _ => panic!(),
    };
    let out = Command::new("/bin/bash")
        .arg("-c")
        .arg(with_helpers(&script))
        .env("TP_SANDBOX", e.stage.parent().unwrap())
        .env("TP_FETCH_STAGE", &e.stage)
        .env("__tp_run", RUN)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("__tp_"), "no trailer: {text}");
    for needle in [
        "OLD-BODY-MARKER".to_string(),
        "NEW-SECRET-VALUE".to_string(),
        sha_of(b"OLD-BODY-MARKER\n"),
        sha_of(b"tok=NEW-SECRET-VALUE\n"),
    ] {
        assert!(!text.contains(&needle), "{needle} in {text}");
    }
    // The backup holds it, in a 0700 directory.
    let b = stage_id(&e);
    assert_eq!(
        std::fs::read_to_string(b.join("old")).unwrap(),
        "OLD-BODY-MARKER\n"
    );
    assert_eq!(
        std::fs::metadata(&b).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(b.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}
