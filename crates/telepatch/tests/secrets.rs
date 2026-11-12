//! `Secret`: references, redaction, prepare-time resolve, and `plan` leaving
//! the store alone.

use std::rc::Rc;

use serde::Deserialize;
use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::report::HostStatus;
use telepatch::testing::{self, FixtureHost};
use telepatch::transport::local::LocalTransport;
use telepatch::vars::{load, secret_literals};
use telepatch::{Host, Playbook, PlaybookFuture, Secret, TraceError, role};

#[derive(Debug, Deserialize)]
struct One {
    pw: Secret,
}

#[derive(Debug, Default, Deserialize)]
struct Admin {
    #[serde(default)]
    default_password: Option<Secret>,
}

#[derive(Debug, Default, Deserialize)]
struct Vars {
    #[serde(default)]
    admin_users: Admin,
}

fn lit(toml: &str) -> Secret {
    toml::from_str::<One>(&format!("pw = {toml}")).unwrap().pw
}

#[test]
fn secret_accepts_a_literal_and_each_reference() {
    let literal = lit(r#""keep-out""#);
    assert_eq!(literal.to_string(), "<secret>");
    assert_eq!(format!("{literal:?}"), "<secret>");
    assert!(!format!("{literal:?}").contains("keep-out"));
    assert!(!literal.to_string().contains("keep-out"));

    let env = lit(r#"{ env = "TP_T88_ENV" }"#);
    let file = lit(r#"{ file = "/tmp/tp-t88" }"#);
    let prompt = lit(r#"{ prompt = "Password: " }"#);
    let bao = lit(r#"{ bao = "kv/infra/telegraf", field = "token" }"#);
    for s in [&env, &file, &prompt, &bao] {
        assert_eq!(s.to_string(), "<secret>");
        assert_eq!(format!("{s:?}"), "<secret>");
    }
    let back = toml::to_string(&bao).unwrap();
    assert!(back.contains("bao"), "{back}");
    assert!(back.contains("kv/infra/telegraf"), "{back}");
    assert!(back.contains("token"), "{back}");
    assert!(!back.contains("keep-out"));
}

#[test]
fn a_literal_is_not_serialized() {
    let literal = lit(r#""keep-out-serial""#);
    let err = toml::to_string(&literal).unwrap_err().to_string();
    assert!(err.contains("literal"), "{err}");
    assert!(!err.contains("keep-out-serial"), "{err}");
}

#[test]
fn secret_literals_names_the_field_and_ignores_references() {
    let dir = tempfile::tempdir().unwrap();
    let literal = dir.path().join("lit.toml");
    let reference = dir.path().join("ref.toml");
    std::fs::write(&literal, "pw = \"keep-out\"\n").unwrap();
    std::fs::write(&reference, "pw = { env = \"TP_T88_NONE\" }\n").unwrap();
    let hits = secret_literals::<One>(&[literal.clone(), reference]).unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].contains("pw"), "{hits:?}");
    assert!(hits[0].contains(literal.to_str().unwrap()), "{hits:?}");
    assert!(!hits[0].contains("keep-out"), "{hits:?}");
}

#[test]
fn plan_does_not_resolve() {
    let secret: Secret = load::<One>("v.toml", "pw = { env = \"TP_T88_UNSET_PLAN\" }\n", "")
        .unwrap()
        .pw;
    fn entry(h: Host<Secret>) -> PlaybookFuture {
        Box::pin(async move {
            h.user("tpdev").password_hash(&h.vars);
            Ok(())
        })
    }
    let pb = Playbook::new("plan-secret", entry);
    let os = telepatch::facts::os::Os::fixture("ubuntu2404").unwrap();
    let host = FixtureHost::new("h", os, Rc::new(secret));
    let plan = testing::plan(&pb, &host);
    assert!(plan.contains("<secret>"), "{plan}");
    assert!(plan.contains("sha256sum"), "{plan}");
    assert!(!plan.contains("TP_T88_UNSET_PLAN"), "{plan}");
}

#[role]
async fn admin_users(h: &Host<Vars>) -> Result<(), TraceError> {
    if let Some(pw) = &h.vars.admin_users.default_password {
        h.user("tpdev").password_hash(pw);
    }
    Ok(())
}

fn entry(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move { admin_users(&h).await })
}

fn run_vars(vars: Vars, d: &std::path::Path) -> (telepatch::report::Report, String) {
    let bin = d.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let getent = r#"#!/bin/bash
case "$1" in
  passwd) exit 2;;
  shadow) exit 2;;
  *) exit 3;;
esac
"#;
    let useradd = r#"#!/bin/bash
echo "useradd $*" >> "$TP_D/log"
mkdir -p "$TP_D/users"
touch "$TP_D/users/$2" 2>/dev/null || true
name=
while [ $# -gt 0 ]; do
  case "$1" in
    --) name=$2; break;;
    *) shift;;
  esac
done
echo "$name" > "$TP_D/created"
"#;
    std::fs::write(bin.join("getent"), getent).unwrap();
    std::fs::write(bin.join("useradd"), useradd).unwrap();
    std::fs::write(
        bin.join("chpasswd"),
        "#!/bin/bash\necho chpasswd >> \"$TP_D/log\"\ncat >> \"$TP_D/shadow\"\n",
    )
    .unwrap();
    for n in ["getent", "useradd", "chpasswd"] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin.join(n), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let out = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    let pb = Playbook::new("secrets", entry);
    let report = run(
        &pb,
        vec![HostSpec {
            groups: Vec::new(),
            name: "h1".into(),
            target: Target::Local(
                LocalTransport::new()
                    .uid(0)
                    .env("PATH", &path)
                    .env("TP_D", &d.display().to_string()),
            ),
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
fn a_missing_env_names_the_var_and_the_role_before_changes() {
    let vars: Vars = load(
        "v.toml",
        "[admin_users]\ndefault_password = { env = \"TP_T88_MISSING\" }\n",
        "",
    )
    .unwrap();
    let d = tempfile::tempdir().unwrap();
    let (report, out) = run_vars(vars, d.path());
    let HostStatus::Error(status) = &report.hosts[0].status else {
        panic!("wanted an error, got {:?}\n{out}", report.hosts[0].status);
    };
    assert!(status.contains("admin_users.default_password"), "{status}");
    assert!(status.contains("TP_T88_MISSING"), "{status}");
    assert!(status.contains("(role admin_users)"), "{status}");
    assert!(
        !d.path().join("created").exists(),
        "useradd ran before the miss"
    );
}

#[test]
fn an_unknown_scheme_names_the_scheme() {
    let vars: Vars = load(
        "v.toml",
        "[admin_users]\ndefault_password = { bao = \"kv/x\", field = \"token\" }\n",
        "",
    )
    .unwrap();
    let d = tempfile::tempdir().unwrap();
    let (report, out) = run_vars(vars, d.path());
    let HostStatus::Error(status) = &report.hosts[0].status else {
        panic!("wanted an error, got {:?}\n{out}", report.hosts[0].status);
    };
    assert!(
        status.contains("unknown scheme") && status.contains("bao"),
        "{status}\n{out}"
    );
    assert!(status.contains("admin_users.default_password"), "{status}");
    assert!(status.contains("(role admin_users)"), "{status}");
}
