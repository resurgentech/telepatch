//! No script on a command line (T111). An op's script, or a check's
//! command, travels to the shell on stdin or a file descriptor, never as an
//! argument: an argument is capped by the kernel (`Argument list too
//! long`) and shows in every `ps` on the host.
//!
//! Hosts are this machine through the local transport with the uid
//! injected as 0 (no sudo), the form `tests/binary_release.rs` uses.

use std::cell::RefCell;
use std::path::PathBuf;
use std::pin::pin;
use std::rc::Rc;
use std::task::Poll;

use sha2::{Digest, Sha256};
use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::report::{HostStatus, Report};
use telepatch::transport::local::LocalTransport;
use telepatch::{Host, Playbook, PlaybookFuture};

/// Run the playbook on a thread, so a run that loses an op's trailer (the
/// shell cannot start the op) fails the test instead of hanging it.
fn go(name: &'static str, f: fn(Host<PathBuf>) -> PlaybookFuture, dir: &std::path::Path) -> Report {
    let (tx, rx) = std::sync::mpsc::channel();
    let vars = dir.to_path_buf();
    std::thread::spawn(move || {
        let pb = Playbook::new(name, f);
        let out = Rc::new(RefCell::new(String::new()));
        let r = run(
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
        let _ = tx.send((r, out.borrow().clone()));
    });
    match rx.recv_timeout(std::time::Duration::from_secs(20)) {
        Ok((r, _)) => r,
        Err(_) => panic!("the run did not finish in 20s: an op's trailer never came"),
    }
}

/// `len` bytes of text that no one wrote by hand: a SHA-256 chain in hex,
/// newline-wrapped so no line is long.
fn random_text(len: usize) -> String {
    let mut s = String::with_capacity(len + 80);
    let mut h = Sha256::digest(format!("{:?}", std::time::SystemTime::now()));
    while s.len() < len {
        h = Sha256::digest(h);
        for b in h {
            s.push_str(&format!("{b:02x}"));
        }
        s.push('\n');
    }
    s.truncate(len);
    s
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ---- (a) a 200 KiB file, plain and as root ----

fn big_plain(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let want = std::fs::read_to_string(h.vars.join("want")).unwrap();
        h.file(h.vars.join("out").to_str().unwrap()).content(want);
        Ok(())
    })
}

fn big_login(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let want = std::fs::read_to_string(h.vars.join("want")).unwrap();
        h.as_user("root")
            .file(h.vars.join("out").to_str().unwrap())
            .content(want);
        Ok(())
    })
}

fn big(name: &'static str, f: fn(Host<PathBuf>) -> PlaybookFuture) {
    let d = tempfile::tempdir().unwrap();
    let body = random_text(200 * 1024);
    std::fs::write(d.path().join("want"), &body).unwrap();
    let r = go(name, f, d.path());
    assert_eq!(r.exit_code(), 0, "{}", r.render());
    assert!(matches!(r.hosts[0].status, HostStatus::Changed));
    let got = std::fs::read(d.path().join("out")).unwrap();
    assert_eq!(sha(&got), sha(body.as_bytes()));
}

#[test]
fn a_200_kib_file_applies_byte_exact() {
    big("big_plain", big_plain);
}

#[test]
fn a_200_kib_file_applies_byte_exact_as_user() {
    big("big_login", big_login);
}

// ---- (b) the cmdline sweep ----

/// The sweep: a script that holds `token` (in a no-op) and reads every
/// `/proc/[0-9]*/cmdline` with builtins only. It builds the token at run
/// time from two halves, so the sweep's own argv never holds it, and
/// prints one `HIT <file>` per match, then `SWEPT <n>`.
fn sweep(token: &str) -> String {
    let (a, b) = token.split_at(6);
    format!(
        ": {token}\n\
         t={a}''{b}; n=0\n\
         for f in /proc/[0-9]*/cmdline; do\n\
         mapfile -d '' -t w < \"$f\" 2>/dev/null || continue\n\
         n=$((n+1)); s=\"${{w[*]}}\"\n\
         case \"$s\" in *\"$t\"*) echo \"HIT $f\";; esac\n\
         done\n\
         echo \"SWEPT $n\"\n"
    )
}

fn token(case: &str) -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("tpnoargv{case}{}x{n}", std::process::id())
}

fn run_sweep_op(case: &'static str) -> String {
    let d = tempfile::tempdir().unwrap();
    let tok = token(case);
    std::fs::write(d.path().join("tok"), &tok).unwrap();
    std::fs::write(d.path().join("case"), case).unwrap();
    let r = go("sweep", sweep_pb, d.path());
    assert_eq!(r.exit_code(), 0, "{}", r.render());
    let hits = std::fs::read_to_string(d.path().join("hits")).unwrap();
    assert!(hits.contains("SWEPT "), "the sweep did not run: {hits:?}");
    hits
}

fn sweep_pb(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let case = std::fs::read_to_string(h.vars.join("case")).unwrap();
        let tok = std::fs::read_to_string(h.vars.join("tok")).unwrap();
        let out = h.vars.join("hits").display().to_string();
        let script = format!("(\n{}) > '{out}'\n", sweep(&tok));
        match case.as_str() {
            "plain" => {
                h.raw(script);
            }
            "login" => {
                h.as_user("root").raw(script);
            }
            "check" => {
                let c = h.check(&sweep(&tok)).await;
                std::fs::write(&out, c.stdout).unwrap();
            }
            "check3" => {
                // A run of three: one bash, three sweeps.
                let (t1, t2, t3) = (format!("{tok}a"), format!("{tok}b"), format!("{tok}c"));
                let (s1, s2, s3) = (sweep(&t1), sweep(&t2), sweep(&t3));
                let mut a = pin!(h.check(&s1));
                let mut b = pin!(h.check(&s2));
                let mut c = pin!(h.check(&s3));
                let all = std::future::poll_fn(|cx| {
                    match (
                        a.as_mut().poll(cx),
                        b.as_mut().poll(cx),
                        c.as_mut().poll(cx),
                    ) {
                        (Poll::Ready(a), Poll::Ready(b), Poll::Ready(c)) => Poll::Ready([a, b, c]),
                        _ => Poll::Pending,
                    }
                })
                .await;
                let text: String = all.iter().map(|c| c.stdout.as_str()).collect();
                std::fs::write(&out, text).unwrap();
            }
            _ => unreachable!(),
        }
        Ok(())
    })
}

fn assert_no_hit(hits: &str) {
    let found: Vec<&str> = hits.lines().filter(|l| l.starts_with("HIT ")).collect();
    assert!(found.is_empty(), "script on a command line: {found:?}");
}

#[test]
fn a_plain_op_script_is_on_no_command_line() {
    assert_no_hit(&run_sweep_op("plain"));
}

#[test]
fn an_as_user_op_script_is_on_no_command_line() {
    assert_no_hit(&run_sweep_op("login"));
}

#[test]
fn a_check_command_is_on_no_command_line() {
    assert_no_hit(&run_sweep_op("check"));
}

#[test]
fn a_run_of_three_checks_is_on_no_command_line() {
    let hits = run_sweep_op("check3");
    assert_eq!(hits.matches("SWEPT ").count(), 3, "{hits}");
    assert_no_hit(&hits);
}

// ---- no here-doc, so no delimiter ----

/// A script line that equalled the old per-op here-doc delimiter used to be
/// a render error (the shell would have ended the script there). A login op
/// has no here-doc now: it is sent exactly like a root op, so the line
/// renders and runs, byte for byte.
#[test]
fn a_login_script_line_equal_to_the_old_delimiter_renders_and_runs() {
    let h: Host<()> = Host::new(
        "t",
        telepatch::facts::os::Os::parse("ID=debian\nVERSION_ID=12\n").unwrap(),
        Rc::new(()),
    );
    h.as_user("root").raw("echo a\n__tp_S_n0nce_5\necho b");
    let nodes = h.snapshot().nodes;
    let telepatch::Node::Op(op) = &nodes[0] else {
        panic!()
    };
    let ok = telepatch::trailer::render_for(op, 5, "n0nce", &h.os, "root").unwrap();
    assert!(!ok.contains("bash -l") && !ok.contains("/dev/fd/9"), "{ok}");
    assert!(!ok.contains("env -i") && !ok.contains("getent"), "{ok}");
}

fn delimiter_pb(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let out = h.vars.join("delim").display().to_string();
        h.as_user("root").raw(format!(
            "cat <<'EOT' > '{out}'\n__tp_S_n0nce_5\n__tp_S_{{n}}\nEOT\n"
        ));
        Ok(())
    })
}

#[test]
fn a_login_script_with_such_a_line_runs_as_user() {
    let d = tempfile::tempdir().unwrap();
    let r = go("delim", delimiter_pb, d.path());
    assert_eq!(r.exit_code(), 0, "{}", r.render());
    assert_eq!(
        std::fs::read_to_string(d.path().join("delim")).unwrap(),
        "__tp_S_n0nce_5\n__tp_S_{n}\n"
    );
}

// ---- one persistent login shell ----

fn shell_pb(h: Host<PathBuf>) -> PlaybookFuture {
    Box::pin(async move {
        let (a, b, c) = (
            h.vars.join("s1").display().to_string(),
            h.vars.join("s2").display().to_string(),
            h.vars.join("kids").display().to_string(),
        );
        h.as_user("root").raw(format!("echo $$ > '{a}'"));
        // The shell's own pid, the op's parent (the op subshell is a child
        // of the login shell), and every other child of the shell while
        // the op runs, by /proc with builtins only.
        h.as_user("root").raw(format!(
            "echo $$ > '{b}'\n\
             {{\n\
             read -r l < /proc/$BASHPID/stat; r=${{l##*) }}; set -- $r\n\
             echo \"PARENT $2 SHELL $$\"\n\
             mapfile -d '' -t me < /proc/$$/cmdline; echo \"SHELLARGV ${{me[*]}}\"\n\
             for f in /proc/[0-9]*/stat; do\n\
               read -r l < \"$f\" 2>/dev/null || continue\n\
               r=${{l##*) }}; set -- $r; pid=${{f#/proc/}}; pid=${{pid%/stat}}\n\
               if [ \"$2\" = \"$$\" ] && [ \"$pid\" != \"$BASHPID\" ]; then\n\
                 mapfile -d '' -t w < /proc/$pid/cmdline; echo \"KID $pid ${{w[*]}}\"\n\
               fi\n\
             done\n\
             }} > '{c}'\n"
        ));
        Ok(())
    })
}

#[test]
fn two_as_user_ops_share_one_login_shell_and_start_no_bash_l() {
    let d = tempfile::tempdir().unwrap();
    let r = go("shell", shell_pb, d.path());
    assert_eq!(r.exit_code(), 0, "{}", r.render());
    let (s1, s2) = (
        std::fs::read_to_string(d.path().join("s1")).unwrap(),
        std::fs::read_to_string(d.path().join("s2")).unwrap(),
    );
    assert!(
        !s1.trim().is_empty() && s1 == s2,
        "two shells: {s1:?} {s2:?}"
    );
    let kids = std::fs::read_to_string(d.path().join("kids")).unwrap();
    // The op's own `$BASHPID` has the login shell as its parent.
    assert!(
        kids.contains(&format!("PARENT {0} SHELL {0}", s1.trim())),
        "{kids}"
    );
    // The shell is the login bash, and no `bash -l` runs as its child.
    assert!(kids.contains("SHELLARGV bash -l"), "{kids}");
    let others: Vec<&str> = kids.lines().filter(|l| l.starts_with("KID ")).collect();
    assert!(
        others.iter().all(|l| !l.contains("bash")),
        "a bash child of the login shell: {others:?}"
    );
}

// ---- the reboot mark and the boot id (T102) ----

/// A marked op's text reaches the host in the mark, and the mark is written
/// by shell builtins from the root shell's stdin: `mkdir` is the only
/// command run, with the directory alone. Shims for the commands the mark
/// script could run log their argv; the op's token is in none of them, and
/// `printf` is never run as a command at all.
#[test]
fn a_reboot_mark_is_written_by_builtins_and_no_op_text_is_on_a_command_line() {
    use std::io::Write as _;
    let d = tempfile::tempdir().unwrap();
    let shims = d.path().join("shims");
    std::fs::create_dir(&shims).unwrap();
    let log = d.path().join("argv.log");
    for name in ["mkdir", "printf", "cat"] {
        let body = format!(
            "#!/bin/sh\necho \"{name} $*\" >> '{}'\nexec /usr/bin/{name} \"$@\"\n",
            log.display()
        );
        let mut c = std::process::Command::new("sh")
            .arg("-c")
            .arg("cat > \"$1\" && chmod 755 \"$1\"")
            .arg("sh")
            .arg(shims.join(name))
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        c.stdin.take().unwrap().write_all(body.as_bytes()).unwrap();
        assert!(c.wait().unwrap().success());
    }
    let tok = token("mark");
    let work = d.path().join(&tok);
    std::fs::create_dir(&work).unwrap();
    let run_dir = d.path().join("run");
    let path = format!("{}:{}", shims.display(), std::env::var("PATH").unwrap());
    let pb = Playbook::new("marked", |h: Host<PathBuf>| -> PlaybookFuture {
        Box::pin(async move {
            let f = h.vars.join("conf").display().to_string();
            h.file(&f).content("x\n").needs_reboot();
            Ok(())
        })
    });
    let out = Rc::new(RefCell::new(String::new()));
    let r = run(
        &pb,
        vec![HostSpec {
            groups: Vec::new(),
            name: "h1".into(),
            target: Target::Local(
                LocalTransport::new()
                    .uid(0)
                    .env("PATH", &path)
                    .env("TP_RUN_DIR", run_dir.to_str().unwrap()),
            ),
            vars: work.clone(),
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    assert_eq!(r.exit_code(), 0, "{}\n{}", r.render(), out.borrow());
    let mark = std::fs::read_to_string(run_dir.join("reboot-pending")).unwrap();
    assert!(mark.contains(&tok), "the mark names the op: {mark}");
    let argv = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(!argv.contains(&tok), "op text on a command line: {argv}");
    assert!(!argv.lines().any(|l| l.starts_with("printf ")), "{argv}");
    assert!(
        argv.lines()
            .any(|l| l == format!("mkdir -p {}", run_dir.display())),
        "{argv}"
    );
}
