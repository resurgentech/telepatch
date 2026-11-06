//! The docker transport runs `docker exec` with this process's environment,
//! so the `DOCKER_HOST` that `just docker-vm` exports (the forwarded socket of
//! a tp-* VM's daemon) is the daemon every live test's shell reaches. A stub
//! `docker` records what it was given and then runs a local bash, so no
//! daemon is involved. One test in its own binary, because it sets an
//! environment variable.

use telepatch::trace::Principal;
use telepatch::transport::Transport;
use telepatch::transport::docker::DockerTransport;

#[tokio::test]
async fn docker_host_reaches_the_docker_exec_of_the_transport() {
    let d = tempfile::tempdir().unwrap();
    let seen = d.path().join("seen");
    let stub = d.path().join("docker");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"${{DOCKER_HOST-<unset>}}\" \"$*\" > '{}'\nexec bash\n",
            seen.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    // SAFETY: the only test in this binary; nothing else reads the
    // environment concurrently.
    unsafe { std::env::set_var("DOCKER_HOST", "unix:///run/tp-docker-vm.test/docker.sock") };

    let mut s = DockerTransport::new()
        .docker(stub.to_str().unwrap())
        .container("h", "c1")
        .open("h", &Principal::Root, 1)
        .await;
    assert!(s.limit.is_none(), "{:?}", s.limit);
    s.got.remove(0).close().await;
    assert_eq!(
        std::fs::read_to_string(&seen).unwrap(),
        "unix:///run/tp-docker-vm.test/docker.sock\nexec -i c1 bash\n"
    );
}
