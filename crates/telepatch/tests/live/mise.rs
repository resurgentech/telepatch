//! Live: the `mise` source's system layer in a fresh ubuntu:24.04 container
//! as root. mise comes from the T41 `InstallerScript`; `curl` and
//! `ca-certificates` (which that installer needs) come from an `h.pkg` op in
//! the same playbook. Run with `just test-live-mise`.

use std::process::{Command, Stdio};

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::ops::pkg::{InstallerScript, Latest};
use telepatch::report::{HostStatus, Report};
use telepatch::transport::docker::DockerTransport;
use telepatch::{Host, Playbook, PlaybookFuture};

use crate::container::Container;

#[derive(Clone, Copy)]
enum Mode {
    Present,
    Pin,
    Latest,
}

fn pb_run(h: Host<Mode>) -> PlaybookFuture {
    Box::pin(async move {
        h.pkg(["curl", "ca-certificates"]);
        h.pkg(["mise"]).installer_script(InstallerScript::new()
                .bin("/usr/local/bin/mise")
                .version("mise version")
                .latest(Latest::github_release("jdx/mise"))
                .install("(set -o pipefail && curl -fsSL https://mise.run | MISE_INSTALL_PATH=/usr/local/bin/mise sh)")
                .update("mise self-update -y"));
        match *h.vars {
            Mode::Present => h.pkg(["just"]).mise(),
            Mode::Pin => h.pkg(["just"]).mise().pin("1.36.0").clone(),
            Mode::Latest => h.pkg(["just"]).mise().latest().clone(),
        };
        Ok(())
    })
}

fn go(ctr: &Container, mode: Mode) -> Report {
    let pb = Playbook::new("mise", pb_run);
    run(
        &pb,
        vec![HostSpec {
            groups: Vec::new(),
            name: "c".into(),
            target: Target::Docker(DockerTransport::new().container("c", &ctr.name)),
            vars: mode,
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Inherit,
        },
    )
}

/// `bash -c cmd` in the container as root: (exit code, stdout+stderr).
fn dx(ctr: &Container, cmd: &str) -> (i32, String) {
    let o = Command::new("docker")
        .args(["exec", &ctr.name, "bash", "-c", cmd])
        .stdin(Stdio::null())
        .output()
        .expect("docker exec");
    let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&o.stderr));
    (o.status.code().unwrap_or(-1), s)
}

fn docker(args: &[&str]) {
    let st = Command::new("docker")
        .args(args)
        .stdin(Stdio::null())
        .status()
        .expect("docker");
    assert!(st.success(), "docker {args:?}");
}

/// `just --version` the way a plain user gets it: only the shims dir added
/// to `PATH`, no `MISE_*` (stderr dropped: `nobody` has no writable home, so
/// mise warns about its cache).
fn nobody_just(ctr: &Container) -> String {
    let (rc, out) = dx(
        ctr,
        "su - nobody -s /bin/bash -c 'PATH=/usr/local/share/mise/shims:$PATH just --version' 2>/dev/null",
    );
    assert_eq!(rc, 0, "nobody just --version: {out}");
    out.trim().to_string()
}

fn mise_op_changed(r: &Report) -> bool {
    r.hosts[0]
        .ops
        .iter()
        .any(|o| o.desc.contains("pkg[mise]") && o.changed)
}

fn unchanged(r: &Report, what: &str) {
    println!("{}", r.render());
    assert_eq!(r.exit_code(), 0, "{what}:\n{}", r.render());
    assert_eq!(r.hosts[0].status, HostStatus::Ok, "{what}");
    assert!(r.hosts[0].ops.iter().any(|o| o.desc.contains("pkg[mise]")));
    for o in &r.hosts[0].ops {
        assert!(!o.changed, "{what} changed {}: {}", o.id, o.desc);
    }
}

#[test]
#[ignore = "needs docker and network"]
fn mise_system_layer() {
    let ctr = Container::start("ubuntu:24.04");

    // Run 1: everything is installed.
    let r1 = go(&ctr, Mode::Present);
    println!("{}", r1.render());
    assert_eq!(r1.exit_code(), 0, "run 1:\n{}", r1.render());
    assert_eq!(r1.hosts[0].status, HostStatus::Changed);
    assert!(mise_op_changed(&r1), "run 1 did not change the mise op");

    // config.toml root-owned and holding just = "latest"; a system shim.
    let (rc, out) = dx(
        &ctr,
        "stat -c '%U:%G' /etc/mise/config.toml; cat /etc/mise/config.toml",
    );
    println!("{out}");
    assert_eq!(rc, 0);
    assert!(out.starts_with("root:root\n"), "{out}");
    assert!(out.lines().any(|l| l == "just = \"latest\""), "{out}");
    let (rc, _) = dx(&ctr, "test -L /usr/local/share/mise/shims/just");
    assert_eq!(rc, 0, "no shim in /usr/local/share/mise/shims/just");
    // Nothing went to root's own mise dir; no sudo anywhere to call.
    let (rc, out) = dx(
        &ctr,
        "test ! -e /root/.local/share/mise/installs && ! command -v sudo",
    );
    assert_eq!(rc, 0, "root's own installs or sudo present: {out}");
    let v = nobody_just(&ctr);
    println!("nobody: {v}");
    assert!(v.starts_with("just "), "{v}");
    let latest = v.clone();

    // Run 2: changed 0. Cache wiped and the network cut first, so a Present
    // probe or template that reached out would fail or change something.
    dx(&ctr, "rm -rf /root/.cache/mise");
    docker(&["network", "disconnect", "bridge", &ctr.name]);
    let (_, out) = dx(
        &ctr,
        "curl -sS -m 5 -o /dev/null https://mise.jdx.dev 2>&1; echo rc=$?",
    );
    println!("network check: {out}");
    assert!(!out.contains("rc=0"), "network still up: {out}");
    let (_, mtime0) = dx(&ctr, "stat -c %y /etc/mise/config.toml");
    let r2 = go(&ctr, Mode::Present);
    unchanged(&r2, "run 2 (network cut)");
    let (_, mtime1) = dx(&ctr, "stat -c %y /etc/mise/config.toml");
    assert_eq!(mtime0, mtime1, "config.toml rewritten on run 2");
    docker(&["network", "connect", "bridge", &ctr.name]);

    // Pin: installs 1.36.0 and the shim now runs it.
    let r3 = go(&ctr, Mode::Pin);
    println!("{}", r3.render());
    assert_eq!(r3.exit_code(), 0, "pin run:\n{}", r3.render());
    assert!(mise_op_changed(&r3));
    assert_eq!(nobody_just(&ctr), "just 1.36.0");
    unchanged(&go(&ctr, Mode::Pin), "pin rerun");

    // Latest: moves it back.
    let r4 = go(&ctr, Mode::Latest);
    println!("{}", r4.render());
    assert_eq!(r4.exit_code(), 0, "latest run:\n{}", r4.render());
    assert!(mise_op_changed(&r4));
    assert_eq!(nobody_just(&ctr), latest);
    unchanged(&go(&ctr, Mode::Latest), "latest rerun");
    let (_, out) = dx(&ctr, "cat /etc/mise/config.toml");
    assert!(out.lines().any(|l| l == "just = \"latest\""), "{out}");
}
