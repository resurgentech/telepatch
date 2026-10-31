//! `tp check-understanding` run as a binary on the fixtures.

use assert_cmd::Command;

fn fixture(name: &str) -> String {
    format!(
        "{}/tests/fixtures/understanding/{name}",
        env!("CARGO_MANIFEST_DIR")
    )
}

#[test]
fn good_fixture_passes() {
    Command::cargo_bin("tp")
        .unwrap()
        .args(["check-understanding", &fixture("good.md")])
        .assert()
        .success()
        .stdout("ok\n");
}

#[test]
fn bad_fixture_fails_with_one_line_per_problem() {
    let out = Command::cargo_bin("tp")
        .unwrap()
        .args(["check-understanding", &fixture("bad.md")])
        .assert()
        .failure()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let s = String::from_utf8(out).unwrap();
    let f = fixture("bad.md");
    assert!(s.contains(&format!("{f}:5:probably")), "{s}");
    assert!(
        s.contains(&format!("{f}:6: source line without a YYYY-MM-DD date")),
        "{s}"
    );
    assert!(s.contains(&format!("{f}:30:could be")), "{s}");
    assert!(
        s.contains("missing heading '## Is the method current'"),
        "{s}"
    );
    assert!(s.contains("no 'Decision:' line"), "{s}");
    assert!(!s.contains("ok"), "{s}");
}

#[test]
fn missing_file_fails() {
    Command::cargo_bin("tp")
        .unwrap()
        .args(["check-understanding", "/nonexistent/x.md"])
        .assert()
        .failure();
}

#[test]
fn skill_template_passes() {
    let p = format!(
        "{}/../../.claude/skills/ansible-port/templates/understanding.md",
        env!("CARGO_MANIFEST_DIR")
    );
    Command::cargo_bin("tp")
        .unwrap()
        .args(["check-understanding", &p])
        .assert()
        .success()
        .stdout("ok\n");
    // The template carries no skill-feedback section: trial docs add it.
    let out = Command::cargo_bin("tp")
        .unwrap()
        .args(["check-understanding", &p, "--require-feedback"])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("missing heading '## skill-feedback'")
    );
}
