//! Two rustup ops for one user and toolchain, from two roles, become one:
//! targets and components unioned, `set_default` or-ed.

use telepatch::testing::plan;
use telepatch::{Host, Playbook, PlaybookFuture, TraceError};

fn run(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        {
            let _r = h.role_enter("a");
            h.pkg(["stable"])
                .rustup()
                .targets(["x86_64-unknown-linux-musl"])
                .components(["clippy"]);
        }
        {
            let _r = h.role_enter("b");
            h.pkg(["stable"])
                .rustup()
                .targets(["x86_64-unknown-linux-musl", "wasm32-unknown-unknown"])
                .components(["rust-src"])
                .set_default();
        }
        {
            // Another toolchain, another user: separate ops.
            let _r = h.role_enter("c");
            h.pkg(["nightly"])
                .rustup()
                .targets(["wasm32-unknown-unknown"]);
            h.as_user("deploy").pkg(["stable"]).rustup();
        }
        Ok::<(), TraceError>(())
    })
}

#[test]
fn two_rustup_ops_merge_across_roles() {
    let out = plan(&Playbook { name: "t", run }, &super::common::ubuntu());
    assert!(out.contains("merged from a #1, b #"), "{out}");
    insta::assert_snapshot!(out);
}
