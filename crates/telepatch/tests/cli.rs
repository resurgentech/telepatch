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
    if h.in_group("web") {
        let w = h.vars.dir.join("web").to_str().unwrap().to_string();
        h.file(&w).content("in web\n");
    }
    Ok(())
}

fn entry(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move { greet(&h).await })
}

fn pbs() -> Vec<Playbook<Vars>> {
    vec![Playbook::new("greet", entry)]
}

fn args(a: &[&str]) -> Vec<String> {
    std::iter::once("pb")
        .chain(a.iter().copied())
        .map(String::from)
        .collect()
}

fn go(a: &[&str]) -> (String, i32) {
    let sb = telepatch::testing::sandbox();
    run_with_args(
        &args(a),
        &pbs(),
        || LocalTransport::new().uid(0).sandboxed(&sb),
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
    let sb = telepatch::testing::sandbox();
    run_with_args(
        &args(a),
        &pbs(),
        || LocalTransport::new().uid(0).sandboxed(&sb),
        || stub.transport().sandboxed(&sb),
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

// ---- vars per role (four layers) through `--vars` ----

mod sectioned {
    use super::*;
    use telepatch::cli::run_with_args;

    #[derive(Deserialize, serde::Serialize, Clone)]
    #[serde(deny_unknown_fields, default)]
    struct Hello {
        greeting: String,
        who: Vec<String>,
    }
    impl Default for Hello {
        fn default() -> Self {
            Hello {
                greeting: "hi".into(),
                who: vec!["root".into()],
            }
        }
    }

    #[derive(Deserialize, serde::Serialize, Default, Clone)]
    #[serde(deny_unknown_fields, default)]
    struct Vars {
        hello: Hello,
    }

    #[role]
    async fn hello(h: &Host<Vars>) -> Result<(), TraceError> {
        h.file("/tmp/g").content(format!(
            "{} {}\n",
            h.vars.hello.greeting,
            h.vars.hello.who.join(",")
        ));
        Ok(())
    }

    fn entry(h: Host<Vars>) -> PlaybookFuture {
        Box::pin(async move { hello(&h).await })
    }

    /// Defaults for the playbook `hello`: `who = ["x"]`, greeting "def".
    fn def_x() -> Vars {
        Vars {
            hello: Hello {
                greeting: "def".into(),
                who: vec!["x".into()],
            },
        }
    }

    /// Defaults for `hello2`: `who = ["y"]`, the role's own greeting.
    fn def_y() -> Vars {
        Vars {
            hello: Hello {
                who: vec!["y".into()],
                ..Hello::default()
            },
        }
    }

    fn go_with(pbs: &[Playbook<Vars>], a: &[&str]) -> (String, i32) {
        let sb = telepatch::testing::sandbox();
        run_with_args(
            &args(a),
            pbs,
            || LocalTransport::new().uid(0).sandboxed(&sb),
            SshTransport::new,
        )
    }

    fn go(defaults: Option<fn() -> Vars>, a: &[&str]) -> (String, i32) {
        let pb = Playbook::new("hello", entry);
        let pb = match defaults {
            Some(f) => pb.defaults(f),
            None => pb,
        };
        go_with(&[pb], a)
    }

    #[test]
    fn layers_reach_the_plan() {
        let d = tempfile::tempdir().unwrap();
        let v = vars_file(d.path(), "who = [\"a\"]\n[hello]\ngreeting = \"yo\"\n");
        let (out, code) = go(
            Some(def_x),
            &["plan", "hello", "--distro", "arch", "--vars", &v],
        );
        assert_eq!(code, 0, "{out}");
        // the plan carries the file body base64: "yo a\n"
        assert!(out.contains("eW8gYQo="), "{out}");
        // no file: the playbook defaults, then the role's own
        let (out, code) = go(Some(def_x), &["plan", "hello", "--distro", "arch"]);
        assert_eq!(code, 0, "{out}");
        // "def x\n": the playbook's greeting and who
        assert!(out.contains("ZGVmIHgK"), "{out}");
    }

    #[test]
    fn errors_name_file_and_line_and_exit_2() {
        let d = tempfile::tempdir().unwrap();
        for (body, line, what) in [
            ("\n\nnope = 1\n", 3, "nope"),
            ("[hello]\nwho = [\"a\"]\nnope = 1\n", 3, "nope"),
            ("\n[hello]\ngreeting = 5\n", 3, "invalid type"),
        ] {
            let v = vars_file(d.path(), body);
            let (out, code) = go(None, &["plan", "hello", "--distro", "arch", "--vars", &v]);
            assert_eq!(code, 2, "{out}");
            assert!(
                out.starts_with(&format!("{v}:{line}:")) && out.contains(what),
                "{out}"
            );
            let (out, code) = go(None, &["run", "hello", "--local", "--vars", &v]);
            assert_eq!(code, 2, "{out}");
            assert!(out.starts_with(&format!("{v}:{line}:")), "{out}");
        }
    }

    #[test]
    fn two_playbooks_in_one_binary_plan_with_their_own_defaults() {
        let pbs = [
            Playbook::new("hello", entry).defaults(def_x),
            Playbook::new("hello2", entry).defaults(def_y),
        ];
        let plan = |name: &str| go_with(&pbs, &["plan", name, "--distro", "arch"]);
        let (a, ca) = plan("hello");
        let (b, cb) = plan("hello2");
        assert_eq!((ca, cb), (0, 0), "{a}{b}");
        assert!(a.contains("ZGVmIHgK"), "{a}"); // "def x\n"
        assert!(b.contains("aGkgeQo="), "{b}"); // "hi y\n"
    }

    /// A default that will not serialize (a `None` inside an array) fails at
    /// load, naming the playbook, for `plan` and `run` alike.
    #[test]
    fn an_unserializable_default_fails_at_load_naming_the_playbook() {
        #[derive(Deserialize, serde::Serialize, Default, Clone)]
        #[serde(default)]
        struct Bad {
            xs: Vec<Option<u32>>,
        }
        fn bad() -> Bad {
            Bad { xs: vec![None] }
        }
        fn e(_h: Host<Bad>) -> PlaybookFuture {
            Box::pin(async { Ok(()) })
        }
        let pbs = [Playbook::new("bad", e).defaults(bad)];
        for a in [
            &["plan", "bad", "--distro", "arch"][..],
            &["run", "bad", "--local"][..],
        ] {
            let sb = telepatch::testing::sandbox();
            let (out, code) = run_with_args(
                &args(a),
                &pbs,
                || LocalTransport::new().uid(0).sandboxed(&sb),
                SshTransport::new,
            );
            assert_eq!(code, 2, "{out}");
            assert!(out.starts_with("playbook bad defaults: "), "{out}");
        }
    }
}

/// No snapshot body carries a `<hex>-<pid>` run id (`<nanos hex>-<pid>`):
/// a per-process value in a plan fails the second process, and this finds
/// it in every snapshot, not only the ones that run twice.
#[test]
fn no_snapshot_carries_a_run_id() {
    fn walk(d: &std::path::Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "snap") {
                out.push(p);
            }
        }
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut snaps = Vec::new();
    walk(&root.join("snapshots"), &mut snaps);
    walk(&root.join("adversarial/snapshots"), &mut snaps);
    assert!(snaps.len() > 50, "found only {} snapshots", snaps.len());
    // 11+ hex digits, a dash, a pid, bounded by non-word characters.
    let is_hex = |c: char| c.is_ascii_hexdigit();
    let mut hits = Vec::new();
    for p in &snaps {
        let text = std::fs::read_to_string(p).unwrap();
        // The body is after the second `---` of the header.
        let body = text.splitn(3, "---").nth(2).unwrap_or(&text);
        for tok in body.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) {
            if let Some((h, pid)) = tok.split_once('-')
                && h.len() >= 11
                && h.chars().all(is_hex)
                && !pid.is_empty()
                && pid.chars().all(|c| c.is_ascii_digit())
            {
                hits.push(format!("{}: {tok}", p.display()));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "run ids in snapshots:\n{}",
        hits.join("\n")
    );
}

// ---- run --check ----

#[test]
fn check_applies_nothing_on_any_host_and_exits_2() {
    let d = tempfile::tempdir().unwrap();
    let v = host_vars(d.path());
    let stub = Stub::new();
    let (out, code) = go_ssh(
        &stub,
        &[
            "run", "greet", "--host", "bot@one", "--host", "bot@two", "--vars", &v, "--check",
            "--diff",
        ],
    );
    assert_eq!(code, 2, "{out}");
    for h in ["bot@one", "bot@two"] {
        assert!(
            out.contains(&format!("== {h} [prepare] file "))
                && out.contains("greeting: will create"),
            "{h}: {out}"
        );
        assert!(
            out.contains(&format!(
                "== {h} [coverage] greet: 2 ops, 1 reversible, 1 sh without undo =="
            )),
            "{h}: {out}"
        );
    }
    assert!(!d.path().join("out/greeting").exists(), "{out}");
    // The sh! is the one `greet` op prepare cannot make "no change".
    assert!(out.contains("sh echo hi: will run"), "{out}");
}

#[test]
fn diff_alone_is_a_usage_error_and_check_takes_no_target_changes() {
    let (out, code) = go(&["run", "greet", "--local", "--diff"]);
    assert_eq!(code, 2);
    assert!(out.contains("--check"), "{out}");
    let (out, code) = go(&["run", "greet", "--check"]);
    assert_eq!(code, 2);
    assert!(out.contains("--local") && out.contains("--host"), "{out}");
}

// ---- run --inventory / --limit ----

fn inventory_file(d: &std::path::Path, body: &str) -> String {
    let p = d.join("inventory.toml");
    std::fs::write(&p, body).unwrap();
    p.to_str().unwrap().to_string()
}

/// Two hosts, `bot@one` in group `web` (and so in group `all` by the
/// host's own `groups`), each with its own output directory.
fn two_host_inventory(d: &std::path::Path) -> String {
    let (one, two) = (d.join("one"), d.join("two"));
    std::fs::create_dir_all(&one).unwrap();
    std::fs::create_dir_all(&two).unwrap();
    inventory_file(
        d,
        &format!(
            "[groups.web]\nhosts = [\"bot@one\"]\nvars = {{ greeting = \"from web\" }}\n\
             [groups.db]\nhosts = [\"bot@two\"]\n\
             [hosts.\"bot@one\"]\nvars = {{ dir = {one:?} }}\n\
             [hosts.\"bot@two\"]\nvars = {{ dir = {two:?} }}\n",
            one = one.to_str().unwrap(),
            two = two.to_str().unwrap(),
        ),
    )
}

fn read(d: &std::path::Path, rel: &str) -> Option<String> {
    std::fs::read_to_string(d.join(rel)).ok()
}

#[test]
fn run_inventory_gives_each_host_its_vars_and_groups() {
    let d = tempfile::tempdir().unwrap();
    let inv = two_host_inventory(d.path());
    // the vars file is the lowest layer: greeting for hosts the group does not set.
    let v = vars_file(d.path(), "greeting = \"base\"\ndir = \"/unused\"\n");
    let stub = Stub::new();
    let (out, code) = go_ssh(&stub, &["run", "greet", "--inventory", &inv, "--vars", &v]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        read(d.path(), "one/greeting").as_deref(),
        Some("from web\n")
    );
    assert_eq!(read(d.path(), "two/greeting").as_deref(), Some("base\n"));
    // in_group: only the host in `web` got the file
    assert_eq!(read(d.path(), "one/web").as_deref(), Some("in web\n"));
    assert_eq!(read(d.path(), "two/web"), None);
    // the inventory's host name is the ssh destination and the report name
    for h in ["bot@one", "bot@two"] {
        assert!(
            out.lines().any(|l| l.split_whitespace().next() == Some(h)),
            "{h}: {out}"
        );
        assert!(
            stub.argv().iter().any(|l| l.split(' ').any(|w| w == h)),
            "{h}"
        );
    }
}

#[test]
fn limit_takes_a_group_a_host_or_a_list() {
    let d = tempfile::tempdir().unwrap();
    let inv = two_host_inventory(d.path());
    let v = vars_file(d.path(), "greeting = \"base\"\ndir = \"/unused\"\n");
    for (limit, one, two) in [
        ("web", true, false),
        ("db", false, true),
        ("bot@two", false, true),
        ("web,bot@two", true, true),
    ] {
        let out_dirs = ["one/greeting", "two/greeting"];
        for f in out_dirs {
            let _ = std::fs::remove_file(d.path().join(f));
        }
        let stub = Stub::new();
        let (out, code) = go_ssh(
            &stub,
            &[
                "run",
                "greet",
                "--inventory",
                &inv,
                "--vars",
                &v,
                "--limit",
                limit,
            ],
        );
        assert_eq!(code, 0, "{limit}: {out}");
        assert_eq!(
            read(d.path(), "one/greeting").is_some(),
            one,
            "{limit}: {out}"
        );
        assert_eq!(
            read(d.path(), "two/greeting").is_some(),
            two,
            "{limit}: {out}"
        );
    }
}

#[test]
fn unknown_limit_name_is_a_usage_error_and_runs_nothing() {
    let d = tempfile::tempdir().unwrap();
    let inv = two_host_inventory(d.path());
    let v = vars_file(d.path(), "greeting = \"base\"\ndir = \"/unused\"\n");
    let stub = Stub::new();
    let (out, code) = go_ssh(
        &stub,
        &[
            "run",
            "greet",
            "--inventory",
            &inv,
            "--vars",
            &v,
            "--limit",
            "wbe",
        ],
    );
    assert_eq!(code, 2, "{out}");
    assert!(
        out.contains("unknown --limit name `wbe`") && out.contains("groups: web, db"),
        "{out}"
    );
    assert!(stub.argv().is_empty());
    assert_eq!(read(d.path(), "one/greeting"), None);
}

#[test]
fn inventory_is_exclusive_with_host_and_local_and_limit_needs_it() {
    let (out, code) = go(&["run", "greet", "--inventory", "i.toml", "--host", "a@b"]);
    assert_eq!(code, 2);
    assert!(out.contains("cannot be used with"), "{out}");
    let (out, code) = go(&["run", "greet", "--inventory", "i.toml", "--local"]);
    assert_eq!(code, 2);
    assert!(out.contains("cannot be used with"), "{out}");
    let (out, code) = go(&["run", "greet", "--local", "--limit", "web"]);
    assert_eq!(code, 2);
    assert!(out.contains("--inventory"), "{out}");
}

#[test]
fn host_still_runs_one_off_without_an_inventory() {
    let d = tempfile::tempdir().unwrap();
    let v = host_vars(d.path());
    let stub = Stub::new();
    let (out, code) = go_ssh(&stub, &["run", "greet", "--host", "bot@one", "--vars", &v]);
    assert_eq!(code, 0, "{out}");
    // not in any group
    assert_eq!(read(d.path(), "out/web"), None);
}

#[test]
fn inventory_errors_name_file_and_line_and_stop_before_any_host() {
    let d = tempfile::tempdir().unwrap();
    let v = vars_file(d.path(), "greeting = \"base\"\ndir = \"/unused\"\n");
    let stub = Stub::new();
    // an unknown vars key in a group the --limit leaves out is still caught
    let inv = inventory_file(
        d.path(),
        "[groups.a]\nhosts = [\"bot@one\"]\n[groups.b]\nhosts = [\"bot@two\"]\nvars = { nope = 1 }\n",
    );
    let (out, code) = go_ssh(
        &stub,
        &[
            "run",
            "greet",
            "--inventory",
            &inv,
            "--vars",
            &v,
            "--limit",
            "a",
        ],
    );
    assert_eq!(code, 2, "{out}");
    assert!(
        out.starts_with(&format!("{inv}:5: unknown key `nope`")),
        "{out}"
    );
    assert!(stub.argv().is_empty());
    let (out, code) = go_ssh(&stub, &["run", "greet", "--inventory", "/no/such/inv.toml"]);
    assert_eq!(code, 2);
    assert!(
        out.starts_with("cannot read inventory /no/such/inv.toml"),
        "{out}"
    );
}

#[test]
fn check_works_over_an_inventory() {
    let d = tempfile::tempdir().unwrap();
    let inv = two_host_inventory(d.path());
    let v = vars_file(d.path(), "greeting = \"base\"\ndir = \"/unused\"\n");
    let stub = Stub::new();
    let (out, code) = go_ssh(
        &stub,
        &[
            "run",
            "greet",
            "--inventory",
            &inv,
            "--vars",
            &v,
            "--check",
            "--limit",
            "db",
        ],
    );
    assert_eq!(code, 2, "{out}");
    assert_eq!(read(d.path(), "two/greeting"), None);
}

// ---- run --reboot ----

#[test]
fn reboot_takes_auto_or_never_and_a_bad_value_is_a_usage_error() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("out");
    std::fs::create_dir(&dir).unwrap();
    let v = vars_file(
        d.path(),
        &format!("dir = {:?}\ngreeting = \"hello\"\n", dir.to_str().unwrap()),
    );
    for value in ["auto", "never"] {
        let (out, code) = go(&["run", "greet", "--local", "--vars", &v, "--reboot", value]);
        assert_eq!(code, 0, "--reboot {value}: {out}");
    }
    let (out, code) = go(&["run", "greet", "--local", "--vars", &v, "--reboot", "maybe"]);
    assert_eq!(code, 2, "{out}");
    assert!(
        out.contains("--reboot") && out.contains("auto") && out.contains("never"),
        "{out}"
    );
    // A check never reboots: the two together are a usage error.
    let (out, code) = go(&["run", "greet", "--local", "--check", "--reboot", "auto"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("--reboot") && out.contains("--check"), "{out}");
}

// ---- cache usage / cache purge (T123) ----
//
// Every host is its own sandbox: the main cache is `<sb>/cache`, so the
// registry is `<sb>/caches` and the run lock `<sb>/.lock`; `<sb>/fs` is the
// one filesystem root (`TP_FS_ROOTS`).

mod cache {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::Path;
    use telepatch::testing::Sandbox;

    pub fn sandbox_() -> Sandbox {
        telepatch::testing::sandbox()
    }

    /// A 64-character name with an upper-case hex digit in it.
    pub fn upper() -> String {
        format!("{:064X}", 0xabcu64)
    }

    pub fn hexn(n: u64) -> String {
        format!("{n:064x}")
    }

    /// A file of `len` bytes (sparse), named `name`, in `dir`.
    pub fn put(dir: &Path, name: &str, len: u64) {
        std::fs::create_dir_all(dir).unwrap();
        let f = std::fs::File::create(dir.join(name)).unwrap();
        f.set_len(len).unwrap();
    }

    /// A record by hand: `(sum, last used, owners)`.
    pub fn record(dir: &Path, lines: &[(String, u64, &str)]) {
        std::fs::create_dir_all(dir).unwrap();
        let mut s = String::from("v1\n");
        for (sum, t, o) in lines {
            s.push_str(&format!("{sum}\t{t}\t{o}\n"));
        }
        std::fs::write(dir.join(".telepatch-usage"), s).unwrap();
        std::fs::write(dir.join(".telepatch-usage.lock"), "").unwrap();
    }

    pub fn registry(sb: &Sandbox, dirs: &[&Path]) {
        let s: String = dirs.iter().map(|d| format!("{}\n", d.display())).collect();
        std::fs::write(sb.path().join("caches"), s).unwrap();
    }

    pub fn fs_cache(sb: &Sandbox) -> PathBuf {
        sb.fs.join(".telepatch/cache/sha256")
    }

    /// Every path under `root`: directories, files with their bytes and
    /// links with their target.
    pub fn tree(root: &Path) -> BTreeMap<String, String> {
        fn walk(root: &Path, d: &Path, out: &mut BTreeMap<String, String>) {
            for e in std::fs::read_dir(d).unwrap() {
                let p = e.unwrap().path();
                let rel = p.strip_prefix(root).unwrap().display().to_string();
                let m = std::fs::symlink_metadata(&p).unwrap();
                if m.file_type().is_symlink() {
                    out.insert(
                        rel,
                        format!("l:{}", std::fs::read_link(&p).unwrap().display()),
                    );
                } else if m.is_dir() {
                    out.insert(rel, "d".into());
                    walk(root, &p, out);
                } else {
                    let b = std::fs::read(&p).unwrap();
                    out.insert(rel, format!("f:{}:{:x}", b.len(), md5ish(&b)));
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    fn md5ish(b: &[u8]) -> u64 {
        b.iter().fold(1469598103934665603u64, |h, x| {
            (h ^ *x as u64).wrapping_mul(1099511628211)
        })
    }

    pub fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .map(|r| {
                r.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    /// `a` with each stub host (in the order it is built) in its own sandbox.
    pub fn go_hosts(stub: &Stub, sbs: &[Sandbox], a: &[&str]) -> (String, i32) {
        let n = std::cell::Cell::new(0usize);
        run_with_args(
            &args(a),
            &pbs(),
            || LocalTransport::new().uid(0).sandboxed(&sbs[0]),
            || {
                let i = n.get();
                n.set(i + 1);
                stub.transport().sandboxed(&sbs[i])
            },
        )
    }

    pub fn go_local(sb: &Sandbox, a: &[&str]) -> (String, i32) {
        run_with_args(
            &args(a),
            &pbs(),
            || LocalTransport::new().uid(0).sandboxed(sb),
            SshTransport::new,
        )
    }

    pub const A: u64 = 1_700_000_000;
    pub const B: u64 = 1_700_100_000;

    /// Host one: a main cache with two items and a record, a filesystem
    /// cache with one unrecorded item, both registered.
    pub fn seed_one(sb: &Sandbox) {
        put(&sb.cache, &hexn(1), 1_572_864);
        put(&sb.cache, &hexn(2), 1000);
        record(&sb.cache, &[(hexn(1), A, "site"), (hexn(2), B, "site,web")]);
        put(&fs_cache(sb), &hexn(3), 2_097_152);
        registry(sb, &[&sb.cache, &fs_cache(sb)]);
    }

    /// Host two: one unrecorded item in the main cache, no registry.
    pub fn seed_two(sb: &Sandbox) {
        put(&sb.cache, &hexn(4), 500);
    }

    pub struct Held(std::process::Child);
    impl Drop for Held {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// Hold the run lock (a shared `flock` on `<sb>/.lock`) in a child
    /// process, as a run's root shell does, until the guard is dropped.
    pub fn hold_run_lock(sb: &Sandbox) -> Held {
        use std::io::BufRead;
        let mut c = std::process::Command::new("bash")
            .arg("-c")
            .arg(r#"exec 7>>"$1"; flock -s -n 7 || exit 3; echo held; exec sleep 600"#)
            .arg("sh")
            .arg(sb.path().join(".lock"))
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(c.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let h = Held(c);
        assert_eq!(line.trim(), "held");
        h
    }
}
use cache::*;
use std::path::Path;
use telepatch::testing::Sandbox;

#[test]
fn cache_usage_over_two_stub_hosts_as_a_table() {
    let d = tempfile::tempdir().unwrap();
    let inv = two_host_inventory(d.path());
    let sbs = [sandbox_(), sandbox_()];
    seed_one(&sbs[0]);
    seed_two(&sbs[1]);
    let before = (tree(sbs[0].path()), tree(sbs[1].path()));
    let stub = Stub::new();
    let (out, code) = go_hosts(&stub, &sbs, &["cache", "usage", "--inventory", &inv]);
    assert_eq!(code, 0, "{out}");
    let main = sbs[0].cache.display().to_string();
    let fsc = fs_cache(&sbs[0]).display().to_string();
    for want in [
        "bot@one",
        "bot@two",
        &format!("{main}: 2 items, 1.5 MB"),
        &format!("{fsc}: 1 item, 2.0 MB"),
        "owner site: 2 items, 1.5 MB",
        "owner web: 1 item, 0.0 MB",
        "unowned: 1 item, 2.0 MB",
        "last used 2023-11-14 22:13 .. 2023-11-16 02:00",
        "host total: 3 items, 3.5 MB",
        "host total: 1 item, 0.0 MB",
        "run total: 2 hosts, 4 items, 3.5 MB",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    // Read-only: nothing on either host changed, not even a lock file.
    assert_eq!(before, (tree(sbs[0].path()), tree(sbs[1].path())));
}

#[test]
fn cache_usage_json_is_one_document_in_bytes() {
    let d = tempfile::tempdir().unwrap();
    let inv = two_host_inventory(d.path());
    let sbs = [sandbox_(), sandbox_()];
    seed_one(&sbs[0]);
    seed_two(&sbs[1]);
    let stub = Stub::new();
    let (out, code) = go_hosts(
        &stub,
        &sbs,
        &["cache", "usage", "--inventory", &inv, "--json"],
    );
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
    let hosts = v["hosts"].as_array().unwrap();
    assert_eq!(hosts.len(), 2);
    assert_eq!(hosts[0]["host"], "bot@one");
    assert_eq!(hosts[0]["status"], "ok");
    assert_eq!(hosts[0]["items"], 3);
    assert_eq!(hosts[0]["bytes"], 1_572_864 + 1000 + 2_097_152);
    let dirs = hosts[0]["dirs"].as_array().unwrap();
    assert_eq!(dirs.len(), 2);
    assert_eq!(dirs[0]["dir"], sbs[0].cache.display().to_string());
    assert_eq!(dirs[0]["items"], 2);
    assert_eq!(dirs[0]["bytes"], 1_572_864 + 1000);
    assert_eq!(dirs[0]["oldest"], A);
    assert_eq!(dirs[0]["newest"], B);
    let owners = dirs[0]["owners"].as_array().unwrap();
    let site = owners.iter().find(|o| o["owner"] == "site").unwrap();
    assert_eq!(
        (&site["items"], &site["bytes"]),
        (&2.into(), &(1_572_864 + 1000).into())
    );
    let web = owners.iter().find(|o| o["owner"] == "web").unwrap();
    assert_eq!((&web["items"], &web["bytes"]), (&1.into(), &1000.into()));
    assert_eq!(dirs[1]["unowned"]["items"], 1);
    assert_eq!(dirs[1]["oldest"], serde_json::Value::Null);
    assert_eq!(hosts[1]["bytes"], 500);
    assert_eq!(v["total"]["hosts"], 2);
    assert_eq!(v["total"]["items"], 4);
    assert_eq!(v["total"]["bytes"], 1_572_864 + 1000 + 2_097_152 + 500);
}

#[test]
fn cache_usage_unreachable_host_is_exit_1_and_listed_with_its_reason() {
    let d = tempfile::tempdir().unwrap();
    let inv = two_host_inventory(d.path());
    let sbs = [sandbox_(), sandbox_()];
    seed_one(&sbs[0]);
    let stub = Stub::new();
    let n = std::cell::Cell::new(0usize);
    let (out, code) = run_with_args(
        &args(&["cache", "usage", "--inventory", &inv]),
        &pbs(),
        LocalTransport::new,
        || {
            let i = n.get();
            n.set(i + 1);
            if i == 1 {
                SshTransport::new().ssh("/no/such/ssh")
            } else {
                stub.transport().sandboxed(&sbs[0])
            }
        },
    );
    assert_eq!(code, 1, "{out}");
    assert!(out.contains("host total: 3 items"), "{out}");
    let failed: Vec<&str> = out.lines().filter(|l| l.contains("bot@two")).collect();
    assert!(
        failed.iter().any(|l| l.contains("failed")),
        "bot@two not listed as failed:\n{out}"
    );
}

#[test]
fn cache_purge_without_yes_prints_what_it_would_remove_and_changes_nothing() {
    let sb = sandbox_();
    seed_one(&sb);
    put(&sb.fs.join(".telepatch/tmp/run1"), "undo-data", 10);
    let before = tree(sb.path());
    let (out, code) = go_local(&sb, &["cache", "purge", "--local"]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("would remove") && out.contains("--yes"),
        "{out}"
    );
    assert!(out.contains(&sb.cache.display().to_string()), "{out}");
    assert!(out.contains("3 items"), "{out}");
    assert_eq!(before, tree(sb.path()));
}

#[test]
fn cache_purge_yes_empties_every_registered_cache_and_leaves_tmp() {
    let sb = sandbox_();
    seed_one(&sb);
    // T106/T119's staging and undo data, a file of someone else's, a
    // hex name that is a directory, a link and a near-miss name.
    put(&sb.fs.join(".telepatch/tmp/run1/undo/root/1"), "saved", 10);
    put(&sb.cache, "notes.txt", 7);
    put(&sb.cache, &upper(), 7);
    put(&sb.cache, &hexn(9)[1..], 7);
    std::fs::create_dir_all(sb.cache.join(hexn(8))).unwrap();
    put(&sb.cache.join(hexn(8)), "inner", 3);
    std::fs::write(sb.path().join("outside"), "keep me").unwrap();
    std::os::unix::fs::symlink(sb.path().join("outside"), sb.cache.join(hexn(7))).unwrap();
    // Not registered: a per-filesystem cache of another root, and a hex
    // file in the stage.
    let other = sb.path().join("other/.telepatch/cache/sha256");
    put(&other, &hexn(5), 11);
    put(&sb.stage, &hexn(6), 12);
    let keep = |sb: &Sandbox| {
        [
            tree(&sb.path().join("other")),
            tree(&sb.stage),
            tree(&sb.fs.join(".telepatch/tmp")),
        ]
    };
    let kept = keep(&sb);
    let (out, code) = go_local(&sb, &["cache", "purge", "--local", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("removed 3 items"), "{out}");
    assert_eq!(keep(&sb), kept, "{out}");
    // The main cache keeps what is not a regular 64-hex file.
    let mut left = vec![
        hexn(7),
        hexn(8),
        "notes.txt".to_string(),
        upper(),
        hexn(9)[1..].to_string(),
    ];
    left.sort();
    assert_eq!(names(&sb.cache), left);
    assert_eq!(
        std::fs::read_to_string(sb.path().join("outside")).unwrap(),
        "keep me"
    );
    assert_eq!(
        std::fs::read_to_string(sb.cache.join(hexn(8)).join("inner"))
            .unwrap()
            .len(),
        3
    );
    // The filesystem cache is gone, `.telepatch` stays because of `tmp`.
    assert!(!sb.fs.join(".telepatch/cache").exists(), "{out}");
    assert!(sb.fs.join(".telepatch/tmp/run1/undo/root/1/saved").exists());
    // The registry keeps only the main cache.
    assert_eq!(
        std::fs::read_to_string(sb.path().join("caches")).unwrap(),
        format!("{}\n", sb.cache.display())
    );
    // Usage now shows nothing recorded or fetched beyond what stayed.
    let (out, code) = go_local(&sb, &["cache", "usage", "--local", "--json"]);
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["total"]["items"], 0);
    assert_eq!(v["total"]["bytes"], 0);
}

#[test]
fn cache_purge_removes_the_whole_empty_telepatch_tree_but_not_a_used_one() {
    let sb = sandbox_();
    seed_one(&sb);
    let (out, code) = go_local(&sb, &["cache", "purge", "--local", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(!sb.fs.join(".telepatch").exists(), "{out}");
    assert!(sb.fs.exists());
    assert_eq!(names(&sb.cache), Vec::<String>::new());
    // A cache with a file of someone else's keeps its directories.
    seed_one(&sb);
    put(&fs_cache(&sb), "README", 4);
    let (out, code) = go_local(&sb, &["cache", "purge", "--local", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(names(&fs_cache(&sb)), ["README"]);
}

#[test]
fn cache_purge_refuses_a_registry_line_outside_the_sandbox() {
    let sb = sandbox_();
    seed_one(&sb);
    // A cache-shaped directory in a tempdir of its own: only `tp_sbx` stops
    // it. Its item, record and lock must be byte-identical afterwards.
    let elsewhere = tempfile::tempdir().unwrap();
    let outside = elsewhere.path().join(".telepatch/cache/sha256");
    put(&outside, &hexn(20), 100);
    record(&outside, &[(hexn(20), A, "site")]);
    registry(&sb, &[&sb.cache, &fs_cache(&sb), &outside]);
    let before = tree(elsewhere.path());
    let (out, code) = go_local(&sb, &["cache", "purge", "--local", "--yes"]);
    assert!(out.contains("sandbox: refusing"), "{out}");
    assert!(out.contains(&outside.display().to_string()), "{out}");
    assert_eq!(code, 0, "{out}");
    assert_eq!(tree(elsewhere.path()), before, "{out}");
    // The sandbox's own were purged; the refused line stays registered.
    assert_eq!(names(&sb.cache), Vec::<String>::new());
    assert!(
        std::fs::read_to_string(sb.path().join("caches"))
            .unwrap()
            .contains(&outside.display().to_string())
    );
    // Usage and the owner form refuse it too.
    for a in [
        &["cache", "usage", "--local"][..],
        &["cache", "purge", "--local", "--yes", "--owner", "site"][..],
    ] {
        let (out, _) = go_local(&sb, a);
        assert!(out.contains("sandbox: refusing"), "{a:?}: {out}");
        assert_eq!(tree(elsewhere.path()), before, "{a:?}: {out}");
    }
}

#[test]
fn cache_purge_ignores_registry_lines_that_are_not_cache_directories() {
    let sb = sandbox_();
    put(&sb.cache, &hexn(1), 5);
    put(&sb.stage, &hexn(2), 5);
    put(&sb.path().join("etc"), &hexn(3), 5);
    registry(
        &sb,
        &[
            &sb.cache,
            &sb.stage,
            &sb.path().join("etc"),
            Path::new("relative/dir"),
        ],
    );
    let before = (tree(&sb.stage), tree(&sb.path().join("etc")));
    let (out, code) = go_local(&sb, &["cache", "purge", "--local", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("not a telepatch cache directory"), "{out}");
    assert_eq!((tree(&sb.stage), tree(&sb.path().join("etc"))), before);
    assert_eq!(names(&sb.cache), Vec::<String>::new());
}

#[test]
fn cache_purge_is_refused_while_a_run_holds_the_lock() {
    let sb = sandbox_();
    seed_one(&sb);
    {
        let _run = hold_run_lock(&sb);
        let before = tree(sb.path());
        let (out, code) = go_local(&sb, &["cache", "purge", "--local", "--yes"]);
        assert_eq!(code, 1, "{out}");
        assert!(out.contains("in use by a running run"), "{out}");
        assert_eq!(before, tree(sb.path()), "{out}");
        // Usage takes no lock: it still reads.
        let (out, code) = go_local(&sb, &["cache", "usage", "--local"]);
        assert_eq!(code, 0, "{out}");
    }
    // The run is over (child killed and reaped): the purge goes through.
    let (out, code) = go_local(&sb, &["cache", "purge", "--local", "--yes"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(names(&sb.cache), Vec::<String>::new());
}

#[test]
fn cache_purge_owner_removes_the_owner_and_deletes_only_what_nobody_owns() {
    let sb = sandbox_();
    for n in 1..=5 {
        put(&sb.cache, &hexn(n), 100 * n);
    }
    // 1 site; 2 site+web; 3 web; 4 no owner; 5 not in the record at all.
    record(
        &sb.cache,
        &[
            (hexn(1), A, "site"),
            (hexn(2), B, "site,web"),
            (hexn(3), A, "web"),
            (hexn(4), B, ""),
        ],
    );
    registry(&sb, &[&sb.cache]);
    let rec = sb.cache.join(".telepatch-usage");
    let before = tree(sb.path());
    let (out, code) = go_local(&sb, &["cache", "purge", "--local", "--owner", "site"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("would"), "{out}");
    assert_eq!(before, tree(sb.path()), "{out}");
    let (out, code) = go_local(
        &sb,
        &["cache", "purge", "--local", "--yes", "--owner", "site"],
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        names(&sb.cache),
        [
            ".telepatch-usage".to_string(),
            ".telepatch-usage.lock".to_string(),
            hexn(2),
            hexn(3),
            hexn(4),
            hexn(5)
        ]
    );
    assert_eq!(
        std::fs::read_to_string(&rec).unwrap(),
        format!(
            "v1\n{}\t{B}\tweb\n{}\t{A}\tweb\n{}\t{B}\t\n",
            hexn(2),
            hexn(3),
            hexn(4)
        )
    );
    // The registry is not touched by the owner form.
    assert_eq!(
        std::fs::read_to_string(sb.path().join("caches")).unwrap(),
        format!("{}\n", sb.cache.display())
    );
    // The last owner leaving deletes the item.
    let (out, code) = go_local(
        &sb,
        &["cache", "purge", "--local", "--yes", "--owner", "web"],
    );
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        names(&sb.cache),
        [
            ".telepatch-usage".to_string(),
            ".telepatch-usage.lock".to_string(),
            hexn(4),
            hexn(5)
        ]
    );
}

#[test]
fn cache_serial_1_takes_one_host_at_a_time_in_order() {
    let d = tempfile::tempdir().unwrap();
    let inv = two_host_inventory(d.path());
    let events = |serial: bool| -> Vec<String> {
        let sbs = [sandbox_(), sandbox_()];
        seed_one(&sbs[0]);
        seed_two(&sbs[1]);
        let log = d.path().join("log.jsonl");
        let mut a = vec![
            "cache",
            "usage",
            "--inventory",
            &inv,
            "--log",
            log.to_str().unwrap(),
        ];
        if serial {
            a.extend(["--serial", "1"]);
        }
        let stub = Stub::new();
        let (out, code) = go_hosts(&stub, &sbs, &a);
        assert_eq!(code, 0, "{out}");
        std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .filter(|v| v["ev"] == "host_open" || v["ev"] == "host_status")
            .map(|v| {
                format!(
                    "{} {}",
                    v["ev"].as_str().unwrap(),
                    v["host"].as_str().unwrap()
                )
            })
            .collect()
    };
    assert_eq!(
        events(true),
        [
            "host_open bot@one",
            "host_status bot@one",
            "host_open bot@two",
            "host_status bot@two"
        ]
    );
    // Without it both hosts are opened before either is done.
    let all = events(false);
    assert_eq!(all[0], "host_open bot@one");
    assert_eq!(all[1], "host_open bot@two");
    let (out, code) = go(&["cache", "usage", "--local", "--serial", "0"]);
    assert_eq!(code, 2, "{out}");
}

#[test]
fn cache_limit_narrows_the_inventory_and_unknown_names_are_usage_errors() {
    let d = tempfile::tempdir().unwrap();
    let inv = two_host_inventory(d.path());
    let sbs = [sandbox_(), sandbox_()];
    seed_one(&sbs[0]);
    seed_two(&sbs[1]);
    for (limit, one, two) in [
        ("web", true, false),
        ("db", false, true),
        ("web,bot@two", true, true),
    ] {
        let stub = Stub::new();
        let (out, code) = go_hosts(
            &stub,
            &sbs,
            &["cache", "usage", "--inventory", &inv, "--limit", limit],
        );
        assert_eq!(code, 0, "{limit}: {out}");
        assert_eq!(out.contains("bot@one"), one, "{limit}: {out}");
        assert_eq!(out.contains("bot@two"), two, "{limit}: {out}");
        let argv = stub.argv();
        assert_eq!(
            argv.iter().any(|l| l.split(' ').any(|w| w == "bot@two")),
            two,
            "{limit}: {argv:?}"
        );
    }
    let stub = Stub::new();
    let (out, code) = go_hosts(
        &stub,
        &sbs,
        &["cache", "usage", "--inventory", &inv, "--limit", "wbe"],
    );
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("unknown --limit name `wbe`"), "{out}");
    assert!(stub.argv().is_empty());
}

#[test]
fn cache_targets_are_exclusive_like_run_and_purge_needs_a_valid_owner() {
    for a in [
        &["cache", "usage"][..],
        &["cache", "usage", "--local", "--host", "a@b"][..],
        &["cache", "usage", "--inventory", "i.toml", "--local"][..],
        &["cache", "purge", "--inventory", "i.toml", "--host", "a@b"][..],
        &["cache", "usage", "--local", "--limit", "web"][..],
        &["cache", "usage", "--local", "--ssh-option", "X=1"][..],
        &["cache", "purge", "--local", "--owner", "a,b"][..],
        &["cache", "usage", "--local", "--owner", "a"][..],
    ] {
        let (out, code) = go(a);
        assert_eq!(code, 2, "{a:?}: {out}");
    }
}

#[test]
fn cache_usage_with_an_unreadable_record_warns_and_owner_purge_leaves_it_alone() {
    let sb = sandbox_();
    put(&sb.cache, &hexn(1), 100);
    std::fs::write(sb.cache.join(".telepatch-usage"), "garbage\n").unwrap();
    let before = tree(&sb.cache);
    let (out, code) = go_local(&sb, &["cache", "usage", "--local"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("is unreadable"), "{out}");
    assert!(out.contains("1 item"), "{out}");
    let (out, code) = go_local(&sb, &["cache", "purge", "--local", "--yes", "--owner", "x"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("nothing changed there"), "{out}");
    assert_eq!(before, tree(&sb.cache));
}

#[test]
fn an_unknown_only_role_exits_2_and_never_contacts_the_host() {
    let d = tempfile::tempdir().unwrap();
    let v = host_vars(d.path());
    let stub = Stub::new();
    let (out, code) = go_ssh(
        &stub,
        &[
            "run", "greet", "--host", "bot@one", "--vars", &v, "--only", "nosuch",
        ],
    );
    assert_eq!(code, 2, "{out}");
    assert!(
        out.contains("--only: no role named nosuch in this binary; roles: "),
        "{out}"
    );
    assert!(out.contains("greet"), "{out}");
    assert!(
        stub.argv().is_empty(),
        "the stub ssh was run: {:?}",
        stub.argv()
    );
}

#[test]
fn max_fail_alone_is_a_usage_error_before_anything_opens() {
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
            "--vars",
            &v,
            "--max-fail",
            "1",
        ],
    );
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("--max-fail needs --serial"), "{out}");
    assert!(stub.argv().is_empty(), "{:?}", stub.argv());
    let (out, code) = go_ssh(
        &stub,
        &[
            "run",
            "greet",
            "--host",
            "bot@one",
            "--vars",
            &v,
            "--serial",
            "1",
            "--max-fail",
            "1",
        ],
    );
    assert_eq!(code, 0, "{out}");
}
