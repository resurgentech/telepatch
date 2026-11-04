//! Live tests: need docker (and the network to pull images). All `#[ignore]`;
//! run with `just test-live-os`, `just test-live-pkg`, `just test-live-mise`,
//! `just test-live-user`,
//! `just test-live-ssh`.

mod container;
mod group;
mod mise;
mod pkg;
mod ssh_exec;
mod sshd;
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
    // Fails whether or not docker exists: either way no shell and a Limit.
    let s = DockerTransport::new()
        .open("tp-test-no-such-container", &Principal::Root, 1)
        .await;
    assert!(s.got.is_empty());
    assert!(s.limit.is_some());
}
