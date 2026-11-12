//! Live OpenBao, only through `just docker-vm test-live-openbao`.
//!
//! The docker daemon is the VM's. This process is the client: it publishes
//! the dev server on `$TP_DOCKER_VM_IP`, copies `/usr/bin/bao` in with
//! `docker cp` (a bind mount would be resolved inside the VM), and talks to
//! `http://$TP_DOCKER_VM_IP:<port>`. No capability is added. The container
//! is removed when the test ends.

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
    let ip = std::env::var("TP_DOCKER_VM_IP").expect("TP_DOCKER_VM_IP");
    let _vm = std::env::var("TP_DOCKER_VM").expect("TP_DOCKER_VM");
    clear_inherited();
    let token = fresh_token();
    set_env("BAO_TOKEN", &token);
    let _remove = Remove;
    let _ = Command::new("docker").args(["rm", "-f", NAME]).output();

    let create = Command::new("docker")
        .args([
            "create",
            "--name",
            NAME,
            "-p",
            &format!("{ip}::8200"),
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
    assert!(
        start.status.success(),
        "docker start exited {} (no capability added)",
        start.status
    );
    if !running() {
        panic!("bao dev server is not running; not adding a capability");
    }

    let port = published_port();
    let addr = format!("http://{ip}:{port}");
    set_env("BAO_ADDR", &addr);
    wait_port(&ip, port);

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

fn published_port() -> u16 {
    let out = Command::new("docker")
        .args(["port", NAME, "8200/tcp"])
        .output()
        .expect("docker port");
    assert!(out.status.success(), "docker port exited {}", out.status);
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find_map(|line| line.rsplit(':').next().and_then(|p| p.trim().parse().ok()))
        .expect("published port")
}

fn wait_port(ip: &str, port: u16) {
    let addr: SocketAddr = format!("{ip}:{port}").parse().unwrap();
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
