//! `build_dep` is a different mode from install: never merged with it.

use telepatch::testing::plan;
use telepatch::{Host, Playbook, PlaybookFuture, TraceError};

fn run(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        let _r = h.role_enter("toolchain");
        h.pkg(["gcc"]);
        h.pkg(["make"]);
        h.pkg(["python3"]).build_dep();
        Ok::<(), TraceError>(())
    })
}

#[test]
fn build_dep_does_not_merge_with_install() {
    insta::assert_snapshot!(plan(&Playbook { name: "t", run }, &super::common::ubuntu()));
}
