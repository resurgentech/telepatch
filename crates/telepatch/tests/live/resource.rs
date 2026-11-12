//! Live: the decided `Terminfo` resource on ubuntu:24.04. Run 1 compiles an
//! entry with `tic`. Run 2 changes nothing. `just docker-vm test-live-resource`.

use std::cell::RefCell;
use std::process::{Command, Stdio};
use std::rc::Rc;

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::report::{HostStatus, Report};
use telepatch::transport::docker::DockerTransport;
use telepatch::{Host, Playbook, PlaybookFuture, Terminfo};

use crate::container::Container;

const ENTRY: &str = "xterm-ghostty|ghostty,\n\tam, cols#80, lines#24,\n";

#[derive(Clone)]
struct V {
    source: String,
}

fn compile(h: Host<V>) -> PlaybookFuture {
    Box::pin(async move {
        h.resource(Terminfo {
            name: "xterm-ghostty".into(),
            source: h.vars.source.clone(),
        });
        Ok(())
    })
}

fn go(ctr: &Container, v: &V) -> (Report, String) {
    let pb = Playbook::new("terminfo", compile);
    let out = Rc::new(RefCell::new(String::new()));
    let r = run(
        &pb,
        vec![HostSpec {
            groups: Vec::new(),
            name: "c".into(),
            target: Target::Docker(DockerTransport::new().container("c", &ctr.name)),
            vars: v.clone(),
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    let text = out.borrow().clone();
    println!("{text}\n{}", r.render());
    (r, text)
}

fn exec(ctr: &Container, script: &str) -> (i32, String, String) {
    let o = Command::new("docker")
        .args(["exec", &ctr.name, "bash", "-lc", script])
        .stdin(Stdio::null())
        .output()
        .expect("docker exec");
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).trim().to_string(),
        String::from_utf8_lossy(&o.stderr).trim().to_string(),
    )
}

#[test]
#[ignore = "needs docker and network"]
fn resource_ubuntu_terminfo() {
    let ctr = Container::start("ubuntu:24.04");
    let (rc, o, e) = exec(
        &ctr,
        "export DEBIAN_FRONTEND=noninteractive; apt-get update && apt-get install -y ncurses-bin",
    );
    assert_eq!(rc, 0, "install ncurses-bin: {o}\n{e}");
    let vars = V {
        source: ENTRY.into(),
    };
    let (r1, out1) = go(&ctr, &vars);
    assert_eq!(r1.exit_code(), 0, "run 1:\n{out1}\n{}", r1.render());
    assert_eq!(r1.hosts[0].status, HostStatus::Changed, "{}", r1.render());
    assert!(
        out1.contains("terminfo xterm-ghostty: will compile"),
        "{out1}"
    );
    assert!(
        r1.hosts[0]
            .resources
            .iter()
            .any(|r| r.kind == "terminfo" && r.name == "xterm-ghostty" && r.irreversible),
        "{:?}",
        r1.hosts[0].resources
    );
    let (rc, dumped, e) = exec(&ctr, "infocmp -x xterm-ghostty");
    assert_eq!(rc, 0, "infocmp: {dumped}\n{e}");
    assert!(dumped.contains("xterm-ghostty"), "{dumped}");

    let (r2, out2) = go(&ctr, &vars);
    assert_eq!(r2.exit_code(), 0, "run 2:\n{out2}\n{}", r2.render());
    assert_eq!(r2.hosts[0].status, HostStatus::Ok, "{}", r2.render());
    assert!(!out2.contains("will compile"), "{out2}");
    assert!(!out2.contains("will replace"), "{out2}");
}
