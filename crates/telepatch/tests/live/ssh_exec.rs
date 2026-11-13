//! Live: playbooks through [`telepatch::executor::run`] with `Target::Ssh`
//! over the sshd container as `tp` (NOPASSWD sudo). Root ops on the root
//! shell, `as_user("alice")` ops and fact reads on alice's `sudo -n -iu`
//! shell; a second run changes nothing; a user that does not exist, or
//! whose login shell refuses, blocks the host with nothing applied. Run
//! with `just test-live-ssh`.

use std::process::{Command, Stdio};
use std::time::Duration;

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::report::{HostStatus, Report};
use telepatch::{Host, Playbook, PlaybookFuture, sh};

use crate::sshd::{SERIAL, Sshd, ssh_processes, transport};

fn go(d: &Sshd, pb: &Playbook<()>) -> Report {
    let r = run(
        pb,
        vec![HostSpec {
            groups: Vec::new(),
            name: d.dest("tp"),
            target: Target::Ssh(transport(&d.key)),
            vars: (),
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Inherit,
        },
    );
    println!("{}", r.render());
    r
}

/// `sh -c script` in the container as root; its stdout.
fn exec(d: &Sshd, script: &str) -> String {
    let out = Command::new("docker")
        .args(["exec", &d.name, "sh", "-c", script])
        .stdin(Stdio::null())
        .output()
        .expect("docker exec");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn none_left() {
    for _ in 0..100 {
        if ssh_processes().is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("ssh processes left: {:#?}", ssh_processes());
}

fn alice_pb(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        h.file("/srv/tp-x").dir().owner("alice");
        let a = h.as_user("alice");
        a.file("/srv/tp-x/note").content("hi from alice\n");
        sh!(
            a,
            "id -un > /srv/tp-x/whoami; printf '%s\\n' \"$HOME\" >> /srv/tp-x/whoami",
            creates = "/srv/tp-x/whoami"
        );
        let bash = a.bin_exists("bash").await;
        let none = a.bin_exists("tp-no-such-bin").await;
        h.file("/srv/tp-x/facts")
            .content(format!("bash={bash} none={none}\n"));
        Ok(())
    })
}

type PlaybookFn = fn(Host<()>) -> PlaybookFuture;

fn blocked_as(h: Host<()>, who: &'static str) -> PlaybookFuture {
    Box::pin(async move {
        h.file(&format!("/srv/tp-marker-{who}")).content("x\n");
        let u = h.as_user(who);
        sh!(u, "true");
        Ok(())
    })
}

fn blocked_nosuch(h: Host<()>) -> PlaybookFuture {
    blocked_as(h, "nosuch")
}

fn blocked_carol(h: Host<()>) -> PlaybookFuture {
    blocked_as(h, "carol")
}

#[test]
#[ignore = "needs docker and the network"]
fn ssh_exec_routes_principals_against_sshd() {
    let _serial = SERIAL.blocking_lock();
    let d = Sshd::start();
    let pb = Playbook::new("alice", alice_pb);

    let r1 = go(&d, &pb);
    assert_eq!(r1.hosts[0].status, HostStatus::Changed, "{}", r1.render());
    assert_eq!(exec(&d, "stat -c %U /srv/tp-x"), "alice\n");
    assert_eq!(exec(&d, "stat -c %U /srv/tp-x/note"), "alice\n");
    assert_eq!(exec(&d, "cat /srv/tp-x/note"), "hi from alice\n");
    assert_eq!(exec(&d, "cat /srv/tp-x/whoami"), "alice\n/home/alice\n");
    assert_eq!(exec(&d, "cat /srv/tp-x/facts"), "bash=true none=false\n");
    assert_eq!(exec(&d, "stat -c %U /srv/tp-x/facts"), "root\n");
    none_left();

    let r2 = go(&d, &pb);
    assert_eq!(r2.hosts[0].status, HostStatus::Ok, "{}", r2.render());
    let changed = r2.hosts[0].ops.iter().filter(|o| o.changed).count();
    assert_eq!(changed, 0, "{}", r2.render());
    println!("second run: changed {changed}");
    none_left();

    // A user with no passwd entry (getent) and one whose login shell
    // refuses (sudo -iu): blocked, the window's root op not applied.
    let cases: [(&str, PlaybookFn, &str); 2] = [
        ("nosuch", blocked_nosuch, "user nosuch does not exist"),
        ("carol", blocked_carol, "sudo -n -iu carol refused"),
    ];
    for (who, run_, want) in cases {
        let pb = Playbook::new(who, run_);
        let r = go(&d, &pb);
        let HostStatus::Blocked(why) = &r.hosts[0].status else {
            panic!("{who}: {}", r.render())
        };
        assert!(why.contains(want), "{who}: {why}");
        assert_eq!(
            exec(
                &d,
                &format!("test -e /srv/tp-marker-{who} && echo present || echo absent")
            ),
            "absent\n",
            "{who}: nothing applied"
        );
        println!("{who}: blocked: {why}");
        none_left();
    }
}

fn dave_pb(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        h.user("dave").shell("/bin/bash");
        let d = h.as_user("dave");
        sh!(
            d,
            "id -un > ~/whoami; printf '%s\\n' \"$HOME\" >> ~/whoami",
            creates = "/home/dave/whoami"
        );
        Ok(())
    })
}

/// An account the run creates is then addressed with `as_user` in the same
/// run: the engine ends the window after the `user` op, so prepare's
/// `getent` check and the `sudo -n -iu dave` shell come after `useradd`.
#[test]
#[ignore = "needs docker and the network"]
fn ssh_exec_runs_as_a_user_the_run_creates() {
    let _serial = SERIAL.blocking_lock();
    let d = Sshd::start();
    assert_eq!(exec(&d, "getent passwd dave || echo none"), "none\n");
    let pb = Playbook::new("dave", dave_pb);

    let r1 = go(&d, &pb);
    assert_eq!(r1.hosts[0].status, HostStatus::Changed, "{}", r1.render());
    assert_eq!(
        exec(&d, "cat /home/dave/whoami"),
        "dave\n/home/dave\n",
        "{}",
        r1.render()
    );
    none_left();

    let r2 = go(&d, &pb);
    assert_eq!(r2.hosts[0].status, HostStatus::Ok, "{}", r2.render());
    let changed = r2.hosts[0].ops.iter().filter(|o| o.changed).count();
    assert_eq!(changed, 0, "{}", r2.render());
    println!("second run: changed {changed}");
    none_left();
}

fn timeout_pb(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        // A group of two `sleep`s; the op's clock stops it. `allow_fail`
        // lets the run go on, as a later op on the same persistent shell.
        sh!(h, "sleep 3051 & sleep 3052 & wait")
            .timeout(Duration::from_secs(2))
            .allow_fail();
        let _ = h.bin_exists("tp-no-such-bin").await;
        sh!(h, "touch /srv/tp-after-timeout");
        Ok(())
    })
}

/// `.timeout` over a real sshd: the second shell (`sudo -n -H bash`, a
/// second channel) takes the op's process group down, the shell that ran
/// the op is still usable, and the next op runs. Run by T104.
#[test]
#[ignore = "needs docker and the network"]
fn ssh_exec_timeout_kills_the_group_and_the_next_op_runs() {
    let _serial = SERIAL.blocking_lock();
    let d = Sshd::start();
    let pb = Playbook::new("timeout", timeout_pb);
    let r = go(&d, &pb);
    assert_ne!(r.exit_code(), 1, "{}", r.render());
    assert_eq!(
        exec(
            &d,
            "test -e /srv/tp-after-timeout && echo present || echo absent"
        ),
        "present\n",
        "the next op did not run: {}",
        r.render()
    );
    // Both sleeps are gone (the pattern's own command line does not match
    // itself: `[12]` is not `1` or `2`).
    let left = exec(
        &d,
        "for f in /proc/[0-9]*/cmdline; do tr '\\0' ' ' < \"$f\" 2>/dev/null; echo; done | grep -c 'sleep 305[12]'",
    );
    assert_eq!(left.trim(), "0", "a sleep outlived its op");
    none_left();
}
