//! Template snapshots: the fully wrapped script of each op kind per manager,
//! with a fixed nonce, plus the plan lines that show effects.

use std::rc::Rc;

use telepatch::facts::os::Os;
use telepatch::ops::pkg::{Latest, ScriptSpec};
use telepatch::plan::render_host;
use telepatch::trailer::render_op;
use telepatch::{Host, Node, NodeId, sh};

fn host(fixture: &str) -> Host<()> {
    let p = format!(
        "{}/tests/fixtures/os-release/{fixture}",
        env!("CARGO_MANIFEST_DIR")
    );
    let os = Os::parse(&std::fs::read_to_string(p).unwrap()).unwrap();
    Host::new("h", os, Rc::new(()))
}

fn wrapped(h: &Host<()>, id: NodeId) -> String {
    match &h.snapshot().nodes[id.0] {
        Node::Op(op) => render_op(op, id.0, "testnonce", &h.os),
        _ => panic!(),
    }
}

#[test]
fn apt_install() {
    let h = host("debian12");
    let id = h.pkg(["curl", "build-essential"]).id();
    insta::assert_snapshot!(wrapped(&h, id));
}

#[test]
fn apt_install_with_cache_update() {
    let h = host("ubuntu2404");
    let p = h.pkg(["curl"]);
    p.update_cache(true);
    insta::assert_snapshot!(wrapped(&h, p.id()));
}

#[test]
fn apt_build_dep() {
    let h = host("debian13");
    let p = h.pkg(["python3"]);
    p.build_dep();
    insta::assert_snapshot!(wrapped(&h, p.id()));
}

#[test]
fn dnf_group_and_provide() {
    let h = host("rockylinux9");
    let id = h
        .pkg(["@Development Tools", "perl(ExtUtils::Embed)", "git"])
        .id();
    insta::assert_snapshot!(wrapped(&h, id));
}

#[test]
fn yum() {
    let h = host("amazonlinux2023");
    let id = h.yum(["git", "@Development Tools"]).id();
    insta::assert_snapshot!(wrapped(&h, id));
}

#[test]
fn pacman() {
    let h = host("arch");
    let id = h.pkg(["base-devel", "git"]).id();
    insta::assert_snapshot!(wrapped(&h, id));
}

#[test]
fn pip() {
    let h = host("debian12");
    let id = h.pip(["requests", "pyyaml"]).id();
    insta::assert_snapshot!(wrapped(&h, id));
}

#[test]
fn pip_latest() {
    let h = host("debian12");
    let p = h.pip(["requests"]);
    p.latest();
    insta::assert_snapshot!(wrapped(&h, p.id()));
}

#[test]
fn file_content_mode_owner() {
    let h = host("debian12");
    let id = h
        .file("/etc/motd")
        .content("hi\n")
        .mode(0o644)
        .owner("root:root")
        .id();
    insta::assert_snapshot!(wrapped(&h, id));
}

#[test]
fn file_lines() {
    let h = host("debian12");
    let id = h
        .file("~/.zshrc")
        .line("export A=1")
        .line_re("^ZSH_THEME=", "ZSH_THEME=\"x\"")
        .id();
    insta::assert_snapshot!(wrapped(&h, id));
}

#[test]
fn file_dir_and_absent() {
    let h = host("debian12");
    let d = h.file("/opt/x").dir().mode(0o755).id();
    let a = h.file("/opt/y").absent().id();
    insta::assert_snapshot!(format!("{}\n{}", wrapped(&h, d), wrapped(&h, a)));
}

#[test]
fn sh_op() {
    let h = host("debian12");
    let id = sh!(h, "make install", creates = "/usr/local/bin/x").id();
    insta::assert_snapshot!(wrapped(&h, id));
}

#[test]
fn plan_shows_pkg_and_file_effects() {
    let h = host("debian12");
    h.pkg(["curl"]);
    h.pip(["requests"]);
    h.pkg(["python3"]).build_dep();
    h.file("/etc/a").line("x");
    h.file("/etc/b").absent();
    insta::assert_snapshot!(render_host("h", &h.os, &h.snapshot()));
}

fn uv_spec() -> ScriptSpec {
    ScriptSpec::new()
        .bin("/usr/local/bin/uv")
        .version("uv self version --short")
        .install(
            "curl -LsSf https://astral.sh/uv/install.sh | env UV_INSTALL_DIR=/usr/local/bin sh",
        )
        .update("uv self update")
}

#[test]
fn script_present() {
    let h = host("debian12");
    let id = h.script("uv", uv_spec()).id();
    assert!(h.snapshot().errors.is_empty(), "{:?}", h.snapshot().errors);
    insta::assert_snapshot!(wrapped(&h, id));
}

#[test]
fn script_pinned() {
    let h = host("debian12");
    let p = h.script("uv", uv_spec());
    p.pin("0.9.5");
    assert!(h.snapshot().errors.is_empty(), "{:?}", h.snapshot().errors);
    insta::assert_snapshot!(wrapped(&h, p.id()));
}

#[test]
fn script_latest_github_release() {
    let h = host("debian12");
    let p = h.script(
        "uv",
        uv_spec().latest(Latest::github_release("astral-sh/uv")),
    );
    p.latest();
    assert!(h.snapshot().errors.is_empty(), "{:?}", h.snapshot().errors);
    insta::assert_snapshot!(wrapped(&h, p.id()));
}

#[test]
fn plan_shows_script_effects() {
    let h = host("debian12");
    h.script("uv", uv_spec());
    h.as_user("deploy")
        .script("tool", uv_spec().bin("/home/deploy/.local/bin/tool"))
        .writes([telepatch::Key::file("/home/deploy/.local/bin/tool")]);
    insta::assert_snapshot!(render_host("h", &h.os, &h.snapshot()));
}

#[test]
fn json_escapes_names() {
    // Trailer JSON stays valid for names with quotes and backslashes.
    let h = host("debian12");
    let id = h.pkg(["a\"b\\c"]).id();
    let d = tempfile::tempdir().unwrap();
    let bin = d.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for (n, body) in [
        ("dpkg-query", "exit 1"),
        ("apt-get", "exit 0"),
        ("find", "exit 0"),
    ] {
        let p = bin.join(n);
        write_exe(&p, &format!("#!/bin/bash\n{body}\n"));
    }
    let out = std::process::Command::new("bash")
        .arg("-c")
        .arg(wrapped(&h, id))
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .output()
        .unwrap();
    let s = String::from_utf8(out.stdout).unwrap();
    let v = telepatch::trailer::split_stream(&s, "testnonce");
    let r = v[0].1.typed::<telepatch::trailer::PkgResult>().unwrap();
    assert_eq!(r.installed, ["a\"b\\c"]);
}

#[test]
fn every_snapshot_template_is_valid_bash() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/snapshots");
    let mut n = 0;
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_str().unwrap().to_string();
        if !name.starts_with("ops_snapshots__") || name.contains("plan_shows") {
            continue;
        }
        let text = std::fs::read_to_string(&p).unwrap();
        let body = text.splitn(3, "---\n").nth(2).unwrap();
        let st = std::process::Command::new("bash")
            .args(["-n", "-c", body])
            .status()
            .unwrap();
        assert!(st.success(), "{name} is not valid bash");
        n += 1;
    }
    assert!(n >= 12, "only {n} snapshots checked");
}

/// Write an executable via a child `sh` so no write fd lives in this test
/// process (a sibling thread's fork could inherit it; exec then fails with
/// ETXTBSY).
fn write_exe(p: &std::path::Path, text: &str) {
    use std::io::Write;
    let mut c = std::process::Command::new("sh")
        .arg("-c")
        .arg("cat > \"$1\" && chmod 755 \"$1\"")
        .arg("sh")
        .arg(p)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
    assert!(c.wait().unwrap().success());
}

#[test]
fn cargo_present() {
    let h = host("debian12");
    let id = h.cargo(["cargo-deb", "cargo-generate-rpm"]).id();
    assert!(h.snapshot().errors.is_empty(), "{:?}", h.snapshot().errors);
    insta::assert_snapshot!(wrapped(&h, id));
}

#[test]
fn cargo_pinned() {
    let h = host("debian12");
    let p = h.cargo(["cargo-deb"]);
    p.pin("2.1.0");
    assert!(h.snapshot().errors.is_empty(), "{:?}", h.snapshot().errors);
    insta::assert_snapshot!(wrapped(&h, p.id()));
}

#[test]
fn cargo_latest() {
    let h = host("debian12");
    let p = h.cargo(["cargo-deb", "cargo-generate-rpm"]);
    p.latest();
    assert!(h.snapshot().errors.is_empty(), "{:?}", h.snapshot().errors);
    insta::assert_snapshot!(wrapped(&h, p.id()));
}

/// `as_user("root")` renders as a login shell for root (the login PATH
/// finds `~/.cargo/bin`); the op body is the same as for `h`.
#[test]
fn cargo_as_user_root() {
    let h = host("debian12");
    let id = h.as_user("root").cargo(["cargo-deb"]).id();
    assert!(h.snapshot().errors.is_empty(), "{:?}", h.snapshot().errors);
    insta::assert_snapshot!(wrapped(&h, id));
}

#[test]
fn plan_shows_cargo_effects() {
    let h = host("debian12");
    h.cargo(["cargo-deb", "cargo-generate-rpm"]);
    h.as_user("deploy")
        .cargo(["ripgrep"])
        .writes([telepatch::Key::file("/home/deploy/.cargo/bin/rg")]);
    insta::assert_snapshot!(render_host("h", &h.os, &h.snapshot()));
}
