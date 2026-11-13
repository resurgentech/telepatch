//! The undo and fetch helpers are defined once per shell, not in each op
//! script (T126). An op script carries one guard line; a shell that lost
//! the helpers, or has another build's, fails the op cleanly with rc 125
//! and a trailer; `trailer::with_helpers` is what a hand-run script uses.

use std::cell::RefCell;
use std::process::Command;
use std::rc::Rc;

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::facts::os::Os;
use telepatch::ops::undo::{HELPERS, guard_line, helpers_hash};
use telepatch::testing::sandbox;
use telepatch::trailer::{FileResult, render_op, split_stream, with_helpers};
use telepatch::transport::local::LocalTransport;
use telepatch::{Host, Node, Playbook, PlaybookFuture, sh};

const NONCE: &str = "t0n";

fn host() -> Host<()> {
    let os = Os::parse("ID=debian\nVERSION_ID=12\n").unwrap();
    Host::new("t", os, Rc::new(()))
}

fn script_for(path: &str) -> String {
    let h = host();
    let id = h.file(path).content("x\n").id();
    match &h.snapshot().nodes[id.0] {
        Node::Op(op) => render_op(op, 7, NONCE, &h.os),
        _ => panic!("not an op"),
    }
}

/// `prefix` then the op, in one `bash -c`: (stderr, the op's trailer).
fn run_with_prefix(prefix: &str, path: &str) -> (String, FileResult) {
    let sb = sandbox();
    let script = format!("{prefix}{}", script_for(path));
    let mut c = Command::new("/bin/bash");
    c.arg("-c").arg(script).env_remove("__tp_run");
    for (k, v) in sb.env() {
        c.env(k, v);
    }
    c.env("__tp_run", "r1");
    let out = c.output().unwrap();
    let stream = String::from_utf8(out.stdout).unwrap();
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    let mut v = split_stream(&stream, NONCE);
    assert_eq!(v.len(), 1, "stream: {stream:?}\n{err}");
    (err, v.remove(0).1.typed::<FileResult>().unwrap())
}

#[test]
fn a_rendered_file_op_carries_the_guard_and_not_the_helpers() {
    let s = script_for("/x/y");
    assert!(!s.contains("tp_undo() {"), "{s}");
    assert!(!s.contains("tp_fsroot() {"), "{s}");
    assert!(!s.contains("tp_sbx() {"), "{s}");
    assert!(s.contains(&guard_line()), "{s}");
    assert!(guard_line().contains(&helpers_hash()), "{}", guard_line());
    assert_eq!(helpers_hash().len(), 16);
}

#[test]
fn the_hash_is_a_stable_function_of_the_helper_text() {
    // FNV-1a 64 of "" is its offset basis; the same text hashes the same.
    assert_eq!(telepatch::ops::undo::fnv1a64(b""), 0xcbf29ce484222325);
    assert_eq!(telepatch::ops::undo::fnv1a64(b"a"), 0xaf63dc4c8601ec8c);
    assert_eq!(
        helpers_hash(),
        format!("{:016x}", telepatch::ops::undo::fnv1a64(HELPERS.as_bytes()))
    );
}

#[test]
fn an_op_in_a_shell_with_the_helpers_runs() {
    let sb = sandbox();
    let p = sb.fs.join("ok.conf");
    let (err, r) = run_with_prefix(&with_helpers(""), p.to_str().unwrap());
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "x\n");
}

#[test]
fn a_shell_that_lost_a_function_fails_the_op_with_125_and_a_trailer() {
    let sb = sandbox();
    let p = sb.fs.join("lost.conf");
    let prefix = with_helpers("unset -f tp_save\n");
    let (err, r) = run_with_prefix(&prefix, p.to_str().unwrap());
    assert_eq!(r.rc, 125, "{err}");
    assert!(!r.changed);
    assert!(
        err.contains(&format!(
            "telepatch: shell helpers missing or stale (want {})",
            helpers_hash()
        )),
        "{err}"
    );
    assert!(!p.exists());
}

#[test]
fn a_shell_from_another_build_fails_the_op_the_same_way() {
    let sb = sandbox();
    let p = sb.fs.join("stale.conf");
    let prefix = with_helpers("__tp_hv=0123456789abcdef\n");
    let (err, r) = run_with_prefix(&prefix, p.to_str().unwrap());
    assert_eq!(r.rc, 125, "{err}");
    assert!(err.contains("shell helpers missing or stale"), "{err}");
    assert!(!p.exists());
    // And a shell with no helpers at all.
    let (err, r) = run_with_prefix("", p.to_str().unwrap());
    assert_eq!(r.rc, 125, "{err}");
    assert!(!p.exists());
}

#[test]
fn a_hand_run_script_with_helpers_but_no_run_saves_nothing() {
    let sb = sandbox();
    let p = sb.fs.join("hand.conf");
    std::fs::write(&p, "old\n").unwrap();
    let script = format!("{}{}", with_helpers(""), script_for(p.to_str().unwrap()));
    let mut c = Command::new("/bin/bash");
    c.arg("-c").arg(script).env_remove("__tp_run");
    for (k, v) in sb.env() {
        c.env(k, v);
    }
    let out = c.output().unwrap();
    let stream = String::from_utf8(out.stdout).unwrap();
    let mut v = split_stream(&stream, NONCE);
    let r = v.remove(0).1.typed::<FileResult>().unwrap();
    assert_eq!((r.rc, r.changed, r.backup.as_str()), (0, true, ""));
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "x\n");
}

// ---- the shells the executor opens -----------------------------------------

fn seen_pb(h: Host<String>) -> PlaybookFuture {
    Box::pin(async move {
        let out = h.vars.clone();
        sh!(
            h,
            "{{ echo \"$__tp_hv\"; declare -F tp_save tp_sbx tp_fsroot; }} > {out}"
        );
        Ok(())
    })
}

#[test]
fn a_root_shell_has_the_helpers_and_their_hash() {
    let sb = sandbox();
    let out = sb.run.join("seen");
    let o = Rc::new(RefCell::new(String::new()));
    let pb = Playbook::new("seen", seen_pb);
    let r = run(
        &pb,
        vec![HostSpec {
            groups: Vec::new(),
            name: "h1".into(),
            target: Target::Local(LocalTransport::new().uid(0).sandboxed(&sb)),
            vars: out.display().to_string(),
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Capture(o.clone()),
        },
    );
    assert_eq!(r.exit_code(), 0, "{}\n{}", r.render(), o.borrow());
    let seen = std::fs::read_to_string(&out).unwrap();
    let mut l = seen.lines();
    assert_eq!(l.next(), Some(helpers_hash().as_str()), "{seen}");
    assert_eq!(l.collect::<Vec<_>>(), ["tp_save", "tp_sbx", "tp_fsroot"]);
}
