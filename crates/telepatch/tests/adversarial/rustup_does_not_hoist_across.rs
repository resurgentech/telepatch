//! A later rustup op for the same user and toolchain stays apart from the
//! earlier one when something between them, or the op itself, forbids the
//! hoist: an undeclared `sh!` (writes `*`), a file op, a barrier; a
//! `.set_default()` or `.latest()` the earlier op lacks (checked as `*`,
//! so a cargo op between blocks it); and a different guard, `allow_fail`
//! or `RunAs` (not one merge key). One plan per case.

use telepatch::testing::plan;
use telepatch::{Host, Playbook, PlaybookFuture, TraceError, sh};

fn head(h: &Host<()>) {
    let _r = h.role_enter("a");
    h.pkg(["stable"]).rustup();
}

fn sh_between(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        head(&h);
        let _r = h.role_enter("b");
        sh!(h, "echo between");
        h.pkg(["stable"]).rustup().components(["rust-src"]);
        Ok::<(), TraceError>(())
    })
}

fn file_between(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        head(&h);
        let _r = h.role_enter("b");
        h.file("/etc/between").content("x\n");
        h.pkg(["stable"]).rustup().components(["rust-src"]);
        Ok::<(), TraceError>(())
    })
}

fn barrier_between(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        head(&h);
        let _r = h.role_enter("b");
        h.barrier();
        h.pkg(["stable"]).rustup().components(["rust-src"]);
        Ok::<(), TraceError>(())
    })
}

fn set_default_across_cargo(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        head(&h);
        let _r = h.role_enter("b");
        h.pkg(["cargo-deb"]).cargo();
        h.pkg(["stable"]).rustup().set_default();
        Ok::<(), TraceError>(())
    })
}

fn latest_onto_present_across_cargo(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        head(&h);
        let _r = h.role_enter("b");
        h.pkg(["cargo-deb"]).cargo();
        h.pkg(["stable"]).rustup().latest();
        Ok::<(), TraceError>(())
    })
}

fn guard_mismatch(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        let git = h.pkg(["git"]);
        head(&h);
        let _r = h.role_enter("b");
        h.pkg(["cargo-deb"]).cargo();
        h.pkg(["stable"])
            .rustup()
            .components(["rust-src"])
            .when(git.changed());
        Ok::<(), TraceError>(())
    })
}

fn allow_fail_mismatch(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        head(&h);
        let _r = h.role_enter("b");
        h.pkg(["cargo-deb"]).cargo();
        h.pkg(["stable"])
            .rustup()
            .components(["rust-src"])
            .allow_fail();
        Ok::<(), TraceError>(())
    })
}

fn run_as_mismatch(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        head(&h);
        let _r = h.role_enter("b");
        h.pkg(["cargo-deb"]).cargo();
        h.as_user("root")
            .pkg(["stable"])
            .rustup()
            .components(["rust-src"]);
        Ok::<(), TraceError>(())
    })
}

type Case = (&'static str, fn(Host<()>) -> PlaybookFuture);

#[test]
fn rustup_does_not_hoist_across() {
    let cases: [Case; 8] = [
        ("sh", sh_between),
        ("file", file_between),
        ("barrier", barrier_between),
        ("set_default across cargo", set_default_across_cargo),
        (
            "latest onto present across cargo",
            latest_onto_present_across_cargo,
        ),
        ("guard", guard_mismatch),
        ("allow_fail", allow_fail_mismatch),
        ("h and h.as_user(\"root\")", run_as_mismatch),
    ];
    let mut all = String::new();
    for (name, run) in cases {
        let out = plan(&Playbook::new("t", run), &super::common::ubuntu());
        assert_eq!(out.matches("pkg[rustup]").count(), 2, "{name}:\n{out}");
        assert!(!out.contains("merged from"), "{name}:\n{out}");
        all.push_str(&format!("=== {name}\n{out}\n"));
    }
    insta::assert_snapshot!(all);
}
