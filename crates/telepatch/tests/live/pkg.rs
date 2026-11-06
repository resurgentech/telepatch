//! Live package playbook through the real executor over `docker exec`:
//! two packages, a dnf group (EL) or an apt build-dep (Ubuntu), then a
//! re-run that must change nothing. Run with `just test-live-pkg`.

use std::process::{Command, Stdio};

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::facts::os::Family;
use telepatch::report::{HostStatus, Report};
use telepatch::transport::docker::DockerTransport;
use telepatch::{Host, Playbook, PlaybookFuture, TraceError, role, sh};

use crate::container::Container;

const SOURCES: &str = "/etc/apt/sources.list.d/ubuntu.sources";

#[role]
async fn tools(h: &Host<()>) -> Result<(), TraceError> {
    h.pkg(["jq", "tree"]);
    Ok(())
}

#[role]
async fn extras(h: &Host<()>) -> Result<(), TraceError> {
    match h.os.family {
        Family::RedHat => {
            // A small group; `@Development Tools` would take minutes.
            h.pkg(["@Console Internet Tools"]);
        }
        Family::Debian => {
            sh!(
                h,
                "sed -i 's/^Types: deb$/Types: deb deb-src/' {SOURCES}",
                unless = "grep -q '^Types: deb deb-src' {SOURCES}"
            );
            h.pkg(["hello"]).build_dep();
        }
        _ => {}
    }
    Ok(())
}

fn pb_run(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        tools(&h).await?;
        extras(&h).await
    })
}

fn go(ctr: &Container) -> Report {
    let pb = Playbook {
        name: "pkg",
        run: pb_run,
    };
    run(
        &pb,
        vec![HostSpec {
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

fn twice(image: &str) {
    let ctr = Container::start(image);
    let r1 = go(&ctr);
    println!("{}", r1.render());
    assert_eq!(r1.exit_code(), 0, "run 1:\n{}", r1.render());
    assert_eq!(r1.hosts[0].status, HostStatus::Changed);
    assert!(r1.hosts[0].ops.iter().any(|o| o.changed));
    let r2 = go(&ctr);
    println!("{}", r2.render());
    assert_eq!(r2.exit_code(), 0, "run 2:\n{}", r2.render());
    assert!(!r2.hosts[0].ops.is_empty());
    for o in &r2.hosts[0].ops {
        assert!(!o.changed, "run 2 changed {}: {}", o.id, o.desc);
    }
    assert_eq!(r2.hosts[0].status, HostStatus::Ok);
}

#[test]
#[ignore = "needs docker and network"]
fn pkg_ubuntu_twice_unchanged() {
    twice("ubuntu:24.04");
}

#[test]
#[ignore = "needs docker and network"]
fn pkg_rocky_twice_unchanged() {
    twice("rockylinux/rockylinux:10");
}

// ---- `.pin(v)` and `.latest()` on the distro manager (T79) ----

/// What a versioned playbook wants of `tree`: `Some(v)` pins, `None` is
/// `.latest()`.
struct Want {
    pin: Option<String>,
}

#[role]
async fn tree_versioned(h: &Host<Want>) -> Result<(), TraceError> {
    match &h.vars.pin {
        Some(v) => h.pkg(["tree"]).pin(v.clone()),
        None => h.pkg(["tree"]).latest(),
    };
    Ok(())
}

fn versioned_run(h: Host<Want>) -> PlaybookFuture {
    Box::pin(async move { tree_versioned(&h).await })
}

fn go_versioned(ctr: &Container, pin: Option<&str>) -> Report {
    let pb = Playbook {
        name: "pkg-versioned",
        run: versioned_run,
    };
    run(
        &pb,
        vec![HostSpec {
            name: "c".into(),
            target: Target::Docker(DockerTransport::new().container("c", &ctr.name)),
            vars: Want {
                pin: pin.map(str::to_string),
            },
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Inherit,
        },
    )
}

/// `docker exec <ctr> sh -c script`: (success, trimmed stdout).
fn exec(ctr: &Container, script: &str) -> (bool, String) {
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

/// The version the container's own repos hold for `tree`, in the form the
/// manager takes as a pin (apt: the dpkg version; dnf: `V-R`; pacman: the
/// repo version). Refreshes the container's package metadata first.
fn repo_version(ctr: &Container, refresh_and_query: &str) -> String {
    let (ok, v) = exec(ctr, refresh_and_query);
    assert!(ok && !v.is_empty(), "no repo version for tree: {v:?}");
    v
}

const APT_QUERY: &str = "apt-get -q update >/dev/null 2>&1; LC_ALL=C apt-cache policy tree | sed -n 's/^  Candidate: //p'";
const DNF_QUERY: &str =
    "dnf -q repoquery --latest-limit=1 --qf '%{version}-%{release}\\n' tree 2>/dev/null | head -n1";
const PACMAN_QUERY: &str = "pacman -Sy --noconfirm >/dev/null 2>&1; pacman -Si tree | sed -n 's/^Version *: //p' | head -n1";

fn assert_unchanged(r: &Report, what: &str) {
    println!("{}", r.render());
    assert_eq!(r.exit_code(), 0, "{what}:\n{}", r.render());
    assert!(!r.hosts[0].ops.is_empty());
    for o in &r.hosts[0].ops {
        assert!(!o.changed, "{what} changed {}: {}", o.id, o.desc);
    }
    assert_eq!(r.hosts[0].status, HostStatus::Ok);
}

/// Pin at the repo's current version: run 1 installs, run 2 changes
/// nothing, and the host's version is the pin. Then `.latest()` on the
/// now-installed package changes nothing either.
fn pin_then_latest(image: &str, query: &str, installed_version: &str) {
    let ctr = Container::start(image);
    let v = repo_version(&ctr, query);
    let r1 = go_versioned(&ctr, Some(&v));
    println!("{}", r1.render());
    assert_eq!(r1.exit_code(), 0, "pin run 1:\n{}", r1.render());
    assert_eq!(r1.hosts[0].status, HostStatus::Changed);
    assert!(r1.hosts[0].ops.iter().any(|o| o.changed));
    assert_eq!(exec(&ctr, installed_version).1, v, "installed version");
    assert_unchanged(&go_versioned(&ctr, Some(&v)), "pin run 2");
    assert_unchanged(&go_versioned(&ctr, None), "latest on an installed tree");
}

/// `.latest()` on a fresh container installs; run 2 changes nothing.
fn latest_twice(image: &str, refresh: &str) {
    let ctr = Container::start(image);
    if !refresh.is_empty() {
        assert!(exec(&ctr, refresh).0, "refresh");
    }
    let r1 = go_versioned(&ctr, None);
    println!("{}", r1.render());
    assert_eq!(r1.exit_code(), 0, "latest run 1:\n{}", r1.render());
    assert_eq!(r1.hosts[0].status, HostStatus::Changed);
    assert_unchanged(&go_versioned(&ctr, None), "latest run 2");
}

const DPKG_VERSION: &str = "dpkg-query -W -f='${Version}' tree";
const RPM_VERSION: &str = "rpm -q --qf '%{VERSION}-%{RELEASE}' tree";
const PACMAN_VERSION: &str = "pacman -Q tree | cut -d' ' -f2";

#[test]
#[ignore = "needs docker and network"]
fn pkg_pin_ubuntu() {
    pin_then_latest("ubuntu:24.04", APT_QUERY, DPKG_VERSION);
}

#[test]
#[ignore = "needs docker and network"]
fn pkg_pin_rocky() {
    pin_then_latest("rockylinux/rockylinux:10", DNF_QUERY, RPM_VERSION);
}

#[test]
#[ignore = "needs docker and network"]
fn pkg_pin_arch() {
    pin_then_latest("archlinux:latest", PACMAN_QUERY, PACMAN_VERSION);
}

#[test]
#[ignore = "needs docker and network"]
fn pkg_latest_ubuntu() {
    latest_twice("ubuntu:24.04", "");
}

#[test]
#[ignore = "needs docker and network"]
fn pkg_latest_rocky() {
    latest_twice("rockylinux/rockylinux:10", "");
}

#[test]
#[ignore = "needs docker and network"]
fn pkg_latest_arch() {
    // The image ships without sync databases.
    latest_twice("archlinux:latest", "pacman -Sy --noconfirm");
}

/// Arch holds one version per package: a pin that is not the repo's blocks
/// the host in prepare, naming the package, the pin and the repo version,
/// with nothing installed.
#[test]
#[ignore = "needs docker and network"]
fn pkg_pin_arch_wrong_version_blocks_in_prepare() {
    let ctr = Container::start("archlinux:latest");
    let repo = repo_version(&ctr, PACMAN_QUERY);
    let r = go_versioned(&ctr, Some("0.0.1-1"));
    println!("{}", r.render());
    let HostStatus::Blocked(why) = &r.hosts[0].status else {
        panic!("expected Blocked, got {:?}", r.hosts[0].status);
    };
    assert!(why.contains("tree"), "{why}");
    assert!(why.contains("0.0.1-1"), "{why}");
    assert!(why.contains(&repo), "{why}");
    assert!(r.hosts[0].ops.iter().all(|o| !o.changed), "nothing applied");
    assert!(
        !exec(&ctr, "pacman -Q tree").0,
        "tree must not be installed"
    );
}
