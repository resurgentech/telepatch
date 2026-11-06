//! Two roles name the same toolchain for the same user: one op. Role `a`
//! wants it `.latest()` with a target, role `b` wants it present with
//! `rust-src`; between them sit an unlocked cargo op and a distro `pkg`
//! op, which touch none of the keys `b`'s op adds, so `b`'s op joins `a`'s
//! (wanting Latest). The distro op stays where it was, after the toolchain.

use telepatch::testing::plan;
use telepatch::{Host, Playbook, PlaybookFuture, TraceError};

fn run(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        {
            let _r = h.role_enter("a");
            h.pkg(["stable"])
                .rustup()
                .targets(["x86_64-unknown-linux-musl"])
                .latest();
            h.pkg(["cargo-deb"]).cargo();
        }
        {
            let _r = h.role_enter("b");
            h.pkg(["libclang-dev"]);
            h.pkg(["stable"]).rustup().components(["rust-src"]);
            h.pkg(["bindgen-cli"]).cargo().locked();
        }
        Ok::<(), TraceError>(())
    })
}

#[test]
fn rustup_present_joins_latest() {
    let out = plan(&Playbook::new("t", run), &super::common::ubuntu());
    assert_eq!(out.matches("pkg[rustup]").count(), 1, "{out}");
    assert!(out.contains("merged from a #1, b #"), "{out}");
    let rustup = out.find("pkg[rustup]").unwrap();
    let distro = out.find("pkg as ").expect("the distro op");
    let first_cargo = out.find("pkg[cargo]").expect("a cargo op");
    assert!(rustup < first_cargo && first_cargo < distro, "{out}");
    let op = &out[rustup..first_cargo];
    assert!(
        op.lines()
            .next()
            .unwrap()
            .contains("pkg:rustup:root:stable/component/rust-src=Installed"),
        "{op}"
    );
    assert!(op.contains("for tp_x in 'rust-src'; do"), "{op}");
    assert!(op.contains("rustup check"), "{op}");
    assert_eq!(out.matches("pkg[cargo]").count(), 2, "{out}");
    insta::assert_snapshot!(out);
}
