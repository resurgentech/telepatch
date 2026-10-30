//! A throwaway container for live tests: `docker run -d --rm <image> sleep
//! infinity`, removed with `docker rm -f` on drop.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static N: AtomicUsize = AtomicUsize::new(0);

pub struct Container {
    pub name: String,
}

impl Container {
    /// Start a container from `image` (pulls if missing). Panics on failure.
    pub fn start(image: &str) -> Container {
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
