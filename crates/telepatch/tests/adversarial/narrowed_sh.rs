//! The same shape as `sh_blocks`, but the `sh!` declares narrow writes.

use telepatch::testing::plan;
use telepatch::{Host, Key, Playbook, PlaybookFuture, TraceError, role, sh};

#[role]
async fn first(h: &Host<()>) -> Result<(), TraceError> {
    h.pkg(["curl"]);
    Ok(())
}

#[role]
async fn script(h: &Host<()>) -> Result<(), TraceError> {
    sh!(h, "touch /var/lib/marker").writes([Key::file("/var/lib/marker")]);
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
fn narrowed_sh_does_not_block_hoist() {
    insta::assert_snapshot!(plan(&Playbook { name: "t", run }, &super::common::ubuntu()));
}
