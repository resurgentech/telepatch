//! Author-defined resources and `cmd!`. A resource op is planned from a
//! recorded probe, keeps `OpSpec`'s `Eq` by kind, key and rendered scripts,
//! and never merges. No inverse counts as irreversible.

use std::process::Command;
use std::rc::Rc;

use telepatch::facts::os::Os;
use telepatch::report::HostStatus;
use telepatch::testing::{FixtureHost, plan};
use telepatch::{
    Change, Host, Key, Node, OpSpec, Playbook, PlaybookFuture, Probe, Probed, Resource, Script,
    Terminfo, cmd, plan_against,
};

fn ubuntu() -> Os {
    Os::parse("ID=ubuntu\nVERSION_ID=24.04\n").unwrap()
}

fn host() -> Host<()> {
    Host::new("t", ubuntu(), Rc::new(()))
}

fn spec_of(h: &Host<()>) -> OpSpec {
    match &h.snapshot().nodes[0] {
        Node::Op(op) => op.spec.clone(),
        _ => panic!("not an op"),
    }
}

/// Same scripts, different plan. `Eq` must ignore the plan closure.
struct Echo {
    name: &'static str,
    verb: &'static str,
    missing: bool,
    undo: bool,
}

impl Resource for Echo {
    const KIND: &'static str = "echo";

    fn key(&self) -> Key {
        Key::custom(Self::KIND, self.name)
    }

    fn probe(&self) -> Probe {
        let p = Probe::stdout(cmd!("true"));
        if self.missing { p.missing_is_none() } else { p }
    }

    fn plan(&self, now: &Probed) -> Change {
        match now.stdout() {
            None => Change::will(self.verb),
            Some(_) => Change::None,
        }
    }

    fn apply(&self) -> Script {
        cmd!("true")
    }

    fn inverse(&self) -> Option<Script> {
        if self.undo {
            Some(cmd!("touch /tmp/tp-t97-inverse-must-not-run"))
        } else {
            None
        }
    }
}

/// Probe exits non-zero and that is a failure, not "missing".
struct BadProbe;

impl Resource for BadProbe {
    const KIND: &'static str = "bad";

    fn key(&self) -> Key {
        Key::custom(Self::KIND, "x")
    }

    fn probe(&self) -> Probe {
        Probe::stdout(cmd!("false"))
    }

    fn plan(&self, _: &Probed) -> Change {
        Change::None
    }

    fn apply(&self) -> Script {
        cmd!("true")
    }
}

/// Missing on the host, then a successful apply. No inverse.
struct Mark;

impl Resource for Mark {
    const KIND: &'static str = "mark";

    fn key(&self) -> Key {
        Key::custom(Self::KIND, "n")
    }

    fn probe(&self) -> Probe {
        Probe::stdout(cmd!("false")).missing_is_none()
    }

    fn plan(&self, now: &Probed) -> Change {
        match now.stdout() {
            None => Change::will("compile"),
            Some(_) => Change::None,
        }
    }

    fn apply(&self) -> Script {
        cmd!("true")
    }
}

const ENTRY: &str = "xterm-ghostty|ghostty,\n\tam, cols#80, lines#24,\n";

#[test]
fn cmd_quotes_like_sh_and_is_a_script() {
    let name = "a b";
    let s = cmd!("infocmp -x {name}");
    assert_eq!(s.body(), "infocmp -x 'a b'");
    let s = cmd!("infocmp -x {}", "a b");
    assert_eq!(s.body(), "infocmp -x 'a b'");
    let tricky = "it's";
    let s = cmd!("echo {}", tricky);
    assert_eq!(s.body(), "echo 'it'\\''s'");
    let s = cmd!("printf {{}}");
    assert_eq!(s.body(), "printf {}");
    let s = cmd!("tic -x -");
    assert_eq!(s.render(), "tic -x -");
    let s = cmd!("tic -x -").stdin("hello");
    let rendered = s.render();
    assert!(rendered.contains("base64 -d"), "{rendered}");
    assert!(rendered.contains("tic -x -"), "{rendered}");
    assert!(rendered.contains("aGVsbG8="), "{rendered}");
    assert!(!rendered.contains("hello"), "{rendered}");
}

#[test]
fn plan_against_recorded_probe_output() {
    let t = Terminfo {
        name: "xterm-ghostty".into(),
        source: ENTRY.into(),
    };
    assert_eq!(plan_against(&t, None), Change::will("compile"));
    assert_eq!(plan_against(&t, Some(ENTRY)), Change::None);
    assert_eq!(
        plan_against(&t, Some("xterm-ghostty|other,\n")),
        Change::will("replace")
    );
    let dumped = "# Reconstructed via infocmp from file: /tmp/db\n\
        xterm-ghostty|ghostty,\n\
        \tam,\n\
        \tcols#80, lines#24,\n";
    assert_eq!(plan_against(&t, Some(dumped)), Change::None);
    let other = "# Reconstructed via infocmp from file: /tmp/db\n\
        xterm-ghostty|ghostty,\n\
        \tam,\n\
        \tcols#100, lines#24,\n";
    assert_eq!(plan_against(&t, Some(other)), Change::will("replace"));
}

#[test]
fn terminfo_compiled_on_this_machine_plans_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");
    std::fs::create_dir(&db).unwrap();
    let src = dir.path().join("entry.ti");
    std::fs::write(&src, ENTRY).unwrap();
    let compiled = Command::new("tic")
        .args(["-x", "-o"])
        .arg(&db)
        .arg(&src)
        .output()
        .expect("tic");
    assert!(
        compiled.status.success(),
        "tic: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let dumped = Command::new("infocmp")
        .env("TERMINFO", &db)
        .args(["-x", "xterm-ghostty"])
        .output()
        .expect("infocmp");
    assert!(
        dumped.status.success(),
        "infocmp: {}",
        String::from_utf8_lossy(&dumped.stderr)
    );
    let text = String::from_utf8_lossy(&dumped.stdout);
    let t = Terminfo {
        name: "xterm-ghostty".into(),
        source: ENTRY.into(),
    };
    assert_eq!(
        plan_against(&t, Some(text.trim_end_matches('\n'))),
        Change::None,
        "{text}"
    );
    assert_ne!(
        text.trim(),
        ENTRY.trim(),
        "infocmp reflows; raw equality is not the check"
    );
}

#[test]
fn specs_compare_kind_key_and_scripts_not_the_plan() {
    let h = host();
    h.resource(Echo {
        name: "n",
        verb: "compile",
        missing: true,
        undo: false,
    });
    let a = spec_of(&h);
    let h = host();
    h.resource(Echo {
        name: "n",
        verb: "replace",
        missing: true,
        undo: false,
    });
    let b = spec_of(&h);
    assert_eq!(a, b);
    let shown = format!("{a:?}");
    assert!(shown.contains("echo"), "{shown}");
    assert!(shown.contains("n"), "{shown}");
    assert!(shown.contains("true"), "{shown}");
    assert!(!shown.contains("plan"), "{shown}");
    let Node::Op(op) = &host_node(&h) else {
        panic!("not an op");
    };
    assert!(op.writes.contains(&Key::custom("echo", "n")));
    assert_eq!(Key::custom("echo", "n").to_string(), "echo:n");
    assert!(!Key::custom("echo", "n").overlaps(&Key::file("/tmp/x")));

    let h = host();
    h.resource(Echo {
        name: "n",
        verb: "compile",
        missing: true,
        undo: true,
    });
    assert_ne!(spec_of(&h), a, "an inverse script is part of the op");
}

fn host_node(h: &Host<()>) -> Node {
    h.snapshot().nodes[0].clone()
}

fn guarded(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        let g = h.raw("true");
        h.resource(Echo {
            name: "n",
            verb: "compile",
            missing: true,
            undo: false,
        })
        .when(g.ok());
        h.resource(Echo {
            name: "n",
            verb: "replace",
            missing: true,
            undo: false,
        })
        .when(g.changed());
        Ok(())
    })
}

#[test]
fn resources_never_merge() {
    let text = plan(
        &Playbook::new("guarded", guarded),
        &FixtureHost::new("t", ubuntu(), Rc::new(())),
    );
    assert!(
        !text.contains("merged from"),
        "equal guarded resources must stay two ops:\n{text}"
    );
    let lines: Vec<&str> = text.lines().filter(|l| l.contains(" echo ")).collect();
    assert_eq!(lines.len(), 2, "{text}");
}

fn mark_play(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        h.resource(Mark);
        Ok(())
    })
}

fn bad_play(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        h.file("/tmp/tp-t97-resource-block").content("nope");
        h.resource(BadProbe);
        Ok(())
    })
}

fn go(
    name: &'static str,
    f: fn(Host<()>) -> PlaybookFuture,
) -> (telepatch::report::Report, String) {
    let pb = Playbook::new(name, f);
    let out = Rc::new(std::cell::RefCell::new(String::new()));
    let report = telepatch::executor::run(
        &pb,
        vec![telepatch::executor::HostSpec {
            name: "h".into(),
            target: telepatch::executor::Target::Local(
                telepatch::transport::local::LocalTransport::new().uid(0),
            ),
            vars: (),
        }],
        telepatch::executor::RunOpts {
            log: None,
            stdout: telepatch::executor::Stdout::Capture(out.clone()),
        },
    );
    (report, out.borrow().clone())
}

#[test]
fn missing_probe_compiles_and_no_inverse_is_irreversible() {
    let _ = std::fs::remove_file("/tmp/tp-t97-inverse-must-not-run");
    let (r, text) = go("mark", mark_play);
    assert_eq!(r.exit_code(), 0, "{text}\n{}", r.render());
    assert_eq!(
        r.hosts[0].status,
        HostStatus::Changed,
        "{text}\n{}",
        r.render()
    );
    assert!(text.contains("mark n: will compile"), "{text}");
    assert_eq!(r.hosts[0].resources.len(), 1, "{:?}", r.hosts[0].resources);
    assert!(r.hosts[0].resources[0].irreversible);
    assert_eq!(r.hosts[0].resources[0].kind, "mark");
    assert!(
        !std::path::Path::new("/tmp/tp-t97-inverse-must-not-run").exists(),
        "inverse is recorded, not run"
    );
}

#[test]
fn probe_failure_blocks_before_the_window_changes() {
    let path = std::path::Path::new("/tmp/tp-t97-resource-block");
    let _ = std::fs::remove_file(path);
    let (r, text) = go("bad", bad_play);
    assert!(
        matches!(r.hosts[0].status, HostStatus::Blocked(_)),
        "{text}\n{}",
        r.render()
    );
    let why = match &r.hosts[0].status {
        HostStatus::Blocked(s) => s.clone(),
        other => format!("{other:?}"),
    };
    assert!(
        why.contains("bad") && why.contains("probe"),
        "{why}\n{text}"
    );
    assert!(
        !path.exists(),
        "a failed probe applies nothing in the window"
    );
}
