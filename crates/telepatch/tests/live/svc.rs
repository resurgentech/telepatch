//! Live: the `svc` primitive on ubuntu:24.04 and rockylinux/rockylinux:10,
//! plain containers with no init (`sleep infinity` is PID 1), through the
//! executor over `docker exec`. Run with `just test-live-svc`.
//!
//! * Without systemd installed (no `systemctl`): the op is skipped, prepare
//!   says `no systemctl: skipped`, runs 1 and 2 change nothing.
//! * With systemd installed by a `pkg` op (and `cron` / `cronie`, which
//!   their package enables on install, so the test disables the unit
//!   first): run 1 says `will enable; no running systemd: start skipped` and
//!   enables the unit offline (plain `systemctl enable`, no `--root=/`);
//!   run 2 changes nothing.
//! * Disabled and masked are applied offline too (`will disable; no running
//!   systemd: stop skipped`, `will mask; ...`), twice with nothing changed
//!   the second time; enabling a masked unit fails.

use std::process::{Command, Stdio};

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::facts::os::Family;
use telepatch::report::{HostStatus, Report};
use telepatch::transport::docker::DockerTransport;
use telepatch::{Host, Playbook, PlaybookFuture};

use crate::container::Container;

/// `cron` on Debian, `crond` on Red Hat.
fn unit(h: &Host<()>) -> &'static str {
    match h.os.family {
        Family::RedHat => "crond",
        _ => "cron",
    }
}

fn pb_install(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        match h.os.family {
            Family::RedHat => h.pkg(["systemd", "cronie"]),
            _ => h.pkg(["systemd", "cron"]),
        };
        Ok(())
    })
}

fn pb_svc(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        h.svc(unit(&h)).enabled().started();
        Ok(())
    })
}

fn go(ctr: &Container, name: &'static str, f: fn(Host<()>) -> PlaybookFuture) -> Report {
    let pb = Playbook::new(name, f);
    run(
        &pb,
        vec![HostSpec {
            groups: Vec::new(),
            name: "c".into(),
            target: Target::Docker(DockerTransport::new().container("c", &ctr.name)),
            vars: (),
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Inherit,
        },
    )
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

fn nothing_changed(r: &Report, what: &str) {
    for o in &r.hosts[0].ops {
        assert!(!o.changed, "{what} changed {}: {}", o.id, o.desc);
    }
}

fn twice(image: &str, unit: &str) {
    let ctr = Container::start(image);
    let (rc, _) = exec(&ctr, "command -v systemctl");
    assert_ne!(rc, 0, "{image} already has systemctl");

    // No systemctl: skipped, never a change.
    for n in 1..=2 {
        let r = go(&ctr, "svc", pb_svc);
        println!("{}", r.render());
        assert_eq!(r.exit_code(), 0, "no systemctl, run {n}:\n{}", r.render());
        assert_eq!(r.hosts[0].status, HostStatus::Ok, "run {n}");
        assert!(
            r.render()
                .contains(&format!("svc {unit}.service: no systemctl: skipped")),
            "run {n}:\n{}",
            r.render()
        );
        nothing_changed(&r, &format!("no systemctl run {n}"));
        assert!(
            r.hosts[0]
                .ops
                .iter()
                .any(|o| o.desc == format!("svc {unit}"))
        );
    }

    // systemd installed by the pkg op; the package enables its unit, the
    // test turns that off so that run 1 has something to do.
    let r = go(&ctr, "install", pb_install);
    assert_eq!(r.exit_code(), 0, "install:\n{}", r.render());
    let (rc, out) = exec(&ctr, "test -d /run/systemd/system && echo booted");
    assert_ne!(rc, 0, "a plain container is not booted: {out}");
    let (rc, out) = exec(
        &ctr,
        &format!("systemctl disable {unit} >/dev/null 2>&1; systemctl is-enabled {unit}"),
    );
    assert_ne!(rc, 0, "unit {unit} still enabled: {out}");
    assert_eq!(out, "disabled");

    let r1 = go(&ctr, "svc", pb_svc);
    println!("{}", r1.render());
    assert_eq!(r1.exit_code(), 0, "run 1:\n{}", r1.render());
    assert_eq!(r1.hosts[0].status, HostStatus::Changed);
    assert!(
        r1.render().contains(&format!(
            "svc {unit}.service: will enable; no running systemd: start skipped"
        )),
        "{}",
        r1.render()
    );
    let (rc, out) = exec(&ctr, &format!("systemctl is-enabled {unit}"));
    assert_eq!((rc, out.as_str()), (0, "enabled"));

    let r2 = go(&ctr, "svc", pb_svc);
    println!("{}", r2.render());
    assert_eq!(r2.exit_code(), 0, "run 2:\n{}", r2.render());
    assert_eq!(r2.hosts[0].status, HostStatus::Ok);
    assert!(r2.hosts[0].will_create.is_empty(), "{}", r2.render());
    nothing_changed(&r2, "run 2");
}

#[test]
#[ignore = "needs docker"]
fn svc_ubuntu_enable_offline_then_unchanged() {
    twice("ubuntu:24.04", "cron");
}

#[test]
#[ignore = "needs docker"]
fn svc_rocky_enable_offline_then_unchanged() {
    twice("rockylinux/rockylinux:10", "crond");
}

fn pb_disable(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        h.svc(unit(&h)).disabled().stopped();
        Ok(())
    })
}

fn pb_mask(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        h.svc(unit(&h)).masked().stopped();
        Ok(())
    })
}

/// Disable and mask are unit-file operations, so they apply offline; stop
/// has no running systemd to talk to, so it is skipped and prepare says so.
fn states(image: &str, unit: &str) {
    let ctr = Container::start(image);
    let r = go(&ctr, "install", pb_install);
    assert_eq!(r.exit_code(), 0, "install:\n{}", r.render());
    let (rc, out) = exec(&ctr, "test -d /run/systemd/system && echo booted");
    assert_ne!(rc, 0, "a plain container is not booted: {out}");
    // the package enables its unit; make sure of it
    exec(&ctr, &format!("systemctl enable {unit} >/dev/null 2>&1"));
    let (rc, out) = exec(&ctr, &format!("systemctl is-enabled {unit}"));
    assert_eq!((rc, out.as_str()), (0, "enabled"));

    // disabled + stopped: disable applied offline, stop skipped.
    let r1 = go(&ctr, "disable", pb_disable);
    println!("{}", r1.render());
    assert_eq!(r1.exit_code(), 0, "disable run 1:\n{}", r1.render());
    assert_eq!(r1.hosts[0].status, HostStatus::Changed);
    assert!(
        r1.render().contains(&format!(
            "svc {unit}.service: will disable; no running systemd: stop skipped"
        )),
        "{}",
        r1.render()
    );
    let (rc, out) = exec(&ctr, &format!("systemctl is-enabled {unit}"));
    assert_ne!(rc, 0, "{unit} still enabled: {out}");
    assert_eq!(out, "disabled");
    let r2 = go(&ctr, "disable", pb_disable);
    println!("{}", r2.render());
    assert_eq!(r2.exit_code(), 0, "disable run 2:\n{}", r2.render());
    assert_eq!(r2.hosts[0].status, HostStatus::Ok);
    assert!(r2.hosts[0].will_create.is_empty(), "{}", r2.render());
    nothing_changed(&r2, "disable run 2");

    // masked + stopped: mask applied offline, stop skipped.
    let r3 = go(&ctr, "mask", pb_mask);
    println!("{}", r3.render());
    assert_eq!(r3.exit_code(), 0, "mask run 1:\n{}", r3.render());
    assert_eq!(r3.hosts[0].status, HostStatus::Changed);
    assert!(
        r3.render().contains(&format!(
            "svc {unit}.service: will mask; no running systemd: stop skipped"
        )),
        "{}",
        r3.render()
    );
    let (_, out) = exec(&ctr, &format!("systemctl is-enabled {unit}"));
    assert_eq!(out, "masked");
    let r4 = go(&ctr, "mask", pb_mask);
    println!("{}", r4.render());
    assert_eq!(r4.exit_code(), 0, "mask run 2:\n{}", r4.render());
    assert_eq!(r4.hosts[0].status, HostStatus::Ok);
    assert!(r4.hosts[0].will_create.is_empty(), "{}", r4.render());
    nothing_changed(&r4, "mask run 2");

    // enabled on a masked unit fails with systemctl's own message.
    let r5 = go(&ctr, "svc", pb_svc);
    assert_ne!(
        r5.exit_code(),
        0,
        "enable of a masked unit:\n{}",
        r5.render()
    );
}

#[test]
#[ignore = "needs docker"]
fn svc_ubuntu_disable_and_mask_offline() {
    states("ubuntu:24.04", "cron");
}

#[test]
#[ignore = "needs docker"]
fn svc_rocky_disable_and_mask_offline() {
    states("rockylinux/rockylinux:10", "crond");
}
