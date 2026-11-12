//! Live OpenBao, only through `just docker-vm test-live-openbao`.
//!
//! The docker daemon is the VM's. This process is the client: it publishes
//! the dev server on `$TP_DOCKER_VM_PUBLISH:P`, P a port of `$TP_DOCKER_VM_PORTS`
//! (the first one not taken in the VM), copies `/usr/bin/bao` in with
//! `docker cp` (a bind mount would be resolved inside the VM), and talks to
//! `http://$TP_DOCKER_VM_ADDR:P`. On a remote VM host both addresses are
//! 127.0.0.1 and the ssh tunnel forwards the port. No capability is added.
//! The container is removed when the test ends.

use std::io::Read;
use std::net::{SocketAddr, TcpStream};
use std::process::Command;
use std::time::{Duration, Instant};

use telepatch::{SecretBackend, SecretError, SecretRef, SecretValue};
use telepatch_openbao::backend;

const NAME: &str = "tp-t91-openbao";
const V2: &str = "t91-v2-marker";
const V1: &str = "t91-v1-marker";

struct Remove;

impl Drop for Remove {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "-f", NAME]).output();
    }
}

#[test]
#[ignore]
fn live_openbao_v1_and_v2_from_a_dev_server() {
    let (publish, addr, first, last) = endpoint();
    let _vm = std::env::var("TP_DOCKER_VM").expect("TP_DOCKER_VM");
    clear_inherited();
    let token = fresh_token();
    set_env("BAO_TOKEN", &token);
    let _remove = Remove;

    // The bind happens at `docker start`; a port taken in the VM moves on to
    // the next one, anything else fails.
    let mut port = None;
    for p in first..=last {
        let _ = Command::new("docker").args(["rm", "-f", NAME]).output();
        let create = Command::new("docker")
            .args([
                "create",
                "--name",
                NAME,
                "-p",
                &format!("{publish}:{p}:8200"),
                "debian:13",
                "/bao",
                "server",
                "-dev",
                "-dev-listen-address=0.0.0.0:8200",
                "-dev-root-token-id",
                &token,
            ])
            .output()
            .expect("docker create");
        assert!(
            create.status.success(),
            "docker create exited {}",
            create.status
        );

        let cp = Command::new("docker")
            .args(["cp", "/usr/bin/bao", &format!("{NAME}:/bao")])
            .output()
            .expect("docker cp");
        assert!(cp.status.success(), "docker cp exited {}", cp.status);

        let start = Command::new("docker")
            .args(["start", NAME])
            .output()
            .expect("docker start");
        if start.status.success() {
            port = Some(p);
            break;
        }
        let err = String::from_utf8_lossy(&start.stderr);
        assert!(
            err.contains("port is already allocated") || err.contains("address already in use"),
            "docker start exited {} (no capability added): {err}",
            start.status
        );
    }
    let port = port.unwrap_or_else(|| {
        panic!("every port of TP_DOCKER_VM_PORTS={first}-{last} is taken in the VM")
    });
    if !running() {
        panic!("bao dev server is not running; not adding a capability");
    }

    let url = format!("http://{addr}:{port}");
    set_env("BAO_ADDR", &url);
    // Through the forward a connect succeeds before the server listens; the
    // retrying `bao` below is what waits for the server itself.
    wait_port(&addr, port);

    bao(&["secrets", "enable", "-path=scratch-v2", "kv"]);
    bao(&["secrets", "enable", "-path=scratch-v1", "-version=1", "kv"]);
    bao(&["kv", "put", "scratch-v2/app", &format!("field={V2}")]);
    bao(&["kv", "put", "scratch-v1/app", &format!("field={V1}")]);

    let got = backend().resolve(&[
        sref("scratch-v2/app", "field"),
        sref("scratch-v1/app", "field"),
    ]);
    assert!(is_value(&got[0], V2), "v2 field");
    assert!(is_value(&got[1], V1), "v1 field");
    println!("PASS openbao v2 and v1");
}

fn sref(path: &str, field: &str) -> SecretRef {
    SecretRef {
        scheme: "bao".into(),
        path: path.into(),
        field: field.into(),
        var: "live.field".into(),
        role: "live".into(),
    }
}

fn is_value(got: &Result<SecretValue, SecretError>, want: &str) -> bool {
    matches!(got, Ok(v) if v == &SecretValue::new(want))
}

fn clear_inherited() {
    let names: Vec<String> = std::env::vars()
        .map(|(k, _)| k)
        .filter(|k| k.starts_with("BAO_") || k.starts_with("VAULT_"))
        .collect();
    for name in names {
        unsafe { std::env::remove_var(name) }
    }
}

fn set_env(name: &str, value: &str) {
    unsafe { std::env::set_var(name, value) }
}

fn fresh_token() -> String {
    let mut bytes = [0u8; 16];
    let mut f = std::fs::File::open("/dev/urandom").unwrap();
    f.read_exact(&mut bytes).unwrap();
    let mut out = String::with_capacity(32);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn running() -> bool {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let out = Command::new("docker")
            .args(["inspect", "-f", "{{.State.Status}}", NAME])
            .output()
            .expect("docker inspect");
        let status = String::from_utf8_lossy(&out.stdout);
        if status.trim() == "running" {
            return true;
        }
        if status.trim() == "exited" || status.trim() == "dead" {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// (publish address, dial address, first port, last port) from
/// `TP_DOCKER_VM_PUBLISH`, `TP_DOCKER_VM_ADDR` and `TP_DOCKER_VM_PORTS`
/// (`first-last`, 1024-65535). The same rules as
/// `../telepatch/tests/live/endpoint.rs`, which a test of this crate cannot
/// import. A missing or malformed variable fails closed.
fn endpoint() -> (String, String, u16, u16) {
    const MSG: &str = "docker workloads run only in a tp-* VM: use `just docker-vm <recipe>`";
    let ip = |name: &str| {
        let v = std::env::var(name).unwrap_or_default();
        match v.parse::<std::net::Ipv4Addr>() {
            Ok(a) if !a.is_unspecified() => v,
            _ => panic!("{MSG} ({name}={v:?} is not set to an IPv4 address)"),
        }
    };
    let (publish, addr) = (ip("TP_DOCKER_VM_PUBLISH"), ip("TP_DOCKER_VM_ADDR"));
    let r = std::env::var("TP_DOCKER_VM_PORTS").unwrap_or_default();
    let port = |s: &str| {
        s.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| s.parse::<u16>().ok())
            .flatten()
            .filter(|p| *p >= 1024)
    };
    match r.split_once('-').map(|(f, l)| (port(f), port(l))) {
        Some((Some(f), Some(l))) if f <= l => (publish, addr, f, l),
        _ => panic!("{MSG} (TP_DOCKER_VM_PORTS={r:?} is not first-last, 1024-65535)"),
    }
}

fn wait_port(host: &str, port: u16) {
    let addr: SocketAddr = format!("{host}:{port}").parse().unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("dev server did not accept connections");
}

fn bao(args: &[&str]) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let out = Command::new("bao").args(args).output().expect("bao");
        if out.status.success() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("bao exited {}", out.status);
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}
