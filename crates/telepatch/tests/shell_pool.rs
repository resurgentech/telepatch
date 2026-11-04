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
    let t = LocalTransport::new().uid(0);
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
    let ids = send_all(&mut sh, &h).await;
    assert!(matches!(sh.wait(1).await, Err(ShellError::Died { .. })));
    assert!(matches!(
        sh.wait(ids[0]).await,
        Err(ShellError::Died { .. })
    ));
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
    let t = LocalTransport::new().uid(1000).sudo(&sudo);
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
    let t = LocalTransport::new().uid(1000).sudo(&sudo);
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
    let t = LocalTransport::new().uid(0).sudo(&sudo);
    let s = t.open("local", &Principal::Root, 1).await;
    assert!(s.limit.is_none());
    assert_eq!(s.got.len(), 1);
}

#[tokio::test]
async fn non_root_user_principal_is_not_supported_yet() {
    let t = LocalTransport::new().uid(0);
    let s = t.open("local", &Principal::User("deploy".into()), 1).await;
    assert!(s.got.is_empty());
    match s.limit {
        Some(Limit::Other(m)) => assert!(m.contains("not supported by this transport yet"), "{m}"),
        l => panic!("{l:?}"),
    }
}

/// Fake `getent` + a HOME with a login profile; `as_user(me)` sees the
/// profile's marker, a plain op does not. Also: another user is a render
/// error, and a missing passwd entry fails the op with a trailer.
#[tokio::test]
async fn as_user_runs_in_a_fresh_login_environment() {
    let me = me();
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(
        home.join(".bash_profile"),
        "export TP_MARK=from-profile\nexport PATH=\"$HOME/bin:$PATH\"\n",
    )
    .unwrap();
    let bin = d.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    script(
        &bin,
        "getent",
        &format!(
            "[ \"$1 $2\" = \"passwd {me}\" ] || exit 2\necho '{me}:x:1:1::{}:/bin/bash'",
            home.display()
        ),
    );
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let t = LocalTransport::new().uid(0).env("PATH", &path);
    let mut s = t.open("local", &Principal::Root, 1).await;
    let sh = &mut s.got[0];

    let h = host();
    h.raw("echo plain ${TP_MARK-unset}");
    let u = h.as_user(&me);
    u.raw(
        "echo login $TP_MARK; echo $HOME $USER; case :$PATH: in *:$HOME/bin:*) echo path-ok;; esac",
    );
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
    sh.send(1, &rendered).await.unwrap();
    assert!(matches!(
        telepatch::trailer::render_for(&op(2), 2, sh.nonce(), &h.os, &me),
        Err(telepatch::trailer::RenderError::UserSwitchUnsupported { .. })
    ));

    assert_eq!(sh.wait(0).await.unwrap().output, b"plain unset\n");
    let r = sh.wait(1).await.unwrap();
    assert_eq!(
        String::from_utf8(r.output).unwrap(),
        format!("login from-profile\n{} {me}\npath-ok\n", home.display())
    );
    assert_eq!(r.trailer.typed::<ShellResult>().unwrap().rc, 0);

    // A user the shell is, but without a passwd entry (fake getent says no).
    let h2 = host();
    h2.as_user("ghost").raw("echo x");
    let Node::Op(g) = &h2.snapshot().nodes[0] else {
        unreachable!()
    };
    let rendered = telepatch::trailer::render_for(g, 9, sh.nonce(), &h2.os, "ghost").unwrap();
    sh.send(9, &rendered).await.unwrap();
    let r = sh.wait(9).await.unwrap();
    assert_eq!(r.trailer.typed::<ShellResult>().unwrap().rc, 1);
    assert!(
        String::from_utf8(r.output)
            .unwrap()
            .contains("no passwd entry")
    );
}
