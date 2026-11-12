//! Controller-side files and commands: read in prepare, streamed only
//! where the host copy's sha256 differs, chunked so a 20 MiB file is
//! not one base64 script.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::local::{self, local, local_dir, local_file};
use telepatch::report::{HostStatus, Report};
use telepatch::transport::local::LocalTransport;
use telepatch::{Host, Playbook, PlaybookFuture, TraceError, role};

fn target() -> Target {
    Target::Local(LocalTransport::new().uid(0))
}

fn spec(name: &str, dir: &std::path::Path) -> HostSpec<PathBuf> {
    HostSpec {
        name: name.into(),
        target: target(),
        vars: dir.to_path_buf(),
    }
}

fn go<V: 'static>(pb: &Playbook<V>, hosts: Vec<HostSpec<V>>) -> (Report, String) {
    let out = Rc::new(RefCell::new(String::new()));
    let report = run(
        pb,
        hosts,
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    (report, out.borrow().clone())
}

fn p(h: &Host<PathBuf>, name: &str) -> String {
    h.vars.join(name).to_str().unwrap().to_string()
}

fn why(r: &Report) -> String {
    match &r.hosts[0].status {
        HostStatus::Blocked(s) | HostStatus::Error(s) => s.clone(),
        other => format!("{other:?}\n{}", r.render()),
    }
}

#[role]
async fn missing(h: &Host<PathBuf>) -> Result<(), TraceError> {
    let marker = p(h, "marker");
    let dest = p(h, "out");
    h.file(&marker).content("should-not-land\n");
    h.file(&dest).from(local_file("no-such-file"));
    Ok(())
}

fn missing_run(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move { missing(&h).await })
}

#[test]
fn missing_local_file_blocks_before_any_host_change() {
    let d = tempfile::tempdir().unwrap();
    local::set_anchor(Some(d.path().to_path_buf()));
    let pb = Playbook::new("missing", missing_run);
    let (r, _) = go(&pb, vec![spec("h", d.path())]);
    assert!(
        !d.path().join("marker").exists(),
        "a missing controller file must end the run before host changes\n{}",
        r.render()
    );
    let msg = why(&r);
    assert!(
        msg.contains("no-such-file"),
        "blocked without the path: {msg}"
    );
}

#[role]
async fn big(h: &Host<PathBuf>) -> Result<(), TraceError> {
    let dest = p(h, "out.bin");
    h.file(&dest).from(local_file("blob.bin"));
    Ok(())
}

fn big_run(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move { big(&h).await })
}

#[test]
fn twenty_mib_binary_is_streamed_and_the_second_run_does_not_rewrite() {
    let d = tempfile::tempdir().unwrap();
    let n = 20 * 1024 * 1024;
    let mut bytes = vec![0u8; n];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = (i % 251) as u8;
    }
    bytes[0] = 0;
    bytes[1] = 255;
    bytes[50] = 0;
    bytes[n - 1] = 0xa5;
    std::fs::write(d.path().join("blob.bin"), &bytes).unwrap();
    local::set_anchor(Some(d.path().to_path_buf()));
    assert!(
        local::chunk_count(n as u64) > 1,
        "a 20 MiB file must be more than one chunk"
    );

    let pb = Playbook::new("big", big_run);
    let (r1, out1) = go(&pb, vec![spec("h", d.path())]);
    assert_eq!(r1.exit_code(), 0, "{}", r1.render());
    assert_eq!(r1.hosts[0].status, HostStatus::Changed, "{}", r1.render());
    let got = std::fs::read(d.path().join("out.bin")).unwrap();
    assert_eq!(got.len(), n);
    assert_eq!(got[0], 0);
    assert_eq!(got[1], 255);
    assert_eq!(got[50], 0, "interior NUL must survive");
    assert_eq!(got[n - 1], 0xa5);
    assert_eq!(local::sha256_bytes(&got), local::sha256_bytes(&bytes));
    assert!(
        out1.contains("will change")
            && out1.contains("from controller")
            && out1.contains("sha256 differs"),
        "prepare line missing: {out1}"
    );
    assert!(out1.contains("MiB"), "size missing: {out1}");

    let stamp = std::fs::metadata(d.path().join("out.bin"))
        .unwrap()
        .modified()
        .unwrap();
    let (r2, out2) = go(&pb, vec![spec("h", d.path())]);
    assert_eq!(r2.hosts[0].status, HostStatus::Ok, "{}", r2.render());
    assert!(r2.hosts[0].ops.iter().all(|o| !o.changed));
    assert_eq!(
        std::fs::metadata(d.path().join("out.bin"))
            .unwrap()
            .modified()
            .unwrap(),
        stamp,
        "a matching sha256 must not rewrite the file"
    );
    assert!(
        !out2.contains("sha256 differs"),
        "second run streamed again: {out2}"
    );
}

#[role]
async fn tree(h: &Host<PathBuf>) -> Result<(), TraceError> {
    let dest = p(h, "certs");
    h.file(&dest).from(local_dir("certs"));
    Ok(())
}

fn tree_run(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move { tree(&h).await })
}

#[test]
fn local_dir_copies_the_tree_and_removes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("certs");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("a.txt"), b"alpha\n").unwrap();
    std::fs::write(src.join("sub").join("b.txt"), b"beta\0\n").unwrap();
    let root = d.path().join("host");
    std::fs::create_dir_all(root.join("certs")).unwrap();
    std::fs::write(root.join("certs").join("extra"), b"keep\n").unwrap();
    local::set_anchor(Some(d.path().to_path_buf()));
    let pb = Playbook::new("tree", tree_run);
    let (r1, _) = go(&pb, vec![spec("h", &root)]);
    assert_eq!(r1.exit_code(), 0, "{}", r1.render());
    assert_eq!(
        std::fs::read(root.join("certs").join("a.txt")).unwrap(),
        b"alpha\n"
    );
    assert_eq!(
        std::fs::read(root.join("certs").join("sub").join("b.txt")).unwrap(),
        b"beta\0\n"
    );
    assert_eq!(
        std::fs::read(root.join("certs").join("extra")).unwrap(),
        b"keep\n",
        "a file that is not in the tree stays"
    );
    let (r2, _) = go(&pb, vec![spec("h", &root)]);
    assert_eq!(r2.hosts[0].status, HostStatus::Ok, "{}", r2.render());
}

#[role]
async fn cmd_ok(h: &Host<PathBuf>) -> Result<(), TraceError> {
    let dest = p(h, "line.txt");
    h.file(&dest)
        .line(local(["/usr/bin/printf", "%s", "a; echo hi"]));
    Ok(())
}

fn cmd_ok_run(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move { cmd_ok(&h).await })
}

#[role]
async fn cmd_bad(h: &Host<PathBuf>) -> Result<(), TraceError> {
    let marker = p(h, "marker");
    let dest = p(h, "line.txt");
    h.file(&marker).content("nope\n");
    h.file(&dest).line(local(["/usr/bin/false"]));
    Ok(())
}

fn cmd_bad_run(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move { cmd_bad(&h).await })
}

#[test]
fn local_command_is_argv_not_a_shell_and_a_failure_blocks() {
    let d = tempfile::tempdir().unwrap();
    let pb = Playbook::new("cmd", cmd_ok_run);
    let (r, _) = go(&pb, vec![spec("h", d.path())]);
    assert_eq!(r.exit_code(), 0, "{}", r.render());
    assert_eq!(
        std::fs::read_to_string(d.path().join("line.txt")).unwrap(),
        "a; echo hi\n",
        "the argv is not parsed by a shell"
    );

    let d2 = tempfile::tempdir().unwrap();
    let pb = Playbook::new("cmd-bad", cmd_bad_run);
    let (r, _) = go(&pb, vec![spec("h", d2.path())]);
    assert!(!d2.path().join("marker").exists(), "{}", r.render());
    assert!(why(&r).contains("false"), "{}", r.render());
}

#[role]
async fn pubkey(h: &Host<PathBuf>) -> Result<(), TraceError> {
    let dest = p(h, "authorized_keys");
    let key = local_file("id.pub");
    h.file(&dest).line(&key);
    Ok(())
}

fn pubkey_run(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move { pubkey(&h).await })
}

#[test]
fn local_file_is_a_line_value() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("id.pub"), b"ssh-ed25519 AAAAtest\n").unwrap();
    local::set_anchor(Some(d.path().to_path_buf()));
    let pb = Playbook::new("key", pubkey_run);
    let (r, _) = go(&pb, vec![spec("h", d.path())]);
    assert_eq!(r.exit_code(), 0, "{}", r.render());
    assert_eq!(
        std::fs::read_to_string(d.path().join("authorized_keys")).unwrap(),
        "ssh-ed25519 AAAAtest\n"
    );
}

struct Via {
    dest: PathBuf,
    url: String,
    sum: String,
}

#[role]
async fn via(h: &Host<Via>) -> Result<(), TraceError> {
    let dest = h.vars.dest.to_str().unwrap().to_string();
    let f = h.fetch(&h.vars.url).sha256(&h.vars.sum).via_controller();
    h.file(&dest).from(&f);
    Ok(())
}

fn via_run(h: Host<Via>) -> PlaybookFuture {
    Box::pin(async move { via(&h).await })
}

#[test]
fn via_controller_downloads_on_the_controller_and_streams() {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("payload.bin");
    let bytes = b"controller-bytes\0more\n".to_vec();
    std::fs::write(&src, &bytes).unwrap();
    let sum = local::sha256_bytes(&bytes);
    let url = format!("file://{}", src.display());
    let root = d.path().join("host");
    std::fs::create_dir_all(&root).unwrap();
    let pb = Playbook::new("via", via_run);
    let (r, out) = go(
        &pb,
        vec![HostSpec {
            name: "h".into(),
            target: target(),
            vars: Via {
                dest: root.join("blob"),
                url,
                sum,
            },
        }],
    );
    assert_eq!(r.exit_code(), 0, "{}\n{out}", r.render());
    assert_eq!(std::fs::read(root.join("blob")).unwrap(), bytes);
    assert!(out.contains("from controller"), "{out}");
}

#[role]
async fn unver(h: &Host<PathBuf>) -> Result<(), TraceError> {
    let marker = p(h, "marker");
    let dest = p(h, "blob");
    h.file(&marker).content("nope\n");
    let f = h.fetch("file:///does-not-need-to-exist").via_controller();
    h.file(&dest).from(&f);
    Ok(())
}

fn unver_run(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move { unver(&h).await })
}

#[test]
fn via_controller_without_a_checksum_is_unverified_before_changes() {
    let d = tempfile::tempdir().unwrap();
    let pb = Playbook::new("unver", unver_run);
    let (r, _) = go(&pb, vec![spec("h", d.path())]);
    assert!(!d.path().join("marker").exists(), "{}", r.render());
    let msg = why(&r);
    assert!(
        msg.contains("sha256") || msg.contains("Unverified") || msg.contains("unverified"),
        "{msg}"
    );
}
