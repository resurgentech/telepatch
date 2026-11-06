//! `.locked()` is part of a cargo op's source: locked ops merge with locked
//! ops (one `cargo install --locked`), never with unlocked ones.

use telepatch::testing::plan;
use telepatch::{Host, Playbook, PlaybookFuture, TraceError};

fn run(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        {
            let _r = h.role_enter("a");
            h.pkg(["cargo-deb"]).cargo().locked();
        }
        {
            let _r = h.role_enter("b");
            h.pkg(["cargo-generate-rpm"]).cargo().locked();
        }
        {
            let _r = h.role_enter("c");
            h.pkg(["ripgrep"]).cargo();
        }
        Ok::<(), TraceError>(())
    })
}

#[test]
fn locked_merges_with_locked_never_with_unlocked() {
    let out = plan(&Playbook::new("t", run), &super::common::ubuntu());
    assert_eq!(out.matches("cargo install --locked").count(), 1, "{out}");
    insta::assert_snapshot!(out);
}
