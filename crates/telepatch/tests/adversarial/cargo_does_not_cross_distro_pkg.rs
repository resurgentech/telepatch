//! A cargo op never moves across a distro `pkg` op, either way: the distro
//! op may install a build dependency the crate needs (`libssl-dev`), so
//! two cargo ops with a distro op between stay three ops in trace order.

use telepatch::testing::plan;
use telepatch::{Host, Playbook, PlaybookFuture, TraceError};

fn run(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        {
            let _r = h.role_enter("a");
            h.pkg(["cargo-deb"]).cargo();
        }
        {
            let _r = h.role_enter("b");
            h.pkg(["libssl-dev"]);
            h.pkg(["cargo-generate-rpm"]).cargo();
        }
        Ok::<(), TraceError>(())
    })
}

#[test]
fn cargo_does_not_cross_distro_pkg() {
    let out = plan(&Playbook::new("t", run), &super::common::ubuntu());
    assert_eq!(out.matches("pkg[cargo]").count(), 2, "{out}");
    assert!(!out.contains("merged from"), "{out}");
    let a = out.find("pkg[cargo]").unwrap();
    let distro = out.find("pkg as ").expect("the distro op");
    let b = out.rfind("pkg[cargo]").unwrap();
    assert!(a < distro && distro < b, "{out}");
    insta::assert_snapshot!(out);
}
