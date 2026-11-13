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

// ---- T115: checks read together share one bash ----

#[derive(Clone, Copy)]
enum Mode {
    /// Polled together: one round, one probe.
    Together,
    /// Awaited one after another: a round each.
    Apart,
    /// Awaited one after another with an op between them.
    WithOp,
}

struct Checks {
    mode: Mode,
    cmds: Vec<String>,
    out: String,
}

fn checks_pb(h: Host<Checks>) -> PlaybookFuture {
    Box::pin(async move {
        let mut got = Vec::new();
        match h.vars.mode {
            Mode::Together => {
                let mut futs: Vec<_> = h.vars.cmds.iter().map(|c| Box::pin(h.check(c))).collect();
                let all = std::future::poll_fn(|cx| {
                    let polled: Vec<_> = futs.iter_mut().map(|f| f.as_mut().poll(cx)).collect();
                    if polled.iter().all(|p| p.is_ready()) {
                        Poll::Ready(
                            polled
                                .into_iter()
                                .map(|p| match p {
                                    Poll::Ready(v) => v,
                                    Poll::Pending => unreachable!(),
                                })
                                .collect::<Vec<_>>(),
                        )
                    } else {
                        Poll::Pending
                    }
                })
                .await;
                got = all;
            }
            Mode::Apart | Mode::WithOp => {
                for c in &h.vars.cmds {
                    got.push(h.check(c).await);
                    if matches!(h.vars.mode, Mode::WithOp) {
                        h.raw("true");
                    }
                }
            }
        }
        let text: String = got
            .iter()
            .map(|c| format!("{}\t{:?}\n", c.rc, c.stdout))
            .collect();
        std::fs::write(&h.vars.out, text).unwrap();
        Ok(())
    })
}

/// Run `cmds` and return each answer as `(rc, stdout)`.
fn run_checks(mode: Mode, cmds: &[&str]) -> Vec<(i32, String)> {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let vars = Checks {
        mode,
        cmds: cmds.iter().map(|s| s.to_string()).collect(),
        out: out.to_str().unwrap().to_string(),
    };
    let (report, log) = go("checks", checks_pb, vars);
    assert!(
        report.hosts[0].status.is_success(),
        "{log}\n{}",
        report.render()
    );
    std::fs::read_to_string(&out)
        .unwrap()
        .lines()
        .map(|l| {
            let (rc, s) = l.split_once('\t').unwrap();
            // Rust's Debug escapes for the ASCII used here match JSON's.
            let s: String = serde_json::from_str(s).unwrap_or_else(|_| panic!("bad answer {s}"));
            (rc.parse().unwrap(), s)
        })
        .collect()
}

/// A check that prints its shell's parent pid, then a tag.
fn ppid_check(tag: &str) -> String {
    format!("read -r _ _ _ pp _ < /proc/$BASHPID/stat; echo \"$pp {tag}\"")
}

fn split_pp(s: &str) -> (String, String) {
    let (pp, tag) = s.trim_end().split_once(' ').unwrap();
    (pp.to_string(), tag.to_string())
}

#[test]
fn three_checks_polled_together_share_one_bash() {
    let cmds: Vec<String> = ["a", "b", "c"].iter().map(|t| ppid_check(t)).collect();
    let refs: Vec<&str> = cmds.iter().map(|s| s.as_str()).collect();
    let together = run_checks(Mode::Together, &refs);
    let apart = run_checks(Mode::Apart, &refs);
    assert_eq!(together.len(), 3);
    let t: Vec<_> = together.iter().map(|(_, s)| split_pp(s)).collect();
    let a: Vec<_> = apart.iter().map(|(_, s)| split_pp(s)).collect();
    // One bash: all three subshells have one parent.
    assert!(t.iter().all(|(pp, _)| *pp == t[0].0), "{t:?}");
    // Three rounds, three bashes.
    assert!(a[0].0 != a[1].0 && a[1].0 != a[2].0, "{a:?}");
    // Output equals three separate checks, apart from the pid.
    let tags = |v: &[(String, String)]| v.iter().map(|(_, t)| t.clone()).collect::<Vec<_>>();
    assert_eq!(tags(&t), tags(&a));
    assert!(together.iter().all(|(rc, _)| *rc == 0));
}

#[test]
fn a_run_answers_each_check_its_own_rc() {
    let got = run_checks(Mode::Together, &["exit 3", "if then fi", "echo ok"]);
    assert_eq!(
        got,
        vec![(3, String::new()), (2, String::new()), (0, "ok\n".into())]
    );
}

#[test]
fn a_cd_in_one_check_does_not_reach_the_next() {
    let alone = run_checks(Mode::Apart, &["pwd"]);
    let got = run_checks(
        Mode::Together,
        &["cd /; v=1; set -e; echo cd", "pwd; echo \"[$v]\""],
    );
    assert_eq!(got[0].1, "cd\n");
    assert_eq!(got[1].1, format!("{}[]\n", alone[0].1));
}

#[test]
fn trailing_newlines_survive_per_check() {
    let got = run_checks(
        Mode::Together,
        &["printf 'a\\n\\n'", "printf 'b'", "printf '\\n'", "true"],
    );
    let outs: Vec<&str> = got.iter().map(|(_, s)| s.as_str()).collect();
    assert_eq!(outs, ["a\n\n", "b", "\n", ""]);
}

#[test]
fn checks_with_an_op_between_use_two_bashes() {
    let cmds: Vec<String> = ["a", "b"].iter().map(|t| ppid_check(t)).collect();
    let refs: Vec<&str> = cmds.iter().map(|s| s.as_str()).collect();
    let got = run_checks(Mode::WithOp, &refs);
    let t: Vec<_> = got.iter().map(|(_, s)| split_pp(s)).collect();
    assert_ne!(t[0].0, t[1].0, "{t:?}");
}

#[test]
fn a_check_with_single_quotes_and_a_delimiter_like_line_runs() {
    let got = run_checks(
        Mode::Together,
        &[
            "echo 'it'\\''s'\n__tp_C_0000000000000000_0",
            "echo \"it's\"",
        ],
    );
    assert_eq!(got[1], (0, "it's\n".to_string()));
    assert_eq!(got[0].0, 127);
}
