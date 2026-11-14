//! `Shell::close` cannot wait forever (T138): it says `exit` first, then
//! drops stdin, then waits a bounded time and kills the local child.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use telepatch::pool::Shell;

static N: AtomicU32 = AtomicU32::new(0);

/// A sleep length unique to this process and this call, as `600.<pid><n>`.
/// The [`Reaper`] pattern ends in `$` so one test never matches another's.
fn long_sleep() -> String {
    format!(
        "600.{}{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    )
}

/// Kills the sleeps a test left behind, by their exact command line.
struct Reaper(String);
impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = std::process::Command::new("pkill")
            .args(["-KILL", "-f", &format!("^sleep {}$", self.0)])
            .status();
    }
}

/// The docker symptom: the group keeps bash's stdin open after the
/// controller's EOF, so bash never sees EOF. The local shell ends when bash
/// does (polled: `wait` on a pipeline's pid does not return here).
fn eof_lost(nap: &str) -> Vec<String> {
    vec![
        "bash".into(),
        "-c".into(),
        format!(
            "{{ cat; exec sleep {nap}; }} | bash & p=$!; \
             while kill -0 $p 2>/dev/null; do sleep 0.05; done"
        ),
    ]
}

/// Alive processes whose command line matches `pattern` (an ERE).
fn pgrep(pattern: &str) -> Vec<String> {
    let o = std::process::Command::new("pgrep")
        .args(["-af", pattern])
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

#[tokio::test]
async fn close_ends_a_shell_whose_eof_never_arrives() {
    let nap = long_sleep();
    let _r = Reaper(nap.clone());
    let mut sh = Shell::spawn(&eof_lost(&nap), &[], "root").await.unwrap();
    sh.set_close_bound(Duration::from_secs(3));
    let t = Instant::now();
    // The test's own bound is longer than the close bound: a close that
    // hangs fails here, one that needed the kill fails the assert below.
    let hit = tokio::time::timeout(Duration::from_secs(8), sh.close())
        .await
        .expect("close hung: the shell never saw EOF");
    assert!(!hit, "exit passes through: the bound is not hit");
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
}

#[tokio::test]
async fn a_shell_that_ignores_stdin_hits_the_bound_and_is_killed() {
    let nap = long_sleep();
    let _r = Reaper(nap.clone());
    let argv: Vec<String> = vec![
        "sh".into(),
        "-c".into(),
        format!("trap '' TERM HUP; {{ cat; exec sleep {nap}; }} | bash & wait"),
    ];
    let mut sh = Shell::spawn(&argv, &[], "root").await.unwrap();
    assert_eq!(sh.argv0(), "sh");
    sh.set_close_bound(Duration::from_millis(400));
    let t = Instant::now();
    let hit = tokio::time::timeout(Duration::from_secs(8), sh.close())
        .await
        .expect("close is unbounded");
    assert!(hit, "the bound was hit");
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    // The local child is gone. (The orphaned sleep is the Reaper's.)
    assert_eq!(pgrep(&format!("^sh -c .*{nap}")), Vec::<String>::new());
}

#[tokio::test]
async fn a_local_root_shell_and_a_login_shell_close_at_once() {
    let sb = telepatch::testing::sandbox();
    let t = telepatch::transport::local::LocalTransport::new()
        .uid(0)
        .sandboxed(&sb);
    use telepatch::transport::Transport;
    let mut s = t.open("local", &telepatch::trace::Principal::Root, 1).await;
    let root = s.got.remove(0);
    let t0 = Instant::now();
    assert!(!root.close().await);
    assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());

    let argv: Vec<String> = vec!["bash".into()];
    let mut login = Shell::spawn(&argv, &[], "root").await.unwrap();
    login
        .exec(
            "env -i HOME=/nonexistent PATH=/usr/bin:/bin bash -l",
            "root",
        )
        .await
        .unwrap();
    let t0 = Instant::now();
    assert!(!login.close().await);
    assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
}
