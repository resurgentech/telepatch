//! Live inventory: two sshd containers in the docker VM, each published on
//! its own port of `TP_DOCKER_VM_PORTS` (`Sshd::start`, `endpoint.rs`), one
//! `inventory.toml` naming both as `ssh://tp@<addr>:<port>` (on a remote VM
//! host both are `127.0.0.1` and differ only by port), and the real
//! `run --inventory` of the playbook runner against them. ssh trusts the
//! host keys through a throwaway known-hosts file (`accept-new`), so
//! `[127.0.0.1]:<port>` entries of another run cannot collide. A group's
//! var reaches one container only. Run with `just test-live-inventory`.

use std::time::Duration;

use serde::Deserialize;
use telepatch::cli::run_with_args;
use telepatch::transport::local::LocalTransport;
use telepatch::transport::ssh::SshTransport;
use telepatch::{Host, Playbook, PlaybookFuture};

use crate::sshd::{SERIAL, Sshd, ssh_processes};

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Vars {
    greeting: String,
}

fn entry(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move {
        h.file("/srv/tp-inv").dir();
        h.file("/srv/tp-inv/greeting")
            .content(format!("{}\n", h.vars.greeting));
        h.file("/srv/tp-inv/name")
            .content(format!("{}\n", h.name()));
        if h.in_group("gpu") {
            h.file("/srv/tp-inv/gpu").content("in gpu\n");
        }
        Ok(())
    })
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

/// The report row for `dest`: its status word, if the host is in `out`.
fn status(out: &str, dest: &str) -> Option<String> {
    out.lines().find_map(|l| {
        let mut w = l.split_whitespace();
        (w.next() == Some(dest)).then(|| w.next().unwrap_or("").to_string())
    })
}

fn absent(d: &Sshd, path: &str) -> bool {
    d.exec(&format!("test -e {path} && echo present || echo absent")) == "absent\n"
}

#[test]
#[ignore = "needs docker and the network"]
fn inventory_runs_two_containers_and_a_group_var_reaches_one() {
    let _serial = SERIAL.blocking_lock();
    let a = Sshd::start();
    let b = Sshd::start_sharing_key(&a);
    assert_eq!(a.host, b.host);
    assert_ne!(a.port, b.port, "two containers, two ports");
    let (da, db) = (a.dest("tp"), b.dest("tp"));
    println!("hosts: {da} and {db}");

    let dir = tempfile::tempdir().unwrap();
    let inv = dir.path().join("inventory.toml");
    std::fs::write(
        &inv,
        format!(
            "[vars]\ngreeting = \"inventory\"\n\n\
             [groups.base]\nhosts = [\"{da}\"]\n\n\
             [groups.gpu]\nhosts = [\"{db}\"]\nvars = {{ greeting = \"gpu box\" }}\n"
        ),
    )
    .unwrap();
    // a fresh file per run: entries `[127.0.0.1]:<port>` of any other run
    // are not in it
    let known = dir.path().join("known_hosts");
    let known_opt = format!("UserKnownHostsFile={}", known.display());
    let pbs = vec![Playbook::new("inv", entry)];
    let go = |extra: &[&str]| -> (String, i32) {
        let mut args: Vec<String> = ["pb", "run", "inv", "--inventory"]
            .into_iter()
            .map(String::from)
            .collect();
        args.push(inv.display().to_string());
        for (flag, val) in [
            ("--ssh-key", a.key.display().to_string()),
            ("--ssh-option", known_opt.clone()),
            (
                "--ssh-option",
                "StrictHostKeyChecking=accept-new".to_string(),
            ),
            ("--ssh-option", "ConnectTimeout=10".to_string()),
        ] {
            args.push(flag.into());
            args.push(val);
        }
        args.extend(extra.iter().map(|s| s.to_string()));
        let (out, code) = run_with_args(&args, &pbs, LocalTransport::new, SshTransport::new);
        println!("{out}");
        (out, code)
    };

    assert!(!known.exists(), "the known-hosts file starts empty");
    let (out, code) = go(&[]);
    assert_eq!(code, 0, "{out}");
    for d in [&da, &db] {
        assert_eq!(status(&out, d).as_deref(), Some("changed"), "{d}: {out}");
    }
    none_left();

    // the group var reached the container in `gpu` only; the other got the
    // inventory's.
    assert_eq!(a.exec("cat /srv/tp-inv/greeting"), "inventory\n");
    assert_eq!(b.exec("cat /srv/tp-inv/greeting"), "gpu box\n");
    // the host name is the destination, and in_group saw the groups
    assert_eq!(a.exec("cat /srv/tp-inv/name"), format!("{da}\n"));
    assert_eq!(b.exec("cat /srv/tp-inv/name"), format!("{db}\n"));
    assert!(absent(&a, "/srv/tp-inv/gpu"));
    assert_eq!(b.exec("cat /srv/tp-inv/gpu"), "in gpu\n");

    // ssh wrote the host keys to the throwaway file, one per host:port
    let kh = std::fs::read_to_string(&known).unwrap();
    let lines = kh.lines().filter(|l| !l.trim().is_empty()).count();
    assert!(lines >= 2, "{kh}");

    // a second run changes nothing
    let (out, code) = go(&[]);
    assert_eq!(code, 0, "{out}");
    for d in [&da, &db] {
        assert_eq!(status(&out, d).as_deref(), Some("ok"), "{d}: {out}");
    }
    none_left();

    // --limit by group: only the gpu host is touched
    a.exec("rm /srv/tp-inv/greeting");
    b.exec("rm /srv/tp-inv/greeting");
    let (out, code) = go(&["--limit", "gpu"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(status(&out, &db).as_deref(), Some("changed"), "{out}");
    assert_eq!(status(&out, &da), None, "{out}");
    assert_eq!(b.exec("cat /srv/tp-inv/greeting"), "gpu box\n");
    assert!(
        absent(&a, "/srv/tp-inv/greeting"),
        "the host outside the limit is not touched"
    );
    none_left();
}
