//! Engine-loop snapshots: a fake executor records windows, round trips and
//! implicit barriers for small playbooks.

use std::rc::Rc;

use telepatch::facts::os::Os;
use telepatch::facts::query::Fact;
use telepatch::testing::{FakeExecutor, FixtureHost, run_engine};
use telepatch::{Host, Key, Playbook, PlaybookFuture, RunAs, TraceError, Value};

fn host() -> FixtureHost<()> {
    let p = format!(
        "{}/tests/fixtures/os-release/debian12",
        env!("CARGO_MANIFEST_DIR")
    );
    let os = Os::parse(&std::fs::read_to_string(p).unwrap()).unwrap();
    FixtureHost::new("web1", os, Rc::new(()))
}

fn pb(run: fn(Host<()>) -> PlaybookFuture) -> Playbook<()> {
    Playbook { name: "t", run }
}

fn go(run: fn(Host<()>) -> PlaybookFuture, fake: FakeExecutor) -> String {
    run_engine(&pb(run), &host(), fake)
}

#[test]
fn pre_existing_state_blocks_once() {
    fn run(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            h.raw("echo setup").writes([Key::file("/srv/other")]);
            if !h.exists("/etc/flag").await {
                h.raw("touch /etc/flag").writes([Key::file("/etc/flag")]);
            }
            Ok::<(), TraceError>(())
        })
    }
    insta::assert_snapshot!(go(run, FakeExecutor::new()));
}

#[test]
fn same_fact_twice_second_confirmed() {
    fn run(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            let a = h.exists("/etc/flag").await;
            let b = h.exists("/etc/flag").await;
            assert_eq!(a, b);
            Ok(())
        })
    }
    let fake = FakeExecutor::new().answer(
        RunAs::root_shell(),
        Fact::exists("/etc/flag"),
        Value::Exists(true),
    );
    insta::assert_snapshot!(go(run, fake));
}

#[test]
fn postcondition_answers_without_round_trip() {
    fn run(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            h.raw("touch /f")
                .writes([Key::file("/f")])
                .post(Key::file("/f"), Value::Exists(true));
            assert!(h.exists("/f").await);
            Ok(())
        })
    }
    insta::assert_snapshot!(go(run, FakeExecutor::new()));
}

#[test]
fn guarded_postcondition_does_not_answer() {
    fn run(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            let a = h.raw("touch /a");
            a.writes([Key::file("/a")]);
            a.post(Key::file("/f"), Value::Exists(true));
            let b = h.raw("touch /f");
            b.writes([Key::file("/f")]);
            b.post(Key::file("/f"), Value::Exists(true));
            b.when(telepatch::Guard::Changed(a.id()));
            let _ = h.exists("/f").await;
            Ok(())
        })
    }
    insta::assert_snapshot!(go(run, FakeExecutor::new()));
}

#[test]
fn write_star_forces_implicit_barrier() {
    fn run(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            h.raw("echo before").writes([Key::file("/unrelated")]);
            h.raw("curl -o /usr/local/bin/mc https://x");
            let has = h.bin_exists("mc").await;
            h.raw("echo after").writes([Key::file("/after")]);
            assert!(has);
            Ok(())
        })
    }
    let fake = FakeExecutor::new().answer(
        RunAs::root_shell(),
        Fact::bin_exists("mc"),
        Value::Exists(true),
    );
    insta::assert_snapshot!(go(run, fake));
}

#[test]
fn applied_write_invalidates_confirmed() {
    fn run(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            // Confirmed by a round trip.
            let _ = h.exists("/usr/bin/foo").await;
            // Writes the file the confirmed fact is about; no postcondition.
            h.raw("install foo").writes([Key::file("/usr/bin/foo")]);
            // bin:foo overlaps file:*, so this read needs the write: barrier.
            let _ = h.bin_exists("foo").await;
            // The write ran, so the earlier confirmed value is stale: re-read.
            let _ = h.exists("/usr/bin/foo").await;
            Ok(())
        })
    }
    insta::assert_snapshot!(go(run, FakeExecutor::new()));
}

#[test]
fn per_user_facts_are_distinct() {
    fn run(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            let root = h.bin_version("cargo").await;
            let deploy = h.as_user("deploy").bin_version("cargo").await;
            let root_login = h.as_user("root").bin_version("cargo").await;
            let deploy_again = h.as_user("deploy").bin_version("cargo").await;
            assert_eq!(root, None);
            assert_eq!(deploy.as_deref(), Some("cargo 1.90.0"));
            assert_eq!(root_login, None);
            assert_eq!(deploy_again, deploy);
            Ok(())
        })
    }
    let fake = FakeExecutor::new().answer(
        RunAs::login("deploy"),
        Fact::bin_version("cargo"),
        Value::Text("cargo 1.90.0".into()),
    );
    insta::assert_snapshot!(go(run, fake));
}

#[test]
fn facts_from_two_roles_share_confirmed() {
    fn run(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            {
                let _r = h.role_enter("a");
                let _ = h.bin_exists("git").await;
            }
            {
                let _r = h.role_enter("b");
                let _ = h.bin_exists("git").await;
            }
            Ok(())
        })
    }
    insta::assert_snapshot!(go(run, FakeExecutor::new()));
}

#[test]
fn non_mutating_nodes_flush_on_block_mutations_wait() {
    fn run(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            h.raw("echo mutate").writes([Key::file("/m")]);
            h.raw("cat /etc/hostname")
                .writes([])
                .reads([Key::file("/etc/hostname")]);
            h.raw("cat /m").writes([]).reads([Key::file("/m")]);
            let _ = h.exists("/etc/flag").await;
            Ok(())
        })
    }
    insta::assert_snapshot!(go(run, FakeExecutor::new()));
}

#[test]
fn explicit_barrier_does_not_force_execution() {
    fn run(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            h.raw("echo a").writes([Key::file("/a")]);
            h.barrier();
            let _ = h.exists("/etc/flag").await;
            h.raw("echo b").writes([Key::file("/b")]);
            Ok(())
        })
    }
    insta::assert_snapshot!(go(run, FakeExecutor::new()));
}

#[test]
fn user_created_then_as_user_op_splits_the_window() {
    fn run(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            h.user("alice");
            h.raw("echo root before");
            h.as_user("alice").raw("cargo install x");
            h.raw("echo root after");
            Ok::<(), TraceError>(())
        })
    }
    insta::assert_snapshot!(go(run, FakeExecutor::new()));
}

#[test]
fn user_created_then_as_user_fact_read_applies_the_user_first() {
    fn run(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            h.user("alice");
            let v = h.as_user("alice").bin_version("cargo").await;
            assert_eq!(v, None);
            Ok::<(), TraceError>(())
        })
    }
    insta::assert_snapshot!(go(run, FakeExecutor::new()));
}

#[test]
fn as_user_op_without_a_user_op_stays_in_one_window() {
    fn run(h: Host<()>) -> PlaybookFuture {
        Box::pin(async move {
            h.raw("echo root before");
            h.as_user("alice").raw("cargo install x");
            h.user("bob");
            h.raw("echo root after");
            Ok::<(), TraceError>(())
        })
    }
    insta::assert_snapshot!(go(run, FakeExecutor::new()));
}
