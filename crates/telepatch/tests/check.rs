//! `run --check` and `--diff`: trace and prepare, apply nothing; exit 0
//! nothing to change, 2 changes pending, 1 a host blocked; a `[coverage]`
//! line per role; a stop at the first implicit barrier.
//!
//! Hosts are this machine through the local transport with the uid
//! injected as 0 (no sudo), working in temp dirs.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use askama::Template;
use telepatch::cli::run_with_args;
use telepatch::executor::{CheckOpts, HostSpec, RunOpts, Stdout, Target, run_check};
use telepatch::facts::os::Os;
use telepatch::report::{HostStatus, Report};
use telepatch::transport::local::LocalTransport;
use telepatch::transport::ssh::SshTransport;
use telepatch::{Host, Node, Playbook, PlaybookFuture, Secret, TraceError, role, sh};

fn p(h: &Host<PathBuf>, name: &str) -> String {
    h.vars.join(name).to_str().unwrap().to_string()
}

fn check(pb: &Playbook<PathBuf>, dir: &Path, diff: bool) -> (Report, String) {
    check_vars(pb, dir.to_path_buf(), diff)
}

fn check_vars<V: 'static>(pb: &Playbook<V>, vars: V, diff: bool) -> (Report, String) {
    let out = Rc::new(RefCell::new(String::new()));
    let report = run_check(
        pb,
        vec![HostSpec {
            name: "h1".into(),
            target: Target::Local(LocalTransport::new().uid(0)),
            vars,
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
        CheckOpts { diff },
    );
    let text = format!("{}{}", out.borrow(), report.render());
    (report, text)
}

fn scrub(s: &str, dir: &Path) -> String {
    s.replace(dir.to_str().unwrap(), "$TMP")
}

// ---- playbooks ----

#[role]
async fn site(h: &Host<PathBuf>) -> Result<(), TraceError> {
    let x = p(h, "x.conf");
    let y = p(h, "y.conf");
    h.file(&x).content("hello\n");
    h.file(&y).line("A=1");
    Ok(())
}

fn site_entry(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move { site(&h).await })
}

fn site_pb() -> Playbook<PathBuf> {
    Playbook::new("site", site_entry)
}

#[role]
async fn demo(h: &Host<PathBuf>) -> Result<(), TraceError> {
    let x = p(h, "x.conf");
    h.file(&x).content("hello\n");
    h.group("tpcheck_demo_group");
    sh!(h, "echo building");
    Ok(())
}

#[role]
async fn quiet(h: &Host<PathBuf>) -> Result<(), TraceError> {
    let x = p(h, "q.conf");
    h.file(&x).content("q\n");
    Ok(())
}

fn demo_entry(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        demo(&h).await?;
        quiet(&h).await
    })
}

#[role]
async fn early(h: &Host<PathBuf>) -> Result<(), TraceError> {
    let made = p(h, "made");
    sh!(h, "echo built > {made}");
    let there = h.exists(&made).await; // BARRIER-READ
    if there {
        h.file(&p(h, "after.conf")).content("after\n");
    }
    Ok(())
}

fn barrier_entry(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move { early(&h).await })
}

#[role]
async fn asserts(h: &Host<PathBuf>) -> Result<(), TraceError> {
    h.assert(false, "the host is not what this playbook wants");
    h.file(&p(h, "never")).content("x\n");
    Ok(())
}

fn assert_entry(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move { asserts(&h).await })
}

#[role]
async fn creates_user(h: &Host<PathBuf>) -> Result<(), TraceError> {
    h.user("tpcheck_nobody_x");
    h.as_user("tpcheck_nobody_x")
        .file(&p(h, "later.conf"))
        .content("later\n");
    Ok(())
}

fn user_entry(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move { creates_user(&h).await })
}

// ---- what a check does ----

#[test]
fn check_applies_nothing_and_exits_2_with_changes_pending() {
    let d = tempfile::tempdir().unwrap();
    let (report, out) = check(&site_pb(), d.path(), false);
    let out = scrub(&out, d.path());
    assert_eq!(report.exit_code(), 2, "{out}");
    assert!(
        out.contains("== h1 [prepare] file $TMP/x.conf: will create =="),
        "{out}"
    );
    assert!(
        out.contains("== h1 [prepare] file $TMP/y.conf: will create =="),
        "{out}"
    );
    assert!(out.contains("2 will change"), "{out}");
    assert!(!d.path().join("x.conf").exists(), "a check wrote x.conf");
    assert!(!d.path().join("y.conf").exists(), "a check wrote y.conf");
    assert_eq!(report.hosts[0].status, HostStatus::Changed);
}

#[test]
fn a_converged_host_is_exit_0() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("x.conf"), "hello\n").unwrap();
    std::fs::write(d.path().join("y.conf"), "A=1\n").unwrap();
    let (report, out) = check(&site_pb(), d.path(), true);
    let out = scrub(&out, d.path());
    assert_eq!(report.exit_code(), 0, "{out}");
    assert!(out.contains("0 will change"), "{out}");
    assert!(
        !out.contains("will create") && !out.contains("will change ("),
        "{out}"
    );
    assert_eq!(report.hosts[0].status, HostStatus::Ok);
}

#[test]
fn diff_shows_what_a_file_would_become() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("x.conf"), "a\nb\nc\n").unwrap();
    std::fs::write(d.path().join("y.conf"), "A=1\n").unwrap();
    #[role]
    async fn r(h: &Host<PathBuf>) -> Result<(), TraceError> {
        h.file(&p(h, "x.conf")).content("a\nB\nc\n");
        h.file(&p(h, "y.conf")).line("A=1");
        Ok(())
    }
    fn e(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move { r(&h).await })
    }
    let pb = Playbook::new("d", e);
    let (report, out) = check(&pb, d.path(), true);
    let out = scrub(&out, d.path());
    assert_eq!(report.exit_code(), 2, "{out}");
    assert!(
        out.contains("== h1 [prepare] file $TMP/x.conf: will change =="),
        "{out}"
    );
    assert!(out.contains("h1 | @@ -1,3 +1,3 @@\n"), "{out}");
    assert!(
        out.contains("h1 |  a\nh1 | -b\nh1 | +B\nh1 |  c\n"),
        "{out}"
    );
    assert!(
        !out.contains("y.conf"),
        "an unchanged file is not listed: {out}"
    );
    assert_eq!(
        std::fs::read_to_string(d.path().join("x.conf")).unwrap(),
        "a\nb\nc\n",
        "a check rewrote the file"
    );
    // Without --diff the line is there and the hunk is not.
    let (_, plain) = check(&pb, d.path(), false);
    assert!(plain.contains("will change"), "{plain}");
    assert!(!plain.contains("@@"), "{plain}");
}

#[test]
fn diff_of_a_line_edit_and_of_a_new_file() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("y.conf"), "B=2\n").unwrap();
    let (_, out) = check(&site_pb(), d.path(), true);
    // y.conf gains its line; x.conf is new: its whole content is added.
    assert!(out.contains("h1 | +A=1\n"), "{out}");
    assert!(out.contains("h1 | @@ -0,0 +1 @@\nh1 | +hello\n"), "{out}");
}

#[test]
fn mode_and_absent_are_listed() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("m.conf"), "m\n").unwrap();
    std::fs::set_permissions(
        d.path().join("m.conf"),
        std::os::unix::fs::PermissionsExt::from_mode(0o644),
    )
    .unwrap();
    std::fs::write(d.path().join("gone"), "g\n").unwrap();
    #[role]
    async fn r(h: &Host<PathBuf>) -> Result<(), TraceError> {
        h.file(&p(h, "m.conf")).content("m\n").mode(0o600);
        h.file(&p(h, "gone")).absent();
        Ok(())
    }
    fn e(h: Host<PathBuf>) -> PlaybookFuture {
        Box::pin(async move { r(&h).await })
    }
    let (report, out) = check(&Playbook::new("m", e), d.path(), true);
    let out = scrub(&out, d.path());
    assert_eq!(report.exit_code(), 2, "{out}");
    assert!(
        out.contains("file $TMP/m.conf: will change (mode 0644 -> 0600)"),
        "{out}"
    );
    assert!(out.contains("file $TMP/gone: will remove"), "{out}");
    assert!(d.path().join("gone").exists());
}

#[test]
fn diff_needs_check() {
    let (out, code) = go(&["run", "site", "--local", "--diff"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("--check"), "{out}");
}

// ---- secrets ----

#[derive(Template, Clone)]
#[template(source = "marker-phrase token={{ token }}\n", ext = "txt")]
struct Conf {
    token: Secret,
}

#[derive(Clone)]
struct SecretVars {
    path: String,
    token: Secret,
}

#[role]
async fn telegraf(h: &Host<SecretVars>) -> Result<(), TraceError> {
    h.file(&h.vars.path).content(Conf {
        token: h.vars.token.clone(),
    });
    Ok(())
}

fn secret_entry(h: Host<SecretVars>) -> PlaybookFuture {
    Box::pin(async move { telegraf(&h).await })
}

#[test]
fn a_secret_file_says_will_change_and_shows_no_diff() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("telegraf.conf");
    std::fs::write(&path, "marker-phrase token=old-value\n").unwrap();
    let pb = Playbook::new("tpl", secret_entry);
    let (report, out) = check_vars(
        &pb,
        SecretVars {
            path: path.display().to_string(),
            token: Secret::literal("new-token-value"),
        },
        true,
    );
    let out = scrub(&out, d.path());
    assert_eq!(report.exit_code(), 2, "{out}");
    assert!(
        out.contains("file $TMP/telegraf.conf (secret): will change"),
        "{out}"
    );
    for leak in ["new-token-value", "old-value", "marker-phrase", "@@"] {
        assert!(!out.contains(leak), "{leak} in:\n{out}");
    }
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "marker-phrase token=old-value\n"
    );
    // The same secret already in place: nothing to change.
    std::fs::write(&path, "marker-phrase token=new-token-value\n").unwrap();
    let (report, out) = check_vars(
        &pb,
        SecretVars {
            path: path.display().to_string(),
            token: Secret::literal("new-token-value"),
        },
        true,
    );
    assert_eq!(report.exit_code(), 0, "{out}");
}

#[test]
fn a_missing_secret_blocks_the_check() {
    let d = tempfile::tempdir().unwrap();
    let token: Secret = toml::from_str::<std::collections::BTreeMap<String, Secret>>(
        "pw = { env = \"TP_T98_MISSING\" }\n",
    )
    .unwrap()
    .remove("pw")
    .unwrap();
    let (report, out) = check_vars(
        &Playbook::new("tpl", secret_entry),
        SecretVars {
            path: d.path().join("t.conf").display().to_string(),
            token,
        },
        false,
    );
    assert_eq!(report.exit_code(), 1, "{out}");
    assert!(out.contains("TP_T98_MISSING"), "{out}");
    assert!(!d.path().join("t.conf").exists());
}

// ---- exit status ----

#[test]
fn a_blocked_host_is_exit_1() {
    let d = tempfile::tempdir().unwrap();
    let (report, out) = check(&Playbook::new("a", assert_entry), d.path(), false);
    assert_eq!(report.exit_code(), 1, "{out}");
    assert!(
        out.contains("blocked: the host is not what this playbook wants"),
        "{out}"
    );
    assert!(!d.path().join("never").exists());
}

// ---- coverage ----

#[test]
fn a_coverage_line_per_role_counts_ops_and_inverses() {
    let d = tempfile::tempdir().unwrap();
    let (_, out) = check(&Playbook::new("demo", demo_entry), d.path(), false);
    // demo: file, group, sh; only the group has an inverse today.
    assert!(
        out.contains("== h1 [coverage] demo: 3 ops, 1 reversible =="),
        "{out}"
    );
    assert!(
        out.contains("== h1 [coverage] quiet: 1 ops, 0 reversible =="),
        "{out}"
    );
}

#[test]
fn which_ops_have_an_inverse() {
    let h: Host<()> = Host::new(
        "t",
        Os::parse("ID=debian\nVERSION_ID=12\n").unwrap(),
        Rc::new(()),
    );
    h.group("g");
    h.svc("ssh").started();
    h.pkg(["curl"]);
    h.file("/etc/a").content("a");
    h.file("/etc/b").absent();
    sh!(h, "true");
    let flags: Vec<(&'static str, bool)> = h
        .snapshot()
        .nodes
        .iter()
        .filter_map(|n| match n {
            Node::Op(op) => Some((op.spec.kind(), op.spec.has_inverse())),
            _ => None,
        })
        .collect();
    assert_eq!(
        flags,
        [
            ("group", true),
            ("svc", true),
            ("pkg", true),
            ("file", false),
            ("file", false),
            ("sh", false),
        ]
    );
}

// ---- probes that depend on an earlier change ----

#[test]
fn ops_for_a_user_an_earlier_window_creates_are_not_probed_and_say_so() {
    let d = tempfile::tempdir().unwrap();
    let (report, out) = check(&Playbook::new("u", user_entry), d.path(), false);
    let out = scrub(&out, d.path());
    assert!(out.contains("user tpcheck_nobody_x: will create"), "{out}");
    assert!(
        out.contains(
            "window 2: ops as tpcheck_nobody_x not checked: the user does not exist yet (window 1 creates it)"
        ),
        "{out}"
    );
    assert_eq!(report.exit_code(), 2, "{out}");
    assert!(!d.path().join("later.conf").exists());
}

// ---- implicit barriers ----

#[test]
fn a_check_stops_at_the_first_implicit_barrier() {
    let d = tempfile::tempdir().unwrap();
    let (report, out) = check(&Playbook::new("b", barrier_entry), d.path(), false);
    let out = scrub(&out, d.path());
    let line = include_str!("check.rs")
        .lines()
        .position(|l| l.contains("// BARRIER-READ"))
        .unwrap()
        + 1;
    let want = format!(
        "check stopped at crates/telepatch/tests/check.rs:{line}: this read needs the changes above"
    );
    let alt = want.replace("crates/telepatch/", "");
    assert!(out.contains(&want) || out.contains(&alt), "{out}");
    assert!(
        out.trim_end()
            .lines()
            .last()
            .unwrap()
            .ends_with("this read needs the changes above"),
        "the stop line ends the output:\n{out}"
    );
    assert_eq!(report.exit_code(), 2, "{out}");
    assert!(!d.path().join("made").exists(), "the check ran the sh");
    assert!(
        !out.contains("after.conf"),
        "traced past the barrier:\n{out}"
    );
}

// ---- the CLI ----

#[derive(serde::Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
struct CliVars {
    dir: PathBuf,
}

#[role]
async fn cli_site(h: &Host<CliVars>) -> Result<(), TraceError> {
    h.file(h.vars.dir.join("greeting").to_str().unwrap())
        .content("hello\n");
    Ok(())
}

fn cli_entry(h: Host<CliVars>) -> PlaybookFuture {
    Box::pin(async move { cli_site(&h).await })
}

fn go(a: &[&str]) -> (String, i32) {
    let args: Vec<String> = std::iter::once("pb")
        .chain(a.iter().copied())
        .map(String::from)
        .collect();
    run_with_args(
        &args,
        &[Playbook::new("site", cli_entry)],
        || LocalTransport::new().uid(0),
        SshTransport::new,
    )
}

#[test]
fn the_cli_check_exits_2_then_0_after_a_real_run() {
    let d = tempfile::tempdir().unwrap();
    let vars = d.path().join("vars.toml");
    let dir = d.path().join("out");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(&vars, format!("dir = {:?}\n", dir.to_str().unwrap())).unwrap();
    let v = vars.to_str().unwrap();
    let (out, code) = go(&["run", "site", "--local", "--vars", v, "--check", "--diff"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("will create"), "{out}");
    assert!(out.contains("| +hello"), "{out}");
    assert!(!dir.join("greeting").exists());
    let (out, code) = go(&["run", "site", "--local", "--vars", v]);
    assert_eq!(code, 0, "{out}");
    let (out, code) = go(&["run", "site", "--local", "--vars", v, "--check"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("0 will change"), "{out}");
}
