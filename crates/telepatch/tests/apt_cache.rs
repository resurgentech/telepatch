//! The pure apt-cache pass: which apt ops need an `apt-get update` first.

use std::rc::Rc;

use telepatch::facts::os::Os;
use telepatch::ops::pkg::apt_update_needed;
use telepatch::{Host, Key, Node, OpNode};

fn host() -> Host<()> {
    Host::new(
        "t",
        Os::parse("ID=debian\nVERSION_ID=12\n").unwrap(),
        Rc::new(()),
    )
}

fn ops(h: &Host<()>) -> Vec<OpNode> {
    h.snapshot()
        .nodes
        .into_iter()
        .filter_map(|n| if let Node::Op(o) = n { Some(o) } else { None })
        .collect()
}

#[test]
fn nothing_touching_apt_means_no_update() {
    let h = host();
    h.pkg(["a"]);
    h.raw("echo hi").writes([Key::file("/tmp/x")]);
    h.pkg(["b"]);
    assert_eq!(apt_update_needed(&ops(&h)), [false, false, false]);
}

#[test]
fn star_writer_before_apt_forces_update_once() {
    let h = host();
    h.raw("curl | sh"); // writes *
    h.pkg(["a"]);
    h.pkg(["b"]);
    assert_eq!(apt_update_needed(&ops(&h)), [false, true, false]);
}

#[test]
fn apt_sources_write_forces_update() {
    let h = host();
    h.file("/etc/apt/sources.list.d/x.list")
        .line("deb http://x y z");
    h.pkg(["a"]);
    h.file("/etc/aptitude").line("x"); // not under /etc/apt/
    h.pkg(["b"]);
    assert_eq!(apt_update_needed(&ops(&h)), [false, true, false, false]);
}

#[test]
fn a_second_sources_write_forces_a_second_update() {
    let h = host();
    h.file("/etc/apt/a.list").line("x");
    h.pkg(["a"]);
    h.raw("x");
    h.pkg(["b"]);
    assert_eq!(apt_update_needed(&ops(&h)), [false, true, false, true]);
}

#[test]
fn non_apt_managers_are_never_flagged() {
    let h = host();
    h.raw("x");
    h.pkg(["requests"]).pip();
    assert_eq!(apt_update_needed(&ops(&h)), [false, false]);
}

#[test]
fn repo_op_forces_one_update_before_the_next_apt_op() {
    let h = host();
    h.pkg(["curl"]);
    h.repo("docker").apt(telepatch::ops::repo::AptRepo {
        uris: vec!["https://download.docker.com/linux/debian".into()],
        suites: vec!["bookworm".into()],
        components: vec!["stable".into()],
        key_url: "https://download.docker.com/linux/debian/gpg".into(),
    });
    h.pkg(["docker-ce-cli"]);
    h.pkg(["jq"]);
    assert_eq!(
        apt_update_needed(&ops(&h)),
        [false, false, true, false],
        "the key and source are under /etc/apt/"
    );
}
