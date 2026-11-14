//! T136: `cache purge --yes` takes no lock, so it can land under a running
//! run. A test run sits between prepare and the op that reads a cached
//! file; the purge (the same script `cache purge --yes` sends) removes the
//! file there. The op fails with T135's gone-file message and places
//! nothing; a rerun downloads the file again and passes. The CLI tests of
//! `cache usage` and `cache purge` are in `cli.rs`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::report::HostStatus;
use telepatch::transport::local::LocalTransport;
use telepatch::{Host, Playbook, PlaybookFuture};

const FILE: &str = "alpha file contents\n";

#[derive(Clone, Default)]
struct V {
    url: String,
    sum: String,
    target: String,
    /// A raw op between prepare and the file op.
    pre: Option<String>,
}

fn pb(h: Host<V>) -> PlaybookFuture {
    Box::pin(async move {
        let v = h.vars.clone();
        if let Some(p) = &v.pre {
            h.raw(p.clone());
        }
        h.file(&v.target).from(h.fetch(&v.url).sha256(&v.sum));
        Ok(())
    })
}

fn sha256(bytes: &[u8]) -> String {
    use std::io::Write;
    let mut c = std::process::Command::new("sha256sum")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(bytes).unwrap();
    let o = c.wait_with_output().unwrap();
    String::from_utf8(o.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

fn go(d: &std::path::Path, v: &V) -> (HostStatus, String) {
    let t = LocalTransport::new()
        .uid(0)
        .sandboxed_at(d)
        .env(
            "PATH",
            &format!(
                "{}:{}",
                d.join("bin").display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .env("TMPDIR", &d.join("tmp").display().to_string())
        .env("TP_FETCH_STAGE", &d.join("stage").display().to_string())
        .env("TP_FETCH_CACHE", &d.join("cache").display().to_string())
        .env("TP_RUN_DIR", &d.join("run").display().to_string());
    let out = Rc::new(RefCell::new(String::new()));
    let r = run(
        &Playbook::new("purged", pb),
        vec![HostSpec {
            groups: Vec::new(),
            name: "h1".into(),
            target: Target::Local(t),
            vars: v.clone(),
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    let text = format!("{}\n{}", out.borrow(), r.render());
    (r.hosts[0].status.clone(), text)
}

#[test]
fn a_purge_between_prepare_and_the_op_fails_the_op_and_a_rerun_passes() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().canonicalize().unwrap();
    for s in ["www", "out", "tmp", "stage", "bin", "run"] {
        std::fs::create_dir_all(p.join(s)).unwrap();
    }
    std::fs::write(p.join("www/a.txt"), FILE).unwrap();
    let curl = format!(
        "#!/bin/bash\nout=; url=\nwhile [ $# -gt 0 ]; do case \"$1\" in -o) out=$2; shift 2;; --) url=$2; shift 2;; *) shift;; esac; done\n\
         echo \"curl $url\" >> {d}/log\ncp {d}/www/${{url#http://fake/}} \"$out\"\n",
        d = p.display()
    );
    std::fs::write(p.join("bin/curl"), curl).unwrap();
    std::fs::set_permissions(
        p.join("bin/curl"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let sum = sha256(FILE.as_bytes());
    let target: PathBuf = p.join("out/a.txt");
    let mut v = V {
        url: "http://fake/a.txt".into(),
        sum: sum.clone(),
        target: target.display().to_string(),
        pre: Some(telepatch::ops::fetch::cache_purge_script(None, true)),
    };
    let (status, text) = go(&p, &v);
    assert!(
        matches!(status, HostStatus::Failed { .. }),
        "{status:?}\n{text}"
    );
    let gone = format!(
        "telepatch: the cached download {} for http://fake/a.txt is gone (a cache purge during the run?); run again to download it",
        &sum[..12]
    );
    assert!(text.contains(&gone), "{text}");
    assert!(!target.exists(), "nothing is placed");
    assert!(!p.join("cache").join(&sum).exists(), "the purge removed it");
    // A rerun, no purge: it downloads the file again and passes.
    v.pre = None;
    let (status, text) = go(&p, &v);
    assert_eq!(status, HostStatus::Changed, "{text}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), FILE);
}
