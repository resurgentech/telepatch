//! Three roles each install packages with other work in between: one apt call.

use telepatch::testing::plan;
use telepatch::{Host, Key, Playbook, PlaybookFuture, TraceError, role, sh};

#[role]
async fn base(h: &Host<()>) -> Result<(), TraceError> {
    h.pkg(["curl", "git"]);
    h.file("/etc/motd").content("hi");
    Ok(())
}

#[role]
async fn web(h: &Host<()>) -> Result<(), TraceError> {
    h.pkg(["nginx", "curl"]);
    sh!(h, "nginx -t").writes([Key::file("/var/log/nginx-test")]);
    Ok(())
}

#[role]
async fn monitoring(h: &Host<()>) -> Result<(), TraceError> {
    h.pkg(["prometheus-node-exporter"]);
    Ok(())
}

fn run(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        base(&h).await?;
        web(&h).await?;
        monitoring(&h).await
    })
}

#[test]
fn merge_across_three_roles() {
    insta::assert_snapshot!(plan(&Playbook::new("t", run), &super::common::ubuntu()));
}
