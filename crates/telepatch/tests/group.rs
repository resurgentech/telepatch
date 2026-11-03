//! The `group` primitive: template run against fake `getent` / `groupadd`
//! on PATH, wrapped-script and plan snapshots, the root-only trace error,
//! and the prepare line through the real executor over the local transport.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::facts::os::Os;
use telepatch::plan::render_host;
use telepatch::report::{HostStatus, Report};
use telepatch::trailer::{GroupResult, render_op, split_stream};
use telepatch::transport::local::LocalTransport;
use telepatch::{Host, Key, Node, NodeId, Playbook, PlaybookFuture, Value};

const NONCE: &str = "t0n";

fn os() -> Os {
    Os::parse("ID=debian\nVERSION_ID=12\n").unwrap()
}

fn host() -> Host<()> {
    Host::new("t", os(), Rc::new(()))
}

fn wrapped(h: &Host<()>, id: NodeId) -> String {
    match &h.snapshot().nodes[id.0] {
        Node::Op(op) => render_op(op, id.0, NONCE, &h.os),
        _ => panic!("not an op"),
    }
}

/// A `bin` dir with fake `getent` (reads `<d>/groups`, one name per line)
/// and `groupadd` (logs its arguments to `<d>/log`, appends the last one to
/// `<d>/groups`).
fn stubs(d: &Path) -> PathBuf {
    let bin = d.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let w = |name: &str, body: &str| {
        let p = bin.join(name);
        std::fs::write(&p, format!("#!/bin/bash\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    };
    let g = d.join("groups");
    let l = d.join("log");
    w(
        "getent",
        &format!(
            "[ \"$1\" = group ] || exit 3\ngrep -qx -- \"$2\" {g} 2>/dev/null && echo \"$2:x:1:\" || exit 2",
            g = g.display()
        ),
    );
    w(
        "groupadd",
        &format!(
            "echo \"groupadd $*\" >> {l}\nif grep -qx -- \"${{@: -1}}\" {g} 2>/dev/null; then exit 9; fi\necho \"${{@: -1}}\" >> {g}",
            l = l.display(),
            g = g.display()
        ),
    );
    bin
}

fn run_script(script: &str, d: &Path, bin: &Path) -> (String, GroupResult) {
    let out = Command::new("bash")
        .arg("-c")
        .arg(script)
        .current_dir(d)
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .output()
        .unwrap();
    let stream = String::from_utf8(out.stdout).unwrap();
    let mut v = split_stream(&stream, NONCE);
    assert_eq!(v.len(), 1, "stream: {stream:?}");
    let (o, t) = v.remove(0);
    (o, t.typed::<GroupResult>().unwrap())
}

fn log(d: &Path) -> String {
    std::fs::read_to_string(d.join("log")).unwrap_or_default()
}

#[test]
fn group_absent_is_created_then_present_changes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    std::fs::write(d.path().join("groups"), "other\n").unwrap();
    let h = host();
    let id = h.group("docker").id();
    let (_, r) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert_eq!(
        r,
        GroupResult {
            rc: 0,
            changed: true,
            created: true
        }
    );
    assert_eq!(log(d.path()), "groupadd -- docker\n");
    let (_, r) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert_eq!(
        r,
        GroupResult {
            rc: 0,
            changed: false,
            created: false
        }
    );
    assert_eq!(log(d.path()).lines().count(), 1, "no second groupadd");
}

#[test]
fn group_system_passes_dash_r_and_failure_is_rc() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let h = host();
    let id = h.group("docker").system().id();
    let (_, r) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert_eq!((r.rc, r.changed, r.created), (0, true, true));
    assert_eq!(log(d.path()), "groupadd -r -- docker\n");
    // groupadd failing: rc is its status, nothing created.
    std::fs::remove_file(d.path().join("groups")).unwrap();
    std::fs::write(
        d.path().join("bin/groupadd"),
        "#!/bin/bash\necho nope >&2\nexit 9\n",
    )
    .unwrap();
    let (_, r) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert_eq!((r.rc, r.changed, r.created), (9, false, false));
}

#[test]
fn group_name_is_quoted() {
    let d = tempfile::tempdir().unwrap();
    let bin = stubs(d.path());
    let h = host();
    let id = h.group("we ird;x").id();
    let (_, r) = run_script(&wrapped(&h, id), d.path(), &bin);
    assert!(r.created);
    assert_eq!(log(d.path()), "groupadd -- we ird;x\n");
}

#[test]
fn group_template_snapshots() {
    let h = host();
    let a = h.group("docker").id();
    let b = h.group("libvirt").system().id();
    insta::assert_snapshot!(format!("{}\n{}", wrapped(&h, a), wrapped(&h, b)));
}

#[test]
fn group_plan_shows_effects() {
    let h = host();
    h.group("docker");
    h.group("kvm").system();
    h.as_user("root").group("login-root");
    insta::assert_snapshot!(render_host("h", &h.os, &h.snapshot()));
}

#[test]
fn group_effects_are_the_group_key() {
    let h = host();
    let g = h.group("docker");
    let snap = h.snapshot();
    let Node::Op(op) = &snap.nodes[g.id().0] else {
        panic!()
    };
    assert_eq!(op.spec.kind(), "group");
    assert_eq!(op.writes, [Key::group("docker")].into());
    assert_eq!(
        op.post,
        [(Key::group("docker"), Value::Exists(true))].into()
    );
    assert!(snap.errors.is_empty());
}

#[test]
fn group_is_root_only() {
    let h = host();
    h.as_user("deploy").group("docker");
    let errs = h.snapshot().errors;
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(
        errs[0].to_string().contains("group") && errs[0].to_string().contains("root"),
        "{}",
        errs[0]
    );
    let ok = host();
    ok.as_user("root").group("docker");
    ok.group("docker");
    assert!(ok.snapshot().errors.is_empty());
}

// --- through the executor ---

fn go(pb: &Playbook<PathBuf>, d: &Path) -> (Report, String) {
    let bin = stubs(d);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let out = Rc::new(RefCell::new(String::new()));
    let report = run(
        pb,
        vec![HostSpec {
            name: "h1".into(),
            target: Target::Local(LocalTransport::new().uid(0).env("PATH", &path)),
            vars: d.to_path_buf(),
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    let text = out.borrow().clone();
    (report, text)
}

#[test]
fn group_executor_prepare_line_then_changed_0() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.group("docker");
            h.group("docker");
            h.group("kvm").system();
            h.group("have");
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    stubs(d.path());
    std::fs::write(d.path().join("groups"), "have\n").unwrap();
    let pb = Playbook {
        name: "group",
        run: run_,
    };
    let (r1, out1) = go(&pb, d.path());
    assert_eq!(
        r1.hosts[0].status,
        HostStatus::Changed,
        "{}\n{out1}",
        r1.render()
    );
    assert!(
        out1.contains("== h1 [prepare] group docker: will create =="),
        "{out1}"
    );
    assert!(
        out1.contains("== h1 [prepare] group kvm: will create =="),
        "{out1}"
    );
    assert_eq!(
        out1.matches("group docker: will create").count(),
        1,
        "named once: {out1}"
    );
    assert!(!out1.contains("group have: will create"), "{out1}");
    assert!(
        r1.render().contains("h1: group docker: will create"),
        "{}",
        r1.render()
    );
    assert_eq!(r1.hosts[0].will_create.len(), 2);
    assert!(out1.contains("group docker"), "executor label: {out1}");
    assert_eq!(
        log(d.path()),
        "groupadd -- docker\ngroupadd -r -- kvm\n",
        "second docker op found it present"
    );

    let (r2, out2) = go(&pb, d.path());
    assert_eq!(r2.hosts[0].status, HostStatus::Ok, "{}", r2.render());
    assert!(r2.hosts[0].ops.iter().all(|o| !o.changed));
    assert!(r2.hosts[0].will_create.is_empty());
    assert!(!out2.contains("[prepare]"), "{out2}");
    assert_eq!(log(d.path()).lines().count(), 2, "nothing ran again");
    assert!(
        r2.hosts[0].ops.iter().any(|o| o.desc == "group docker"),
        "{:?}",
        r2.hosts[0].ops
    );
}

#[test]
fn group_will_create_is_a_json_event() {
    fn run_(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move {
            h.group("docker");
            Ok(())
        })
    }
    let d = tempfile::tempdir().unwrap();
    stubs(d.path());
    let pb = Playbook {
        name: "group",
        run: run_,
    };
    let bin = d.path().join("bin");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let log_path = d.path().join("events.jsonl");
    let out = Rc::new(RefCell::new(String::new()));
    run(
        &pb,
        vec![HostSpec {
            name: "h1".into(),
            target: Target::Local(LocalTransport::new().uid(0).env("PATH", &path)),
            vars: d.path().to_path_buf(),
        }],
        RunOpts {
            log: Some(log_path.clone()),
            stdout: Stdout::Capture(out),
        },
    );
    let text = std::fs::read_to_string(&log_path).unwrap();
    let ev = text
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|e| e["ev"] == "will_create")
        .unwrap_or_else(|| panic!("no will_create event:\n{text}"));
    assert_eq!(ev["kind"], "group");
    assert_eq!(ev["name"], "docker");
    assert_eq!(ev["host"], "h1");
}
