//! The `repo` primitive: templates run against fake `curl` / `dpkg` on
//! PATH with `/etc/` redirected into a temp dir, the prepare probe the same
//! way, builder and key tests, the wrong-family trace error, and
//! wrapped-script and plan snapshots.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use telepatch::facts::os::Os;
use telepatch::ops::repo::{AptRepo, DnfRepo, RepoSpec, findings, probe};
use telepatch::plan::render_host;
use telepatch::report::WillCreate;
use telepatch::trailer::{OpKind, RepoResult, render_op, split_stream, wrap};
use telepatch::{Host, Key, Node, NodeId, Value};

const NONCE: &str = "t0n";

fn ubuntu() -> Os {
    Os::parse("ID=ubuntu\nVERSION_ID=24.04\nVERSION_CODENAME=noble\nUBUNTU_CODENAME=noble\n")
        .unwrap()
}

fn fedora() -> Os {
    Os::parse("ID=fedora\nVERSION_ID=41\n").unwrap()
}

fn host(os: Os) -> Host<()> {
    Host::new("t", os, Rc::new(()))
}

fn docker_apt(h: &Host<()>) -> AptRepo {
    AptRepo {
        uris: vec!["https://download.docker.com/linux/ubuntu".into()],
        suites: vec![h.os.codename().unwrap().to_string()],
        components: vec!["stable".into()],
        key_url: "https://download.docker.com/linux/ubuntu/gpg".into(),
    }
}

fn wrapped_display(h: &Host<()>, id: NodeId) -> String {
    match &h.snapshot().nodes[id.0] {
        Node::Op(op) => wrap(OpKind::Repo, &op.spec.script(&h.os), id.0, NONCE),
        _ => panic!("not an op"),
    }
}

fn wrapped(h: &Host<()>, id: NodeId) -> String {
    match &h.snapshot().nodes[id.0] {
        Node::Op(op) => render_op(op, id.0, NONCE, &h.os),
        _ => panic!("not an op"),
    }
}

fn spec_of(h: &Host<()>, id: NodeId) -> RepoSpec {
    match &h.snapshot().nodes[id.0] {
        Node::Op(op) => match &op.spec {
            telepatch::OpSpec::Repo(r) => r.clone(),
            _ => panic!("not a repo"),
        },
        _ => panic!("not an op"),
    }
}

// --- the fake host ---

const CURL: &str = r#"url="${@: -1}"
echo "curl $1 $url" >> @D@/curllog
[ -e @D@/curlfail ] && exit 22
out=
while [ $# -gt 0 ]; do
  case "$1" in -o) out=$2; shift 2;; *) shift;; esac
done
printf 'FETCHED %s\n' "$url" > "$out""#;

const DPKG: &str = r#"[ "$1" = --print-architecture ] || exit 64
cat @D@/arch 2>/dev/null || echo amd64"#;

fn stubs(d: &Path) -> PathBuf {
    let bin = d.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let w = |name: &str, body: &str| {
        let p = bin.join(name);
        let body = body.replace("@D@", &d.display().to_string());
        std::fs::write(&p, format!("#!/bin/bash\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    };
    w("curl", CURL);
    w("dpkg", DPKG);
    bin
}

/// The script with `/etc/` moved under `d`, as `bash -c` would run it.
fn redirect(script: &str, d: &Path) -> String {
    script.replace("/etc/", &format!("{}/etc/", d.display()))
}

fn bash(script: &str, d: &Path, path: &str) -> std::process::Output {
    Command::new("/usr/bin/env")
        .arg("bash")
        .arg("-c")
        .arg(redirect(script, d))
        .current_dir(d)
        .env("PATH", path)
        .output()
        .unwrap()
}

fn full_path(d: &Path) -> String {
    format!(
        "{}:{}",
        d.join("bin").display(),
        std::env::var("PATH").unwrap()
    )
}

/// (result, stderr)
fn run_script(script: &str, d: &Path) -> (RepoResult, String) {
    let out = bash(script, d, &full_path(d));
    let stream = String::from_utf8(out.stdout).unwrap();
    let mut v = split_stream(&stream, NONCE);
    assert_eq!(v.len(), 1, "stream: {stream:?}");
    let (_, t) = v.remove(0);
    (
        t.typed::<RepoResult>().unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

fn res(rc: i32, changed: bool) -> RepoResult {
    RepoResult {
        rc,
        changed,
        ..Default::default()
    }
}

/// `r` without the capture fields (T107), for the older tests.
fn core(r: &RepoResult) -> RepoResult {
    RepoResult {
        rc: r.rc,
        changed: r.changed,
        ..Default::default()
    }
}

fn curllog(d: &Path) -> String {
    std::fs::read_to_string(d.join("curllog")).unwrap_or_default()
}

fn read(d: &Path, rel: &str) -> String {
    std::fs::read_to_string(d.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn exists(d: &Path, rel: &str) -> bool {
    d.join(rel).exists()
}

fn mode(d: &Path, rel: &str) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(d.join(rel)).unwrap().permissions().mode() & 0o777
}

/// Files in `dir` (relative to `d`).
fn listing(d: &Path, dir: &str) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(d.join(dir))
        .map(|r| {
            r.map(|e| e.unwrap().file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn want_source(d: &Path, arch: &str) -> String {
    format!(
        "Types: deb\nURIs: https://download.docker.com/linux/ubuntu\nSuites: noble\nComponents: stable\nArchitectures: {arch}\nSigned-By: {}/etc/apt/keyrings/docker.asc\n",
        d.display()
    )
}

const KEY: &str = "etc/apt/keyrings/docker.asc";
const SRC: &str = "etc/apt/sources.list.d/docker.sources";
const REPOFILE: &str = "etc/yum.repos.d/docker.repo";
const DNF_URL: &str = "https://download.docker.com/linux/rhel/docker-ce.repo";
const KEY_URL: &str = "https://download.docker.com/linux/ubuntu/gpg";

// --- apt ---

#[test]
fn repo_apt_first_run_writes_key_and_source_then_rerun_makes_no_call() {
    let d = tempfile::tempdir().unwrap();
    stubs(d.path());
    let h = host(ubuntu());
    let id = h.repo("docker").apt(docker_apt(&h)).id();
    let script = wrapped(&h, id);

    let (r, err) = run_script(&script, d.path());
    assert_eq!(core(&r), res(0, true), "{err}");
    assert_eq!(read(d.path(), KEY), format!("FETCHED {KEY_URL}\n"));
    assert_eq!(read(d.path(), SRC), want_source(d.path(), "amd64"));
    assert_eq!(mode(d.path(), KEY), 0o644);
    assert_eq!(mode(d.path(), SRC), 0o644);
    assert_eq!(curllog(d.path()), format!("curl -fsSL {KEY_URL}\n"));
    assert_eq!(listing(d.path(), "etc/apt/keyrings"), ["docker.asc"]);
    assert_eq!(
        listing(d.path(), "etc/apt/sources.list.d"),
        ["docker.sources"],
        "no temp files left"
    );

    let (r, _) = run_script(&script, d.path());
    assert_eq!(core(&r), res(0, false));
    assert_eq!(curllog(d.path()).lines().count(), 1, "no network call");
}

#[test]
fn repo_apt_architecture_comes_from_the_host() {
    let d = tempfile::tempdir().unwrap();
    stubs(d.path());
    std::fs::write(d.path().join("arch"), "arm64\n").unwrap();
    let h = host(ubuntu());
    let id = h.repo("docker").apt(docker_apt(&h)).id();
    let (r, err) = run_script(&wrapped(&h, id), d.path());
    assert_eq!(core(&r), res(0, true), "{err}");
    assert_eq!(read(d.path(), SRC), want_source(d.path(), "arm64"));
}

#[test]
fn repo_apt_changed_source_is_rewritten_without_a_fetch() {
    let d = tempfile::tempdir().unwrap();
    stubs(d.path());
    let h = host(ubuntu());
    let id = h.repo("docker").apt(docker_apt(&h)).id();
    let script = wrapped(&h, id);
    run_script(&script, d.path());
    std::fs::write(d.path().join(SRC), "hand edited\n").unwrap();
    std::fs::write(d.path().join(KEY), "my own key\n").unwrap();
    let (r, _) = run_script(&script, d.path());
    assert_eq!(core(&r), res(0, true));
    assert_eq!(read(d.path(), SRC), want_source(d.path(), "amd64"));
    assert_eq!(read(d.path(), KEY), "my own key\n", "an existing key stays");
    assert_eq!(curllog(d.path()).lines().count(), 1);
    // A byte-for-byte compare: a trailing blank line is a difference.
    std::fs::write(
        d.path().join(SRC),
        format!("{}\n", want_source(d.path(), "amd64")),
    )
    .unwrap();
    let (r, _) = run_script(&script, d.path());
    assert_eq!(core(&r), res(0, true));
}

#[test]
fn repo_apt_missing_key_is_fetched_when_the_source_is_fine() {
    let d = tempfile::tempdir().unwrap();
    stubs(d.path());
    let h = host(ubuntu());
    let id = h.repo("docker").apt(docker_apt(&h)).id();
    let script = wrapped(&h, id);
    run_script(&script, d.path());
    std::fs::remove_file(d.path().join(KEY)).unwrap();
    let (r, _) = run_script(&script, d.path());
    assert_eq!(core(&r), res(0, true));
    assert_eq!(curllog(d.path()).lines().count(), 2);
    assert_eq!(read(d.path(), KEY), format!("FETCHED {KEY_URL}\n"));
}

#[test]
fn repo_apt_failed_fetch_fails_and_leaves_nothing() {
    let d = tempfile::tempdir().unwrap();
    stubs(d.path());
    std::fs::write(d.path().join("curlfail"), "").unwrap();
    let h = host(ubuntu());
    let id = h.repo("docker").apt(docker_apt(&h)).id();
    let (r, _) = run_script(&wrapped(&h, id), d.path());
    assert_eq!(core(&r), res(1, false));
    assert!(!exists(d.path(), KEY));
    assert!(!exists(d.path(), SRC), "no source without its key");
    assert!(listing(d.path(), "etc/apt/keyrings").is_empty(), "no temp");
}

#[test]
fn repo_without_curl_errors_clearly_and_installs_nothing() {
    let d = tempfile::tempdir().unwrap();
    stubs(d.path());
    // A PATH with the tools the template needs, and no curl.
    let tools = d.path().join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    for t in [
        "bash",
        "env",
        "mktemp",
        "sha256sum",
        "cut",
        "mv",
        "rm",
        "chmod",
        "mkdir",
        "dirname",
        "cat",
    ] {
        let found = std::env::var("PATH")
            .unwrap()
            .split(':')
            .map(|p| Path::new(p).join(t))
            .find(|p| p.exists())
            .unwrap_or_else(|| panic!("{t} on PATH"));
        std::os::unix::fs::symlink(found, tools.join(t)).unwrap();
    }
    std::fs::copy(d.path().join("bin/dpkg"), tools.join("dpkg")).unwrap();
    let path = format!("{}", tools.display());
    let h = host(ubuntu());
    let id = h.repo("docker").apt(docker_apt(&h)).id();
    let out = Command::new(tools.join("env"))
        .arg("bash")
        .arg("-c")
        .arg(redirect(&wrapped(&h, id), d.path()))
        .env("PATH", &path)
        .output()
        .unwrap();
    let stream = String::from_utf8(out.stdout).unwrap();
    let (_, t) = split_stream(&stream, NONCE).remove(0);
    assert_eq!(core(&t.typed::<RepoResult>().unwrap()), res(1, false));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(
        err.contains("telepatch: repo docker: curl not found"),
        "{err}"
    );
    assert!(!exists(d.path(), KEY) && !exists(d.path(), SRC));
}

#[test]
fn repo_apt_lists_and_quotes_render_as_deb822_words() {
    let d = tempfile::tempdir().unwrap();
    stubs(d.path());
    let h = host(ubuntu());
    let id = h
        .repo("multi")
        .apt(AptRepo {
            uris: vec![
                "https://a.example/deb".into(),
                "https://b.example/deb".into(),
            ],
            suites: vec!["noble".into(), "noble-updates".into()],
            components: vec!["main".into(), "universe".into()],
            key_url: "https://a.example/key?x=1&y='2'".into(),
        })
        .id();
    let (r, err) = run_script(&wrapped(&h, id), d.path());
    assert_eq!(core(&r), res(0, true), "{err}");
    assert_eq!(
        read(d.path(), "etc/apt/sources.list.d/multi.sources"),
        format!(
            "Types: deb\nURIs: https://a.example/deb https://b.example/deb\nSuites: noble noble-updates\nComponents: main universe\nArchitectures: amd64\nSigned-By: {}/etc/apt/keyrings/multi.asc\n",
            d.path().display()
        )
    );
    assert_eq!(
        curllog(d.path()),
        "curl -fsSL https://a.example/key?x=1&y='2'\n"
    );
}

// --- dnf ---

#[test]
fn repo_dnf_url_fetches_only_when_absent() {
    let d = tempfile::tempdir().unwrap();
    stubs(d.path());
    let h = host(fedora());
    let id = h.repo("docker").dnf_url(DNF_URL).id();
    let script = wrapped(&h, id);
    let (r, err) = run_script(&script, d.path());
    assert_eq!(core(&r), res(0, true), "{err}");
    assert_eq!(read(d.path(), REPOFILE), format!("FETCHED {DNF_URL}\n"));
    assert_eq!(mode(d.path(), REPOFILE), 0o644);
    assert_eq!(listing(d.path(), "etc/yum.repos.d"), ["docker.repo"]);
    let (r, _) = run_script(&script, d.path());
    assert_eq!(core(&r), res(0, false));
    // Present but different: still untouched, no call.
    std::fs::write(d.path().join(REPOFILE), "local edit\n").unwrap();
    let (r, _) = run_script(&script, d.path());
    assert_eq!(core(&r), res(0, false));
    assert_eq!(read(d.path(), REPOFILE), "local edit\n");
    assert_eq!(curllog(d.path()).lines().count(), 1);
}

#[test]
fn repo_dnf_url_failed_fetch_fails() {
    let d = tempfile::tempdir().unwrap();
    stubs(d.path());
    std::fs::write(d.path().join("curlfail"), "").unwrap();
    let h = host(fedora());
    let id = h.repo("docker").dnf_url(DNF_URL).id();
    let (r, _) = run_script(&wrapped(&h, id), d.path());
    assert_eq!(core(&r), res(1, false));
    assert!(listing(d.path(), "etc/yum.repos.d").is_empty());
}

fn dnf_struct() -> DnfRepo {
    DnfRepo {
        name: "Docker CE".into(),
        baseurl: "https://download.docker.com/linux/rhel/$releasever/$basearch/stable".into(),
        gpgkey: "https://download.docker.com/linux/rhel/gpg".into(),
    }
}

#[test]
fn repo_dnf_struct_is_written_and_compared_byte_for_byte() {
    let d = tempfile::tempdir().unwrap();
    stubs(d.path());
    let h = host(fedora());
    let id = h.repo("docker").dnf(dnf_struct()).id();
    let script = wrapped(&h, id);
    let want = "[docker]\nname=Docker CE\nbaseurl=https://download.docker.com/linux/rhel/$releasever/$basearch/stable\nenabled=1\ngpgcheck=1\ngpgkey=https://download.docker.com/linux/rhel/gpg\n";
    let (r, err) = run_script(&script, d.path());
    assert_eq!(core(&r), res(0, true), "{err}");
    assert_eq!(read(d.path(), REPOFILE), want, "no shell expansion");
    assert_eq!(mode(d.path(), REPOFILE), 0o644);
    let (r, _) = run_script(&script, d.path());
    assert_eq!(core(&r), res(0, false));
    std::fs::write(d.path().join(REPOFILE), "changed\n").unwrap();
    let (r, _) = run_script(&script, d.path());
    assert_eq!(core(&r), res(0, true));
    assert_eq!(read(d.path(), REPOFILE), want);
    assert_eq!(curllog(d.path()), "", "a known body needs no network");
    assert_eq!(listing(d.path(), "etc/yum.repos.d"), ["docker.repo"]);
}

// --- builder ---

#[test]
fn repo_apt_effects_are_the_repo_and_file_keys() {
    let h = host(ubuntu());
    let id = h.repo("docker").apt(docker_apt(&h)).id();
    let snap = h.snapshot();
    let Node::Op(op) = &snap.nodes[id.0] else {
        panic!()
    };
    assert_eq!(op.spec.kind(), "repo");
    assert_eq!(
        op.writes,
        [
            Key::repo("docker"),
            Key::file("/etc/apt/keyrings/docker.asc"),
            Key::file("/etc/apt/sources.list.d/docker.sources"),
        ]
        .into()
    );
    assert_eq!(
        op.post.get(&Key::file("/etc/apt/keyrings/docker.asc")),
        Some(&Value::Exists(true))
    );
    assert!(snap.errors.is_empty(), "{:?}", snap.errors);
}

#[test]
fn repo_dnf_effects_are_the_repo_and_file_keys() {
    let h = host(fedora());
    let a = h.repo("docker").dnf_url(DNF_URL).id();
    let b = h.repo("other").dnf(dnf_struct()).id();
    let snap = h.snapshot();
    for (id, n) in [(a, "docker"), (b, "other")] {
        let Node::Op(op) = &snap.nodes[id.0] else {
            panic!()
        };
        assert_eq!(
            op.writes,
            [
                Key::repo(n),
                Key::file(format!("/etc/yum.repos.d/{n}.repo"))
            ]
            .into()
        );
    }
    assert!(snap.errors.is_empty());
}

fn errors(h: &Host<()>) -> Vec<String> {
    h.snapshot().errors.iter().map(|e| e.to_string()).collect()
}

#[test]
fn repo_wrong_family_is_a_trace_error() {
    let h = host(fedora());
    h.repo("docker").apt(docker_apt(&host(ubuntu())));
    let e = errors(&h);
    assert_eq!(e.len(), 1, "{e:?}");
    assert!(e[0].contains("repo") && e[0].contains("apt"), "{}", e[0]);

    let h = host(ubuntu());
    h.repo("a").dnf_url(DNF_URL);
    h.repo("b").dnf(dnf_struct());
    let e = errors(&h);
    assert_eq!(e.len(), 2, "{e:?}");
    assert!(e.iter().all(|m| m.contains("repo") && m.contains("dnf")));

    let ok = host(ubuntu());
    ok.repo("docker").apt(docker_apt(&ok));
    assert!(errors(&ok).is_empty());
}

#[test]
fn repo_bad_name_and_bad_values_are_trace_errors() {
    let h = host(ubuntu());
    h.repo("../evil").apt(docker_apt(&h));
    h.repo("").apt(docker_apt(&h));
    h.repo("ok").apt(AptRepo {
        uris: vec!["https://x y".into()],
        ..docker_apt(&h)
    });
    h.repo("ok2").apt(AptRepo {
        suites: vec![],
        ..docker_apt(&h)
    });
    h.repo("ok3").apt(AptRepo {
        key_url: "https://x\nrm -rf /".into(),
        ..docker_apt(&h)
    });
    assert_eq!(errors(&h).len(), 5, "{:?}", errors(&h));
    let f = host(fedora());
    f.repo("d").dnf(DnfRepo {
        name: "a\nb".into(),
        ..dnf_struct()
    });
    assert_eq!(errors(&f).len(), 1);
}

#[test]
fn repo_is_root_only() {
    let h = host(ubuntu());
    h.as_user("deploy").repo("docker").apt(docker_apt(&h));
    let e = errors(&h);
    assert_eq!(e.len(), 1, "{e:?}");
    assert!(e[0].contains("root"), "{}", e[0]);
    let ok = host(ubuntu());
    ok.as_user("root").repo("docker").apt(docker_apt(&ok));
    assert!(errors(&ok).is_empty());
}

#[test]
fn repo_without_a_source_fails_when_run() {
    let d = tempfile::tempdir().unwrap();
    stubs(d.path());
    let h = host(ubuntu());
    let id = h.repo("docker").id();
    let (r, err) = run_script(&wrapped(&h, id), d.path());
    assert_eq!(core(&r), res(1, false));
    assert!(err.contains("telepatch: repo docker:"), "{err}");
}

#[test]
fn repo_template_snapshots() {
    let h = host(ubuntu());
    let a = h.repo("docker").apt(docker_apt(&h)).id();
    let f = host(fedora());
    let b = f.repo("docker").dnf_url(DNF_URL).id();
    let c = f.repo("docker").dnf(dnf_struct()).id();
    insta::assert_snapshot!(format!(
        "{}\n{}\n{}",
        wrapped_display(&h, a),
        wrapped_display(&f, b),
        wrapped_display(&f, c)
    ));
}

#[test]
fn repo_plan_shows_effects() {
    let h = host(ubuntu());
    h.repo("docker").apt(docker_apt(&h));
    h.pkg(["docker-ce-cli"]);
    insta::assert_snapshot!(render_host("h", &h.os, &h.snapshot()));
}

// --- the prepare probe ---

/// Run the probe for `specs` against `d`; the lines it printed.
fn probed(specs: &[&RepoSpec], d: &Path) -> Vec<String> {
    let out = bash(&probe(specs), d, &full_path(d));
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn repo_probe_reports_absent_or_different_and_nothing_when_settled() {
    let d = tempfile::tempdir().unwrap();
    stubs(d.path());
    let h = host(ubuntu());
    let id = h.repo("docker").apt(docker_apt(&h)).id();
    let spec = spec_of(&h, id);
    assert_eq!(findings(&probed(&[&spec], d.path()).join("\n")), ["docker"]);

    run_script(&wrapped(&h, id), d.path());
    assert!(probed(&[&spec], d.path()).is_empty());

    // Source differs: will add. Key differs: nothing (only absent counts).
    std::fs::write(d.path().join(KEY), "other key\n").unwrap();
    assert!(probed(&[&spec], d.path()).is_empty());
    std::fs::write(d.path().join(SRC), "edited\n").unwrap();
    assert_eq!(findings(&probed(&[&spec], d.path()).join("\n")), ["docker"]);
    std::fs::write(d.path().join(SRC), want_source(d.path(), "amd64")).unwrap();
    std::fs::remove_file(d.path().join(KEY)).unwrap();
    assert_eq!(findings(&probed(&[&spec], d.path()).join("\n")), ["docker"]);
}

#[test]
fn repo_probe_dnf() {
    let d = tempfile::tempdir().unwrap();
    stubs(d.path());
    let f = host(fedora());
    let a = f.repo("docker").dnf_url(DNF_URL).id();
    let b = f.repo("other").dnf(dnf_struct()).id();
    let (sa, sb) = (spec_of(&f, a), spec_of(&f, b));
    let got = findings(&probed(&[&sa, &sb], d.path()).join("\n"));
    assert_eq!(got, ["docker", "other"]);
    run_script(&wrapped(&f, a), d.path());
    std::fs::create_dir_all(d.path().join("etc/yum.repos.d")).unwrap();
    std::fs::write(d.path().join("etc/yum.repos.d/other.repo"), "stale\n").unwrap();
    assert_eq!(
        findings(&probed(&[&sa, &sb], d.path()).join("\n")),
        ["other"]
    );
}

#[test]
fn repo_probe_with_a_failing_dpkg_reports_nothing() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    std::fs::write(bin.join("dpkg"), "#!/bin/bash\nexit 1\n").unwrap();
    let h = host(ubuntu());
    let id = h.repo("docker").apt(docker_apt(&h)).id();
    let spec = spec_of(&h, id);
    // The key is absent, so that alone is a will-add; make it present.
    std::fs::create_dir_all(d.path().join("etc/apt/keyrings")).unwrap();
    std::fs::write(d.path().join(KEY), "k\n").unwrap();
    assert!(probed(&[&spec], d.path()).is_empty());
}

#[test]
fn repo_will_create_line_reads_will_add() {
    let w = WillCreate {
        window: 1,
        kind: "repo".into(),
        name: "docker".into(),
        detail: Some("will add".into()),
    };
    assert_eq!(w.line(), "repo docker: will add");
}

// --- no cmp, no diff on the host ---

/// Every `/usr/bin` entry except `cmp` and `diff`, with the stub `curl` and
/// `dpkg` in front.
fn nocmp_path(d: &Path) -> String {
    stubs(d);
    let farm = d.join("nocmp");
    std::fs::create_dir_all(&farm).unwrap();
    for e in std::fs::read_dir("/usr/bin").unwrap().flatten() {
        let n = e.file_name();
        if matches!(n.to_str(), Some("cmp" | "diff")) {
            continue;
        }
        let _ = std::os::unix::fs::symlink(e.path(), farm.join(&n));
    }
    format!("{}:{}", d.join("bin").display(), farm.display())
}

#[test]
fn repo_op_and_probe_need_no_cmp_or_diff_on_the_host() {
    for (os, dnf) in [(ubuntu(), false), (fedora(), true)] {
        let d = tempfile::tempdir().unwrap();
        let path = nocmp_path(d.path());
        let h = host(os);
        let id = if dnf {
            h.repo("other").dnf(dnf_struct()).id()
        } else {
            h.repo("docker").apt(docker_apt(&h)).id()
        };
        let spec = spec_of(&h, id);
        let run = |d: &Path| {
            let out = bash(&wrapped(&h, id), d, &path);
            let stream = String::from_utf8(out.stdout).unwrap();
            let err = String::from_utf8(out.stderr).unwrap();
            assert!(!err.contains("command not found"), "{err}");
            let (_, t) = split_stream(&stream, NONCE).remove(0);
            t.typed::<RepoResult>().unwrap()
        };
        let probed = |d: &Path| {
            let out = bash(&probe(&[&spec]), d, &path);
            let err = String::from_utf8(out.stderr).unwrap();
            assert!(!err.contains("command not found"), "{err}");
            String::from_utf8(out.stdout).unwrap()
        };
        assert!(!probed(d.path()).is_empty(), "absent: will add");
        assert_eq!(core(&run(d.path())), res(0, true));
        assert_eq!(core(&run(d.path())), res(0, false), "settled: quiet");
        assert_eq!(probed(d.path()), "", "settled: nothing to add");
        let file = if dnf {
            "etc/yum.repos.d/other.repo"
        } else {
            SRC
        };
        std::fs::write(d.path().join(file), "edited\n").unwrap();
        assert!(!probed(d.path()).is_empty(), "edited: will add");
        assert_eq!(run(d.path()), res(0, true), "edited: rewritten");
    }
}

// --- inverse capture (T107) ---

const RUN: &str = "r1";
const OP_ID: usize = 7;

fn me() -> String {
    let o = Command::new("id").arg("-un").output().unwrap();
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

/// Run the op as a run's op: a stage in `d`, `__tp_run` set, and `roots` as
/// `TP_FS_ROOTS`. (result, stderr)
fn run_capture(h: &Host<()>, id: NodeId, d: &Path, roots: Option<&Path>) -> (RepoResult, String) {
    let script = match &h.snapshot().nodes[id.0] {
        Node::Op(op) => render_op(op, OP_ID, NONCE, &h.os),
        _ => panic!("not an op"),
    };
    let mut c = Command::new("/usr/bin/env");
    c.arg("bash")
        .arg("-c")
        .arg(redirect(&script, d))
        .current_dir(d)
        .env("PATH", full_path(d))
        .env_remove("TP_FS_ROOTS")
        .env("TP_FETCH_STAGE", d.join("stage"))
        .env("__tp_run", RUN);
    if let Some(r) = roots {
        c.env("TP_FS_ROOTS", r);
    }
    let out = c.output().unwrap();
    let stream = String::from_utf8(out.stdout).unwrap();
    let mut v = split_stream(&stream, NONCE);
    assert_eq!(v.len(), 1, "stream: {stream:?}");
    (
        v.remove(0).1.typed::<RepoResult>().unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

fn stage_dir(d: &Path) -> PathBuf {
    d.join("stage")
        .join(RUN)
        .join("undo")
        .join(me())
        .join(OP_ID.to_string())
}

#[test]
fn repo_apt_on_a_bare_host_says_it_created_both_files() {
    let d = tempfile::tempdir().unwrap();
    stubs(d.path());
    let h = host(ubuntu());
    let id = h.repo("docker").apt(docker_apt(&h)).id();
    let (r, err) = run_capture(&h, id, d.path(), None);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert!(r.key_created && r.file_created, "{r:?}");
    assert_eq!(r.backup, "");
    assert!(!stage_dir(d.path()).exists(), "a new file saves nothing");
}

#[test]
fn repo_apt_changed_source_is_backed_up_and_the_key_is_untouched() {
    let d = tempfile::tempdir().unwrap();
    stubs(d.path());
    let h = host(ubuntu());
    let id = h.repo("docker").apt(docker_apt(&h)).id();
    run_script(&wrapped(&h, id), d.path());
    std::fs::write(d.path().join(SRC), "hand edited\n").unwrap();
    let (r, err) = run_capture(&h, id, d.path(), None);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert!(!r.key_created && !r.file_created, "{r:?}");
    assert_eq!(r.backup, stage_dir(d.path()).to_str().unwrap());
    let meta = std::fs::symlink_metadata(stage_dir(d.path())).unwrap();
    assert!(meta.is_dir() && !meta.file_type().is_symlink());
    assert_eq!(
        std::fs::read_to_string(stage_dir(d.path()).join("old")).unwrap(),
        "hand edited\n"
    );
    assert_eq!(read(d.path(), SRC), want_source(d.path(), "amd64"));
    assert_eq!(
        curllog(d.path()).lines().count(),
        1,
        "the key is not fetched"
    );
    // Settled: nothing changes, nothing is saved.
    std::fs::remove_dir_all(d.path().join("stage")).unwrap();
    let (r, _) = run_capture(&h, id, d.path(), None);
    assert_eq!((r.changed, r.backup.as_str()), (false, ""));
    assert!(!d.path().join("stage").exists());
}

#[test]
fn repo_source_on_another_filesystem_is_backed_up_there_and_linked() {
    let d = tempfile::tempdir().unwrap();
    stubs(d.path());
    let h = host(ubuntu());
    let id = h.repo("docker").apt(docker_apt(&h)).id();
    run_script(&wrapped(&h, id), d.path());
    std::fs::write(d.path().join(SRC), "hand edited\n").unwrap();
    let etc = d.path().join("etc");
    let (r, err) = run_capture(&h, id, d.path(), Some(&etc));
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert_eq!(r.backup, stage_dir(d.path()).to_str().unwrap());
    let there = etc
        .join(".telepatch/tmp")
        .join(RUN)
        .join("undo")
        .join(me())
        .join(OP_ID.to_string());
    assert_eq!(std::fs::read_link(stage_dir(d.path())).unwrap(), there);
    assert_eq!(
        std::fs::read_to_string(there.join("old")).unwrap(),
        "hand edited\n"
    );
    assert!(!d.path().join(".telepatch").exists());
}

#[test]
fn repo_backup_failure_fails_the_op_before_the_source_is_rewritten() {
    use std::os::unix::fs::PermissionsExt;
    if Command::new("id").arg("-u").output().unwrap().stdout == b"0\n" {
        return;
    }
    let d = tempfile::tempdir().unwrap();
    stubs(d.path());
    let h = host(ubuntu());
    let id = h.repo("docker").apt(docker_apt(&h)).id();
    run_script(&wrapped(&h, id), d.path());
    std::fs::write(d.path().join(SRC), "hand edited\n").unwrap();
    let etc = d.path().join("etc");
    std::fs::set_permissions(&etc, std::fs::Permissions::from_mode(0o555)).unwrap();
    let (r, err) = run_capture(&h, id, d.path(), Some(&etc));
    std::fs::set_permissions(&etc, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!((r.rc, r.changed), (1, false), "{err}");
    assert!(err.contains("cannot save undo data"), "{err}");
    assert_eq!(read(d.path(), SRC), "hand edited\n");
    assert_eq!(r.backup, "");
}

#[test]
fn repo_dnf_url_says_whether_it_created_the_file() {
    let d = tempfile::tempdir().unwrap();
    stubs(d.path());
    let h = host(fedora());
    let id = h.repo("docker").dnf_url(DNF_URL).id();
    let (r, err) = run_capture(&h, id, d.path(), None);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert!(
        r.file_created && !r.key_created && r.backup.is_empty(),
        "{r:?}"
    );
    // Present but different: untouched, so neither created nor saved.
    std::fs::write(d.path().join(REPOFILE), "local edit\n").unwrap();
    let (r, _) = run_capture(&h, id, d.path(), None);
    assert!(
        !r.changed && !r.file_created && r.backup.is_empty(),
        "{r:?}"
    );
    assert_eq!(read(d.path(), REPOFILE), "local edit\n");
}

#[test]
fn repo_dnf_body_rewrite_backs_up_the_old_repo_file() {
    let d = tempfile::tempdir().unwrap();
    stubs(d.path());
    let h = host(fedora());
    let id = h.repo("docker").dnf(dnf_struct()).id();
    let (r, err) = run_capture(&h, id, d.path(), None);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert!(r.file_created && r.backup.is_empty(), "{r:?}");
    std::fs::write(d.path().join(REPOFILE), "old body\n").unwrap();
    let (r, err) = run_capture(&h, id, d.path(), None);
    assert_eq!((r.rc, r.changed), (0, true), "{err}");
    assert!(!r.file_created, "{r:?}");
    assert_eq!(r.backup, stage_dir(d.path()).to_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(stage_dir(d.path()).join("old")).unwrap(),
        "old body\n"
    );
}

#[test]
fn repo_has_an_inverse_unless_it_has_no_source() {
    let h = host(ubuntu());
    let apt = h.repo("a").apt(docker_apt(&h)).id();
    let none = h.repo("b").id();
    let inv = |id: NodeId| match &h.snapshot().nodes[id.0] {
        Node::Op(op) => op.spec.has_inverse(),
        _ => panic!("not an op"),
    };
    assert!(inv(apt));
    assert!(!inv(none));
}
