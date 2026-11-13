//! T112: one process group per op, so a controller can kill an op from a
//! second shell, and the start marker that names the group.
//!
//! Local shells only (`LocalTransport::new().uid(0)` is a transport
//! setting, not root). Every process a test starts is recorded in a
//! [`Reaper`], which kills it on drop, so a failing test leaves nothing
//! running.

use std::time::{Duration, Instant};

use telepatch::ops::shell::raw_body;
use telepatch::pool::{Event, Shell};
use telepatch::trace::Principal;
use telepatch::trailer::{OpKind, ShellResult, guard_skip, wrap};
use telepatch::transport::Transport;
use telepatch::transport::local::LocalTransport;

/// Kills what a test started, on drop (also when an assert fails).
#[derive(Default)]
struct Reaper {
    pids: Vec<i32>,
    groups: Vec<i32>,
}

impl Reaper {
    fn group(&mut self, pgid: i32) {
        self.groups.push(pgid);
    }
    fn pid(&mut self, pid: i32) {
        self.pids.push(pid);
    }
}

impl Drop for Reaper {
    fn drop(&mut self) {
        for g in &self.groups {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", "--", &format!("-{g}")])
                .stderr(std::process::Stdio::null())
                .status();
        }
        for p in &self.pids {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", &p.to_string()])
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
}

async fn open() -> Shell {
    // No op of these tests names a stage or a cache; the sandbox is only
    // what the switch needs to see (T124), so it may go with this function.
    let sb = telepatch::testing::sandbox();
    let t = LocalTransport::new().uid(0).sandboxed(&sb);
    let mut s = t.open("local", &Principal::Root, 1).await;
    assert!(s.limit.is_none(), "{:?}", s.limit);
    s.got.remove(0)
}

/// An op's wrapped script, id `id`.
fn op(sh: &Shell, id: usize, body: &str) -> String {
    wrap(OpKind::Sh, &raw_body(body), id, sh.nonce())
}

/// A shell as the executor leaves it: job control on, so each op's
/// subshell is its own process group.
async fn open_jc() -> Shell {
    let mut sh = open().await;
    let ack = op(&sh, 0, "true");
    sh.send(0, &format!("set -m\n{ack}")).await.unwrap();
    sh.wait(0).await.unwrap();
    sh
}

/// Everything the shell said, per event, until `needle` is in the output
/// of the ops seen so far. Panics after 15 s.
async fn read_until(sh: &mut Shell, seen: &mut String, needle: &str) {
    let r = tokio::time::timeout(Duration::from_secs(15), async {
        while !seen.contains(needle) {
            match sh.next_event().await {
                Some(Event::Output(_, b)) => seen.push_str(&String::from_utf8_lossy(&b)),
                Some(_) => {}
                None => panic!("stream ended before {needle:?}: {seen}"),
            }
        }
    })
    .await;
    assert!(r.is_ok(), "timed out waiting for {needle:?}; saw {seen:?}");
}

fn field(seen: &str, key: &str) -> i32 {
    seen.lines()
        .find_map(|l| l.strip_prefix(key))
        .unwrap_or_else(|| panic!("no {key} in {seen:?}"))
        .trim()
        .parse()
        .unwrap()
}

/// (state, pgrp) of a pid from /proc, `None` if gone.
fn stat(pid: i32) -> Option<(char, i32)> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &s[s.rfind(')')? + 2..];
    let mut f = rest.split(' ');
    let state = f.next()?.chars().next()?;
    let _ppid = f.next()?;
    let pgrp = f.next()?.parse().ok()?;
    Some((state, pgrp))
}

/// Live (non-zombie) processes whose process group is `pgid`.
fn members(pgid: i32) -> Vec<i32> {
    let mut v = Vec::new();
    for e in std::fs::read_dir("/proc").unwrap().flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<i32>().ok()) else {
            continue;
        };
        if let Some((state, g)) = stat(pid)
            && g == pgid
            && state != 'Z'
        {
            v.push(pid);
        }
    }
    v
}

async fn gone(pgid: i32, within: Duration) -> bool {
    let t = Instant::now();
    while t.elapsed() < within {
        if members(pgid).is_empty() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    members(pgid).is_empty()
}

/// An op that holds a group: a TERM-ignoring background loop, a plain
/// background `sleep`, and a foreground `sleep`.
const HOLD: &str = "echo PID=$BASHPID\n\
    sh -c 'trap \"\" TERM; echo IGN=$$; while :; do sleep 1; done' &\n\
    sleep 300 &\n\
    echo BG=$!\n\
    sh -c 'echo FG=$$; exec sleep 300'\n";

/// Like [`HOLD`] but every member dies on TERM.
const HOLD_TERM: &str = "echo PID=$BASHPID\n\
    sleep 300 &\n\
    echo BG=$!\n\
    sh -c 'echo FG=$$; exec sleep 300'\n";

/// Send `script` as op `id` on `sh` and wait for it (a second shell used
/// as the controller).
async fn control(sh: &mut Shell, id: usize, body: &str) {
    let s = op(sh, id, body);
    sh.send(id, &s).await.unwrap();
    let o = sh.wait(id).await.unwrap();
    assert_eq!(o.trailer.typed::<ShellResult>().unwrap().rc, 0);
}

/// A TERM can be lost: a child bash has forked but not yet exec'd still
/// carries the op's TERM trap, and the signal is consumed there. A
/// controller therefore re-sends TERM (and, in T101, escalates to KILL).
/// Sends TERM to `-pgid` until the group is gone; how many sends it took.
async fn term_until_gone(k: &mut Shell, pgid: i32) -> usize {
    for n in 1..=5 {
        control(k, 100 + n, &format!("kill -TERM -- -{pgid}")).await;
        if gone(pgid, Duration::from_millis(1500)).await {
            return n;
        }
    }
    panic!("group {pgid} survived 5 TERMs: {:?}", members(pgid));
}

/// [`term_until_gone`] for a group whose op's end is read from `sh`: TERM
/// until op `id` ends. Returns the events up to its end and the sends.
async fn term_until_end(
    k: &mut Shell,
    sh: &mut Shell,
    pgid: i32,
    id: usize,
) -> (Vec<Event>, usize) {
    let mut v = Vec::new();
    for n in 1..=5 {
        control(k, 100 + n, &format!("kill -TERM -- -{pgid}")).await;
        let pump = async {
            while let Some(e) = sh.next_event().await {
                let end = matches!(&e, Event::Done(i, _) | Event::Failed(i, _) if *i == id);
                v.push(e);
                if end {
                    return;
                }
            }
        };
        if tokio::time::timeout(Duration::from_millis(2500), pump)
            .await
            .is_ok()
        {
            return (v, n);
        }
    }
    panic!("op {id} did not end after 5 TERMs: {:?}", members(pgid));
}

#[tokio::test]
async fn proof_op_subshell_is_its_own_group_and_children_share_it() {
    let mut r = Reaper::default();
    let mut sh = open_jc().await;
    let s = op(&sh, 1, HOLD_TERM);
    sh.send(1, &s).await.unwrap();
    let mut seen = String::new();
    read_until(&mut sh, &mut seen, "FG=").await;
    let (pid, bg, fg) = (
        field(&seen, "PID="),
        field(&seen, "BG="),
        field(&seen, "FG="),
    );
    r.group(pid);
    assert_eq!(stat(pid).unwrap().1, pid, "op subshell leads its group");
    assert_eq!(stat(bg).unwrap().1, pid, "background child shares it");
    assert_eq!(stat(fg).unwrap().1, pid, "foreground child shares it");
    // Clean up through the proof of item 2 (TERM from a second shell).
    let mut k = open_jc().await;
    term_until_gone(&mut k, pid).await;
    assert!(gone(pid, Duration::from_secs(5)).await);
}

#[tokio::test]
async fn proof_term_from_a_second_shell_clears_the_group() {
    let mut r = Reaper::default();
    let mut sh = open_jc().await;
    let mut k = open_jc().await;
    let s = op(&sh, 1, HOLD_TERM);
    sh.send(1, &s).await.unwrap();
    let mut seen = String::new();
    read_until(&mut sh, &mut seen, "FG=").await;
    let pid = field(&seen, "PID=");
    r.group(pid);
    assert!(members(pid).len() >= 3, "{:?}", members(pid));
    term_until_gone(&mut k, pid).await;
    assert!(
        gone(pid, Duration::from_secs(5)).await,
        "left {:?}",
        members(pid)
    );
    // The persistent shell still runs the next op and returns its trailer.
    let s = op(&sh, 2, "echo next");
    sh.send(2, &s).await.unwrap();
    let o = sh.wait(2).await.unwrap();
    assert_eq!(o.trailer.typed::<ShellResult>().unwrap().rc, 0);
    assert!(String::from_utf8_lossy(&o.output).contains("next"));
}

#[tokio::test]
async fn proof_kill_clears_a_group_whose_members_ignore_term() {
    let mut r = Reaper::default();
    let mut sh = open_jc().await;
    let mut k = open_jc().await;
    let s = op(&sh, 1, HOLD);
    sh.send(1, &s).await.unwrap();
    let mut seen = String::new();
    read_until(&mut sh, &mut seen, "FG=").await;
    read_until(&mut sh, &mut seen, "IGN=").await;
    let pid = field(&seen, "PID=");
    r.group(pid);
    control(&mut k, 1, &format!("kill -TERM -- -{pid}")).await;
    // The TERM-ignoring loop outlives TERM.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let left = members(pid);
    assert!(
        left.contains(&field(&seen, "IGN=")),
        "the ignorer must survive TERM: {left:?}"
    );
    control(&mut k, 2, &format!("kill -KILL -- -{pid}")).await;
    assert!(
        gone(pid, Duration::from_secs(5)).await,
        "left {:?}",
        members(pid)
    );
    let s = op(&sh, 2, "echo next");
    sh.send(2, &s).await.unwrap();
    let o = sh.wait(2).await.unwrap();
    assert_eq!(o.trailer.typed::<ShellResult>().unwrap().rc, 0);
}

#[tokio::test]
async fn proof_a_setsid_child_survives_the_group_kill() {
    let mut r = Reaper::default();
    let mut sh = open_jc().await;
    let mut k = open_jc().await;
    let body = "echo PID=$BASHPID\n\
        setsid sh -c 'echo SID=$$; exec sleep 301' &\n\
        sleep 300\n";
    let s = op(&sh, 1, body);
    sh.send(1, &s).await.unwrap();
    let mut seen = String::new();
    read_until(&mut sh, &mut seen, "SID=").await;
    let (pid, sid) = (field(&seen, "PID="), field(&seen, "SID="));
    r.group(pid);
    r.pid(sid);
    assert_ne!(stat(sid).unwrap().1, pid, "setsid left the group");
    term_until_gone(&mut k, pid).await;
    assert!(gone(pid, Duration::from_secs(5)).await);
    assert!(
        matches!(stat(sid), Some((s, _)) if s != 'Z'),
        "the setsid child must survive"
    );
    // Clean up now; the Reaper is the backstop.
    control(&mut k, 2, &format!("kill -KILL {sid}")).await;
    // The shell still works.
    let s = op(&sh, 2, "echo next");
    sh.send(2, &s).await.unwrap();
    assert_eq!(
        sh.wait(2)
            .await
            .unwrap()
            .trailer
            .typed::<ShellResult>()
            .unwrap()
            .rc,
        0
    );
}

// ---- the start marker and the wrapper -------------------------------

/// Pump `sh` until op `id` ends; every event seen, in order.
async fn events_until_end(sh: &mut Shell, id: usize) -> Vec<Event> {
    let r = tokio::time::timeout(Duration::from_secs(20), async {
        let mut v = Vec::new();
        while let Some(e) = sh.next_event().await {
            let end = matches!(&e, Event::Done(i, _) | Event::Failed(i, _) if *i == id);
            v.push(e);
            if end {
                break;
            }
        }
        v
    })
    .await;
    r.expect("timed out waiting for the op to end")
}

fn text_of(events: &[Event]) -> String {
    let mut s = String::new();
    for e in events {
        if let Event::Output(_, b) = e {
            s.push_str(&String::from_utf8_lossy(b));
        }
    }
    s
}

fn started(events: &[Event], id: usize) -> Option<(i32, i32)> {
    events.iter().find_map(|e| match e {
        Event::Started {
            id: i,
            pid,
            shell_pid,
        } if *i == id => Some((*pid, *shell_pid)),
        _ => None,
    })
}

fn rc_of(events: &[Event], id: usize) -> Option<i32> {
    events.iter().find_map(|e| match e {
        Event::Done(i, t) if *i == id => Some(t.typed::<ShellResult>().unwrap().rc),
        _ => None,
    })
}

/// Start `body` as op `id` on `sh` and read until its start line has
/// arrived and `needle` has been printed. Returns the events so far.
async fn start_and_wait_for(sh: &mut Shell, id: usize, body: &str, needle: &str) -> Vec<Event> {
    let s = op(sh, id, body);
    sh.send(id, &s).await.unwrap();
    start_and_wait_for_sent(sh, id, needle).await
}

/// [`start_and_wait_for`] for an op already sent.
async fn start_and_wait_for_sent(sh: &mut Shell, id: usize, needle: &str) -> Vec<Event> {
    let r = tokio::time::timeout(Duration::from_secs(15), async {
        let mut v = Vec::new();
        loop {
            let seen = text_of(&v);
            if started(&v, id).is_some() && seen.contains(needle) {
                return v;
            }
            v.push(sh.next_event().await.expect("stream ended"));
        }
    })
    .await;
    r.expect("timed out waiting for the op to start")
}

#[tokio::test]
async fn started_event_comes_first_with_pid_and_shell_pid() {
    let mut sh = open_jc().await;
    let s = op(&sh, 1, "echo \"P=$BASHPID S=$$\"");
    sh.send(1, &s).await.unwrap();
    let ev = events_until_end(&mut sh, 1).await;
    assert!(matches!(ev[0], Event::Started { id: 1, .. }), "{ev:?}");
    let (pid, shell_pid) = started(&ev, 1).unwrap();
    assert_ne!(pid, shell_pid);
    // The start line is never op output: the output is the op's alone.
    assert_eq!(text_of(&ev), format!("P={pid} S={shell_pid}\n"), "{ev:?}");
    assert_eq!(rc_of(&ev, 1), Some(0));
}

#[tokio::test]
async fn a_guard_skip_and_a_halt_skip_print_no_start_line() {
    let mut sh = open_jc().await;
    let n = sh.nonce().to_string();
    sh.send(1, &guard_skip(OpKind::Sh, 1, &n)).await.unwrap();
    // The executor's halt gate: the skip branch runs after a failure.
    let gated = format!(
        "__tp_halt=1\nif [ -z \"${{__tp_halt-}}\" ]; then\n{}else\n{}fi\n",
        op(&sh, 2, "echo ran"),
        telepatch::trailer::skip(OpKind::Sh, 2, &n)
    );
    sh.send(2, &gated).await.unwrap();
    let a = events_until_end(&mut sh, 1).await;
    let b = events_until_end(&mut sh, 2).await;
    for ev in [&a, &b] {
        assert!(
            started(ev, 1).is_none() && started(ev, 2).is_none(),
            "{ev:?}"
        );
        assert_eq!(text_of(ev), "", "{ev:?}");
        assert_eq!(ev.len(), 1, "a Done without a Started: {ev:?}");
    }
}

#[tokio::test]
async fn a_termed_op_still_prints_its_trailer_with_rc_143() {
    let mut r = Reaper::default();
    let mut sh = open_jc().await;
    let mut k = open_jc().await;
    let ev = start_and_wait_for(
        &mut sh,
        1,
        ": SCRIPTMARKER_term\necho up\nsleep 300 &\nsleep 300\n",
        "up",
    )
    .await;
    let (pid, _) = started(&ev, 1).unwrap();
    r.group(pid);
    let (rest, _) = term_until_end(&mut k, &mut sh, pid, 1).await;
    assert_eq!(rc_of(&rest, 1), Some(143), "{rest:?}");
    assert!(!text_of(&rest).contains("SCRIPTMARKER"), "{rest:?}");
    assert!(gone(pid, Duration::from_secs(5)).await);
}

#[tokio::test]
async fn a_killed_op_leaves_no_trailer_and_no_script_text() {
    let mut r = Reaper::default();
    let mut sh = open_jc().await;
    let mut k = open_jc().await;
    let mut seen = start_and_wait_for(
        &mut sh,
        1,
        ": SCRIPTMARKER_kill\necho up\nsleep 300\n",
        "up",
    )
    .await;
    let (pid, _) = started(&seen, 1).unwrap();
    r.group(pid);
    control(&mut k, 1, &format!("kill -KILL -- -{pid}")).await;
    let s = op(&sh, 2, "echo next");
    sh.send(2, &s).await.unwrap();
    seen.extend(events_until_end(&mut sh, 2).await);
    assert_eq!(
        rc_of(&seen, 1),
        None,
        "a killed op has no trailer: {seen:?}"
    );
    assert!(
        seen.iter().any(|e| matches!(
            e,
            Event::Failed(1, telepatch::pool::ShellError::TrailerLost)
        )),
        "{seen:?}"
    );
    assert_eq!(rc_of(&seen, 2), Some(0));
    assert!(!text_of(&seen).contains("SCRIPTMARKER"), "{seen:?}");
    assert_eq!(text_of(&seen).matches("next").count(), 1, "{seen:?}");
}

/// A login op is a root op sent to the login shell: one start line, its
/// own `$BASHPID` is the group leader (no inner `bash -l`); TERM ends in
/// the trailer, rc 143.
#[tokio::test]
async fn a_login_op_has_one_start_line_and_is_its_own_group_leader() {
    let me = String::from_utf8(
        std::process::Command::new("id")
            .arg("-un")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    let mut r = Reaper::default();
    let mut sh = open_jc().await;
    let mut k = open_jc().await;
    let h = telepatch::Host::new(
        "t",
        telepatch::facts::os::Os::parse("ID=debian\nVERSION_ID=12\n").unwrap(),
        std::rc::Rc::new(()),
    );
    h.as_user(&me)
        .raw("echo L=$BASHPID\n: SCRIPTMARKER_login\nsleep 300");
    let telepatch::Node::Op(node) = &h.snapshot().nodes[0] else {
        unreachable!()
    };
    let script = telepatch::trailer::render_for(node, 1, sh.nonce(), &h.os, &me).unwrap();
    assert_eq!(script.matches("__tpS_").count(), 1, "{script}");
    sh.send(1, &script).await.unwrap();
    let ev = start_and_wait_for_sent(&mut sh, 1, "L=").await;
    let (pid, _) = started(&ev, 1).unwrap();
    r.group(pid);
    let inner: i32 = text_of(&ev)
        .lines()
        .find_map(|l| l.strip_prefix("L="))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(inner, pid, "the op's subshell is the group leader");
    let (rest, _) = term_until_end(&mut k, &mut sh, pid, 1).await;
    assert_eq!(rc_of(&rest, 1), Some(143), "{rest:?}");
    assert!(
        !text_of(&rest).contains("SCRIPTMARKER"),
        "{}",
        text_of(&rest)
    );
    assert!(gone(pid, Duration::from_secs(5)).await);
}

#[tokio::test]
async fn an_op_that_kills_itself_leaks_no_script_text() {
    let mut sh = open_jc().await;
    let s = op(&sh, 1, ": SCRIPTMARKER_self\nkill -9 $BASHPID\n");
    sh.send(1, &s).await.unwrap();
    let s = op(&sh, 2, "echo next");
    sh.send(2, &s).await.unwrap();
    let mut seen = events_until_end(&mut sh, 1).await;
    seen.extend(events_until_end(&mut sh, 2).await);
    assert!(!text_of(&seen).contains("SCRIPTMARKER"), "{seen:?}");
    assert_eq!(rc_of(&seen, 2), Some(0), "{seen:?}");
}

#[tokio::test]
async fn an_ops_own_stderr_reaches_the_stream_and_the_parents_is_intact() {
    let mut sh = open_jc().await;
    let s = op(&sh, 1, "echo from-op >&2");
    sh.send(1, &s).await.unwrap();
    let ev = events_until_end(&mut sh, 1).await;
    assert_eq!(text_of(&ev), "from-op\n", "{ev:?}");
    // Between ops the parent's stderr is still the merged stream.
    let s = format!("echo from-parent >&2\n{}", op(&sh, 2, "true"));
    sh.send(2, &s).await.unwrap();
    let ev = events_until_end(&mut sh, 2).await;
    assert!(text_of(&ev).contains("from-parent"), "{ev:?}");
}

// ---- the executor ----------------------------------------------------

mod exec {
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

    use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
    use telepatch::report::HostStatus;
    use telepatch::transport::local::LocalTransport;
    use telepatch::{Host, Playbook, PlaybookFuture};

    fn pb(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.raw("echo SCRIPTMARKER_halt; kill -9 $BASHPID");
            let after = h.vars.join("after");
            h.raw(format!("touch {}", after.display()));
            Ok(())
        })
    }

    #[test]
    fn halt_trips_after_a_killed_op_and_the_next_gated_op_is_skipped() {
        let d = tempfile::tempdir().unwrap();
        let out = Rc::new(RefCell::new(String::new()));
        let r = run(
            &Playbook::new("killed", pb),
            vec![HostSpec {
                groups: Vec::new(),
                name: "h".into(),
                target: Target::Local(LocalTransport::new().uid(0).sandboxed_at(d.path())),
                vars: d.path().to_path_buf(),
            }],
            RunOpts {
                log: None,
                stdout: Stdout::Capture(out.clone()),
            },
        );
        assert!(
            matches!(r.hosts[0].status, HostStatus::Failed { .. }),
            "{}",
            r.render()
        );
        assert!(!d.path().join("after").exists(), "the next op must not run");
        let text = format!("{}{}", out.borrow(), r.render());
        // The op's own description may quote its first line; the job
        // notice bash prints for a killed job holds the whole script.
        assert!(!text.contains("Killed"), "{text}");
    }
}
