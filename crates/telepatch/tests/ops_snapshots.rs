//! Template snapshots: the fully wrapped script of each op kind per manager,
//! with a fixed nonce, plus the plan lines that show effects.

use std::rc::Rc;

use telepatch::facts::os::Os;
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
        std::fs::write(&p, format!("#!/bin/bash\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
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
