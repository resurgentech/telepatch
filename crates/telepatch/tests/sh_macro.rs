//! Behavioural tests for `sh!`: run the rendered body under bash.

use std::process::Command;
use std::rc::Rc;

use telepatch::facts::os::Os;
use telepatch::sh::quote;
use telepatch::{Host, Key, Node, OpSpec, sh};

fn host() -> Host<()> {
    let os = Os::parse("ID=debian\nVERSION_ID=12\n").unwrap();
    Host::new("t", os, Rc::new(()))
}

fn body(h: &Host<()>, id: telepatch::NodeId) -> String {
    match &h.snapshot().nodes[id.0] {
        Node::Op(op) => op.spec.script(&h.os),
        _ => panic!(),
    }
}

/// Run the body in `dir`; returns "rc changed skipped" plus raw stdout.
fn run(body: &str, dir: &std::path::Path) -> (String, String) {
    let script = format!("{body}\necho \"@@ $rc $changed $skipped\"");
    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .current_dir(dir)
        .output()
        .unwrap();
    let s = String::from_utf8(out.stdout).unwrap();
    let (before, after) = s.rsplit_once("@@ ").unwrap();
    (after.trim().to_string(), before.to_string())
}

#[test]
fn quote_cases() {
    assert_eq!(quote("abc"), "'abc'");
    assert_eq!(quote("a b"), "'a b'");
    assert_eq!(quote("it's"), r"'it'\''s'");
    assert_eq!(quote("$HOME"), "'$HOME'");
    assert_eq!(quote("`id`"), "'`id`'");
    assert_eq!(quote(""), "''");
    assert_eq!(quote("a\nb"), "'a\nb'");
}

#[test]
fn plain_command() {
    let h = host();
    let d = tempfile::tempdir().unwrap();
    let id = sh!(h, "echo hi > out.txt").id();
    assert_eq!(run(&body(&h, id), d.path()).0, "0 1 0");
    assert_eq!(
        std::fs::read_to_string(d.path().join("out.txt")).unwrap(),
        "hi\n"
    );
}

#[test]
fn failing_command() {
    let h = host();
    let d = tempfile::tempdir().unwrap();
    let id = sh!(h, "(exit 3)").id();
    assert_eq!(run(&body(&h, id), d.path()).0, "3 0 0");
}

#[test]
fn unless_true_and_false() {
    let h = host();
    let d = tempfile::tempdir().unwrap();
    let a = sh!(h, "touch ran", unless = "true").id();
    assert_eq!(run(&body(&h, a), d.path()).0, "0 0 1");
    assert!(!d.path().join("ran").exists());
    let b = sh!(h, "touch ran", unless = "false").id();
    assert_eq!(run(&body(&h, b), d.path()).0, "0 1 0");
    assert!(d.path().join("ran").exists());
}

#[test]
fn creates_present_and_absent() {
    let h = host();
    let d = tempfile::tempdir().unwrap();
    let a = sh!(h, "touch made", creates = "made").id();
    assert_eq!(run(&body(&h, a), d.path()).0, "0 1 0");
    assert_eq!(run(&body(&h, a), d.path()).0, "0 0 1");
}

#[test]
fn changed_if_match_and_no_match() {
    let h = host();
    let d = tempfile::tempdir().unwrap();
    let a = sh!(
        h,
        "echo 'installed 3 packages'",
        changed_if = "installed [1-9]"
    )
    .id();
    let (st, out) = run(&body(&h, a), d.path());
    assert_eq!(st, "0 1 0");
    assert!(
        out.contains("installed 3 packages"),
        "output streamed: {out}"
    );
    let b = sh!(
        h,
        "echo 'installed 0 packages' >&2",
        changed_if = "installed [1-9]"
    )
    .id();
    assert_eq!(run(&body(&h, b), d.path()).0, "0 0 0");
}

#[test]
fn changed_if_keeps_exit_status() {
    let h = host();
    let d = tempfile::tempdir().unwrap();
    let a = sh!(h, "echo installed 1; exit 4", changed_if = "installed").id();
    assert_eq!(run(&body(&h, a), d.path()).0.split(' ').next(), Some("4"));
}

#[test]
fn interpolation_quotes_values() {
    let h = host();
    let d = tempfile::tempdir().unwrap();
    let name = "a b'; touch pwned; echo '";
    let id = sh!(h, "printf %s {name} > out.txt").id();
    assert_eq!(run(&body(&h, id), d.path()).0, "0 1 0");
    assert_eq!(
        std::fs::read_to_string(d.path().join("out.txt")).unwrap(),
        name
    );
    assert!(!d.path().join("pwned").exists());
}

#[test]
fn named_args_interpolate_and_braces_escape() {
    let h = host();
    let d = tempfile::tempdir().unwrap();
    let f = "my file";
    let id = sh!(
        h,
        "echo '{{x}}' > {f}",
        creates = "./{f}",
        unless = "test -e other-{f}"
    )
    .id();
    let b = body(&h, id);
    assert!(b.contains("'my file'"), "{b}");
    assert!(b.contains("'{x}'"), "{b}");
    assert_eq!(run(&b, d.path()).0, "0 1 0");
    assert_eq!(
        std::fs::read_to_string(d.path().join("my file")).unwrap(),
        "{x}\n"
    );
}

#[test]
fn works_on_scope_and_chains() {
    let h = host();
    let s = h.as_user("deploy");
    let id = sh!(s, "echo hi")
        .writes([Key::file("/x")])
        .allow_fail()
        .id();
    let snap = h.snapshot();
    let Node::Op(op) = &snap.nodes[id.0] else {
        panic!()
    };
    assert!(op.allow_fail);
    assert!(op.writes.contains(&Key::file("/x")) && op.writes.len() == 1);
    assert!(matches!(op.spec, OpSpec::Shell(_)));
    let id2 = sh!(&h, "echo").id();
    let Node::Op(op2) = &h.snapshot().nodes[id2.0] else {
        panic!()
    };
    assert!(op2.writes.contains(&Key::Any) && !op2.allow_fail);
}

#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/*.rs");
}
