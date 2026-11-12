//! Live tests: need the daemon of a tp-* VM (`just docker-vm <recipe>`; every
//! container start checks it) and the network to pull images. All `#[ignore]`
//! except the guard checks, which reach no daemon;
//! run with `just test-live-os`, `just test-live-pkg`, `just test-live-mise`,
//! `just test-live-user`, `just test-live-repo`, `just test-live-svc`,
//! `just test-live-unit`, `just test-live-fetch`, `just test-live-git`,
//! `just test-live-resource`, `just test-live-ssh`.

mod container;
mod fetch;
mod git;
mod group;
mod mise;
mod pkg;
mod repo;
mod resource;
mod ssh_exec;
mod sshd;
mod svc;
mod unit;
mod user;

use std::path::Path;

use container::Container;
use telepatch::facts::os::Os;
use telepatch::trace::Principal;
use telepatch::transport::docker::DockerTransport;
use telepatch::transport::{Transport, read_os};

const BAKE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../docker_images/docker-bake.hcl"
);

/// (distro, image) for every distro in the upstream bake file; the same
/// lines the `os-fixtures` recipe reads. Parsed at test time.
fn distros() -> Vec<(String, String)> {
    let text = std::fs::read_to_string(BAKE).expect("../docker_images/docker-bake.hcl");
    let mut out = vec![];
    for line in text.lines() {
        let Some((name, rest)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix("{ root = \"") else {
            continue;
        };
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        {
            continue;
        }
        let Some((image, _)) = rest.split_once('"') else {
            continue;
        };
        out.push((name.to_string(), image.to_string()));
    }
    out
}

fn fixture(name: &str) -> Os {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/os-release")
        .join(name);
    Os::parse(&std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())))
        .unwrap()
}

#[test]
fn bake_file_lists_thirteen_distros() {
    assert_eq!(distros().len(), 13, "{:?}", distros());
}

/// Start `image`, read `os` through the docker transport.
async fn read_through_docker(image: &str) -> Result<Os, String> {
    let img = image.to_string();
    let c = tokio::task::spawn_blocking(move || Container::start(&img))
        .await
        .map_err(|e| e.to_string())?;
    let t = DockerTransport::new();
    let mut s = t.open(&c.name, &Principal::Root, 1).await;
    if s.got.is_empty() {
        return Err(format!("no shell: {:?}", s.limit));
    }
    let mut sh = s.got.remove(0);
    let r = read_os(&mut sh).await;
    sh.close().await;
    r
}

#[test]
#[ignore = "needs docker"]
fn os_matches_fixture_for_every_upstream_image() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local = tokio::task::LocalSet::new();
    let bad: Vec<String> = rt.block_on(local.run_until(async {
        let tasks: Vec<_> = distros()
            .into_iter()
            .map(|(distro, image)| {
                tokio::task::spawn_local(async move {
                    let want = fixture(&distro);
                    match read_through_docker(&image).await {
                        Ok(os) if os == want => None,
                        Ok(os) => Some(format!("{distro} ({image}): got {os:?}, want {want:?}")),
                        Err(e) => Some(format!("{distro} ({image}): {e}")),
                    }
                })
            })
            .collect();
        let mut bad = vec![];
        for t in tasks {
            bad.extend(t.await.unwrap());
        }
        bad
    }));
    assert!(bad.is_empty(), "mismatches:\n{}", bad.join("\n"));
}

#[tokio::test]
async fn user_principal_is_not_supported_yet() {
    let s = DockerTransport::new()
        .open("x", &Principal::User("bob".into()), 1)
        .await;
    assert!(s.got.is_empty());
    assert!(
        matches!(s.limit, Some(telepatch::transport::Limit::Other(m)) if m.contains("not supported"))
    );
}

#[tokio::test]
async fn missing_container_is_a_limit_not_a_panic() {
    // A stub docker that answers as the daemon does for a missing container,
    // so this (not ignored) test never reaches a daemon.
    let d = tempfile::tempdir().unwrap();
    let stub = d.path().join("docker");
    std::fs::write(
        &stub,
        "#!/bin/sh\necho 'Error response from daemon: No such container: tp-test-no-such-container' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let s = DockerTransport::new()
        .docker(stub.to_str().unwrap())
        .open("tp-test-no-such-container", &Principal::Root, 1)
        .await;
    assert!(s.got.is_empty());
    assert!(
        matches!(&s.limit, Some(telepatch::transport::Limit::Other(m)) if m.contains("No such container")),
        "{:?}",
        s.limit
    );
}

// ---- the guard: docker only in a tp-* VM -------------------------------------

const VM_LABELS: &str = r#"["tp-docker-vm=tp-docker-x"]"#;

#[test]
fn docker_vm_check_refuses_without_tp_docker_vm_and_asks_no_daemon() {
    let e = container::docker_vm_check(None, || panic!("docker info must not run")).unwrap_err();
    assert!(
        e.starts_with("docker workloads run only in a tp-* VM: use `just docker-vm <recipe>`"),
        "{e}"
    );
    assert!(e.contains("TP_DOCKER_VM is not set"), "{e}");
    let e =
        container::docker_vm_check(Some(""), || panic!("docker info must not run")).unwrap_err();
    assert!(e.contains("TP_DOCKER_VM is not set"), "{e}");
    let e = container::docker_vm_check(Some("docker-x"), || panic!("docker info must not run"))
        .unwrap_err();
    assert!(e.contains("not a tp-* name"), "{e}");
}

#[test]
fn docker_vm_check_refuses_a_daemon_without_the_vm_label() {
    // this machine's daemon: no labels
    let e = container::docker_vm_check(Some("tp-docker-x"), || Ok("[]".into())).unwrap_err();
    assert!(e.contains("no label tp-docker-vm=tp-docker-x"), "{e}");
    // another VM's daemon
    let e = container::docker_vm_check(Some("tp-docker-x"), || {
        Ok(r#"["tp-docker-vm=tp-docker-xy"]"#.into())
    })
    .unwrap_err();
    assert!(e.contains("no label tp-docker-vm=tp-docker-x"), "{e}");
    // docker info failing
    let e =
        container::docker_vm_check(Some("tp-docker-x"), || Err("no daemon".into())).unwrap_err();
    assert!(e.contains("docker info failed: no daemon"), "{e}");
}

#[test]
fn docker_vm_check_passes_on_the_vm_label() {
    container::docker_vm_check(Some("tp-docker-x"), || Ok(VM_LABELS.into())).unwrap();
}

#[test]
fn sshd_publishes_on_the_vm_address() {
    assert_eq!(
        sshd::publish_arg(Some("192.168.122.17")).unwrap(),
        "192.168.122.17::22"
    );
    let e = sshd::publish_arg(None).unwrap_err();
    assert!(e.contains("TP_DOCKER_VM_IP is not set"), "{e}");
    assert!(sshd::publish_arg(Some("127.0.0.1")).is_err());
    assert!(sshd::publish_arg(Some("example.com")).is_err());
}
