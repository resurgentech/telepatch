//! Live: the `unit` primitive on ubuntu:24.04 and rockylinux/rockylinux:10,
//! plain containers with no init (`sleep infinity` is PID 1), through the
//! executor over `docker exec`. Run with `just test-live-unit`.
//!
//! systemd is installed by a `pkg` op (it brings `systemctl` and
//! `systemd-analyze`). Then one playbook writes a unit and a drop-in for it
//! with `h.unit` and enables the unit (offline: no running systemd):
//!
//! * run 1 says `unit tp-live.service: will create` and the same for the
//!   drop-in, verifies both (no "not verified" line), writes them, enables
//!   the unit; there is no running systemd, so no `daemon-reload` is tried
//!   (offline it would fail the run: "System has not been booted");
//! * run 2 changes nothing and prepare says nothing;
//! * a unit with an unknown key fails `systemd-analyze verify`: the op
//!   fails and the file is not written.

use std::cell::RefCell;
use std::process::{Command, Stdio};
use std::rc::Rc;

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::report::{HostStatus, Report};
use telepatch::transport::docker::DockerTransport;
use telepatch::{Host, Playbook, PlaybookFuture};

use crate::container::Container;

const UNIT: &str = "[Unit]\nDescription=telepatch live unit\n\n[Service]\nExecStart=/bin/sleep infinity\n\n[Install]\nWantedBy=multi-user.target\n";
const DROP_IN: &str = "[Service]\nEnvironment=TP_LIVE=1\n";

fn pb_install(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        h.pkg(["systemd"]);
        Ok(())
    })
}

fn pb_unit(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        let unit = h.unit("tp-live.service");
        unit.content(UNIT).mode(0o644);
        h.unit("tp-live.service").drop_in("override", DROP_IN);
        h.svc("tp-live").enabled();
        Ok(())
    })
}

fn pb_bad(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        h.unit("tp-bad.service")
            .content("[Unit]\nDescription=bad\nNoSuchKey=1\n\n[Service]\nExecStart=/bin/true\n");
        Ok(())
    })
}

/// Run `f` on the container; the report and the captured stream.
fn go(ctr: &Container, name: &'static str, f: fn(Host<()>) -> PlaybookFuture) -> (Report, String) {
    let pb = Playbook { name, run: f };
    let out = Rc::new(RefCell::new(String::new()));
    let r = run(
        &pb,
        vec![HostSpec {
            name: "c".into(),
            target: Target::Docker(DockerTransport::new().container("c", &ctr.name)),
            vars: (),
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

/// `docker exec <ctr> sh -c <script>`: (rc, stdout).
fn exec(ctr: &Container, script: &str) -> (i32, String) {
    let o = Command::new("docker")
        .args(["exec", &ctr.name, "sh", "-c", script])
        .stdin(Stdio::null())
        .output()
        .expect("docker exec");
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).trim().to_string(),
    )
}

fn unit_and_drop_in(image: &str) {
    let ctr = Container::start(image);
    let (r, _) = go(&ctr, "install", pb_install);
    assert_eq!(r.exit_code(), 0, "install:\n{}", r.render());
    let (rc, out) = exec(&ctr, "test -d /run/systemd/system && echo booted");
    assert_ne!(rc, 0, "a plain container is not booted: {out}");
    let (rc, _) = exec(&ctr, "command -v systemd-analyze");
    assert_eq!(rc, 0, "{image}: the systemd package brings systemd-analyze");

    // run 1: written, verified, enabled offline, no reload
    let (r1, out1) = go(&ctr, "unit", pb_unit);
    assert_eq!(r1.exit_code(), 0, "run 1:\n{out1}\n{}", r1.render());
    assert_eq!(r1.hosts[0].status, HostStatus::Changed);
    assert!(
        out1.contains("== c [prepare] unit tp-live.service: will create =="),
        "{out1}"
    );
    assert!(
        out1.contains("== c [prepare] unit tp-live.service.d/override.conf: will create =="),
        "{out1}"
    );
    assert!(!out1.contains("not verified"), "verified: {out1}");
    assert!(!out1.contains("has not been booted"), "no reload: {out1}");
    let (_, body) = exec(&ctr, "cat /etc/systemd/system/tp-live.service");
    assert_eq!(body, UNIT.trim());
    let (_, body) = exec(
        &ctr,
        "cat /etc/systemd/system/tp-live.service.d/override.conf",
    );
    assert_eq!(body, DROP_IN.trim());
    let (_, m) = exec(&ctr, "stat -c %a /etc/systemd/system/tp-live.service");
    assert_eq!(m, "644");
    let (rc, out) = exec(&ctr, "systemctl is-enabled tp-live");
    assert_eq!((rc, out.as_str()), (0, "enabled"));
    let (_, left) = exec(&ctr, "ls -a /etc/systemd/system | grep '\\.tp' || true");
    assert_eq!(left, "", "no candidate left behind");

    // run 2: nothing to do
    let (r2, out2) = go(&ctr, "unit", pb_unit);
    assert_eq!(r2.exit_code(), 0, "run 2:\n{out2}");
    assert_eq!(r2.hosts[0].status, HostStatus::Ok, "{}", r2.render());
    assert!(r2.hosts[0].will_create.is_empty(), "{}", r2.render());
    for o in &r2.hosts[0].ops {
        assert!(!o.changed, "run 2 changed {}: {}", o.id, o.desc);
    }

    // a unit systemd-analyze refuses is not written
    let (r3, out3) = go(&ctr, "bad", pb_bad);
    assert_ne!(r3.exit_code(), 0, "a bad unit:\n{out3}");
    let (rc, _) = exec(&ctr, "test -e /etc/systemd/system/tp-bad.service");
    assert_ne!(rc, 0, "the refused unit was written");
}

#[test]
#[ignore = "needs docker"]
fn unit_ubuntu_unit_and_drop_in_offline() {
    unit_and_drop_in("ubuntu:24.04");
}

#[test]
#[ignore = "needs docker"]
fn unit_rocky_unit_and_drop_in_offline() {
    unit_and_drop_in("rockylinux/rockylinux:10");
}
