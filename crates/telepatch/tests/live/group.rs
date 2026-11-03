//! Live: the `group` primitive on ubuntu:24.04 and rockylinux/rockylinux:10
//! through the executor over `docker exec`. Run 1 creates a plain and a
//! system group (and leaves `root`, which exists, alone); run 2 changes
//! nothing. Run with `just test-live-user`.

use std::process::{Command, Stdio};

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::report::{HostStatus, Report};
use telepatch::transport::docker::DockerTransport;
use telepatch::{Host, Playbook, PlaybookFuture};

use crate::container::Container;

fn pb_run(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        h.group("tpgroup");
        h.group("tpsys").system();
        h.group("root");
        Ok(())
    })
}

fn go(ctr: &Container) -> Report {
    let pb = Playbook {
        name: "group",
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

/// `getent group n` in the container: the gid, or `None` when absent.
fn gid(ctr: &Container, name: &str) -> Option<u32> {
    let o = Command::new("docker")
        .args(["exec", &ctr.name, "getent", "group", name])
        .stdin(Stdio::null())
        .output()
        .expect("docker exec");
    if !o.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&o.stdout).into_owned();
    Some(line.trim().split(':').nth(2).unwrap().parse().unwrap())
}

fn twice(image: &str) {
    let ctr = Container::start(image);
    assert_eq!(gid(&ctr, "tpgroup"), None);
    assert_eq!(gid(&ctr, "tpsys"), None);
    let root_gid = gid(&ctr, "root").expect("root group");

    let r1 = go(&ctr);
    println!("{}", r1.render());
    assert_eq!(r1.exit_code(), 0, "run 1:\n{}", r1.render());
    assert_eq!(r1.hosts[0].status, HostStatus::Changed);
    assert!(r1.render().contains("group tpgroup: will create"));
    assert!(!r1.render().contains("group root: will create"));
    let plain = gid(&ctr, "tpgroup").expect("tpgroup created");
    let sys = gid(&ctr, "tpsys").expect("tpsys created");
    assert!(plain >= 1000, "plain group gid {plain}");
    assert!(sys < 1000, "system group gid {sys}");
    assert_eq!(gid(&ctr, "root"), Some(root_gid));

    let r2 = go(&ctr);
    println!("{}", r2.render());
    assert_eq!(r2.exit_code(), 0, "run 2:\n{}", r2.render());
    assert_eq!(r2.hosts[0].status, HostStatus::Ok);
    assert!(r2.hosts[0].ops.iter().any(|o| o.desc == "group tpgroup"));
    assert!(r2.hosts[0].will_create.is_empty());
    for o in &r2.hosts[0].ops {
        assert!(!o.changed, "run 2 changed {}: {}", o.id, o.desc);
    }
    assert_eq!(gid(&ctr, "tpgroup"), Some(plain));
    assert_eq!(gid(&ctr, "tpsys"), Some(sys));
}

#[test]
#[ignore = "needs docker"]
fn group_ubuntu_twice_unchanged() {
    twice("ubuntu:24.04");
}

#[test]
#[ignore = "needs docker"]
fn group_rocky_twice_unchanged() {
    twice("rockylinux/rockylinux:10");
}
