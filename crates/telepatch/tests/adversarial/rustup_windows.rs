//! The same user and toolchain in two windows: the later op only adds what
//! the earlier one lacked.

use telepatch::testing::plan;
use telepatch::{Host, Playbook, PlaybookFuture, TraceError};

fn run(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        {
            let _r = h.role_enter("a");
            h.pkg(["stable"])
                .rustup()
                .targets(["x86_64-unknown-linux-musl", "wasm32-unknown-unknown"])
                .components(["clippy"]);
            // bin:git overlaps the rustup op's `*` write: window 1 ends here.
            let _ = h.bin_exists("git").await;
        }
        {
            let _r = h.role_enter("b");
            h.pkg(["stable"])
                .rustup()
                .targets(["wasm32-unknown-unknown", "aarch64-unknown-linux-gnu"])
                .components(["clippy"]);
            // Nothing left to add, and another user keeps its own.
            h.pkg(["stable"]).rustup().components(["clippy"]);
            h.as_user("deploy")
                .pkg(["stable"])
                .rustup()
                .targets(["wasm32-unknown-unknown"]);
        }
        Ok::<(), TraceError>(())
    })
}

#[test]
fn later_window_only_adds_what_the_earlier_lacked() {
    let out = plan(&Playbook { name: "t", run }, &super::common::ubuntu());
    let w2 = out.split("window 2").nth(1).expect("two windows");
    assert!(w2.contains("aarch64-unknown-linux-gnu"), "{out}");
    assert!(!w2.contains("'x86_64-unknown-linux-musl'"), "{out}");
    assert!(!w2.contains("'clippy'"), "{out}");
    // The other user's op is untouched.
    assert!(w2.contains("wasm32-unknown-unknown"), "{out}");
    insta::assert_snapshot!(out);
}
