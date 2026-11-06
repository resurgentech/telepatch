//! Plan snapshot of `sh!` with each option.

use std::rc::Rc;

use telepatch::facts::os::Os;
use telepatch::testing::{FixtureHost, trace};
use telepatch::{Host, Key, Playbook, PlaybookFuture, TraceError, role, sh};

#[role]
async fn demo(h: &Host<()>) -> Result<(), TraceError> {
    let target = "x86_64-unknown-linux-musl";
    let tool = "ripgrep";
    sh!(h, "echo plain");
    sh!(
        h,
        "rustup target add {target}",
        unless = "rustup target list --installed | grep -qx {target}"
    );
    let s = h.as_user("deploy");
    sh!(s, "cargo install {tool}", creates = "~/.cargo/bin/{tool}");
    sh!(
        h,
        "apt-get install -y {tool}",
        changed_if = "[1-9][0-9]* newly installed"
    )
    .writes([Key::pkg("ripgrep")]);
    sh!(h, "systemctl stop {{maybe}}").allow_fail();
    Ok(())
}

fn run(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move { demo(&h).await })
}

#[test]
fn sh_options_plan() {
    let os = Os::parse("ID=debian\nVERSION_ID=12\n").unwrap();
    let host = FixtureHost::new("h1", os, Rc::new(()));
    insta::assert_snapshot!(trace(&Playbook { name: "demo", run }, &host));
}

#[test]
fn optimized_plan_ends_with_the_warnings_too() {
    let os = Os::parse("ID=debian\nVERSION_ID=12\n").unwrap();
    let host = FixtureHost::new("h1", os, Rc::new(()));
    let text = telepatch::testing::plan(&Playbook { name: "demo", run }, &host);
    let warnings: Vec<&str> = text.lines().filter(|l| l.starts_with("warning:")).collect();
    assert_eq!(warnings.len(), 5, "{text}");
    assert!(
        warnings[0]
            .starts_with("warning: sh! in a role body at crates/telepatch/tests/sh_snapshot.rs:"),
        "{text}"
    );
    assert!(text.trim_end().ends_with(warnings[4]), "{text}");
}
