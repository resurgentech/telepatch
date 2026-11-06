//! `tp import`: emitted text for the five corpus roles (insta snapshots),
//! the checked-in emitted crate, and the mapping rules on a fixture role.

use std::path::PathBuf;

use assert_cmd::Command;
use tp::import::{Options, Output, import};

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn run(input: PathBuf) -> Output {
    import(&Options {
        input,
        roles_path: None,
        base: None,
    })
    .unwrap()
}

fn corpus() -> Output {
    run(manifest().join("tests/corpus/roles"))
}

fn demo() -> Output {
    run(manifest().join("tests/fixtures/import/roles/demo"))
}

macro_rules! snapshot_role {
    ($test:ident, $role:literal) => {
        #[test]
        fn $test() {
            let out = corpus();
            insta::assert_snapshot!(out.file(concat!($role, ".rs")).unwrap());
        }
    };
}

snapshot_role!(snap_dev_common, "dev_common");
snapshot_role!(snap_src_repos, "src_repos");
snapshot_role!(snap_kernel_build, "kernel_build");
snapshot_role!(snap_rust_build, "rust_build");
snapshot_role!(snap_rust_kernel, "rust_kernel");

#[test]
fn snap_corpus_report() {
    insta::assert_snapshot!(corpus().report());
}

#[test]
fn snap_corpus_flags_and_lib() {
    let out = corpus();
    insta::assert_snapshot!(format!(
        "{}\n=====\n{}",
        out.file("lib.rs").unwrap(),
        out.file("flags.rs").unwrap()
    ));
}

#[test]
fn snap_demo() {
    let out = demo();
    insta::assert_snapshot!(format!(
        "{}\n=====\n{}",
        out.file("demo.rs").unwrap(),
        out.report()
    ));
}

/// `tests/emitted/src` holds exactly what `tp import` emits now. Regenerate
/// with `UPDATE_EMITTED=1 cargo test -p tp --test import` (or `just
/// import-regen`).
#[test]
fn checked_in_emitted_code_is_current() {
    let dir = manifest().join("tests/emitted/src");
    let update = std::env::var_os("UPDATE_EMITTED").is_some();
    for (name, text) in &corpus().files {
        let path = dir.join(name);
        if update {
            std::fs::write(&path, text).unwrap();
            continue;
        }
        let have = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("{} missing; run `just import-regen`", path.display()));
        assert_eq!(
            &have, text,
            "{name} is stale; run `just import-regen` and review the diff"
        );
    }
}

#[test]
fn demo_mappings() {
    let out = demo();
    let code = out.file("demo.rs").unwrap();
    // package modules
    assert!(code.contains("h.pkg([\"git\", \"curl\"]);"), "{code}");
    assert!(code.contains("h.pkg([\"wget\"]).yum();"));
    assert!(code.contains("h.pkg([\"linux\"]).build_dep();"));
    assert!(code.contains("h.pkg([\"pip\"]).pip().latest();"));
    assert!(code.contains("h.file(\"/tmp/old\").absent();"));
    assert!(code.contains(".dir()"));
    assert!(code.contains(".mode(0o755)"));
    // notify is a guard placeholder
    assert!(code.contains("TODO(port): roles/demo/tasks/main.yml:2 guard"));
    // register + changed_when false + ignore_errors + rc != 0 -> unless
    assert!(
        code.contains("unless = \"bash -lc 'which cargo'\""),
        "{code}"
    );
    // creates, with ~{{ item }} in the user's own scope
    assert!(code.contains("creates = \"~/.thing\""), "{code}");
    // vars bound locally; literal braces doubled
    assert!(code.contains("let rustup_version = &h.vars.rustup_version;"));
    assert!(code.contains("echo {rustup_version} ${{HOME}} {{ a; }}"));
    // unknown jinja keeps the raw text, still compiles as a literal
    assert!(code.contains("{{{{ somevar | default('x') }}}}"));
    // pipefail does not leak into the persistent shell
    assert!(code.contains("\"(set -o pipefail && curl -s x | sh)\""));
    // changed_when on an output marker
    assert!(code.contains("changed_if = \"DONE\""));
    // delegate_to is never guessed
    assert!(code.contains("TODO(port): roles/demo/tasks/main.yml:"));
    assert!(
        !code.contains("sh!(h, \"ls\")"),
        "delegated task must not be emitted"
    );
    // loops over admin_users and [root], with as_user
    assert!(code.contains("for item in h.vars.admin_users.iter() {"));
    assert!(code.contains("for item in [\"root\"] {"));
    assert!(code.contains("let s = h.as_user(item);"));
    // distro flags
    assert!(
        code.contains("if debian_based(&h.os) && !(ubuntu_based(&h.os)) {"),
        "{code}"
    );
    assert!(code.contains("h.os.distro == Distro::Fedora"));
    assert!(code.contains("h.os.version.major().is_some_and(|v| v >= 10)"));
    // unknown condition is inert, not guessed
    assert!(code.contains("if false /* TODO(port)"));
    // handlers are a TODO
    assert!(code.contains("handler"));
}

#[test]
fn demo_report_counts_reasons() {
    let r = demo().report();
    for reason in [
        "notify",
        "delegate_to",
        "when not translated",
        "unmapped module `get_url`",
        "set_fact",
        "failed_when",
        "handler",
        "unreachable branch",
        "Jinja not translated",
        "task-level loop",
    ] {
        assert!(r.contains(reason), "report lacks `{reason}`:\n{r}");
    }
}

#[test]
fn out_of_scope_is_todo_never_a_guess() {
    let out = run(manifest().join("tests/fixtures/ansible/out_of_scope/site.yml"));
    let lib = out.file("lib.rs").unwrap();
    assert!(lib.contains("TODO(port)"));
    assert!(lib.contains("vars_prompt"));
    assert!(lib.contains("galaxy_role"));
    assert!(out.report().contains("galaxy_role"));

    let out = run(manifest().join("tests/fixtures/import/oos/site.yml"));
    let code = out.file("play_1.rs").unwrap();
    assert!(code.contains("sh!(h, \"uptime\")"));
    assert!(
        !code.contains("sh!(h, \"date\")"),
        "delegated task must not be emitted: {code}"
    );
    assert!(code.contains("TODO(port)"));
}

#[test]
fn cli_report_and_out_dir() {
    let roles = manifest().join("tests/corpus/roles");
    let out = Command::cargo_bin("tp")
        .unwrap()
        .args(["import"])
        .arg(&roles)
        .arg("--report")
        .assert()
        .success();
    let text = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(text.contains("## dev_common"));

    let tmp = std::env::temp_dir().join(format!("tp-import-test-{}", std::process::id()));
    Command::cargo_bin("tp")
        .unwrap()
        .args(["import"])
        .arg(roles.join("src_repos"))
        .arg("--out")
        .arg(&tmp)
        .assert()
        .success();
    let written = std::fs::read_to_string(tmp.join("src_repos.rs")).unwrap();
    assert!(written.contains("pub async fn src_repos"));
    std::fs::remove_dir_all(&tmp).unwrap();
}
