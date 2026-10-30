//! Live package playbook through the real executor over `docker exec`:
//! two packages, a dnf group (EL) or an apt build-dep (Ubuntu), then a
//! re-run that must change nothing. Run with `just test-live-pkg`.

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
