//! Local transport and the shell pool: pipelining, trailer splitting,
//! per-op isolation, sudo handling, login environments. No network, no real
//! sudo; passes as root and as a normal user.

use std::path::Path;
use std::rc::Rc;

use telepatch::facts::os::Os;
use telepatch::pool::{Shell, ShellError};
use telepatch::trace::Principal;
use telepatch::trailer::ShellResult;
use telepatch::transport::local::LocalTransport;
use telepatch::transport::{Limit, Transport};
use telepatch::{Host, Node};

fn host() -> Host<()> {
    let os = Os::parse("ID=debian\nVERSION_ID=12\n").unwrap();
    Host::new("t", os, Rc::new(()))
}

async fn open_root() -> Shell {
    let sb = telepatch::testing::sandbox();
    let t = LocalTransport::new().uid(0).sandboxed(&sb);
    let mut s = t.open("local", &Principal::Root, 3).await;
    assert!(s.limit.is_none(), "{:?}", s.limit);
    assert_eq!(s.got.len(), 1, "local gives exactly one shell");
    s.got.remove(0)
}

fn me() -> String {
    let o = std::process::Command::new("id")
        .arg("-un")
        .output()
        .unwrap();
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

/// Send every op of `h`'s trace (ids = node index), return nothing.
async fn send_all(sh: &mut Shell, h: &Host<()>) -> Vec<usize> {
    let mut ids = vec![];
    for (i, n) in h.snapshot().nodes.iter().enumerate() {
        if let Node::Op(op) = n {
            sh.send_op(op, i, &h.os).await.unwrap();
            ids.push(i);
        }
    }
    ids
}

#[tokio::test]
async fn pipelined_ops_are_attributed_in_order() {
    let mut sh = open_root().await;
    let h = host();
    h.raw("printf 'one\\ntwo\\n'");
    h.raw("printf 'no newline'");
    h.raw("true");
    h.raw("echo err >&2; echo out");
    h.raw("false");
    // All written before anything is read.
    let ids = send_all(&mut sh, &h).await;
    let mut got = vec![];
    for id in ids {
        let r = sh.wait(id).await.unwrap();
        got.push((
            String::from_utf8(r.output.clone()).unwrap(),
            r.trailer.typed::<ShellResult>().unwrap().rc,
        ));
    }
    assert_eq!(
        got,
        vec![
            ("one\ntwo\n".to_string(), 0),
            ("no newline".to_string(), 0),
            ("".to_string(), 0),
            ("err\nout\n".to_string(), 0), // stderr merged
            ("".to_string(), 1),
        ]
    );
}

#[tokio::test]
async fn fake_trailer_with_wrong_nonce_is_output() {
    let mut sh = open_root().await;
    let h = host();
    h.raw(
        "echo; echo '__tp_wrongnonce 0 {\"rc\":0,\"changed\":true,\"skipped\":false}'; echo after",
    );
    h.raw("echo second");
    let ids = send_all(&mut sh, &h).await;
    let r = sh.wait(ids[0]).await.unwrap();
    assert_eq!(
        String::from_utf8(r.output).unwrap(),
        "\n__tp_wrongnonce 0 {\"rc\":0,\"changed\":true,\"skipped\":false}\nafter\n"
    );
    assert_eq!(r.trailer.id, ids[0]);
    let r = sh.wait(ids[1]).await.unwrap();
    assert_eq!(r.output, b"second\n");
}

#[tokio::test]
async fn cd_and_export_do_not_leak_between_ops() {
    let mut sh = open_root().await;
    let h = host();
    h.raw("cd /tmp; export TP_X=1; echo $PWD $TP_X");
    h.raw("echo \"$PWD\" ${TP_X-unset}");
    let start = std::env::current_dir().unwrap();
    let ids = send_all(&mut sh, &h).await;
    let a = sh.wait(ids[0]).await.unwrap();
    let b = sh.wait(ids[1]).await.unwrap();
    assert_eq!(a.output, b"/tmp 1\n");
    assert_eq!(
        String::from_utf8(b.output).unwrap(),
        format!("{} unset\n", start.display())
    );
}

#[tokio::test]
async fn op_reading_stdin_does_not_swallow_later_ops() {
    // Ops are pipelined into the shell's stdin; an op that reads stdin must
    // see EOF, not the scripts queued behind it.
    let mut sh = open_root().await;
    let h = host();
    h.raw("cat; echo cat-done");
    h.raw("echo second");
    let ids = send_all(&mut sh, &h).await;
    let ten = std::time::Duration::from_secs(10);
    let a = tokio::time::timeout(ten, sh.wait(ids[0]))
        .await
        .expect("op 0 hung: it read the queued ops from stdin")
        .unwrap();
    assert_eq!(a.output, b"cat-done\n");
    let b = tokio::time::timeout(ten, sh.wait(ids[1]))
        .await
        .expect("op 1 hung")
        .unwrap();
    assert_eq!(b.output, b"second\n");
}

#[tokio::test]
async fn exit_in_op_reports_rc_and_shell_survives() {
    let mut sh = open_root().await;
    let h = host();
    h.raw("echo bye; exit 7");
    h.raw("echo still here");
    let ids = send_all(&mut sh, &h).await;
    let a = sh.wait(ids[0]).await.unwrap();
    assert_eq!(a.output, b"bye\n");
    assert_eq!(a.trailer.typed::<ShellResult>().unwrap().rc, 7);
    let b = sh.wait(ids[1]).await.unwrap();
    assert_eq!(b.output, b"still here\n");
}

#[tokio::test]
async fn large_output_does_not_deadlock() {
    let mut sh = open_root().await;
    let h = host();
    for _ in 0..20 {
        h.raw("head -c 300000 /dev/zero | tr '\\0' x");
    }
    let ids = send_all(&mut sh, &h).await;
    for id in ids {
        assert_eq!(sh.wait(id).await.unwrap().output.len(), 300000);
    }
}

#[tokio::test]
async fn shell_death_fails_in_flight_ops() {
    let mut sh = open_root().await;
    // Kill the persistent shell itself (not the op's subshell).
    sh.send(1, "kill -9 $$\n").await.unwrap();
    let h = host();
    h.raw("echo never");
    // The shell may already be gone when the next op is written: the send
    // then fails with `Died` (a broken pipe), and that is the op's failure,
    // not a bug. Either outcome is fine; nothing else is.
    let mut ids = vec![];
    for (i, n) in h.snapshot().nodes.iter().enumerate() {
        if let Node::Op(op) = n {
            match sh.send_op(op, i, &h.os).await {
                Ok(()) | Err(ShellError::Died { .. }) => ids.push(i),
                Err(e) => panic!("send after the kill: {e:?}"),
            }
        }
    }
    assert!(matches!(sh.wait(1).await, Err(ShellError::Died { .. })));
    assert!(matches!(
        sh.wait(ids[0]).await,
        Err(ShellError::Died { .. })
    ));
}

/// The race above made deterministic: the shell is known dead before the
/// send. The send fails with `Died`, and the op's `wait` says `Died` too
/// (not `UnknownOp`, and it does not hang).
///
/// "Dead" is the process gone, not only its output closed: the kernel
/// closes a dying process's descriptors one at a time, so for a moment
/// after stdout's EOF a write to stdin can still land in the pipe (about 1
/// run in 200 under load). The test waits until `/proc/<pid>` is gone or a
/// zombie.
#[tokio::test]
async fn send_to_a_dead_shell_is_the_ops_failure() {
    let mut sh = open_root().await;
    let h = host();
    h.raw("echo $$");
    h.raw("echo never");
    let snap = h.snapshot();
    let ops: Vec<_> = snap
        .nodes
        .iter()
        .filter_map(|n| match n {
            Node::Op(op) => Some(op),
            _ => None,
        })
        .collect();
    sh.send_op(ops[0], 0, &h.os).await.unwrap();
    let out = sh.wait(0).await.unwrap().output;
    let pid = String::from_utf8(out).unwrap().trim().to_string();
    sh.send(1, "kill -9 $$\n").await.unwrap();
    assert!(matches!(sh.wait(1).await, Err(ShellError::Died { .. })));
    let t0 = std::time::Instant::now();
    loop {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => break,
            Ok(st) if st.rsplit(") ").next().is_some_and(|r| r.starts_with('Z')) => break,
            Ok(_) => {}
        }
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(10),
            "shell {pid} never ended"
        );
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    let sent = sh.send_op(ops[1], 2, &h.os).await;
    assert!(matches!(sent, Err(ShellError::Died { .. })), "{sent:?}");
    let got = tokio::time::timeout(std::time::Duration::from_secs(10), sh.wait(2))
        .await
        .expect("wait on an op sent to a dead shell hung");
    assert!(matches!(got, Err(ShellError::Died { .. })), "{got:?}");
}

/// Write an executable script. It is written by a child `sh`, not by this
/// process: a write fd held here can leak into a sibling test thread's
/// fork and make exec fail with ETXTBSY ("Text file busy").
fn script(dir: &Path, name: &str, body: &str) -> String {
    let p = dir.join(name);
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg("cat > \"$1\" && chmod 755 \"$1\"")
        .arg("sh")
        .arg(&p)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("#!/bin/sh\n{body}\n").as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    p.display().to_string()
}

#[tokio::test]
async fn sudo_refusal_is_a_limit() {
    let d = tempfile::tempdir().unwrap();
    let sudo = script(
        d.path(),
        "sudo",
        "echo 'sudo: a password is required' >&2; exit 1",
    );
    let t = LocalTransport::new()
        .uid(1000)
        .sandboxed_at(d.path())
        .sudo(&sudo);
    let s = t.open("local", &Principal::Root, 1).await;
    assert!(s.got.is_empty());
    assert_eq!(s.limit, Some(Limit::SudoRefused));
}

#[tokio::test]
async fn sudo_that_execs_bash_gives_a_working_shell() {
    let d = tempfile::tempdir().unwrap();
    // Real sudo is called as `sudo -n -H bash`: `-H` gives the root shell
    // root's HOME even where sudoers says `!env_reset` for the caller. The
    // stub refuses anything else and sets HOME the way sudo -H does.
    let sudo = script(
        d.path(),
        "sudo",
        "[ \"$1\" = -n ] && [ \"$2\" = -H ] || exit 2\nshift 2\nHOME=/root; export HOME\nexec \"$@\"",
    );
    let t = LocalTransport::new()
        .uid(1000)
        .sandboxed_at(d.path())
        .sudo(&sudo);
    let mut s = t.open("local", &Principal::Root, 1).await;
    assert!(s.limit.is_none(), "{:?}", s.limit);
    let sh = &mut s.got[0];
    let h = host();
    h.raw("echo hi");
    h.raw("echo \"$HOME\"");
    let ids = send_all(sh, &h).await;
    assert_eq!(sh.wait(ids[0]).await.unwrap().output, b"hi\n");
    assert_eq!(sh.wait(ids[1]).await.unwrap().output, b"/root\n");
}

#[tokio::test]
async fn root_uid_does_not_call_sudo() {
    let d = tempfile::tempdir().unwrap();
    let sudo = script(d.path(), "sudo", "exit 1");
    let t = LocalTransport::new()
        .uid(0)
        .sandboxed_at(d.path())
        .sudo(&sudo);
    let s = t.open("local", &Principal::Root, 1).await;
    assert!(s.limit.is_none());
    assert_eq!(s.got.len(), 1);
}

/// Real sudo is called as `sudo -n -iu <u> bash` for a user other than the
/// caller (T120), as over ssh: the stub refuses anything else, sets HOME the
/// way `-i` does and execs the `bash` it is given. The caller is the `.user`
/// seam's `alice`, so no real account is named.
#[tokio::test]
async fn another_user_principal_is_sudo_n_iu_u_bash() {
    let d = tempfile::tempdir().unwrap();
    let sudo = script(
        d.path(),
        "sudo",
        "[ \"$1 $2 $3 $4\" = '-n -iu deploy bash' ] || exit 2\nshift 3\nHOME=/home/deploy; export HOME\nexec \"$@\"",
    );
    let t = LocalTransport::new()
        .uid(1000)
        .user("alice")
        .sandboxed_at(d.path())
        .sudo(&sudo);
    let mut s = t.open("local", &Principal::User("deploy".into()), 1).await;
    assert!(s.limit.is_none(), "{:?}", s.limit);
    assert_eq!(s.got[0].user(), "deploy");
    let sh = &mut s.got[0];
    let h = host();
    h.raw("echo \"$HOME\"");
    let ids = send_all(sh, &h).await;
    assert_eq!(sh.wait(ids[0]).await.unwrap().output, b"/home/deploy\n");
}

/// A refusal of that call is the same limit the root path has, with its
/// words kept for the report.
#[tokio::test]
async fn a_refused_user_principal_is_sudo_refused_with_its_words() {
    let d = tempfile::tempdir().unwrap();
    let sudo = script(
        d.path(),
        "sudo",
        "echo 'sudo: a password is required' >&2; exit 1",
    );
    let t = LocalTransport::new()
        .uid(1000)
        .user("alice")
        .sandboxed_at(d.path())
        .sudo(&sudo);
    let s = t.open("local", &Principal::User("deploy".into()), 1).await;
    assert!(s.got.is_empty());
    assert_eq!(s.limit, Some(Limit::SudoRefused));
    let m = t.message().unwrap();
    assert!(m.starts_with("sudo -n -iu deploy bash refused"), "{m}");
    assert!(m.ends_with("sudo: a password is required"), "{m}");
}

/// An `as_user(me)` op sent to a shell that is `me`'s is a plain op there:
/// no wrapper, the shell's own environment (the login environment is the
/// shell's, opened by the executor: `tests/login_shell.rs`). Another user is
/// a render error.
#[tokio::test]
async fn as_user_for_the_shells_own_user_is_a_plain_op_in_it() {
    let me = me();
    let sb = telepatch::testing::sandbox();
    let t = LocalTransport::new().uid(0).sandboxed(&sb);
    let mut s = t.open("local", &Principal::Root, 1).await;
    let sh = &mut s.got[0];

    let h = host();
    h.raw("echo plain ${TP_MARK-unset}");
    let u = h.as_user(&me);
    u.raw("echo login ${TP_MARK-unset}");
    let other = h.as_user("nobody-such");
    other.raw("echo x");
    let nodes = h.snapshot().nodes;
    let op = |i: usize| match &nodes[i] {
        Node::Op(o) => o.clone(),
        _ => unreachable!(),
    };

    sh.send_op(&op(0), 0, &h.os).await.unwrap();
    // The shell's identity for rendering is `me` (we may not really be root).
    let rendered = telepatch::trailer::render_for(&op(1), 1, sh.nonce(), &h.os, &me).unwrap();
    assert!(!rendered.contains("bash -l") && !rendered.contains("env -i"));
    sh.send(1, &rendered).await.unwrap();
    assert!(matches!(
        telepatch::trailer::render_for(&op(2), 2, sh.nonce(), &h.os, &me),
        Err(telepatch::trailer::RenderError::UserSwitchUnsupported { .. })
    ));

    assert_eq!(sh.wait(0).await.unwrap().output, b"plain unset\n");
    let r = sh.wait(1).await.unwrap();
    assert_eq!(String::from_utf8(r.output).unwrap(), "login unset\n");
    assert_eq!(r.trailer.typed::<ShellResult>().unwrap().rc, 0);
}
