//! An explicit `barrier()` between two pkg ops keeps them apart.

use telepatch::testing::plan;
use telepatch::{Host, Playbook, PlaybookFuture, TraceError};

fn run(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        {
            let _r = h.role_enter("a");
            h.pkg(["curl"]);
            h.barrier();
        }
        {
            let _r = h.role_enter("b");
            h.pkg(["git"]);
            h.pkg(["jq"]);
        }
        Ok::<(), TraceError>(())
    })
}

#[test]
fn explicit_barrier_blocks_merge() {
    insta::assert_snapshot!(plan(&Playbook { name: "t", run }, &super::common::ubuntu()));
}
