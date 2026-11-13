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
        run_with_args(
            &args(a),
            pbs,
            || LocalTransport::new().uid(0),
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
            let (out, code) = run_with_args(
                &args(a),
                &pbs,
                || LocalTransport::new().uid(0),
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
            out.contains(&format!("== {h} [coverage] greet: 2 ops, 1 reversible ==")),
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
