//! `h.git(url)`: clone a checkout, leave a clean checkout of the same
//! remote alone unless `.rev` or `.latest()` asks it to move, and block
//! in prepare on a different remote or a dirty tree.

use std::cell::RefCell;
use std::path::Path;
use std::process::Command;
use std::rc::Rc;

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::facts::os::Os;
use telepatch::report::{HostStatus, Report};
use telepatch::transport::local::LocalTransport;
use telepatch::{Host, Key, Node, Playbook, PlaybookFuture, role};

fn ubuntu() -> Os {
    Os::parse("ID=ubuntu\nVERSION_ID=24.04\n").unwrap()
}

fn host() -> Host<()> {
    Host::new("t", ubuntu(), Rc::new(()))
}

fn git_spec(h: &Host<()>) -> telepatch::ops::git::GitSpec {
    match &h.snapshot().nodes[0] {
        Node::Op(op) => match &op.spec {
            telepatch::OpSpec::Git(g) => g.clone(),
            other => panic!("not a git op: {other:?}"),
        },
        _ => panic!("not an op"),
    }
}

#[test]
fn checkout_script_clones_at_depth_and_records_the_key() {
    let h = host();
    h.git("https://example.invalid/a.git")
        .depth(1)
        .into("/srv/a");
    assert!(h.snapshot().errors.is_empty(), "{:?}", h.snapshot().errors);
    let g = git_spec(&h);
    let script = g.render();
    assert!(script.contains("git clone"), "{script}");
    assert!(script.contains("--depth 1"), "{script}");
    assert!(script.contains("https://example.invalid/a.git"), "{script}");
    assert!(script.contains("/srv/a"), "{script}");
    let Node::Op(op) = &h.snapshot().nodes[0] else {
        panic!("not an op");
    };
    assert!(op.writes.contains(&Key::git("/srv/a")));
    assert_eq!(op.run_as.to_string(), "shell:root");
}

#[test]
fn as_user_runs_in_that_login_and_tilde_is_home() {
    let h = host();
    h.as_user("tpdev")
        .git("https://example.invalid/theme.git")
        .depth(1)
        .into("~/.oh-my-zsh/custom/themes/powerlevel10k");
    assert!(h.snapshot().errors.is_empty(), "{:?}", h.snapshot().errors);
    let Node::Op(op) = &h.snapshot().nodes[0] else {
        panic!("not an op");
    };
    assert_eq!(op.run_as.to_string(), "login:tpdev");
    let script = op.spec.script(&h.os);
    assert!(script.contains("$HOME"), "{script}");
    assert!(!script.contains("'~/.oh-my-zsh"), "{script}");
}

#[test]
fn rev_and_latest_together_and_a_missing_path_are_trace_errors() {
    let h = host();
    h.git("https://example.invalid/a.git")
        .rev("v1")
        .latest()
        .into("/srv/a");
    let msgs: Vec<String> = h.snapshot().errors.iter().map(|e| e.to_string()).collect();
    assert!(msgs.iter().any(|m| m.contains("exclusive")), "{msgs:?}");
    let h = host();
    h.git("https://example.invalid/a.git").depth(1);
    let msgs: Vec<String> = h.snapshot().errors.iter().map(|e| e.to_string()).collect();
    assert!(msgs.iter().any(|m| m.contains(".into(")), "{msgs:?}");
}

fn target(sb: &telepatch::testing::Sandbox) -> Target {
    Target::Local(
        LocalTransport::new()
            .uid(0)
            .sandboxed(sb)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null"),
    )
}

fn go<V: 'static>(name: &'static str, f: fn(Host<V>) -> PlaybookFuture, v: V) -> (Report, String) {
    let pb = Playbook::new(name, f);
    let sb = telepatch::testing::sandbox();
    let out = Rc::new(RefCell::new(String::new()));
    let report = run(
        &pb,
        vec![HostSpec {
            groups: Vec::new(),
            name: "h".into(),
            target: target(&sb),
            vars: v,
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    (report, out.borrow().clone())
}

fn git(args: &[&str], dir: &Path) -> String {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .output()
        .expect("git");
    assert!(
        o.status.success(),
        "git {args:?} in {}: {}\n{}",
        dir.display(),
        String::from_utf8_lossy(&o.stderr),
        String::from_utf8_lossy(&o.stdout)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn bare_with_file(dir: &Path, body: &str) -> (String, String) {
    let bare = dir.join("remote.git");
    let work = dir.join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(
        &["init", "--bare", "-b", "main", bare.to_str().unwrap()],
        dir,
    );
    git(&["init", "-b", "main", work.to_str().unwrap()], dir);
    std::fs::write(work.join("f.txt"), body).unwrap();
    git(&["add", "f.txt"], &work);
    git(&["commit", "-m", "a"], &work);
    let sha = git(&["rev-parse", "HEAD"], &work);
    let url = bare.to_str().unwrap().to_string();
    git(&["remote", "add", "origin", &url], &work);
    git(&["push", "-u", "origin", "main"], &work);
    (url, sha)
}

#[derive(Clone)]
struct G {
    url: String,
    dest: String,
    rev: String,
}

#[role]
async fn clone_leave(h: &Host<G>) -> Result<(), telepatch::TraceError> {
    h.git(&h.vars.url).depth(1).into(&h.vars.dest);
    Ok(())
}

fn clone_leave_run(h: Host<G>) -> PlaybookFuture {
    Box::pin(async move { clone_leave(&h).await })
}

#[role]
async fn clone_latest(h: &Host<G>) -> Result<(), telepatch::TraceError> {
    h.git(&h.vars.url).depth(1).latest().into(&h.vars.dest);
    Ok(())
}

fn clone_latest_run(h: Host<G>) -> PlaybookFuture {
    Box::pin(async move { clone_latest(&h).await })
}

#[role]
async fn clone_rev(h: &Host<G>) -> Result<(), telepatch::TraceError> {
    h.git(&h.vars.url)
        .depth(1)
        .rev(&h.vars.rev)
        .into(&h.vars.dest);
    Ok(())
}

fn clone_rev_run(h: Host<G>) -> PlaybookFuture {
    Box::pin(async move { clone_rev(&h).await })
}

fn head(dest: &Path) -> String {
    git(&["rev-parse", "HEAD"], dest)
}

#[test]
fn no_rev_leaves_an_existing_checkout_and_latest_and_rev_move_it() {
    let d = tempfile::tempdir().unwrap();
    let (url, first) = bare_with_file(d.path(), "one\n");
    let dest = d.path().join("checkout");
    let vars = G {
        url: url.clone(),
        dest: dest.to_str().unwrap().to_string(),
        rev: first.clone(),
    };
    let (r1, out1) = go("clone", clone_leave_run, vars.clone());
    assert_eq!(r1.exit_code(), 0, "{out1}\n{}", r1.render());
    assert_eq!(r1.hosts[0].status, HostStatus::Changed, "{}", r1.render());
    assert_eq!(head(&dest), first);
    assert!(
        r1.hosts[0]
            .git
            .iter()
            .any(|g| g.created && g.path == vars.dest),
        "inverse should record a checkout this run created: {:?}",
        r1.hosts[0].git
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("f.txt")).unwrap(),
        "one\n"
    );

    let (r2, out2) = go("clone", clone_leave_run, vars.clone());
    assert_eq!(r2.exit_code(), 0, "{out2}\n{}", r2.render());
    assert_eq!(r2.hosts[0].status, HostStatus::Ok, "{}", r2.render());
    assert_eq!(head(&dest), first);

    let work = d.path().join("work");
    std::fs::write(work.join("f.txt"), "two\n").unwrap();
    git(&["add", "f.txt"], &work);
    git(&["commit", "-m", "b"], &work);
    let second = git(&["rev-parse", "HEAD"], &work);
    git(&["push", "origin", "main"], &work);
    assert_ne!(first, second);

    let (r3, out3) = go("clone", clone_leave_run, vars.clone());
    assert_eq!(r3.exit_code(), 0, "{out3}\n{}", r3.render());
    assert_eq!(head(&dest), first, "no .rev must leave the checkout alone");
    assert_eq!(r3.hosts[0].status, HostStatus::Ok, "{}", r3.render());

    let (r4, out4) = go("latest", clone_latest_run, vars.clone());
    assert_eq!(r4.exit_code(), 0, "{out4}\n{}", r4.render());
    assert_eq!(head(&dest), second, "latest follows the branch");
    assert_eq!(r4.hosts[0].status, HostStatus::Changed, "{}", r4.render());
    assert!(
        r4.hosts[0]
            .git
            .iter()
            .any(|g| !g.created && g.was_rev == first),
        "inverse should record the previous revision: {:?}",
        r4.hosts[0].git
    );

    let (r5, out5) = go(
        "rev",
        clone_rev_run,
        G {
            rev: first.clone(),
            ..vars.clone()
        },
    );
    assert_eq!(r5.exit_code(), 0, "{out5}\n{}", r5.render());
    assert_eq!(head(&dest), first, ".rev checks out that revision");
}

#[test]
fn a_dirty_tree_or_a_different_remote_blocks_before_the_checkout_moves() {
    let d = tempfile::tempdir().unwrap();
    let (url, first) = bare_with_file(d.path(), "one\n");
    let dest = d.path().join("checkout");
    let vars = G {
        url: url.clone(),
        dest: dest.to_str().unwrap().to_string(),
        rev: first.clone(),
    };
    let (r1, out1) = go("clone", clone_leave_run, vars.clone());
    assert_eq!(r1.exit_code(), 0, "{out1}\n{}", r1.render());

    std::fs::write(dest.join("f.txt"), "dirty\n").unwrap();
    let (r2, out2) = go("clone", clone_latest_run, vars.clone());
    let why = match &r2.hosts[0].status {
        HostStatus::Blocked(s) | HostStatus::Error(s) => s.clone(),
        other => format!("{other:?}\n{out2}\n{}", r2.render()),
    };
    assert!(why.contains("dirty"), "{why}");
    assert_eq!(
        std::fs::read_to_string(dest.join("f.txt")).unwrap(),
        "dirty\n"
    );
    assert_eq!(head(&dest), first, "a dirty tree is not reset");

    std::fs::write(dest.join("f.txt"), "one\n").unwrap();
    let other = d.path().join("other.git");
    git(
        &["init", "--bare", "-b", "main", other.to_str().unwrap()],
        d.path(),
    );
    let (r3, _) = go(
        "other",
        clone_leave_run,
        G {
            url: other.to_str().unwrap().to_string(),
            ..vars
        },
    );
    let why = match &r3.hosts[0].status {
        HostStatus::Blocked(s) | HostStatus::Error(s) => s.clone(),
        other => format!("{other:?}\n{}", r3.render()),
    };
    assert!(
        why.contains("remote"),
        "a different remote must block: {why}"
    );
    assert_eq!(head(&dest), first);
}
