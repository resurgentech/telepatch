//! The playbook runner library: `cli::run_with_args` and `testing::plan_for`.

use std::path::PathBuf;

use serde::Deserialize;
use telepatch::cli::run_with_args;
use telepatch::transport::local::LocalTransport;
use telepatch::transport::ssh::SshTransport;
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
    run_with_args(
        &args(a),
        &pbs(),
        || LocalTransport::new().uid(0),
        SshTransport::new,
    )
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

// ---- run --host over a stub ssh ----

/// Write an executable via a child `sh` (no write fd in this process, so
/// no ETXTBSY from a sibling thread's fork).
fn write_exe(p: &std::path::Path, body: &str) {
    use std::io::Write;
    let mut c = std::process::Command::new("sh")
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

/// The stub from `tests/executor_ssh.rs`, minus the channel capture, plus
/// one line per invocation in `argv` (its whole command line).
const SSH: &str = r#"
d="$TP_STUB_DIR"
printf '%s\n' "$*" >> "$d/argv"
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
if [ -n "$master" ]; then
  echo $$ > "$cp.pid"
  : > "$cp"
  exec sleep 100000
fi
[ -e "$cp" ] || { echo "stub ssh: no master at $cp" >&2; exit 255; }
exec bash -c "exec $*"
"#;

/// A fake `id` that reports uid 0, so the channel needs no sudo.
const ID: &str = r#"
if [ "$1" = -u ]; then echo 0; exit 0; fi
exec /usr/bin/id "$@"
"#;

struct Stub {
    dir: tempfile::TempDir,
}

impl Stub {
    fn new() -> Stub {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        write_exe(&dir.path().join("ssh"), SSH);
        write_exe(&bin.join("id"), ID);
        Stub { dir }
    }

    fn transport(&self) -> SshTransport {
        let path = format!(
            "{}:{}",
            self.dir.path().join("bin").display(),
            std::env::var("PATH").unwrap()
        );
        SshTransport::new()
            .ssh(&self.dir.path().join("ssh").display().to_string())
            .env("TP_STUB_DIR", &self.dir.path().display().to_string())
            .env("PATH", &path)
    }

    fn argv(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("argv"))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }
}

fn go_ssh(stub: &Stub, a: &[&str]) -> (String, i32) {
    run_with_args(
        &args(a),
        &pbs(),
        || LocalTransport::new().uid(0),
        || stub.transport(),
    )
}

fn host_vars(d: &std::path::Path) -> String {
    let dir = d.join("out");
    std::fs::create_dir_all(&dir).unwrap();
    vars_file(
        d,
        &format!("dir = {:?}\ngreeting = \"hello\"\n", dir.to_str().unwrap()),
    )
}

#[test]
fn run_needs_exactly_one_target() {
    let (out, code) = go(&["run", "greet"]);
    assert_eq!(code, 2);
    assert!(out.contains("--local") && out.contains("--host"), "{out}");
    let (out, code) = go(&["run", "greet", "--local", "--host", "a@b"]);
    assert_eq!(code, 2);
    assert!(out.contains("cannot be used with"), "{out}");
}

#[test]
fn ssh_flags_need_host_and_option_needs_equals() {
    let (out, code) = go(&["run", "greet", "--local", "--ssh-key", "/k"]);
    assert_eq!(code, 2);
    assert!(out.contains("--local"), "{out}");
    let (out, code) = go(&["run", "greet", "--local", "--ssh-option", "A=b"]);
    assert_eq!(code, 2);
    assert!(out.contains("--local"), "{out}");
    let (out, code) = go(&["run", "greet", "--host", "a@b", "--ssh-option", "nope"]);
    assert_eq!(code, 2);
    assert!(out.contains("K=V"), "{out}");
}

#[test]
fn run_two_hosts_over_ssh() {
    let d = tempfile::tempdir().unwrap();
    let v = host_vars(d.path());
    let stub = Stub::new();
    let (out, code) = go_ssh(
        &stub,
        &[
            "run", "greet", "--host", "bot@one", "--host", "bot@two", "--vars", &v,
        ],
    );
    assert_eq!(code, 0, "{out}");
    let rows: Vec<&str> = out
        .lines()
        .filter(|l| {
            ["bot@one", "bot@two"]
                .iter()
                .any(|h| l.split_whitespace().take(2).eq([*h, "changed"]))
        })
        .collect();
    assert_eq!(rows.len(), 2, "{out}");
    assert!(out.contains("HOST"), "{out}");
    assert_eq!(
        std::fs::read_to_string(d.path().join("out/greeting")).unwrap(),
        "hello\n"
    );
    let argv = stub.argv();
    for h in ["bot@one", "bot@two"] {
        assert!(
            argv.iter().any(|l| l.split(' ').any(|w| w == h)),
            "{h}: {argv:?}"
        );
    }
}

#[test]
fn ssh_key_and_options_reach_every_host() {
    let d = tempfile::tempdir().unwrap();
    let v = host_vars(d.path());
    let stub = Stub::new();
    let (out, code) = go_ssh(
        &stub,
        &[
            "run",
            "greet",
            "--host",
            "bot@one",
            "--host",
            "bot@two",
            "--ssh-key",
            "/keys/id_x",
            "--ssh-option",
            "Port=2222",
            "--ssh-option",
            "ConnectTimeout=7",
            "--vars",
            &v,
        ],
    );
    assert_eq!(code, 0, "{out}");
    let argv = stub.argv();
    for h in ["bot@one", "bot@two"] {
        let lines: Vec<&String> = argv
            .iter()
            .filter(|l| l.split(' ').any(|w| w == h))
            .collect();
        assert!(!lines.is_empty(), "{h}: {argv:?}");
        for l in lines {
            for want in [
                "-i /keys/id_x",
                "-o IdentitiesOnly=yes",
                "-o Port=2222",
                "-o ConnectTimeout=7",
            ] {
                assert!(l.contains(want), "{h}: `{want}` not in `{l}`");
            }
        }
    }
}
