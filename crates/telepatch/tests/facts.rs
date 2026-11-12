//! Typed facts (`user_info`, `stat`, `pkg_installed`, `check`) and
//! `h.assert`. Reads polled together are one round trip. A failing assert
//! blocks the host in prepare before that window changes anything.

use std::future::Future;
use std::pin::pin;
use std::rc::Rc;
use std::task::Poll;

use telepatch::engine::Engine;
use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::facts::os::Os;
use telepatch::report::HostStatus;
use telepatch::testing::{FakeExecutor, block_on};
use telepatch::trace::RunAs;
use telepatch::transport::local::LocalTransport;
use telepatch::{Host, Playbook, PlaybookFuture, TraceError};

fn ubuntu() -> Os {
    Os::parse("ID=ubuntu\nVERSION_ID=24.04\n").unwrap()
}

fn target() -> Target {
    Target::Local(LocalTransport::new().uid(0))
}

fn go<V: 'static>(
    name: &'static str,
    f: fn(Host<V>) -> PlaybookFuture,
    v: V,
) -> (telepatch::report::Report, String) {
    let pb = Playbook::new(name, f);
    let out = Rc::new(std::cell::RefCell::new(String::new()));
    let report = run(
        &pb,
        vec![HostSpec {
            groups: Vec::new(),
            name: "h".into(),
            target: target(),
            vars: v,
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    (report, out.borrow().clone())
}

/// Poll every fact once before yielding, so the engine sees one batch.
async fn together(
    h: &Host<()>,
) -> (
    telepatch::key::UserInfo,
    telepatch::key::Stat,
    Option<String>,
    telepatch::key::Check,
) {
    let mut info = pin!(h.user_info("root"));
    let mut st = pin!(h.stat("/etc/os-release"));
    let mut ver = pin!(h.pkg_installed("bash"));
    let mut chk = pin!(h.check("printf hi"));
    std::future::poll_fn(|cx| {
        let a = info.as_mut().poll(cx);
        let b = st.as_mut().poll(cx);
        let c = ver.as_mut().poll(cx);
        let d = chk.as_mut().poll(cx);
        match (a, b, c, d) {
            (Poll::Ready(a), Poll::Ready(b), Poll::Ready(c), Poll::Ready(d)) => {
                Poll::Ready((a, b, c, d))
            }
            _ => Poll::Pending,
        }
    })
    .await
}

fn reads_of(fake: &FakeExecutor) -> usize {
    fake.calls
        .iter()
        .filter(|(_, c)| matches!(c, telepatch::testing::fake::Call::Read { .. }))
        .count()
}

#[test]
fn facts_asked_together_are_one_round_trip() {
    let h = Host::new("t", ubuntu(), Rc::new(()));
    let pb = Playbook::new("t", |h: Host<()>| -> PlaybookFuture {
        Box::pin(async move {
            let (info, st, ver, chk) = together(&h).await;
            assert!(info.exists, "{info:?}");
            assert_eq!(info.home, "/root");
            assert!(st.exists);
            assert_eq!(st.mode, "644");
            assert_eq!(ver.as_deref(), Some("1.2.3"));
            assert_eq!(chk.rc, 0);
            assert_eq!(chk.stdout, "hi");
            Ok::<(), TraceError>(())
        })
    });
    let fake = FakeExecutor::new()
        .answer(
            RunAs::root_shell(),
            telepatch::facts::query::Fact::user_info("root"),
            telepatch::Value::User(telepatch::key::UserInfo {
                home: "/root".into(),
                shell: "/bin/bash".into(),
                exists: true,
            }),
        )
        .answer(
            RunAs::root_shell(),
            telepatch::facts::query::Fact::stat("/etc/os-release"),
            telepatch::Value::Stat(telepatch::key::Stat {
                mode: "644".into(),
                owner: "root".into(),
                size: 12,
                exists: true,
            }),
        )
        .answer(
            RunAs::root_shell(),
            telepatch::facts::query::Fact::pkg_installed("bash"),
            telepatch::Value::Pkg(Some("1.2.3".into())),
        )
        .answer(
            RunAs::root_shell(),
            telepatch::facts::query::Fact::check("printf hi"),
            telepatch::Value::Check(telepatch::key::Check {
                rc: 0,
                stdout: "hi".into(),
            }),
        );
    let mut engine = Engine::new(fake);
    block_on(engine.run_host(&pb, h)).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(reads_of(&engine.executor), 1, "{:?}", engine.executor.calls);
}

#[test]
fn assert_blocks_before_the_window_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("should-not-exist");
    let vars = path.to_str().unwrap().to_string();
    let pb = Playbook::new("t", |h: Host<String>| -> PlaybookFuture {
        Box::pin(async move {
            let info = h.user_info("root").await;
            h.assert(info.exists, "user missing");
            h.assert(false, "telepatch-assert-stop");
            h.file(&h.vars).content("nope");
            Ok::<(), TraceError>(())
        })
    });
    let fake = FakeExecutor::new().answer(
        RunAs::root_shell(),
        telepatch::facts::query::Fact::user_info("root"),
        telepatch::Value::User(telepatch::key::UserInfo {
            home: "/root".into(),
            shell: "/bin/bash".into(),
            exists: true,
        }),
    );
    let h = Host::new("t", ubuntu(), Rc::new(vars.clone()));
    let mut engine = Engine::new(fake);
    let err = block_on(engine.run_host(&pb, h)).expect_err("assert must block");
    assert!(err.to_string().contains("telepatch-assert-stop"), "{err}");
    assert!(
        engine.executor.calls.iter().all(|(_, c)| {
            !matches!(c, telepatch::testing::fake::Call::Apply { ops, .. } if ops.iter().any(|o| o.script.contains("nope")))
        }),
        "the file op ran despite the assert: {:?}",
        engine.executor.calls
    );
    assert!(!path.exists(), "host file was written");
}

struct Live {
    marker: String,
    body: String,
    missing: String,
}

fn live_run(h: Host<Live>) -> PlaybookFuture {
    Box::pin(async move {
        let mut info = pin!(h.user_info("root"));
        let mut absent = pin!(h.user_info("tp-no-such-user-telepatch"));
        let mut st = pin!(h.stat(&h.vars.body));
        let mut gone = pin!(h.stat(&h.vars.missing));
        let mut ver = pin!(h.pkg_installed("bash"));
        let mut none = pin!(h.pkg_installed("tp-no-such-pkg-telepatch"));
        let mut chk = pin!(h.check("printf 'hi\\n'"));
        let mut bad = pin!(h.check("exit 3"));
        let (info, absent, st, gone, ver, none, chk, bad) = std::future::poll_fn(|cx| {
            let a = info.as_mut().poll(cx);
            let b = absent.as_mut().poll(cx);
            let c = st.as_mut().poll(cx);
            let d = gone.as_mut().poll(cx);
            let e = ver.as_mut().poll(cx);
            let f = none.as_mut().poll(cx);
            let g = chk.as_mut().poll(cx);
            let h = bad.as_mut().poll(cx);
            match (a, b, c, d, e, f, g, h) {
                (
                    Poll::Ready(a),
                    Poll::Ready(b),
                    Poll::Ready(c),
                    Poll::Ready(d),
                    Poll::Ready(e),
                    Poll::Ready(f),
                    Poll::Ready(g),
                    Poll::Ready(h),
                ) => Poll::Ready((a, b, c, d, e, f, g, h)),
                _ => Poll::Pending,
            }
        })
        .await;
        let detail = format!(
            "user exists={} home={} shell={} absent={} stat exists={} mode={} owner={} size={} gone={} ver={ver:?} none={none:?} rc={} out={:?} bad={}",
            info.exists,
            info.home,
            info.shell,
            absent.exists,
            st.exists,
            st.mode,
            st.owner,
            st.size,
            gone.exists,
            chk.rc,
            chk.stdout,
            bad.rc
        );
        let ok = info.exists
            && info.home.starts_with('/')
            && !info.shell.is_empty()
            && !absent.exists
            && st.exists
            && !st.mode.is_empty()
            && !st.owner.is_empty()
            && st.size == 3
            && !gone.exists
            && ver.is_some()
            && none.is_none()
            && chk.rc == 0
            && chk.stdout == "hi\n"
            && bad.rc == 3;
        h.assert(ok, detail.clone());
        h.file(&h.vars.marker).content(detail);
        Ok(())
    })
}

#[test]
fn typed_facts_come_back_from_the_host() {
    let dir = tempfile::tempdir().unwrap();
    let body = dir.path().join("body");
    std::fs::write(&body, "hi\n").unwrap();
    let marker = dir.path().join("marker");
    let missing = dir.path().join("missing");
    let vars = Live {
        marker: marker.to_str().unwrap().to_string(),
        body: body.to_str().unwrap().to_string(),
        missing: missing.to_str().unwrap().to_string(),
    };
    let (report, out) = go("facts", live_run, vars);
    let status = &report.hosts[0].status;
    assert!(
        status.is_success(),
        "{status:?}\n{out}\n{}",
        report.render()
    );
    let got = std::fs::read_to_string(&marker).unwrap();
    assert!(got.contains("user exists=true"), "{got}");
    assert!(
        got.contains("out=\"hi\\n\"") || got.contains("out=\"hi\n\""),
        "{got}"
    );
}

#[test]
fn a_failing_assert_blocks_the_live_host_before_the_file_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nope");
    let vars = path.to_str().unwrap().to_string();
    let (report, out) = go(
        "block",
        |h: Host<String>| -> PlaybookFuture {
            Box::pin(async move {
                let info = h.user_info("root").await;
                h.assert(info.exists, "root should exist");
                h.assert(false, "telepatch-assert-stop");
                h.file(&h.vars).content("nope");
                Ok(())
            })
        },
        vars,
    );
    match &report.hosts[0].status {
        HostStatus::Blocked(why) => {
            assert!(why.contains("telepatch-assert-stop"), "{why}\n{out}")
        }
        other => panic!("{other:?}\n{out}\n{}", report.render()),
    }
    assert!(!path.exists(), "the window wrote the file");
}
