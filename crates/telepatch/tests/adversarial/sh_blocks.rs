//! An undeclared `sh!` (writes `*`) between two pkg ops blocks the hoist.

use telepatch::testing::plan;
use telepatch::{Host, Playbook, PlaybookFuture, TraceError, role, sh};

#[role]
async fn first(h: &Host<()>) -> Result<(), TraceError> {
    h.pkg(["curl"]);
    Ok(())
}

#[role]
async fn script(h: &Host<()>) -> Result<(), TraceError> {
    sh!(h, "curl -fsSL https://example.org/install | sh");
    Ok(())
}

#[role]
async fn last(h: &Host<()>) -> Result<(), TraceError> {
    h.pkg(["git"]);
    Ok(())
}

fn run(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        first(&h).await?;
        script(&h).await?;
        last(&h).await
    })
}

#[test]
fn sh_between_pkgs_blocks_hoist() {
    insta::assert_snapshot!(plan(&Playbook { name: "t", run }, &super::common::ubuntu()));
}
