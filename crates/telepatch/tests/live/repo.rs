//! Live: the `repo` primitive on ubuntu:24.04 and rockylinux/rockylinux:10
//! through the executor over `docker exec`, with Docker's real repository
//! and `docker-ce-cli` installed from it in the same run (one window). On
//! Ubuntu the apt cache is refreshed by the engine because the repo op
//! wrote `/etc/apt/`; on Rocky dnf fetches the metadata of a repo it has no
//! cache for. Run 2 changes nothing and says nothing in prepare. Run with
//! `just test-live-repo`; needs docker and the network.

use std::process::{Command, Stdio};

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::facts::os::Family;
use telepatch::ops::repo::AptRepo;
use telepatch::report::{HostStatus, Report};
use telepatch::transport::docker::DockerTransport;
use telepatch::{Host, Playbook, PlaybookFuture, TraceError, role};

use crate::container::Container;

#[role]
async fn docker_repo(h: &Host<()>) -> Result<(), TraceError> {
    match h.os.family {
        Family::Debian => {
            // The image has neither curl nor CA certificates.
            h.pkg(["ca-certificates", "curl"]);
            h.repo("docker").apt(AptRepo {
                uris: vec!["https://download.docker.com/linux/ubuntu".into()],
                suites: vec![h.os.codename().unwrap_or("noble").to_string()],
                components: vec!["stable".into()],
                key_url: "https://download.docker.com/linux/ubuntu/gpg".into(),
            });
        }
        Family::RedHat => {
            h.repo("docker-ce")
                .dnf_url("https://download.docker.com/linux/rhel/docker-ce.repo");
        }
        _ => {}
    }
    h.pkg(["docker-ce-cli"]);
    Ok(())
}

fn pb_run(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move { docker_repo(&h).await })
}

fn go(ctr: &Container) -> Report {
    let pb = Playbook::new("repo", pb_run);
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

fn twice(image: &str, name: &str, files: &[&str], from_repo: &str) {
    let ctr = Container::start(image);
    assert!(!sh(&ctr, "command -v docker").0, "docker not preinstalled");

    let r1 = go(&ctr);
    println!("{}", r1.render());
    assert_eq!(r1.exit_code(), 0, "run 1:\n{}", r1.render());
    assert_eq!(r1.hosts[0].status, HostStatus::Changed);
    let text = r1.render();
    assert!(text.contains(&format!("repo {name}: will add")), "{text}");
    // One window: the repo op and both installs were prepared and applied
    // together.
    let h = &r1.hosts[0];
    assert!(h.will_create.iter().all(|c| c.window == 1), "{text}");
    assert!(h.will_install.iter().all(|w| w.window == 1), "{text}");
    assert!(
        h.ops.iter().any(|o| o.desc.starts_with("repo docker")),
        "{text}"
    );
    for f in files {
        assert!(sh(&ctr, &format!("test -s {f}")).0, "{f} written");
    }
    let (ok, out) = sh(&ctr, "docker --version");
    assert!(ok, "docker-ce-cli installed: {out}");
    assert!(out.starts_with("Docker version"), "{out}");
    assert!(
        sh(&ctr, from_repo).0,
        "docker-ce-cli came from Docker's repo"
    );

    let r2 = go(&ctr);
    println!("{}", r2.render());
    assert_eq!(r2.exit_code(), 0, "run 2:\n{}", r2.render());
    assert_eq!(r2.hosts[0].status, HostStatus::Ok);
    assert!(r2.hosts[0].will_create.is_empty());
    assert!(r2.hosts[0].ops.iter().any(|o| o.desc.starts_with("repo")));
    for o in &r2.hosts[0].ops {
        assert!(!o.changed, "run 2 changed {}: {}", o.id, o.desc);
    }
}

#[test]
#[ignore = "needs docker and network"]
fn repo_ubuntu_twice_unchanged() {
    twice(
        "ubuntu:24.04",
        "docker",
        &[
            "/etc/apt/keyrings/docker.asc",
            "/etc/apt/sources.list.d/docker.sources",
        ],
        "apt-cache policy docker-ce-cli | grep -q 'download.docker.com'",
    );
}

#[test]
#[ignore = "needs docker and network"]
fn repo_rocky_twice_unchanged() {
    twice(
        "rockylinux/rockylinux:10",
        "docker-ce",
        &["/etc/yum.repos.d/docker-ce.repo"],
        "dnf -q info --installed docker-ce-cli | grep -qi 'From repo *: *docker'",
    );
}
