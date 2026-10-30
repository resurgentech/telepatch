//! The same fact read from two roles costs one round trip.

use telepatch::testing::plan;
use telepatch::{Host, Playbook, PlaybookFuture, TraceError};

fn run(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        {
            let _r = h.role_enter("a");
            if !h.exists("/etc/bootstrapped").await {
                h.file("/etc/a").content("a");
            }
        }
        {
            let _r = h.role_enter("b");
            if !h.exists("/etc/bootstrapped").await {
                h.file("/etc/b").content("b");
            }
        }
        Ok::<(), TraceError>(())
    })
}

#[test]
fn same_fact_from_two_roles_one_round_trip() {
    insta::assert_snapshot!(plan(&Playbook { name: "t", run }, &super::common::ubuntu()));
}
