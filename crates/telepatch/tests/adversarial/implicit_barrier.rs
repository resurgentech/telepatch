//! A read that needs a pending install forces an implicit barrier; ops
//! applied before it cannot merge with ops traced after it.

use telepatch::testing::plan;
use telepatch::{Host, Playbook, PlaybookFuture, TraceError};

fn run(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        {
            let _r = h.role_enter("a");
            h.pkg(["curl"]);
            h.pkg(["jq"]);
            // bin:git overlaps every pkg: key, so this needs the installs.
            let _ = h.bin_exists("git").await;
        }
        {
            let _r = h.role_enter("b");
            h.pkg(["git"]);
            h.pkg(["tmux"]);
        }
        Ok::<(), TraceError>(())
    })
}

#[test]
fn read_forces_implicit_barrier_and_splits_merging() {
    insta::assert_snapshot!(plan(&Playbook { name: "t", run }, &super::common::ubuntu()));
}
