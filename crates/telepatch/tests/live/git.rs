//! Live: `git` checkout on ubuntu:24.04. The remote is a bare repo created
//! inside the container. Run with `just docker-vm test-live-git`.
//!
//! `as_user` is not executed here: the docker transport cannot open a user
//! shell. The trace test covers `login:tpdev` and `~/` expanding as `$HOME`.

use std::cell::RefCell;
use std::process::{Command, Stdio};
use std::rc::Rc;

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::report::{HostStatus, Report};
use telepatch::transport::docker::DockerTransport;
use telepatch::{Host, Playbook, PlaybookFuture};

use crate::container::Container;

#[derive(Clone)]
struct G {
    url: String,
    dest: String,
    rev: String,
}

fn leave(h: Host<G>) -> PlaybookFuture {
    Box::pin(async move {
        h.git(&h.vars.url).depth(1).into(&h.vars.dest);
        Ok(())
    })
}

fn latest(h: Host<G>) -> PlaybookFuture {
    Box::pin(async move {
        h.git(&h.vars.url).depth(1).latest().into(&h.vars.dest);
        Ok(())
    })
}

fn at_rev(h: Host<G>) -> PlaybookFuture {
    Box::pin(async move {
        h.git(&h.vars.url)
            .depth(1)
            .rev(&h.vars.rev)
            .into(&h.vars.dest);
        Ok(())
    })
}

fn go(
    ctr: &Container,
    name: &'static str,
    v: &G,
    f: fn(Host<G>) -> PlaybookFuture,
) -> (Report, String) {
    let pb = Playbook::new(name, f);
    let out = Rc::new(RefCell::new(String::new()));
    let r = run(
        &pb,
        vec![HostSpec {
            name: "c".into(),
            target: Target::Docker(DockerTransport::new().container("c", &ctr.name)),
            vars: v.clone(),
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    let text = out.borrow().clone();
    println!("{text}\n{}", r.render());
    (r, text)
}

/// (rc, stdout, stderr), both trimmed.
fn exec(ctr: &Container, script: &str) -> (i32, String, String) {
    let o = Command::new("docker")
        .args(["exec", &ctr.name, "bash", "-lc", script])
        .stdin(Stdio::null())
        .output()
        .expect("docker exec");
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).trim().to_string(),
        String::from_utf8_lossy(&o.stderr).trim().to_string(),
    )
}

fn why(r: &Report) -> String {
    match &r.hosts[0].status {
        HostStatus::Blocked(s) | HostStatus::Error(s) => s.clone(),
        other => format!("{other:?}\n{}", r.render()),
    }
}

#[test]
#[ignore = "needs docker and network"]
fn git_ubuntu_checkout() {
    let ctr = Container::start("ubuntu:24.04");
    let (rc, o, e) = exec(
        &ctr,
        "export DEBIAN_FRONTEND=noninteractive; apt-get update && apt-get install -y git",
    );
    assert_eq!(rc, 0, "install git: {o}\n{e}");
    let (rc, o, e) = exec(
        &ctr,
        "git init --bare -b main /srv/remote.git \
         && git init -b main /srv/work \
         && printf 'one\\n' > /srv/work/f.txt \
         && git -C /srv/work -c user.email=t@example.invalid -c user.name=t add f.txt \
         && git -C /srv/work -c user.email=t@example.invalid -c user.name=t commit -m a \
         && git -C /srv/work remote add origin file:///srv/remote.git \
         && git -C /srv/work push -u origin main \
         && git -C /srv/work rev-parse HEAD",
    );
    assert_eq!(rc, 0, "bare repo: {o}\n{e}");
    let first = o.lines().last().unwrap().trim().to_string();
    assert_eq!(first.len(), 40, "{o}");
    let vars = G {
        url: "file:///srv/remote.git".into(),
        dest: "/srv/checkout".into(),
        rev: first.clone(),
    };

    let (r1, out1) = go(&ctr, "leave", &vars, leave);
    assert_eq!(r1.exit_code(), 0, "run 1:\n{out1}\n{}", r1.render());
    assert_eq!(r1.hosts[0].status, HostStatus::Changed, "{}", r1.render());
    assert!(
        r1.hosts[0]
            .git
            .iter()
            .any(|g| g.created && g.path == "/srv/checkout"),
        "{:?}",
        r1.hosts[0].git
    );
    let (rc, head, e) = exec(&ctr, "git -C /srv/checkout rev-parse HEAD");
    assert_eq!((rc, head.as_str()), (0, first.as_str()), "{e}");
    let (rc, body, e) = exec(&ctr, "cat /srv/checkout/f.txt");
    assert_eq!((rc, body.as_str()), (0, "one"), "{e}");

    let (r2, out2) = go(&ctr, "leave", &vars, leave);
    assert_eq!(r2.exit_code(), 0, "run 2:\n{out2}\n{}", r2.render());
    assert_eq!(r2.hosts[0].status, HostStatus::Ok, "{}", r2.render());

    let (rc, o, e) = exec(
        &ctr,
        "printf 'two\\n' > /srv/work/f.txt \
         && git -C /srv/work -c user.email=t@example.invalid -c user.name=t add f.txt \
         && git -C /srv/work -c user.email=t@example.invalid -c user.name=t commit -m b \
         && git -C /srv/work push origin main \
         && git -C /srv/work rev-parse HEAD",
    );
    assert_eq!(rc, 0, "second commit: {o}\n{e}");
    let second = o.lines().last().unwrap().trim().to_string();
    assert_ne!(first, second);

    let (r3, out3) = go(&ctr, "leave", &vars, leave);
    assert_eq!(r3.exit_code(), 0, "leave:\n{out3}\n{}", r3.render());
    assert_eq!(r3.hosts[0].status, HostStatus::Ok, "{}", r3.render());
    let (_, head, _) = exec(&ctr, "git -C /srv/checkout rev-parse HEAD");
    assert_eq!(head, first, "no .rev leaves the checkout alone");

    let (r4, out4) = go(&ctr, "latest", &vars, latest);
    assert_eq!(r4.exit_code(), 0, "latest:\n{out4}\n{}", r4.render());
    assert_eq!(r4.hosts[0].status, HostStatus::Changed, "{}", r4.render());
    let (_, head, _) = exec(&ctr, "git -C /srv/checkout rev-parse HEAD");
    assert_eq!(head, second);
    assert!(
        r4.hosts[0]
            .git
            .iter()
            .any(|g| !g.created && g.was_rev == first),
        "{:?}",
        r4.hosts[0].git
    );

    let (r5, out5) = go(&ctr, "rev", &vars, at_rev);
    assert_eq!(r5.exit_code(), 0, "rev:\n{out5}\n{}", r5.render());
    let (_, head, _) = exec(&ctr, "git -C /srv/checkout rev-parse HEAD");
    assert_eq!(head, first, ".rev checks out that revision");

    let (rc, _, e) = exec(&ctr, "printf 'dirty\\n' > /srv/checkout/f.txt");
    assert_eq!(rc, 0, "{e}");
    let (r6, _) = go(&ctr, "latest", &vars, latest);
    let msg = why(&r6);
    assert!(msg.contains("dirty"), "{msg}");
    let (_, body, _) = exec(&ctr, "cat /srv/checkout/f.txt");
    assert_eq!(body, "dirty");
    let (_, head, _) = exec(&ctr, "git -C /srv/checkout rev-parse HEAD");
    assert_eq!(head, first, "a dirty tree is not reset");

    let (rc, o, e) = exec(&ctr, "git init --bare -b main /srv/other.git");
    assert_eq!(rc, 0, "{o}\n{e}");
    let (rc, _, e) = exec(&ctr, "printf 'one\\n' > /srv/checkout/f.txt");
    assert_eq!(rc, 0, "{e}");
    let (r7, _) = go(
        &ctr,
        "other",
        &G {
            url: "file:///srv/other.git".into(),
            ..vars
        },
        leave,
    );
    let msg = why(&r7);
    assert!(msg.contains("remote"), "{msg}");
    let (_, head, _) = exec(&ctr, "git -C /srv/checkout rev-parse HEAD");
    assert_eq!(head, first);
}
