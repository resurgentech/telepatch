//! Live: the `user` primitive on ubuntu:24.04 and rockylinux/rockylinux:10
//! through the executor over `docker exec`, with a `group` op first. Run 1
//! creates `tpdev` (home, shell, group, password hash) and fixes the shell
//! and groups of a pre-existing `tpbot` without touching its password; run
//! 2 changes nothing. The hash is a throwaway fixture generated here. Run
//! with `just test-live-user`.

use std::process::{Command, Stdio};

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::report::{HostStatus, Report};
use telepatch::transport::docker::DockerTransport;
use telepatch::{Host, Playbook, PlaybookFuture, Secret};

use crate::container::Container;

fn pb_run(h: Host<String>) -> PlaybookFuture {
    Box::pin(async move {
        let hash = Secret::literal(&*h.vars);
        h.group("tpgroup");
        h.user("tpdev")
            .shell("/bin/bash")
            .groups_add(["tpgroup"])
            .password_hash(&hash);
        h.user("tpbot")
            .shell("/bin/bash")
            .groups_add(["tpgroup"])
            .password_hash(&hash);
        Ok(())
    })
}

fn go(ctr: &Container, hash: &str) -> Report {
    let pb = Playbook::new("user", pb_run);
    run(
        &pb,
        vec![HostSpec {
            name: "c".into(),
            target: Target::Docker(DockerTransport::new().container("c", &ctr.name)),
            vars: hash.to_string(),
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Inherit,
        },
    )
}

/// `docker exec <ctr> sh -c script`: (success, stdout).
fn sh(ctr: &Container, script: &str) -> (bool, String) {
    let o = Command::new("docker")
        .args(["exec", &ctr.name, "sh", "-c", script])
        .stdin(Stdio::null())
        .output()
        .expect("docker exec");
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).trim().to_string(),
    )
}

/// Field `n` (1-based) of `getent <db> <name>`; `None` when absent.
fn field(ctr: &Container, db: &str, name: &str, n: usize) -> Option<String> {
    let (ok, out) = sh(ctr, &format!("getent {db} {name}"));
    if !ok {
        return None;
    }
    Some(out.split(':').nth(n - 1).unwrap_or("").to_string())
}

/// A throwaway sha512-crypt-shaped fixture, different every call.
fn fixture_hash() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!(
        "$6$tpfx{:x}${}",
        n % 0xffff_ffff,
        format!("{n:x}").repeat(6)
    )
}

fn twice(image: &str) {
    let ctr = Container::start(image);
    let hash = fixture_hash();
    assert_eq!(field(&ctr, "passwd", "tpdev", 7), None);
    // tpbot exists before telepatch runs, with another shell.
    let (ok, _) = sh(&ctr, "useradd -m -s /bin/sh tpbot");
    assert!(ok, "pre-create tpbot");
    let bot_pw = field(&ctr, "shadow", "tpbot", 2).expect("tpbot shadow");

    let r1 = go(&ctr, &hash);
    println!("{}", r1.render());
    assert_eq!(r1.exit_code(), 0, "run 1:\n{}", r1.render());
    assert_eq!(r1.hosts[0].status, HostStatus::Changed);
    let text = r1.render();
    assert!(text.contains("user tpdev: will create"), "{text}");
    assert!(text.contains("user tpbot: groups +tpgroup"), "{text}");
    assert!(text.contains("user tpbot: shell -> /bin/bash"), "{text}");
    assert!(!text.contains(&hash), "hash leaked into the report");
    assert_eq!(
        field(&ctr, "passwd", "tpdev", 7).as_deref(),
        Some("/bin/bash")
    );
    assert!(sh(&ctr, "test -d /home/tpdev").0, "home created");
    assert!(
        sh(&ctr, "id -nG tpdev")
            .1
            .split(' ')
            .any(|g| g == "tpgroup")
    );
    assert_eq!(
        field(&ctr, "shadow", "tpdev", 2).as_deref(),
        Some(hash.as_str()),
        "password applied on create"
    );
    assert_eq!(
        field(&ctr, "passwd", "tpbot", 7).as_deref(),
        Some("/bin/bash"),
        "existing shell fixed"
    );
    assert!(
        sh(&ctr, "id -nG tpbot")
            .1
            .split(' ')
            .any(|g| g == "tpgroup")
    );
    assert_eq!(
        field(&ctr, "shadow", "tpbot", 2),
        Some(bot_pw),
        "existing password untouched"
    );

    let other = fixture_hash();
    let r2 = go(&ctr, &other);
    println!("{}", r2.render());
    assert_eq!(r2.exit_code(), 0, "run 2:\n{}", r2.render());
    assert_eq!(r2.hosts[0].status, HostStatus::Ok);
    assert!(r2.hosts[0].will_create.is_empty());
    for o in &r2.hosts[0].ops {
        assert!(!o.changed, "run 2 changed {}: {}", o.id, o.desc);
    }
    assert_eq!(
        field(&ctr, "shadow", "tpdev", 2).as_deref(),
        Some(hash.as_str()),
        "a new hash does not change an existing account"
    );
}

#[test]
#[ignore = "needs docker"]
fn user_ubuntu_twice_unchanged() {
    twice("ubuntu:24.04");
}

#[test]
#[ignore = "needs docker"]
fn user_rocky_twice_unchanged() {
    twice("rockylinux/rockylinux:10");
}
