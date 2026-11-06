//! pip and apt are different managers: each merges only with its own kind.

use telepatch::testing::plan;
use telepatch::{Host, Playbook, PlaybookFuture, TraceError};

fn run(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        {
            let _r = h.role_enter("a");
            h.pkg(["python3-pip"]);
            h.pkg(["requests"]).pip();
        }
        {
            let _r = h.role_enter("b");
            h.pkg(["git"]);
            h.pkg(["flask"]).pip();
        }
        Ok::<(), TraceError>(())
    })
}

#[test]
fn pip_does_not_merge_with_apt() {
    insta::assert_snapshot!(plan(&Playbook::new("t", run), &super::common::ubuntu()));
}
