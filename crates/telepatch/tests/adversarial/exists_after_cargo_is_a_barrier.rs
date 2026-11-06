//! A cargo op writes `home:<user>`, which overlaps every `file:` key: a
//! fact read under the user's home after it waits for the op (an implicit
//! barrier), so a later cargo op does not merge back into it.

use telepatch::testing::plan;
use telepatch::{Host, Playbook, PlaybookFuture, TraceError};

fn run(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        {
            let _r = h.role_enter("a");
            h.pkg(["cargo-deb"]).cargo();
            let _ = h.exists("/root/.cargo/bin/x").await;
        }
        {
            let _r = h.role_enter("b");
            h.pkg(["cargo-generate-rpm"]).cargo();
        }
        Ok::<(), TraceError>(())
    })
}

#[test]
fn exists_after_cargo_is_a_barrier() {
    let out = plan(&Playbook::new("t", run), &super::common::ubuntu());
    assert!(out.contains("window 1 (implicit barrier)"), "{out}");
    assert!(out.contains("blocking reads: 1"), "{out}");
    assert_eq!(out.matches("pkg[cargo]").count(), 2, "{out}");
    assert!(!out.contains("merged from"), "{out}");
    insta::assert_snapshot!(out);
}
