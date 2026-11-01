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
    let pb = Playbook {
        name: "alice",
        run: alice_pb,
    };

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
        let pb = Playbook {
            name: who,
            run: run_,
        };
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
