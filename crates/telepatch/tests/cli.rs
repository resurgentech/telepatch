//! The playbook runner library: `cli::run_with_args` and `testing::plan_for`.

use std::path::PathBuf;

use serde::Deserialize;
use telepatch::cli::run_with_args;
use telepatch::transport::local::LocalTransport;
use telepatch::{Host, Playbook, PlaybookFuture, TraceError, role, sh};

#[derive(Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
struct Vars {
    dir: PathBuf,
    greeting: String,
}

#[role]
async fn greet(h: &Host<Vars>) -> Result<(), TraceError> {
    let f = h.vars.dir.join("greeting").to_str().unwrap().to_string();
    h.file(&f).content(format!("{}\n", h.vars.greeting));
    sh!(h, "echo hi");
    Ok(())
}

fn entry(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move { greet(&h).await })
}

fn pbs() -> Vec<Playbook<Vars>> {
    vec![Playbook {
        name: "greet",
        run: entry,
    }]
}

fn args(a: &[&str]) -> Vec<String> {
    std::iter::once("pb")
        .chain(a.iter().copied())
        .map(String::from)
        .collect()
}

fn go(a: &[&str]) -> (String, i32) {
    run_with_args(&args(a), &pbs(), || LocalTransport::new().uid(0))
}

fn vars_file(d: &std::path::Path, body: &str) -> String {
    let p = d.join("vars.toml");
    std::fs::write(&p, body).unwrap();
    p.to_str().unwrap().to_string()
}

#[test]
fn list_prints_names() {
    let (out, code) = go(&["list"]);
    assert_eq!((out.as_str(), code), ("greet\n", 0));
}

#[test]
fn plan_distro_snapshot() {
    let d = tempfile::tempdir().unwrap();
    let v = vars_file(d.path(), "dir = \"/srv\"\ngreeting = \"hello\"\n");
    let (out, code) = go(&["plan", "greet", "--distro", "ubuntu2404", "--vars", &v]);
    assert_eq!(code, 0, "{out}");
    insta::assert_snapshot!("cli_plan_ubuntu2404", out);
}

#[test]
fn plan_for_matches_cli_and_is_deterministic() {
    let vars = Vars {
        dir: "/srv".into(),
        greeting: "hello".into(),
    };
    let a = telepatch::testing::plan_for("fedora44", &pbs()[0], vars.clone());
    let b = telepatch::testing::plan_for("fedora44", &pbs()[0], vars);
    assert_eq!(a, b);
    assert!(a.starts_with("host fedora44 (os: RedHat/Fedora 44)"), "{a}");
}

#[test]
fn plan_for_knows_all_downstream_distros() {
    for d in [
        "debian12",
        "debian13",
        "ubuntu2404",
        "ubuntu2604",
        "fedora43",
        "fedora44",
        "rockylinux9",
        "rockylinux10",
        "almalinux9",
        "almalinux10",
        "amazonlinux2023",
        "arch",
        "omarchy",
        "pop-os",
        "omarchy-host",
    ] {
        let s = telepatch::testing::plan_for(d, &pbs()[0], Vars::default());
        assert!(s.starts_with(&format!("host {d} ")), "{d}: {s}");
    }
}

#[test]
fn unknown_distro_lists_fixtures() {
    let (out, code) = go(&["plan", "greet", "--distro", "nope"]);
    assert_eq!(code, 2);
    assert!(
        out.contains("unknown distro `nope`") && out.contains("ubuntu2404"),
        "{out}"
    );
}

#[test]
fn unknown_playbook_lists_names() {
    let (out, code) = go(&["plan", "nope", "--distro", "arch"]);
    assert_eq!(code, 2);
    assert_eq!(out, "unknown playbook `nope`; available: greet\n");
}

#[test]
fn bad_toml_is_a_clear_error() {
    let d = tempfile::tempdir().unwrap();
    let v = vars_file(d.path(), "dir = \n");
    let (out, code) = go(&["plan", "greet", "--distro", "arch", "--vars", &v]);
    assert_eq!(code, 2);
    assert!(
        out.starts_with(&format!("cannot parse vars file {v}:")),
        "{out}"
    );
    let v = vars_file(d.path(), "dir = \"/x\"\ngreeting = \"a\"\nbogus = 1\n");
    let (out, code) = go(&["plan", "greet", "--distro", "arch", "--vars", &v]);
    assert_eq!(code, 2);
    assert!(out.contains("bogus"), "{out}");
    let (out, code) = go(&[
        "plan",
        "greet",
        "--distro",
        "arch",
        "--vars",
        "/no/such.toml",
    ]);
    assert_eq!(code, 2);
    assert!(
        out.starts_with("cannot read vars file /no/such.toml"),
        "{out}"
    );
}

#[test]
fn plan_without_distro_reads_local_os() {
    let d = tempfile::tempdir().unwrap();
    let v = vars_file(d.path(), "dir = \"/srv\"\ngreeting = \"hello\"\n");
    let (out, code) = go(&["plan", "greet", "--vars", &v]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.starts_with("note: local plan; facts assumed absent (no probes)\nhost local "),
        "{out}"
    );
}

#[test]
fn clap_error_is_exit_2() {
    let (out, code) = go(&["frobnicate"]);
    assert_eq!(code, 2);
    assert!(out.contains("frobnicate"), "{out}");
}

#[test]
fn run_local_twice() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("out");
    std::fs::create_dir(&dir).unwrap();
    let v = vars_file(
        d.path(),
        &format!("dir = {:?}\ngreeting = \"hello\"\n", dir.to_str().unwrap()),
    );
    let log = d.path().join("log.jsonl");
    let (out, code) = go(&[
        "run",
        "greet",
        "--local",
        "--vars",
        &v,
        "--log",
        log.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("HOST") && out.contains("changed"), "{out}");
    assert_eq!(
        std::fs::read_to_string(dir.join("greeting")).unwrap(),
        "hello\n"
    );
    assert!(std::fs::metadata(&log).unwrap().len() > 0);
    let (out, code) = go(&["run", "greet", "--local", "--vars", &v]);
    assert_eq!(code, 0, "{out}");
}

#[test]
fn run_requires_local() {
    let (out, code) = go(&["run", "greet"]);
    assert_eq!(code, 2);
    assert!(out.contains("--local"), "{out}");
}
