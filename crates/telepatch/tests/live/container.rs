//! A throwaway container for live tests: `docker run -d --rm <image> sleep
//! infinity`, removed with `docker rm -f` on drop. Only in a tp-* VM's daemon
//! (`just docker-vm <recipe>`): [`require_docker_vm`] runs first.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

pub const DOCKER_VM_MSG: &str =
    "docker workloads run only in a tp-* VM: use `just docker-vm <recipe>`";

/// The guard's decision, without side effects: `vm` is `TP_DOCKER_VM`, and
/// `labels` runs `docker info --format '{{json .Labels}}'`, only when `vm` is
/// a tp-* name. Passes only when the labels hold `tp-docker-vm=<vm>`, which
/// `docker-vm.sh` writes into the VM's daemon.json; this machine's daemon has
/// none. The same rule as `../tp_basic/scripts/lib/docker-guard.sh`.
pub fn docker_vm_check(
    vm: Option<&str>,
    labels: impl FnOnce() -> Result<String, String>,
) -> Result<(), String> {
    let vm = match vm {
        None | Some("") => return Err(format!("{DOCKER_VM_MSG} (TP_DOCKER_VM is not set)")),
        Some(v) if !v.starts_with("tp-") => {
            return Err(format!(
                "{DOCKER_VM_MSG} (TP_DOCKER_VM={v} is not a tp-* name)"
            ));
        }
        Some(v) => v,
    };
    let labels = labels().map_err(|e| format!("{DOCKER_VM_MSG} (docker info failed: {e})"))?;
    if labels.contains(&format!("\"tp-docker-vm={vm}\"")) {
        Ok(())
    } else {
        Err(format!(
            "{DOCKER_VM_MSG} (the daemon has no label tp-docker-vm={vm}; labels: {})",
            labels.trim()
        ))
    }
}

/// Panics unless the daemon `docker` reaches is the tp-* VM's that
/// `TP_DOCKER_VM` names. Called before every container a live test starts.
pub fn require_docker_vm() {
    let vm = std::env::var("TP_DOCKER_VM").ok();
    let r = docker_vm_check(vm.as_deref(), || {
        let out = Command::new("docker")
            .args(["info", "--format", "{{json .Labels}}"])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    });
    if let Err(e) = r {
        panic!("{e}");
    }
}

pub struct Container {
    pub name: String,
}

impl Container {
    /// Start a container from `image` (pulls if missing) in the tp-* VM's
    /// daemon. Panics on failure, or outside `just docker-vm`.
    pub fn start(image: &str) -> Container {
        require_docker_vm();
        let name = format!(
            "tp-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        );
        let out = Command::new("docker")
            .args([
                "run", "-d", "--rm", "--name", &name, image, "sleep", "infinity",
            ])
            .stdin(Stdio::null())
            .output()
            .expect("running docker");
        assert!(
            out.status.success(),
            "docker run {image}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Container { name }
    }
}

impl Drop for Container {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}
