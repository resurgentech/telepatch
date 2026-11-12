//! Templates render when the script is built. A non-secret plan is the
//! rendered text. A template that formats a `Secret` is a secret file:
//! the plan, the log and `Debug` show `(secret)` and not the body.

use std::rc::Rc;

use askama::Template;
use serde::Deserialize;
use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::facts::os::Os;
use telepatch::report::HostStatus;
use telepatch::testing::{self, FixtureHost};
use telepatch::transport::local::LocalTransport;
use telepatch::vars::load;
use telepatch::{Host, Node, Playbook, PlaybookFuture, Secret, TraceError, role};

const TOKEN: &str = "t89-token-value";
const MARKER: &str = "marker-phrase";

#[derive(Template, Clone)]
#[template(source = "hello {{ name }}\n", ext = "txt")]
struct Hello {
    name: String,
}

#[derive(Template, Clone)]
#[template(source = "marker-phrase token={{ token }}\n", ext = "txt")]
struct Conf {
    token: Secret,
}

#[derive(Clone)]
struct Vars {
    path: String,
    token: Secret,
}

fn host() -> Host<()> {
    Host::new(
        "t",
        Os::parse("ID=debian\nVERSION_ID=12\n").unwrap(),
        Rc::new(()),
    )
}

fn scripts(h: &Host<()>, id: telepatch::NodeId) -> (String, String) {
    let Node::Op(op) = &h.snapshot().nodes[id.0] else {
        panic!("not an op");
    };
    (op.spec.script(&h.os), op.spec.exec_script(&h.os))
}

#[test]
fn a_template_without_a_secret_matches_the_same_bytes() {
    let h = host();
    let text = "hello world\n";
    let a = h
        .file("/etc/a.conf")
        .content(Hello {
            name: "world".into(),
        })
        .id();
    let b = h
        .file("/etc/b.conf")
        .content(text)
        .validate("visudo -cf %s")
        .id();
    let c = h
        .file("/etc/a.conf")
        .content(Hello {
            name: "world".into(),
        })
        .validate("visudo -cf %s")
        .id();
    let bare = h.file("/etc/c.conf").content(text).id();
    let (plain, exec) = scripts(&h, a);
    assert_eq!(plain, exec);
    let (bytes, _) = scripts(&h, bare);
    assert_eq!(plain.replace("/etc/a.conf", "/etc/c.conf"), bytes);
    let (bytes, _) = scripts(&h, b);
    let (tpl, _) = scripts(&h, c);
    assert_eq!(tpl.replace("/etc/a.conf", "/etc/b.conf"), bytes);
}

#[test]
fn a_secret_template_omits_the_body_and_marks_the_path() {
    let h = host();
    let id = h
        .file("/etc/telegraf/telegraf.conf")
        .content(Conf {
            token: Secret::literal(TOKEN),
        })
        .id();
    let rendered = Conf {
        token: Secret::literal(TOKEN),
    }
    .render()
    .unwrap();
    assert!(rendered.contains(MARKER) && rendered.contains("<secret>"));
    assert!(!rendered.contains(TOKEN));
    let (plan, exec) = scripts(&h, id);
    assert!(plan.contains("/etc/telegraf/telegraf.conf"), "{plan}");
    assert!(plan.contains("(secret)"), "{plan}");
    assert!(!plan.contains("tp_b="), "{plan}");
    assert!(!plan.contains(TOKEN) && !plan.contains(MARKER), "{plan}");
    let body = tp_body(&exec);
    assert!(
        body.contains(TOKEN) && body.contains(MARKER),
        "host body mismatch"
    );
    assert!(!exec.contains(TOKEN) && !exec.contains(MARKER));
    let debug = format!("{:?}", h.snapshot());
    assert!(!debug.contains(TOKEN), "{debug}");
    assert!(!debug.contains(MARKER), "{debug}");
    assert_eq!(Secret::literal(TOKEN).to_string(), "<secret>");

    let uid = h
        .unit("demo.service")
        .content(Conf {
            token: Secret::literal(TOKEN),
        })
        .id();
    let (uplan, uexec) = scripts(&h, uid);
    assert!(uplan.contains("(secret)"), "{uplan}");
    assert!(
        !uplan.contains("tp_b=") && !uplan.contains(TOKEN) && !uplan.contains(MARKER),
        "{uplan}"
    );
    let ubody = tp_body(&uexec);
    assert!(ubody.contains(TOKEN) && ubody.contains(MARKER));
    assert!(!uexec.contains(TOKEN) && !uexec.contains(MARKER));
}

/// The `tp_b='...'` payload, decoded. File bodies travel as base64.
fn tp_body(script: &str) -> String {
    let rest = script
        .split("tp_b='")
        .nth(1)
        .unwrap_or_else(|| panic!("no payload"));
    let b64 = rest.split('\'').next().unwrap();
    String::from_utf8(b64_decode(b64)).unwrap()
}

fn b64_decode(s: &str) -> Vec<u8> {
    fn val(c: u8) -> u8 {
        match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => 0,
        }
    }
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 4 <= bytes.len() {
        let n = (u32::from(val(bytes[i])) << 18)
            | (u32::from(val(bytes[i + 1])) << 12)
            | (u32::from(val(bytes[i + 2])) << 6)
            | u32::from(val(bytes[i + 3]));
        out.push((n >> 16) as u8);
        if bytes[i + 2] != b'=' {
            out.push((n >> 8) as u8);
        }
        if bytes[i + 3] != b'=' {
            out.push(n as u8);
        }
        i += 4;
    }
    out
}

#[test]
fn plan_does_not_resolve_or_print_a_template_secret() {
    fn entry(h: Host<Vars>) -> PlaybookFuture {
        Box::pin(async move {
            h.file("/etc/telegraf/telegraf.conf").content(Conf {
                token: h.vars.token.clone(),
            });
            Ok(())
        })
    }
    let token: Secret = load::<One>("v.toml", "pw = { env = \"TP_T89_UNSET_PLAN\" }\n", "")
        .unwrap()
        .pw;
    let pb = Playbook::new("plan-tpl", entry);
    let os = Os::fixture("ubuntu2404").unwrap();
    let plan = testing::plan(
        &pb,
        &FixtureHost::new(
            "h",
            os,
            Rc::new(Vars {
                path: String::new(),
                token,
            }),
        ),
    );
    assert!(plan.contains("(secret)"), "{plan}");
    assert!(!plan.contains(MARKER), "{plan}");
    assert!(!plan.contains("TP_T89_UNSET_PLAN"), "{plan}");
}

#[derive(Debug, Deserialize)]
struct One {
    pw: Secret,
}

#[role]
async fn telegraf(h: &Host<Vars>) -> Result<(), TraceError> {
    h.file(&h.vars.path).content(Conf {
        token: h.vars.token.clone(),
    });
    Ok(())
}

fn entry(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move { telegraf(&h).await })
}

fn run_vars(vars: Vars) -> (telepatch::report::Report, String) {
    let out = Rc::new(std::cell::RefCell::new(String::new()));
    let pb = Playbook::new("tpl", entry);
    let report = run(
        &pb,
        vec![HostSpec {
            groups: Vec::new(),
            name: "h1".into(),
            target: Target::Local(LocalTransport::new().uid(0)),
            vars,
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    (report, out.borrow().clone())
}

#[test]
fn a_missing_template_secret_names_the_var_and_the_role_before_the_file_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("telegraf.conf");
    let token: Secret = load::<One>("v.toml", "pw = { env = \"TP_T89_MISSING\" }\n", "")
        .unwrap()
        .pw;
    let (report, out) = run_vars(Vars {
        path: path.display().to_string(),
        token,
    });
    let HostStatus::Error(status) = &report.hosts[0].status else {
        panic!("wanted an error, got {:?}", report.hosts[0].status);
    };
    assert!(status.contains("pw"), "{status}");
    assert!(status.contains("TP_T89_MISSING"), "{status}");
    assert!(status.contains("(role telegraf)"), "{status}");
    assert!(!path.exists(), "file written before the miss");
    assert!(!out.contains(MARKER), "{out}");
}

#[test]
fn a_resolved_template_is_written_and_stays_out_of_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("telegraf.conf");
    let (report, out) = run_vars(Vars {
        path: path.display().to_string(),
        token: Secret::literal(TOKEN),
    });
    assert_eq!(report.hosts[0].status, HostStatus::Changed, "{out}");
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.contains(TOKEN) && body.contains(MARKER));
    assert!(out.contains("(secret)"), "{out}");
    assert!(!out.contains(TOKEN), "{out}");
    assert!(!out.contains(MARKER), "{out}");
    assert!(!report.render().contains(TOKEN), "{}", report.render());
    let (report2, out2) = run_vars(Vars {
        path: path.display().to_string(),
        token: Secret::literal(TOKEN),
    });
    assert_eq!(report2.hosts[0].status, HostStatus::Ok, "{out2}");
    assert!(!out2.contains(TOKEN) && !out2.contains(MARKER), "{out2}");
}

#[test]
fn missing_field_is_a_compile_error() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/templates/missing_field.rs");
}
