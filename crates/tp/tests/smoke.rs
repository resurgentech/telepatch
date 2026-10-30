use assert_cmd::Command;

#[test]
fn help_runs() {
    let out = Command::cargo_bin("tp")
        .unwrap()
        .arg("--help")
        .assert()
        .success();
    let text = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(text.contains("Usage"));
}
