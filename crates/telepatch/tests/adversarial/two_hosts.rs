//! Two hosts with different `os` get different merged plans.

use telepatch::facts::os::Family;
use telepatch::testing::plan_all;
use telepatch::{Host, Playbook, PlaybookFuture, TraceError, role};

#[role]
async fn web(h: &Host<()>) -> Result<(), TraceError> {
    match h.os.family {
        Family::Debian => h.pkg(["nginx", "libpq-dev"]),
        _ => h.pkg(["nginx", "libpq-devel"]),
    };
    Ok(())
}

#[role]
async fn tools(h: &Host<()>) -> Result<(), TraceError> {
    match h.os.family {
        Family::Debian => h.pkg(["build-essential"]),
        _ => h.pkg(["gcc", "make"]),
    };
    h.pkg(["git"]);
    Ok(())
}

fn run(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        web(&h).await?;
        tools(&h).await
    })
}

#[test]
fn different_os_different_merged_plans() {
    let hosts = [
        super::common::fixture("u1", "ubuntu2404"),
        super::common::fixture("r1", "rockylinux10"),
    ];
    insta::assert_snapshot!(plan_all(&Playbook { name: "t", run }, &hosts));
}
