//! The `file` op's comparisons need no `cmp` and no `diff` on the host
//! (diffutils is not in Rocky's minimal image; T81's live gate failed on
//! it): each case runs the wrapped op under `bash` with a PATH that has no
//! `cmp` and no `diff`, so a script that calls either reports `command not
//! found`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use telepatch::facts::os::Os;
use telepatch::trailer::{FileResult, render_op, split_stream};
use telepatch::{Host, Node, NodeId};

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

/// A PATH directory holding every `/usr/bin` entry except `cmp` and `diff`.
fn farm(d: &Path) -> PathBuf {
    let bin = d.join("farm");
    std::fs::create_dir_all(&bin).unwrap();
    for e in std::fs::read_dir("/usr/bin").unwrap().flatten() {
        let n = e.file_name();
        if matches!(n.to_str(), Some("cmp" | "diff")) {
            continue;
        }
        let _ = std::os::unix::fs::symlink(e.path(), bin.join(&n));
    }
    bin
}

/// (stderr, result) of the wrapped script run in `d` with the farm as PATH.
fn run(h: &Host<()>, id: NodeId, d: &Path) -> (String, FileResult) {
    let out = Command::new("/bin/bash")
        .arg("-c")
        .arg(telepatch::trailer::with_helpers(&wrapped(h, id)))
        .current_dir(d)
        .env("PATH", farm(d))
        .output()
        .unwrap();
    let stream = String::from_utf8(out.stdout).unwrap();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    let mut v = split_stream(&stream, NONCE);
    assert_eq!(v.len(), 1, "stream: {stream:?}\n{err}");
    let (_, t) = v.remove(0);
    let r = t.typed::<FileResult>().unwrap();
    // The undo fields are undo_capture.rs's; these tests compare rc and changed.
    let r = FileResult {
        rc: r.rc,
        changed: r.changed,
        ..Default::default()
    };
    assert!(!err.contains("command not found"), "{err}");
    (err, r)
}

fn res(rc: i32, changed: bool) -> FileResult {
    FileResult {
        rc,
        changed,
        ..Default::default()
    }
}

fn leftovers(d: &Path) -> Vec<String> {
    std::fs::read_dir(d)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains(".tp"))
        .collect()
}

#[test]
fn file_content_needs_no_cmp_or_diff_on_the_host() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("c.txt");
    let h = host();
    let id = h.file(p.to_str().unwrap()).content("one\ntwo\n").id();
    assert_eq!(run(&h, id, d.path()).1, res(0, true));
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "one\ntwo\n");
    assert_eq!(
        run(&h, id, d.path()).1,
        res(0, false),
        "same content: quiet"
    );
    std::fs::write(&p, "one\ntwo").unwrap();
    assert_eq!(run(&h, id, d.path()).1, res(0, true), "differs: rewritten");
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "one\ntwo\n");
    assert!(leftovers(d.path()).is_empty());
}

#[test]
fn file_validate_content_needs_no_cmp_or_diff_on_the_host() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("v.txt");
    let h = host();
    let id = h
        .file(p.to_str().unwrap())
        .content("good\n")
        .validate("test -s %s")
        .id();
    assert_eq!(run(&h, id, d.path()).1, res(0, true));
    assert_eq!(run(&h, id, d.path()).1, res(0, false));
    std::fs::write(&p, "other\n").unwrap();
    assert_eq!(run(&h, id, d.path()).1, res(0, true));
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "good\n");
    assert!(leftovers(d.path()).is_empty());
}

#[test]
fn file_validate_lines_needs_no_cmp_or_diff_on_the_host() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("l.conf");
    std::fs::write(&p, "a=1\nb=2\n").unwrap();
    let h = host();
    let id = h
        .file(p.to_str().unwrap())
        .line_re("^b=", "b=3")
        .line("c=4")
        .validate("test -s %s")
        .id();
    assert_eq!(run(&h, id, d.path()).1, res(0, true));
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "a=1\nb=3\nc=4\n");
    assert_eq!(run(&h, id, d.path()).1, res(0, false), "settled: quiet");
    assert!(leftovers(d.path()).is_empty());
}
