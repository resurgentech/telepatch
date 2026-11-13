//! The `fetch` value and the file ops that use it (`.from(f)`,
//! `.unpack(f).strip(n)`): the trace (keys, trace errors, the `Unverified`
//! policy error and its allowance) and the executor over the local transport.
//!
//! No test touches the network. A PATH-fake `curl` / `wget` copies from
//! `<d>/www/<name>` for a URL `http://fake/<name>` and logs every call to
//! `<d>/log`, so "no download call" is an empty log. The staging root and the
//! cache are `TP_FETCH_STAGE` and `TP_FETCH_CACHE` (stand-ins for
//! `/var/tmp/telepatch` and `/var/cache/telepatch/sha256`), and `TMPDIR`
//! points at an empty directory that must still be empty afterwards: nothing
//! may be left in the default temp directory. A case that needs `zip`,
//! `zstd` or `xz` to build its archive skips (prints, passes) when the
//! machine lacks it.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::facts::os::Os;
use telepatch::report::{FetchState, HostStatus, Report};
use telepatch::transport::local::LocalTransport;
use telepatch::{Host, Key, Node, NodeId, Playbook, PlaybookFuture, Policy};

const FILE: &str = "hello from the file server\n";

fn os() -> Os {
    Os::parse("ID=debian\nVERSION_ID=12\n").unwrap()
}

fn host() -> Host<()> {
    Host::new("t", os(), Rc::new(()))
}

fn errors(h: &Host<()>) -> Vec<String> {
    h.snapshot()
        .errors
        .iter()
        .map(|e| e.message().to_string())
        .collect()
}

fn op(h: &Host<()>, id: NodeId) -> telepatch::OpNode {
    match &h.snapshot().nodes[id.0] {
        Node::Op(op) => op.clone(),
        _ => panic!("not an op"),
    }
}

const SUM: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

// --- the trace ---

#[test]
fn from_writes_the_file_key_and_takes_a_fetched_by_reference_or_value() {
    let h = host();
    let f = h.fetch("https://example.com/k.gpg").sha256(SUM);
    let a = h.file("/etc/k1.gpg").from(&f).mode(0o644).id();
    let b = h.file("/etc/k2.gpg").from(f.clone()).id();
    let c = h.file("/srv/data").unpack(&f).strip(1).id();
    let d = h.file("/srv/data2").unpack(f).id();
    assert!(errors(&h).is_empty(), "{:?}", errors(&h));
    for (id, p) in [
        (a, "/etc/k1.gpg"),
        (b, "/etc/k2.gpg"),
        (c, "/srv/data"),
        (d, "/srv/data2"),
    ] {
        let o = op(&h, id);
        assert_eq!(o.spec.kind(), "file");
        assert_eq!(
            o.writes.iter().cloned().collect::<Vec<_>>(),
            vec![Key::file(p)]
        );
    }
}

#[test]
fn a_fetch_no_op_uses_is_ignored() {
    let h = host();
    let _unused = h.fetch("https://example.com/unused").sha256(SUM);
    let _no_sum = h.fetch("https://example.com/unused-too");
    assert!(h.snapshot().nodes.is_empty());
    assert!(errors(&h).is_empty());
}

#[test]
fn no_checksum_is_the_unverified_policy_error_and_an_allowance_permits_it() {
    let h = host();
    let f = h.fetch("https://example.com/blob");
    h.file("/opt/blob").from(&f);
    let t = h.snapshot();
    assert_eq!(t.errors.len(), 1, "{:?}", t.errors);
    let e = &t.errors[0];
    assert_eq!(e.policy, Some(Policy::Unverified));
    assert_eq!(
        e.message(),
        "file /opt/blob: fetch https://example.com/blob has no sha256"
    );
    let text = e.to_string();
    assert!(
        text.starts_with(
            "trace error: file /opt/blob: fetch https://example.com/blob has no sha256 (policy: unverified)\n  at "
        ),
        "{text}"
    );
    assert!(
        text.ends_with("\n  to permit: .allow(Policy::Unverified, \"<reason>\")"),
        "{text}"
    );

    // the allowance counts whichever statement it comes in
    let h = host();
    let f = h.fetch("https://example.com/blob");
    let o = h.file("/opt/blob").from(&f);
    o.allow(Policy::Unverified, "the vendor publishes no sums");
    assert!(errors(&h).is_empty(), "{:?}", errors(&h));
    let h = host();
    h.file("/opt/dir")
        .unpack(h.fetch("https://example.com/b.tgz"))
        .allow(Policy::Unverified, "no sums");
    assert!(errors(&h).is_empty(), "{:?}", errors(&h));
    // a checksum needs no allowance
    let h = host();
    h.file("/opt/blob")
        .from(h.fetch("https://example.com/blob").sha256(SUM));
    assert!(errors(&h).is_empty());
}

#[test]
fn trace_errors() {
    let h = host();
    let f = h.fetch("https://example.com/x").sha256(SUM);
    h.file("/a").from(&f).content("x");
    h.file("/b").content("x").from(&f);
    h.file("/c").from(&f).line("x");
    h.file("/d").from(&f).dir();
    h.file("/e").unpack(&f).absent();
    h.file("/f").from(&f).unpack(&f);
    h.file("/g").unpack(&f).from(&f);
    h.file("/h").strip(1);
    h.file("/i").from(&f).strip(1);
    h.file("/j").from(h.fetch("").sha256(SUM));
    h.file("/k")
        .from(h.fetch("https://example.com/x").sha256("nothex"));
    h.file("/l").from(
        h.fetch("https://example.com/x")
            .sha256(SUM.to_uppercase().as_str()),
    );
    assert_eq!(
        errors(&h),
        [
            "file /a: .from() and .content() are exclusive",
            "file /b: .content() and .from() are exclusive",
            "file /c: .from() and .line() are exclusive",
            "file /d: .from() and .dir() are exclusive",
            "file /e: .unpack() and .absent() are exclusive",
            "file /f: .from() and .unpack() are exclusive",
            "file /g: .unpack() and .from() are exclusive",
            "file /h: .strip(1) needs .unpack(..)",
            "file /i: .strip(1) needs .unpack(..)",
            "file /j: fetch has no url",
            "file /k: fetch https://example.com/x: sha256 must be 64 hex digits, got \"nothex\"",
        ],
        "an upper-case sum is accepted and is the same sum"
    );
}

// --- the executor over the local transport ---

#[derive(Clone)]
struct V {
    url: String,
    sum: Option<String>,
    target: String,
    unpack: bool,
    strip: u32,
    mode: Option<u32>,
    owner: Option<String>,
    allow: bool,
    /// A plain file written first, to prove a blocked host applies nothing.
    first: Option<String>,
    /// `.retry(n)` and `.timeout(d)` on the fetch.
    retry: Option<u32>,
    timeout: Option<std::time::Duration>,
}

impl V {
    fn from(url: &str, sum: Option<&str>, target: &Path) -> V {
        V {
            url: url.to_string(),
            sum: sum.map(str::to_string),
            target: target.display().to_string(),
            unpack: false,
            strip: 0,
            mode: None,
            owner: None,
            allow: false,
            first: None,
            retry: None,
            timeout: None,
        }
    }
    fn unpack(url: &str, sum: &str, target: &Path, strip: u32) -> V {
        V {
            unpack: true,
            strip,
            ..V::from(url, Some(sum), target)
        }
    }
}

fn pb_fetch(h: Host<V>) -> PlaybookFuture {
    Box::pin(async move {
        let v = h.vars.clone();
        if let Some(p) = &v.first {
            h.file(p).content("first");
        }
        let mut f = h.fetch(&v.url);
        if let Some(s) = &v.sum {
            f = f.sha256(s);
        }
        if let Some(n) = v.retry {
            f = f.retry(n);
        }
        if let Some(d) = v.timeout {
            f = f.timeout(d);
        }
        let o = h.file(&v.target);
        if v.unpack {
            o.unpack(&f).strip(v.strip);
        } else {
            o.from(&f);
        }
        if let Some(m) = v.mode {
            o.mode(m);
        }
        if let Some(w) = &v.owner {
            o.owner(w);
        }
        if v.allow {
            o.allow(Policy::Unverified, "the test server publishes no sums");
        }
        Ok(())
    })
}

struct Env {
    d: tempfile::TempDir,
}

impl Env {
    fn new() -> Env {
        let e = Env {
            d: tempfile::tempdir().unwrap(),
        };
        for s in ["www", "out", "tmp", "stage", "src"] {
            std::fs::create_dir_all(e.p(s)).unwrap();
        }
        e
    }
    fn p(&self, s: &str) -> PathBuf {
        self.d.path().join(s)
    }
    fn serve(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.p("www").join(name), bytes).unwrap();
    }
    /// Fake `curl` and `wget` (only the ones named) in `<d>/bin`.
    fn fakes(&self, tools: &[&str]) -> PathBuf {
        let bin = self.p("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let dd = self.d.path().display();
        for t in tools {
            let body = match *t {
                "curl" => format!(
                    r#"D={dd}
out=; url=
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out=$2; shift 2;;
    --) url=$2; shift 2;;
    *) shift;;
  esac
done
echo "curl $url" >> $D/log
n=${{url#http://fake/}}
if [ -f "$D/www/$n" ]; then cp "$D/www/$n" "$out"; else echo "curl: (22) The requested URL returned error: 404" >&2; exit 22; fi"#
                ),
                _ => format!(
                    r#"D={dd}
out=; url=
while [ $# -gt 0 ]; do
  case "$1" in
    -O) out=$2; shift 2;;
    -O*) out=${{1#-O}}; shift;;
    --) url=$2; shift 2;;
    *) shift;;
  esac
done
echo "wget $url" >> $D/log
n=${{url#http://fake/}}
if [ -f "$D/www/$n" ]; then cp "$D/www/$n" "$out"; else echo "wget: server returned error: HTTP/1.1 404 Not Found" >&2; exit 8; fi"#
                ),
            };
            let p = bin.join(t);
            std::fs::write(&p, format!("#!/bin/bash\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
        }
        bin
    }
    /// Every `/usr/bin` entry except those named (and their prefixes'
    /// siblings are kept): a host without them.
    fn farm_without(&self, names: &[&str]) -> PathBuf {
        let bin = self.p("farm");
        std::fs::create_dir_all(&bin).unwrap();
        for e in std::fs::read_dir("/usr/bin").unwrap().flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if names.contains(&n.as_str()) {
                continue;
            }
            let _ = std::os::unix::fs::symlink(e.path(), bin.join(&n));
        }
        bin
    }
    fn log(&self) -> String {
        std::fs::read_to_string(self.p("log")).unwrap_or_default()
    }
    fn clear_log(&self) {
        let _ = std::fs::remove_file(self.p("log"));
    }
    /// Run on one local host with `path` as the whole PATH.
    fn go(&self, v: &V, path: &str) -> (Report, String) {
        self.clear_log();
        let pb = Playbook::new("fetch", pb_fetch);
        let out = Rc::new(RefCell::new(String::new()));
        let report = run(
            &pb,
            vec![HostSpec {
                groups: Vec::new(),
                name: "h1".into(),
                target: Target::Local(
                    LocalTransport::new()
                        .uid(0)
                        .sandboxed_at(self.d.path())
                        .env("PATH", path)
                        .env("TMPDIR", &self.p("tmp").display().to_string())
                        .env("TP_FETCH_STAGE", &self.p("stage").display().to_string())
                        .env("TP_FETCH_CACHE", &self.p("cache").display().to_string()),
                ),
                vars: v.clone(),
            }],
            RunOpts {
                log: None,
                stdout: Stdout::Capture(out.clone()),
            },
        );
        let text = out.borrow().clone();
        (report, text)
    }
    /// Fake curl, then the real PATH.
    fn go_curl(&self, v: &V) -> (Report, String) {
        let bin = self.fakes(&["curl"]);
        self.go(
            v,
            &format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
    }
    fn assert_clean(&self) {
        assert_eq!(ls(&self.p("tmp")), Vec::<String>::new(), "TMPDIR is empty");
        assert_eq!(
            ls(&self.p("stage")),
            Vec::<String>::new(),
            "the staging dir is cleaned"
        );
    }
    /// The cache entries; the usage record and its lock (T121) are not entries.
    fn cache(&self) -> Vec<String> {
        ls(&self.p("cache"))
            .into_iter()
            .filter(|n| !n.starts_with(".telepatch-usage"))
            .collect()
    }
}

fn ls(d: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(d)
        .map(|r| {
            r.flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn sha256(bytes: &[u8]) -> String {
    use std::io::Write;
    let mut c = Command::new("sha256sum")
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

fn stat(p: &Path, fmt: &str) -> String {
    let o = Command::new("stat")
        .args(["-c", fmt])
        .arg(p)
        .output()
        .unwrap();
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

fn status(r: &Report) -> &HostStatus {
    &r.hosts[0].status
}

#[test]
fn a_download_is_verified_cached_and_a_cache_hit_makes_no_call() {
    let e = Env::new();
    e.serve("a.txt", FILE.as_bytes());
    let sum = sha256(FILE.as_bytes());
    let target = e.p("out/a.txt");
    let v = V::from("http://fake/a.txt", Some(&sum), &target);

    let (r1, out1) = e.go_curl(&v);
    assert_eq!(status(&r1), &HostStatus::Changed, "{out1}\n{}", r1.render());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), FILE);
    assert_eq!(e.log(), "curl http://fake/a.txt\n");
    assert_eq!(e.cache(), std::slice::from_ref(&sum));
    assert_eq!(
        std::fs::read_to_string(e.p("cache").join(&sum)).unwrap(),
        FILE
    );
    assert!(
        out1.contains("== h1 [prepare] fetch http://fake/a.txt: fetched, sha256 ok =="),
        "{out1}"
    );
    let f = &r1.hosts[0].fetches;
    assert_eq!(f.len(), 1, "{}", r1.render());
    assert_eq!(f[0].url, "http://fake/a.txt");
    assert_eq!(f[0].sha256.as_deref(), Some(sum.as_str()));
    assert_eq!(f[0].state, FetchState::Fetched);
    assert!(
        r1.render()
            .contains("\nh1: fetch http://fake/a.txt: fetched, sha256 ok\n"),
        "{}",
        r1.render()
    );
    e.assert_clean();

    // run 2: nothing changes and the fake is never called
    let (r2, out2) = e.go_curl(&v);
    assert_eq!(status(&r2), &HostStatus::Ok, "{out2}\n{}", r2.render());
    assert_eq!(e.log(), "", "a cache hit makes no download call");
    assert!(
        out2.contains("== h1 [prepare] fetch http://fake/a.txt: cached =="),
        "{out2}"
    );
    assert_eq!(r2.hosts[0].fetches[0].state, FetchState::Cached);
    e.assert_clean();

    // the cache holds for a second target of the same sum
    let t2 = e.p("out/b.txt");
    let (r3, out3) = e.go_curl(&V::from("http://fake/other-url", Some(&sum), &t2));
    assert_eq!(status(&r3), &HostStatus::Changed, "{out3}");
    assert_eq!(e.log(), "", "the cache is keyed by checksum, not url");
    assert_eq!(std::fs::read_to_string(&t2).unwrap(), FILE);
    e.assert_clean();
}

#[test]
fn a_corrupt_cache_entry_is_fetched_again() {
    let e = Env::new();
    e.serve("a.txt", FILE.as_bytes());
    let sum = sha256(FILE.as_bytes());
    let v = V::from("http://fake/a.txt", Some(&sum), &e.p("out/a.txt"));
    let (r1, _) = e.go_curl(&v);
    assert_eq!(r1.exit_code(), 0);
    std::fs::write(e.p("cache").join(&sum), "truncated").unwrap();
    let (r2, out2) = e.go_curl(&v);
    assert_eq!(r2.exit_code(), 0, "{out2}\n{}", r2.render());
    assert_eq!(e.log(), "curl http://fake/a.txt\n");
    assert_eq!(
        std::fs::read_to_string(e.p("cache").join(&sum)).unwrap(),
        FILE
    );
    e.assert_clean();
}

#[test]
fn wget_is_the_fallback_when_curl_is_absent() {
    let e = Env::new();
    e.serve("a.txt", FILE.as_bytes());
    let sum = sha256(FILE.as_bytes());
    let farm = e.farm_without(&["curl", "wget"]);
    let bin = e.fakes(&["wget"]);
    let v = V::from("http://fake/a.txt", Some(&sum), &e.p("out/a.txt"));
    let (r, out) = e.go(&v, &format!("{}:{}", bin.display(), farm.display()));
    assert_eq!(status(&r), &HostStatus::Changed, "{out}\n{}", r.render());
    assert_eq!(e.log(), "wget http://fake/a.txt\n");
    assert_eq!(
        std::fs::read_to_string(e.p("out/a.txt")).unwrap(),
        FILE,
        "{out}"
    );
    e.assert_clean();
}

#[test]
fn a_checksum_mismatch_blocks_the_host_before_apply_and_leaves_no_cache_entry() {
    let e = Env::new();
    e.serve("a.txt", FILE.as_bytes());
    let want = sha256(b"something else");
    let got = sha256(FILE.as_bytes());
    let mut v = V::from("http://fake/a.txt", Some(&want), &e.p("out/a.txt"));
    v.first = Some(e.p("out/first").display().to_string());
    let (r, out) = e.go_curl(&v);
    let HostStatus::Blocked(why) = status(&r) else {
        panic!("{:?}\n{out}\n{}", status(&r), r.render());
    };
    assert_eq!(
        why,
        &format!("fetch http://fake/a.txt: sha256 mismatch: want {want} got {got} (window 1)")
    );
    assert!(
        !e.p("out/first").exists() && !e.p("out/a.txt").exists(),
        "nothing was applied"
    );
    assert_eq!(e.cache(), Vec::<String>::new(), "no cache entry");
    e.assert_clean();
}

#[test]
fn a_failed_download_blocks_naming_the_url_and_the_tool_s_words() {
    let e = Env::new();
    let sum = sha256(b"x");
    let v = V::from("http://fake/missing", Some(&sum), &e.p("out/a"));
    let (r, out) = e.go_curl(&v);
    let HostStatus::Blocked(why) = status(&r) else {
        panic!("{:?}\n{out}", status(&r));
    };
    assert!(
        why.starts_with("fetch http://fake/missing: download failed (curl rc 22): ")
            && why.contains("404"),
        "{why}"
    );
    assert!(!e.p("out/a").exists());
    assert_eq!(e.cache(), Vec::<String>::new());
    e.assert_clean();
}

#[test]
fn neither_curl_nor_wget_blocks_naming_them_but_a_cache_hit_needs_neither() {
    let e = Env::new();
    e.serve("a.txt", FILE.as_bytes());
    let sum = sha256(FILE.as_bytes());
    let farm = e.farm_without(&["curl", "wget"]);
    let v = V::from("http://fake/a.txt", Some(&sum), &e.p("out/a.txt"));
    let (r, out) = e.go(&v, &farm.display().to_string());
    let HostStatus::Blocked(why) = status(&r) else {
        panic!("{:?}\n{out}", status(&r));
    };
    assert_eq!(
        why,
        "fetch http://fake/a.txt: neither curl nor wget is on the host (window 1)"
    );
    assert!(!e.p("out/a.txt").exists());
    e.assert_clean();

    // fill the cache with a tool, then run again without any
    let (r, out) = e.go_curl(&v);
    assert_eq!(r.exit_code(), 0, "{out}");
    let _ = std::fs::remove_file(e.p("out/a.txt"));
    let (r, out) = e.go(&v, &farm.display().to_string());
    assert_eq!(status(&r), &HostStatus::Changed, "{out}\n{}", r.render());
    assert_eq!(std::fs::read_to_string(e.p("out/a.txt")).unwrap(), FILE);
    e.assert_clean();
}

#[test]
fn from_places_the_file_with_mode_and_owner_and_a_second_run_changes_nothing() {
    let e = Env::new();
    e.serve("k.gpg", b"key bytes\n");
    let sum = sha256(b"key bytes\n");
    let target = e.p("out/k.gpg");
    let me = String::from_utf8(Command::new("id").arg("-un").output().unwrap().stdout)
        .unwrap()
        .trim()
        .to_string();
    let grp = String::from_utf8(Command::new("id").arg("-gn").output().unwrap().stdout)
        .unwrap()
        .trim()
        .to_string();
    let mut v = V::from("http://fake/k.gpg", Some(&sum), &target);
    v.mode = Some(0o640);
    v.owner = Some(format!("{me}:{grp}"));
    let (r1, out1) = e.go_curl(&v);
    assert_eq!(status(&r1), &HostStatus::Changed, "{out1}\n{}", r1.render());
    assert_eq!(std::fs::read(&target).unwrap(), b"key bytes\n");
    assert_eq!(stat(&target, "%a"), "640");
    assert_eq!(stat(&target, "%U:%G"), format!("{me}:{grp}"));
    // only the target in its directory: the candidate was moved
    assert_eq!(ls(&e.p("out")), ["k.gpg"]);

    let (r2, out2) = e.go_curl(&v);
    assert_eq!(status(&r2), &HostStatus::Ok, "{out2}\n{}", r2.render());
    assert!(r2.hosts[0].ops.iter().all(|o| !o.changed));

    // a different file there is replaced; the mode is kept without .mode()
    std::fs::write(&target, "stale").unwrap();
    let mut v2 = v.clone();
    v2.mode = None;
    v2.owner = None;
    let (r3, out3) = e.go_curl(&v2);
    assert_eq!(status(&r3), &HostStatus::Changed, "{out3}");
    assert_eq!(std::fs::read(&target).unwrap(), b"key bytes\n");
    e.assert_clean();
}

#[test]
fn no_checksum_is_staged_not_cached_cleaned_and_runs_only_with_the_allowance() {
    let e = Env::new();
    e.serve("blob", b"unverified bytes\n");
    let target = e.p("out/blob");
    let mut v = V::from("http://fake/blob", None, &target);

    // without the allowance the run fails before anything is sent
    let (r, out) = e.go_curl(&v);
    assert!(
        matches!(status(&r), HostStatus::Error(m) if m.contains("has no sha256") && m.contains("unverified")),
        "{:?}\n{out}",
        status(&r)
    );
    assert_eq!(e.log(), "", "nothing was downloaded");
    assert!(!target.exists());

    v.allow = true;
    let (r1, out1) = e.go_curl(&v);
    assert_eq!(status(&r1), &HostStatus::Changed, "{out1}\n{}", r1.render());
    assert_eq!(std::fs::read(&target).unwrap(), b"unverified bytes\n");
    assert_eq!(e.cache(), Vec::<String>::new(), "never cached");
    assert!(
        out1.contains("== h1 [prepare] fetch http://fake/blob: fetched, not verified =="),
        "{out1}"
    );
    assert_eq!(r1.hosts[0].fetches[0].state, FetchState::Unverified);
    assert!(
        r1.render()
            .contains("allowed unverified: the test server publishes no sums"),
        "{}",
        r1.render()
    );
    e.assert_clean();

    // every run downloads it again; the placed file is the same, so nothing changes
    let (r2, out2) = e.go_curl(&v);
    assert_eq!(status(&r2), &HostStatus::Ok, "{out2}\n{}", r2.render());
    assert_eq!(e.log(), "curl http://fake/blob\n");
    e.assert_clean();
}

// --- unpack ---

/// `<d>/src/pkg-1.0/{README,bin/tool,sub/deep/file}` and a top-level file
/// that `--strip-components=1` drops.
fn make_tree(e: &Env, label: &str) -> PathBuf {
    let root = e.p("src").join(label);
    for (p, c) in [
        ("pkg-1.0/README", "readme\n"),
        ("pkg-1.0/bin/tool", "#!/bin/sh\necho tool\n"),
        ("pkg-1.0/sub/deep/file", "deep\n"),
        ("pkg-1.0/.hidden", "hidden\n"),
    ] {
        let f = root.join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, c).unwrap();
    }
    std::fs::set_permissions(
        root.join("pkg-1.0/bin/tool"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    root
}

fn have(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool} >/dev/null 2>&1")])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Build the archive `name` from `tree`; `false` when a tool is missing.
fn make_archive(tree: &Path, out: &Path, kind: &str) -> bool {
    let need = match kind {
        "tgz" => "gzip",
        "txz" => "xz",
        "tzst" => "zstd",
        "zip" => "zip",
        _ => "tar",
    };
    if !have(need) || !have("tar") {
        eprintln!("skipping {kind}: {need} is not on this machine");
        return false;
    }
    let st = match kind {
        "tgz" => Command::new("tar")
            .args(["-czf"])
            .arg(out)
            .arg("-C")
            .arg(tree)
            .arg("pkg-1.0")
            .status(),
        "txz" => Command::new("tar")
            .args(["-cJf"])
            .arg(out)
            .arg("-C")
            .arg(tree)
            .arg("pkg-1.0")
            .status(),
        "tzst" => Command::new("tar")
            .args(["--zstd", "-cf"])
            .arg(out)
            .arg("-C")
            .arg(tree)
            .arg("pkg-1.0")
            .status(),
        "tar" => Command::new("tar")
            .args(["-cf"])
            .arg(out)
            .arg("-C")
            .arg(tree)
            .arg("pkg-1.0")
            .status(),
        _ => Command::new("zip")
            .args(["-q", "-r"])
            .arg(out)
            .arg("pkg-1.0")
            .current_dir(tree)
            .status(),
    };
    assert!(st.unwrap().success(), "building {kind}");
    true
}

fn tree_files(root: &Path) -> Vec<String> {
    let o = Command::new("find")
        .arg(".")
        .args(["-type", "f"])
        .current_dir(root)
        .output()
        .unwrap();
    let mut v: Vec<String> = String::from_utf8(o.stdout)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    v.sort();
    v
}

#[test]
fn unpack_replaces_the_directory_for_every_format_and_a_second_run_changes_nothing() {
    for kind in ["tgz", "txz", "tzst", "zip", "tar"] {
        let e = Env::new();
        let tree = make_tree(&e, kind);
        let arc = e.p("www").join(format!("pkg.{kind}"));
        if !make_archive(&tree, &arc, kind) {
            continue;
        }
        let sum = sha256(&std::fs::read(&arc).unwrap());
        let target = e.p("out/dataset");
        // an older tree is there: it is replaced, not merged into
        std::fs::create_dir_all(target.join("old")).unwrap();
        std::fs::write(target.join("old/leftover"), "x").unwrap();
        std::fs::write(target.join("README"), "stale readme").unwrap();

        let v = V::unpack(&format!("http://fake/pkg.{kind}"), &sum, &target, 1);
        let (r1, out1) = e.go_curl(&v);
        assert_eq!(
            status(&r1),
            &HostStatus::Changed,
            "{kind}: {out1}\n{}",
            r1.render()
        );
        assert_eq!(
            tree_files(&target),
            [
                "./.hidden",
                "./.telepatch-fetch",
                "./README",
                "./bin/tool",
                "./sub/deep/file"
            ],
            "{kind}"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("README")).unwrap(),
            "readme\n",
            "{kind}"
        );
        assert_eq!(stat(&target.join("bin/tool"), "%a"), "755", "{kind}");
        assert_eq!(stat(&target, "%a"), "755", "{kind}: default dir mode");
        // nothing but the target beside it: no sibling temp dir
        assert_eq!(ls(&e.p("out")), ["dataset"], "{kind}");
        e.assert_clean();

        // run 2: settled; a file added since stays (nothing was re-extracted)
        std::fs::write(target.join("added-later"), "kept").unwrap();
        let (r2, out2) = e.go_curl(&v);
        assert_eq!(
            status(&r2),
            &HostStatus::Ok,
            "{kind}: {out2}\n{}",
            r2.render()
        );
        assert_eq!(e.log(), "", "{kind}: cached");
        assert!(target.join("added-later").exists(), "{kind}");
        e.assert_clean();
    }
}

#[test]
fn unpack_without_strip_keeps_the_top_directory_and_a_new_sum_or_strip_replaces() {
    let e = Env::new();
    let tree = make_tree(&e, "t");
    let arc = e.p("www/pkg.tgz");
    assert!(make_archive(&tree, &arc, "tgz"));
    let sum = sha256(&std::fs::read(&arc).unwrap());
    let target = e.p("out/dataset");
    let v0 = V::unpack("http://fake/pkg.tgz", &sum, &target, 0);
    let (r, out) = e.go_curl(&v0);
    assert_eq!(status(&r), &HostStatus::Changed, "{out}");
    assert!(target.join("pkg-1.0/README").exists());
    let (r, out) = e.go_curl(&v0);
    assert_eq!(status(&r), &HostStatus::Ok, "{out}");

    // the same archive with another strip is another tree
    let v1 = V::unpack("http://fake/pkg.tgz", &sum, &target, 1);
    let (r, out) = e.go_curl(&v1);
    assert_eq!(status(&r), &HostStatus::Changed, "{out}");
    assert!(target.join("README").exists() && !target.join("pkg-1.0").exists());

    // a new archive (new sum) at the same url replaces the tree
    std::fs::write(tree.join("pkg-1.0/README"), "version two\n").unwrap();
    assert!(make_archive(&tree, &arc, "tgz"));
    let sum2 = sha256(&std::fs::read(&arc).unwrap());
    assert_ne!(sum, sum2);
    let v2 = V::unpack("http://fake/pkg.tgz", &sum2, &target, 1);
    let (r, out) = e.go_curl(&v2);
    assert_eq!(status(&r), &HostStatus::Changed, "{out}");
    assert_eq!(
        std::fs::read_to_string(target.join("README")).unwrap(),
        "version two\n"
    );
    // The playbook no longer fetches the first archive (T122): only the new sum stays.
    assert_eq!(
        e.cache(),
        std::slice::from_ref(&sum2),
        "the old archive is removed, the new one cached"
    );
    e.assert_clean();
}

#[test]
fn unpack_into_a_new_path_takes_mode_and_owner() {
    let e = Env::new();
    let tree = make_tree(&e, "t");
    let arc = e.p("www/pkg.tgz");
    assert!(make_archive(&tree, &arc, "tgz"));
    let sum = sha256(&std::fs::read(&arc).unwrap());
    let target = e.p("out/dataset");
    let me = String::from_utf8(Command::new("id").arg("-un").output().unwrap().stdout)
        .unwrap()
        .trim()
        .to_string();
    let mut v = V::unpack("http://fake/pkg.tgz", &sum, &target, 1);
    v.mode = Some(0o750);
    v.owner = Some(me.clone());
    let (r, out) = e.go_curl(&v);
    assert_eq!(status(&r), &HostStatus::Changed, "{out}\n{}", r.render());
    assert_eq!(stat(&target, "%a"), "750");
    assert_eq!(stat(&target.join("sub/deep/file"), "%U"), me);
    let (r, out) = e.go_curl(&v);
    assert_eq!(status(&r), &HostStatus::Ok, "{out}\n{}", r.render());
    e.assert_clean();
}

#[test]
fn something_that_is_not_an_archive_blocks_in_prepare() {
    let e = Env::new();
    e.serve("plain", b"just text\n");
    let sum = sha256(b"just text\n");
    let v = V::unpack("http://fake/plain", &sum, &e.p("out/d"), 0);
    let (r, out) = e.go_curl(&v);
    let HostStatus::Blocked(why) = status(&r) else {
        panic!("{:?}\n{out}", status(&r));
    };
    assert_eq!(
        why,
        "unpack http://fake/plain: not a tar (gz, xz, zst) or zip archive (window 1)"
    );
    assert!(!e.p("out/d").exists());
    e.assert_clean();
}

#[test]
fn a_missing_unpack_tool_blocks_in_prepare_naming_it() {
    for (kind, hide, named) in [
        ("zip", vec!["unzip", "bsdtar"], "unzip or bsdtar"),
        ("tzst", vec!["zstd"], "zstd"),
        ("txz", vec!["xz"], "xz"),
    ] {
        let e = Env::new();
        let tree = make_tree(&e, kind);
        let arc = e.p("www").join(format!("pkg.{kind}"));
        if !make_archive(&tree, &arc, kind) {
            continue;
        }
        let sum = sha256(&std::fs::read(&arc).unwrap());
        let farm = e.farm_without(&hide);
        let bin = e.fakes(&["curl"]);
        let v = V::unpack(&format!("http://fake/pkg.{kind}"), &sum, &e.p("out/d"), 1);
        let (r, out) = e.go(&v, &format!("{}:{}", bin.display(), farm.display()));
        let HostStatus::Blocked(why) = status(&r) else {
            panic!("{kind}: {:?}\n{out}", status(&r));
        };
        assert_eq!(
            why,
            &format!("unpack http://fake/pkg.{kind}: {named} not found on the host (window 1)"),
            "{kind}"
        );
        assert!(!e.p("out/d").exists(), "{kind}");
        e.assert_clean();
    }
}

#[test]
fn two_ops_on_one_fetch_download_it_once() {
    fn pb(h: Host<V>) -> PlaybookFuture {
        Box::pin(async move {
            let v = h.vars.clone();
            let f = h.fetch(&v.url).sha256(v.sum.as_deref().unwrap());
            h.file(&v.target).from(&f);
            h.file(&format!("{}.2", v.target)).from(f);
            Ok(())
        })
    }
    let e = Env::new();
    e.serve("a.txt", FILE.as_bytes());
    let sum = sha256(FILE.as_bytes());
    let v = V::from("http://fake/a.txt", Some(&sum), &e.p("out/a.txt"));
    let bin = e.fakes(&["curl"]);
    e.clear_log();
    let out = Rc::new(RefCell::new(String::new()));
    let r = run(
        &Playbook::new("two", pb),
        vec![HostSpec {
            groups: Vec::new(),
            name: "h1".into(),
            target: Target::Local(
                LocalTransport::new()
                    .uid(0)
                    .sandboxed_at(e.d.path())
                    .env(
                        "PATH",
                        &format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
                    )
                    .env("TMPDIR", &e.p("tmp").display().to_string())
                    .env("TP_FETCH_STAGE", &e.p("stage").display().to_string())
                    .env("TP_FETCH_CACHE", &e.p("cache").display().to_string()),
            ),
            vars: v,
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    assert_eq!(r.exit_code(), 0, "{}\n{}", out.borrow(), r.render());
    assert_eq!(e.log(), "curl http://fake/a.txt\n");
    assert_eq!(r.hosts[0].fetches.len(), 1);
    assert!(e.p("out/a.txt").exists() && e.p("out/a.txt.2").exists());
    e.assert_clean();
}

#[test]
fn an_allowed_unverified_unpack_is_staged_extracted_and_settled_on_the_second_run() {
    let e = Env::new();
    let tree = make_tree(&e, "t");
    let arc = e.p("www/pkg.tgz");
    assert!(make_archive(&tree, &arc, "tgz"));
    let target = e.p("out/dataset");
    let mut v = V::from("http://fake/pkg.tgz", None, &target);
    v.unpack = true;
    v.strip = 1;
    v.allow = true;
    let (r1, out1) = e.go_curl(&v);
    assert_eq!(status(&r1), &HostStatus::Changed, "{out1}\n{}", r1.render());
    assert!(target.join("README").exists());
    assert_eq!(e.cache(), Vec::<String>::new(), "never cached");
    e.assert_clean();
    let (r2, out2) = e.go_curl(&v);
    assert_eq!(status(&r2), &HostStatus::Ok, "{out2}\n{}", r2.render());
    assert_eq!(e.log(), "curl http://fake/pkg.tgz\n", "downloaded again");
    e.assert_clean();
}

#[test]
fn the_real_curl_downloads_a_file_url() {
    if !have("curl") {
        eprintln!("skipping: curl is not on this machine");
        return;
    }
    let e = Env::new();
    e.serve("a.txt", FILE.as_bytes());
    let sum = sha256(FILE.as_bytes());
    let url = format!("file://{}", e.p("www/a.txt").display());
    let v = V::from(&url, Some(&sum), &e.p("out/a.txt"));
    let (r, out) = e.go(&v, &std::env::var("PATH").unwrap());
    assert_eq!(status(&r), &HostStatus::Changed, "{out}\n{}", r.render());
    assert_eq!(std::fs::read_to_string(e.p("out/a.txt")).unwrap(), FILE);
    assert_eq!(e.cache(), [sum]);
    e.assert_clean();
}

/// Rocky's minimal image has no `cmp` (diffutils): `.from(f)` compares by
/// the checksum it already knows (found by the live gate, T81).
#[test]
fn from_needs_no_cmp_on_the_host() {
    let e = Env::new();
    e.serve("k.gpg", b"key bytes\n");
    let sum = sha256(b"key bytes\n");
    let target = e.p("out/k.gpg");
    let v = V::from("http://fake/k.gpg", Some(&sum), &target);
    let farm = e.farm_without(&["cmp"]);
    let bin = e.fakes(&["curl"]);
    let path = format!("{}:{}", bin.display(), farm.display());
    let (r1, out1) = e.go(&v, &path);
    assert_eq!(status(&r1), &HostStatus::Changed, "{out1}\n{}", r1.render());
    assert!(!out1.contains("command not found"), "{out1}");
    let (r2, out2) = e.go(&v, &path);
    assert_eq!(status(&r2), &HostStatus::Ok, "{out2}\n{}", r2.render());
    assert!(
        r2.hosts[0].ops.iter().all(|o| !o.changed),
        "{}",
        r2.render()
    );
    std::fs::write(&target, "stale").unwrap();
    let (r3, out3) = e.go(&v, &path);
    assert_eq!(status(&r3), &HostStatus::Changed, "{out3}");
    assert_eq!(std::fs::read(&target).unwrap(), b"key bytes\n");
    e.assert_clean();
}

// --- a fetch after the op that installs curl (T83a) ---

/// `h.pkg(["curl"])` (when `curl`) then `h.file(target).from(f)`; `early`
/// places the same fetch before the pkg op.
#[derive(Clone)]
struct C {
    url: String,
    sum: String,
    target: String,
    curl: bool,
    /// `h.pkg(["wget"])` in place of curl's op.
    wget: bool,
    early: Option<String>,
}

fn pb_curl(h: Host<C>) -> PlaybookFuture {
    Box::pin(async move {
        let v = h.vars.clone();
        let f = h.fetch(&v.url).sha256(&v.sum);
        if let Some(p) = &v.early {
            h.file(p).from(&f);
        }
        if v.curl {
            h.pkg(["curl"]);
        }
        if v.wget {
            h.pkg(["wget"]);
        }
        h.file(&v.target).from(&f);
        Ok(())
    })
}

impl Env {
    /// A fake package manager for every distro family in `<d>/pm`
    /// (`dpkg-query`, `apt-get`, `rpm`, `dnf`, `yum`, `pacman`): a name is
    /// installed when `<d>/inst/<name>` exists, and installing `curl` copies
    /// the fake curl (`<d>/bin/curl`) into `<d>/inst`, a directory already
    /// on the PATH. Each install is logged to `<d>/pmlog`. Returns the
    /// PATH: `<d>/pm:<d>/inst:<farm without curl, wget>`.
    fn pm_path(&self) -> String {
        self.fakes(&["curl", "wget"]);
        let pm = self.p("pm");
        let inst = self.p("inst");
        std::fs::create_dir_all(&pm).unwrap();
        std::fs::create_dir_all(&inst).unwrap();
        let dd = self.d.path().display();
        let body = format!(
            r#"#!/bin/bash
D={dd}
me=${{0##*/}}
inst() {{ for n; do case "$n" in curl|wget) cp "$D/bin/$n" "$D/inst/$n"; echo "$me install $n" >> "$D/pmlog";; esac; done; }}
has() {{ [ -e "$D/inst/$1" ]; }}
last=${{@: -1}}
case "$me" in
  dpkg-query) if has "$last"; then echo installed; else exit 1; fi;;
  rpm) has "$last" || exit 1;;
  apt-get|dnf|yum) case " $* " in *" install "*) inst "$@";; esac;;
  pacman)
    case "$1" in
      -T) has "$last" || {{ echo "$last"; exit 127; }};;
      -S) inst "$@";;
      *) exit 1;;
    esac;;
esac
"#
        );
        for t in ["dpkg-query", "apt-get", "rpm", "dnf", "yum", "pacman"] {
            let p = pm.join(t);
            std::fs::write(&p, &body).unwrap();
            std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
        }
        let farm = self.farm_without(&["curl", "wget"]);
        format!("{}:{}:{}", pm.display(), inst.display(), farm.display())
    }
    fn pmlog(&self) -> String {
        std::fs::read_to_string(self.p("pmlog")).unwrap_or_default()
    }
    /// Run `pb_curl` on one local host with `path` as the whole PATH; the
    /// report, the output and the JSON events.
    fn go_c(&self, v: &C, path: &str) -> (Report, String, Vec<serde_json::Value>) {
        self.clear_log();
        let pb = Playbook::new("curl_fetch", pb_curl);
        let out = Rc::new(RefCell::new(String::new()));
        let events = self.p("events.jsonl");
        let report = run(
            &pb,
            vec![HostSpec {
                groups: Vec::new(),
                name: "h1".into(),
                target: Target::Local(
                    LocalTransport::new()
                        .uid(0)
                        .sandboxed_at(self.d.path())
                        .env("PATH", path)
                        .env("TMPDIR", &self.p("tmp").display().to_string())
                        .env("TP_FETCH_STAGE", &self.p("stage").display().to_string())
                        .env("TP_FETCH_CACHE", &self.p("cache").display().to_string()),
                ),
                vars: v.clone(),
            }],
            RunOpts {
                log: Some(events.clone()),
                stdout: Stdout::Capture(out.clone()),
            },
        );
        let text = out.borrow().clone();
        let evs = std::fs::read_to_string(&events)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        (report, text, evs)
    }
    fn c(&self, curl: bool) -> C {
        self.serve("a.txt", FILE.as_bytes());
        C {
            url: "http://fake/a.txt".into(),
            sum: sha256(FILE.as_bytes()),
            target: self.p("out/a.txt").display().to_string(),
            curl,
            wget: false,
            early: None,
        }
    }
}

/// `(window number, op ids)` of every `window` event.
fn windows(evs: &[serde_json::Value]) -> Vec<(u64, Vec<u64>)> {
    evs.iter()
        .filter(|e| e["ev"] == "window")
        .map(|e| {
            (
                e["n"].as_u64().unwrap(),
                e["ops"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|i| i.as_u64().unwrap())
                    .collect(),
            )
        })
        .collect()
}

const SPLIT_LINE: &str = "== h1 [prepare] window 1: neither curl nor wget is on the host; split after #0 (pkg curl): #1 runs in window 2 ==";

#[test]
fn a_fetch_after_the_pkg_op_that_installs_curl_runs_after_it() {
    let e = Env::new();
    let path = e.pm_path();
    let v = e.c(true);
    let (r, out, evs) = e.go_c(&v, &path);
    assert_eq!(status(&r), &HostStatus::Changed, "{out}\n{}", r.render());
    assert_eq!(
        e.pmlog().lines().count(),
        1,
        "curl installed once: {}",
        e.pmlog()
    );
    assert_eq!(
        e.log(),
        "curl http://fake/a.txt\n",
        "the installed curl downloads"
    );
    assert_eq!(std::fs::read_to_string(&v.target).unwrap(), FILE);
    // the split, then the rest's prepare line, in that order
    let split = out
        .find(SPLIT_LINE)
        .unwrap_or_else(|| panic!("no split line:\n{out}"));
    let fetch = out
        .find("== h1 [prepare] fetch http://fake/a.txt: fetched, sha256 ok ==")
        .unwrap_or_else(|| panic!("no fetch line:\n{out}"));
    assert!(split < fetch, "{out}");
    // both ran, in order, in windows 1 and 2; no phantom window 1 of both
    assert_eq!(windows(&evs), [(1, vec![0]), (2, vec![1])], "{evs:#?}");
    let ops = &r.hosts[0].ops;
    assert_eq!(
        ops.iter().map(|o| (o.id, o.changed)).collect::<Vec<_>>(),
        [(0, true), (1, true)],
        "{}",
        r.render()
    );
    assert_eq!(r.hosts[0].fetches.len(), 1, "{}", r.render());
    assert_eq!(r.hosts[0].fetches[0].window, 2);
    let s: Vec<&serde_json::Value> = evs.iter().filter(|e| e["ev"] == "window_split").collect();
    assert_eq!(s.len(), 1, "{evs:#?}");
    assert_eq!(s[0]["window"], 1);
    assert_eq!(s[0]["after"], 0);
    assert_eq!(s[0]["tool"], "curl");
    assert_eq!(s[0]["consumer"], 1);
    assert!(
        r.render().contains(
            "\nh1: window 1: neither curl nor wget is on the host; split after #0 (pkg curl): #1 runs in window 2\n"
        ),
        "{}",
        r.render()
    );
    e.assert_clean();
}

#[test]
fn a_fetch_after_the_pkg_op_that_installs_wget_runs_after_it_through_wget() {
    let e = Env::new();
    let path = e.pm_path();
    let mut v = e.c(false);
    v.wget = true;
    let (r, out, evs) = e.go_c(&v, &path);
    assert_eq!(status(&r), &HostStatus::Changed, "{out}\n{}", r.render());
    let pm = e.pmlog();
    assert!(
        pm.lines().count() == 1 && pm.ends_with(" install wget\n"),
        "wget installed once: {pm}"
    );
    assert_eq!(
        e.log(),
        "wget http://fake/a.txt\n",
        "the installed wget downloads"
    );
    assert_eq!(std::fs::read_to_string(&v.target).unwrap(), FILE);
    let line = "== h1 [prepare] window 1: neither curl nor wget is on the host; split after #0 (pkg wget): #1 runs in window 2 ==";
    let split = out
        .find(line)
        .unwrap_or_else(|| panic!("no split line:\n{out}"));
    let fetch = out
        .find("== h1 [prepare] fetch http://fake/a.txt: fetched, sha256 ok ==")
        .unwrap_or_else(|| panic!("no fetch line:\n{out}"));
    assert!(split < fetch, "{out}");
    assert_eq!(windows(&evs), [(1, vec![0]), (2, vec![1])], "{evs:#?}");
    let s: Vec<&serde_json::Value> = evs.iter().filter(|e| e["ev"] == "window_split").collect();
    assert_eq!(s.len(), 1, "{evs:#?}");
    assert_eq!(s[0]["tool"], "wget");
    e.assert_clean();
}

#[test]
fn with_curl_on_the_host_the_window_is_not_split() {
    let e = Env::new();
    let path = e.pm_path();
    // curl is already installed (the package and the tool)
    std::fs::copy(e.p("bin/curl"), e.p("inst/curl")).unwrap();
    let v = e.c(true);
    let (r, out, evs) = e.go_c(&v, &path);
    assert_eq!(status(&r), &HostStatus::Changed, "{out}\n{}", r.render());
    assert_eq!(e.pmlog(), "", "nothing installed");
    assert_eq!(std::fs::read_to_string(&v.target).unwrap(), FILE);
    assert!(!out.contains("split"), "{out}");
    assert!(!r.render().contains("split"), "{}", r.render());
    assert_eq!(windows(&evs), [(1, vec![0, 1])], "{evs:#?}");
    assert!(evs.iter().all(|e| e["ev"] != "window_split"));
    assert_eq!(r.hosts[0].fetches[0].window, 1);
    assert!(
        out.contains("== h1 [prepare] fetch http://fake/a.txt: fetched, sha256 ok =="),
        "{out}"
    );
    e.assert_clean();
}

#[test]
fn no_downloader_and_none_installed_blocks_as_before() {
    let e = Env::new();
    let path = e.pm_path();
    let v = e.c(false);
    let (r, out, evs) = e.go_c(&v, &path);
    let HostStatus::Blocked(why) = status(&r) else {
        panic!("{:?}\n{out}", status(&r));
    };
    assert_eq!(
        why,
        "fetch http://fake/a.txt: neither curl nor wget is on the host (window 1)"
    );
    assert!(!out.contains("split"), "{out}");
    assert_eq!(windows(&evs), [(1, vec![0])]);
    assert!(!Path::new(&v.target).exists());
    e.assert_clean();
}

#[test]
fn a_fetch_before_the_pkg_op_that_installs_curl_blocks_as_before() {
    let e = Env::new();
    let path = e.pm_path();
    let mut v = e.c(true);
    v.early = Some(e.p("out/early.txt").display().to_string());
    let (r, out, _) = e.go_c(&v, &path);
    let HostStatus::Blocked(why) = status(&r) else {
        panic!("{:?}\n{out}", status(&r));
    };
    assert_eq!(
        why,
        "fetch http://fake/a.txt: neither curl nor wget is on the host (window 1)"
    );
    assert!(!out.contains("split after"), "{out}");
    assert_eq!(e.pmlog(), "", "nothing applied");
    assert!(!e.p("out/early.txt").exists());
    e.assert_clean();
}

#[test]
fn the_plan_shows_where_a_window_would_split_and_why() {
    let e = Env::new();
    let pb = Playbook::new("curl_fetch", pb_curl);
    let fh = |v: C| telepatch::testing::FixtureHost::new("h1", os(), Rc::new(v));
    let plan = telepatch::testing::plan(&pb, &fh(e.c(true)));
    let line = "  -- split after #0 (pkg curl) when the host has neither curl nor wget: #1 needs a download --";
    assert!(plan.contains(&format!("\n{line}\n")), "{plan}");
    // printed right after the installing op
    let at = plan.find(line).unwrap();
    assert!(plan.find("  #1 file").unwrap() > at, "{plan}");
    // no installer, or no consumer after it: no line
    assert!(!telepatch::testing::plan(&pb, &fh(e.c(false))).contains("split"));
    let mut early = e.c(false);
    early.early = Some("/x".into());
    assert!(!telepatch::testing::plan(&pb, &fh(early)).contains("split"));
}

/// `pkg curl`, a `.from`, then an `sh!` (it writes `*`: the apt cache is
/// stale after it).
fn pb_stale(h: Host<C>) -> PlaybookFuture {
    Box::pin(async move {
        let v = h.vars.clone();
        let f = h.fetch(&v.url).sha256(&v.sum);
        h.pkg(["curl"]);
        h.file(&v.target).from(&f);
        telepatch::sh!(h, "true");
        Ok(())
    })
}

#[test]
fn the_engine_runs_the_head_then_the_rest_and_a_declined_window_notes_nothing() {
    let e = Env::new();
    let pb = Playbook::new("stale", pb_stale);
    let fh = telepatch::testing::FixtureHost::new("h1", os(), Rc::new(e.c(true)));
    let whole = telepatch::testing::plan(&pb, &fh);
    assert!(whole.contains("\nwindow 1 (final)\n"), "{whole}");
    assert!(!whole.contains("\nwindow 2"), "{whole}");
    assert!(whole.contains("  -- split after #0 (pkg curl)"), "{whole}");

    let split = telepatch::testing::plan_with(
        &pb,
        &fh,
        telepatch::testing::fake::FakeExecutor::new().without_downloader(),
    );
    let (w1, w2) = split
        .split_once("\nwindow 2 (final)\n")
        .unwrap_or_else(|| panic!("no window 2:\n{split}"));
    assert!(w1.contains("\nwindow 1 (final)\n"), "{split}");
    assert!(w1.contains("  #0 pkg"), "{split}");
    assert!(
        !w1.contains("  #1 file") && !w1.contains("  #2 sh"),
        "{split}"
    );
    assert!(
        w2.contains("  #1 file") && w2.contains("  #2 sh"),
        "{split}"
    );
    assert!(
        !split.contains("-- split"),
        "neither piece splits again:\n{split}"
    );
    // the declined window's `sh!` made the apt cache stale; the head did
    // not inherit that: its pkg op does not update first
    assert!(
        !w1.contains("      | apt-get -q update || rc=$?"),
        "{split}"
    );
    assert_eq!(
        whole.matches("      | apt-get -q update || rc=$?").count(),
        0,
        "{whole}"
    );
}

fn pb_fetch_plan(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        let f = h.fetch("https://example.invalid/tool.bin");
        h.file("/opt/tool/tool.bin")
            .from(&f)
            .allow(Policy::Unverified, "the test server publishes no sums");
        Ok(())
    })
}

/// A no-sum `.from` fetch renders its staging path from `$__tp_run`, so the
/// plan is the same in every process.
#[test]
fn fetch_plan() {
    let pb = Playbook::new("fetch_plan", pb_fetch_plan);
    let plan = telepatch::testing::plan_for("debian12", &pb, ());
    assert!(plan.contains("$__tp_run"), "{plan}");

    insta::assert_snapshot!("fetch_plan", plan);
}

// --- T113: .retry and .timeout on a fetch ---

const SECS: fn(u64) -> std::time::Duration = std::time::Duration::from_secs;

impl Env {
    /// A `curl` for these tests, in `<d>/bin` ahead of the real PATH. Each
    /// call is logged (`curl <args>`). Calls up to the `fail_first`-th
    /// fail with rc 7. Then it serves `www/<name>`, or, with `hang`, it
    /// sleeps `sleep <hang>` (an odd number, so the test can look for the
    /// process) instead of answering: with `honours`, it behaves as curl
    /// does under `--max-time m` (gives up after `m` seconds with rc 28);
    /// without, it ignores the flag.
    fn curl_that(&self, fail_first: u32, hang: Option<u32>, honours: bool) -> String {
        let bin = self.p("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let dd = self.d.path().display();
        let hang_part = match hang {
            None => String::new(),
            Some(n) if honours => format!(
                "if [ -n \"$max\" ]; then sleep \"$max\"; echo 'curl: (28) Operation timed out' >&2; exit 28; fi\nsleep {n}\n"
            ),
            Some(n) => format!("sleep {n}\n"),
        };
        let body = format!(
            r#"D={dd}
out=; url=; max=
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out=$2; shift 2;;
    --max-time) max=$2; shift 2;;
    --) url=$2; shift 2;;
    *) shift;;
  esac
done
echo "curl max=$max $url" >> $D/log
{hang_part}n=$(wc -l < $D/log)
if [ "$n" -le {fail_first} ]; then echo "curl: (7) Failed to connect" >&2; exit 7; fi
cp "$D/www/${{url#http://fake/}}" "$out""#
        );
        let f = bin.join("curl");
        std::fs::write(&f, format!("#!/bin/bash\n{body}\n")).unwrap();
        std::fs::set_permissions(&f, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        format!("{}:{}", bin.display(), std::env::var("PATH").unwrap())
    }
}

fn limited(e: &Env, retry: Option<u32>, timeout: Option<std::time::Duration>) -> V {
    e.serve("a", FILE.as_bytes());
    let mut v = V::from(
        "http://fake/a",
        Some(&sha256(FILE.as_bytes())),
        &e.p("out/a"),
    );
    v.retry = retry;
    v.timeout = timeout;
    v
}

#[test]
fn a_retried_fetch_downloads_again_after_a_failure_and_places_the_file() {
    let e = Env::new();
    let v = limited(&e, Some(2), Some(SECS(30)));
    let path = e.curl_that(1, None, true);
    let (r, out) = e.go(&v, &path);
    assert_eq!(status(&r), &HostStatus::Changed, "{out}\n{}", r.render());
    assert_eq!(std::fs::read_to_string(e.p("out/a")).unwrap(), FILE);
    assert_eq!(e.log().lines().count(), 2, "{}", e.log());
    e.assert_clean();
}

#[test]
fn a_fetch_without_retry_gives_up_after_one_failure() {
    let e = Env::new();
    let v = limited(&e, None, Some(SECS(30)));
    let path = e.curl_that(1, None, true);
    let (r, out) = e.go(&v, &path);
    let HostStatus::Blocked(why) = status(&r) else {
        panic!("{:?}\n{out}", status(&r));
    };
    assert!(why.contains("download failed (curl rc 7)"), "{why}");
    assert_eq!(e.log().lines().count(), 1);
}

#[test]
fn a_checksum_mismatch_is_not_retried() {
    let e = Env::new();
    let mut v = limited(&e, Some(3), Some(SECS(30)));
    v.sum = Some(sha256(b"something else"));
    let path = e.curl_that(0, None, true);
    let (r, out) = e.go(&v, &path);
    let HostStatus::Blocked(why) = status(&r) else {
        panic!("{:?}\n{out}", status(&r));
    };
    assert!(why.contains("sha256 mismatch"), "{why}");
    assert_eq!(e.log().lines().count(), 1, "{}", e.log());
}

#[test]
fn every_attempt_is_bounded_by_the_downloader_s_own_flag_and_the_default_is_ten_minutes() {
    let e = Env::new();
    let path = e.curl_that(0, None, true);
    let v = limited(&e, None, Some(SECS(45)));
    let (r, out) = e.go(&v, &path);
    assert_eq!(status(&r), &HostStatus::Changed, "{out}\n{}", r.render());
    assert!(e.log().contains("curl max=45 http://fake/a"), "{}", e.log());
    let e2 = Env::new();
    let path = e2.curl_that(0, None, true);
    let v = limited(&e2, None, None);
    let (r, out) = e2.go(&v, &path);
    assert_eq!(status(&r), &HostStatus::Changed, "{out}\n{}", r.render());
    assert!(
        e2.log().contains("curl max=600 http://fake/a"),
        "{}",
        e2.log()
    );
}

#[test]
fn a_fetch_that_hangs_is_stopped_by_curl_max_time_and_blocks_naming_it() {
    let e = Env::new();
    let v = limited(&e, None, Some(SECS(1)));
    let path = e.curl_that(0, Some(977), true);
    let t = std::time::Instant::now();
    let (r, out) = e.go(&v, &path);
    assert!(t.elapsed() < SECS(20), "{:?}", t.elapsed());
    let HostStatus::Blocked(why) = status(&r) else {
        panic!("{:?}\n{out}", status(&r));
    };
    assert_eq!(
        why,
        "fetch http://fake/a: download timed out after 1s (window 1)"
    );
    assert!(!e.p("out/a").exists());
    e.assert_clean();
}

#[test]
fn a_timed_out_download_is_retried_with_a_fresh_flag_each_time() {
    let e = Env::new();
    let v = limited(&e, Some(1), Some(SECS(1)));
    let path = e.curl_that(0, Some(977), true);
    let (r, out) = e.go(&v, &path);
    let HostStatus::Blocked(why) = status(&r) else {
        panic!("{:?}\n{out}", status(&r));
    };
    assert!(why.contains("download timed out after 1s"), "{why}");
    assert_eq!(e.log().lines().count(), 2, "{}", e.log());
}

#[test]
fn a_wget_timeout_blocks_as_a_timed_out_download() {
    let e = Env::new();
    let v = limited(&e, None, Some(SECS(1)));
    let bin = e.p("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let wget = bin.join("wget");
    // wget's `--timeout=n` bounds a stall; on one it exits 4.
    std::fs::write(
        &wget,
        format!(
            "#!/bin/bash\necho \"wget $*\" >> {}/log\necho 'failed: Connection timed out.' >&2\nexit 4\n",
            e.d.path().display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wget, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let farm = e.farm_without(&["curl", "wget"]);
    let (r, out) = e.go(&v, &format!("{}:{}", bin.display(), farm.display()));
    let HostStatus::Blocked(why) = status(&r) else {
        panic!("{:?}\n{out}", status(&r));
    };
    assert_eq!(
        why,
        "fetch http://fake/a: download timed out after 1s (window 1)"
    );
    assert!(e.log().contains("--timeout=1"), "{}", e.log());
}

/// Is a process whose command line has `needle` running?
fn running(needle: &str) -> bool {
    Command::new("pgrep")
        .args(["-f", needle])
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
}

#[test]
fn a_curl_that_ignores_max_time_is_stopped_by_the_controller_and_nothing_is_left() {
    let e = Env::new();
    // The bound is (1 s + 5 s) for the one attempt plus 30 s of slack.
    let v = limited(&e, None, Some(SECS(1)));
    let path = e.curl_that(0, Some(978), false);
    let t = std::time::Instant::now();
    let (r, out) = e.go(&v, &path);
    let took = t.elapsed();
    let HostStatus::Blocked(why) = status(&r) else {
        panic!("{:?}\n{out}", status(&r));
    };
    assert_eq!(
        why,
        "fetch http://fake/a: download timed out after 1s (stopped by the controller) (window 1)"
    );
    assert!(took >= SECS(30), "stopped before its bound: {took:?}");
    assert!(took < SECS(60), "{took:?}");
    assert!(!running("sleep 978"), "the download was left running");
    assert!(!e.p("out/a").exists());
    e.assert_clean();
}

#[test]
fn a_zero_fetch_timeout_is_a_trace_error() {
    let h = host();
    h.file("/a").from(
        h.fetch("https://example.com/x")
            .sha256(SUM)
            .timeout(std::time::Duration::ZERO),
    );
    assert_eq!(
        errors(&h),
        ["file /a: fetch https://example.com/x: timeout(..): the duration must be more than zero"]
    );
}

fn pb_two_limits(h: Host<V>) -> PlaybookFuture {
    Box::pin(async move {
        let v = h.vars.clone();
        let sum = v.sum.clone().unwrap();
        let f1 = h.fetch(&v.url).sha256(&sum).timeout(SECS(5)).retry(0);
        let f2 = h.fetch(&v.url).sha256(&sum).timeout(SECS(45)).retry(1);
        h.file(&format!("{}.1", v.target)).from(&f1);
        h.file(&format!("{}.2", v.target)).from(&f2);
        Ok(())
    })
}

#[test]
fn two_fetches_of_one_url_download_once_with_the_longest_timeout() {
    let e = Env::new();
    let v = limited(&e, None, None);
    let path = e.curl_that(0, None, true);
    e.clear_log();
    let out = Rc::new(RefCell::new(String::new()));
    let r = run(
        &Playbook::new("fetch", pb_two_limits),
        vec![HostSpec {
            groups: Vec::new(),
            name: "h1".into(),
            target: Target::Local(
                LocalTransport::new()
                    .uid(0)
                    .sandboxed_at(e.d.path())
                    .env("PATH", &path)
                    .env("TMPDIR", &e.p("tmp").display().to_string())
                    .env("TP_FETCH_STAGE", &e.p("stage").display().to_string())
                    .env("TP_FETCH_CACHE", &e.p("cache").display().to_string()),
            ),
            vars: v,
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    assert_eq!(status(&r), &HostStatus::Changed, "{}", r.render());
    assert_eq!(e.log().lines().count(), 1, "{}", e.log());
    assert!(e.log().contains("curl max=45 "), "{}", e.log());
}

// --- T119: a download lands on its target's filesystem ---
//
// A filesystem of its own is the `TP_FS_ROOTS` seam: `<d>/fsB` (and `<d>/fsC`)
// are roots of their own. The stage and the main cache are on the temp
// dir's real filesystem. A wrapper `curl` records the `-o` path of every
// download, and a wrapper `tar` the `-C` directory of every unpack.

/// One consumer of a fetch: a `.from` or an `.unpack` at `target`.
#[derive(Clone)]
struct Tgt {
    url: String,
    sum: Option<String>,
    target: String,
    unpack: bool,
    allow: bool,
}

#[derive(Clone)]
struct W {
    d: PathBuf,
    sets: Vec<Tgt>,
    /// Directories made by ops before the fetch ops (not there at prepare).
    dirs: Vec<String>,
    /// The ops run in `as_user("root")`'s login shell.
    login: bool,
    /// Write the stage and `<d>/fsB/.telepatch` listings after the ops.
    mid: bool,
}

impl Tgt {
    fn from(url: &str, sum: Option<&str>, target: &Path) -> Tgt {
        Tgt {
            url: url.to_string(),
            sum: sum.map(str::to_string),
            target: target.display().to_string(),
            unpack: false,
            allow: sum.is_none(),
        }
    }
    fn unpack(url: &str, sum: &str, target: &Path) -> Tgt {
        Tgt {
            unpack: true,
            ..Tgt::from(url, Some(sum), target)
        }
    }
}

fn pb_w(h: Host<W>) -> PlaybookFuture {
    Box::pin(async move {
        let w = h.vars.clone();
        for d in &w.dirs {
            h.file(d).dir();
        }
        for t in &w.sets {
            let mut f = h.fetch(&t.url);
            if let Some(s) = &t.sum {
                f = f.sha256(s);
            }
            let o = if w.login {
                h.as_user("root").file(&t.target)
            } else {
                h.file(&t.target)
            };
            if t.unpack {
                o.unpack(&f).strip(1);
            } else {
                o.from(&f);
            }
            if t.allow {
                o.allow(Policy::Unverified, "the test server publishes no sums");
            }
        }
        if w.mid {
            let d = w.d.display();
            h.raw(format!(
                "S=\"$TP_FETCH_STAGE/$__tp_run\"\n\
                 for f in \"$S\"/*; do if [ ! -e \"$f\" ] && [ ! -L \"$f\" ]; then :; elif [ -L \"$f\" ]; then echo \"link ${{f##*/}} $(readlink \"$f\")\"; else echo \"other ${{f##*/}}\"; fi; done > {d}/mid_stage\n\
                 cat \"$S/fsroots\" > {d}/mid_roots 2>/dev/null\n\
                 find {d}/fsB/.telepatch -mindepth 1 -printf '%P %y %m\\n' 2>/dev/null | sed \"s/$__tp_run/<run>/\" | sort > {d}/mid_fsb\n\
                 true"
            ));
        }
        Ok(())
    })
}

impl Env {
    fn fsb(&self) -> PathBuf {
        self.p("fsB")
    }
    /// Wrappers in `<d>/wrap`: `curl` records its `-o` file, `tar` its
    /// `-C` directory, then each runs the real one.
    fn wrappers(&self) -> PathBuf {
        let fake = self.fakes(&["curl"]);
        let wrap = self.p("wrap");
        std::fs::create_dir_all(&wrap).unwrap();
        let dd = self.d.path().display();
        let real_tar = String::from_utf8(
            Command::new("sh")
                .args(["-c", "command -v tar"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        for (n, body) in [
            (
                "curl",
                format!(
                    "a=(\"$@\"); for ((i=0;i<${{#a[@]}};i++)); do [ \"${{a[i]}}\" = -o ] && echo \"${{a[i+1]}}\" >> {dd}/outlog; done\nexec {}/curl \"$@\"",
                    fake.display()
                ),
            ),
            (
                "tar",
                format!(
                    "a=(\"$@\"); for ((i=0;i<${{#a[@]}};i++)); do [ \"${{a[i]}}\" = -C ] && echo \"${{a[i+1]}}\" >> {dd}/tarlog; done\nexec {real_tar} \"$@\""
                ),
            ),
        ] {
            let p = wrap.join(n);
            std::fs::write(&p, format!("#!/bin/bash\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
        }
        wrap
    }
    fn outlog(&self) -> Vec<String> {
        std::fs::read_to_string(self.p("outlog"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
    /// Run `w` with `roots` as filesystems of their own.
    fn go_w(&self, w: &W, roots: &[PathBuf]) -> (Report, String) {
        self.clear_log();
        for f in ["outlog", "tarlog", "mid_stage", "mid_roots", "mid_fsb"] {
            let _ = std::fs::remove_file(self.p(f));
        }
        let wrap = self.wrappers();
        let mut path = format!(
            "{}:{}:{}",
            wrap.display(),
            self.p("bin").display(),
            std::env::var("PATH").unwrap()
        );
        let mut t = LocalTransport::new();
        if w.login {
            let (b, home) = (self.p("lbin"), self.p("home"));
            std::fs::create_dir_all(&b).unwrap();
            std::fs::create_dir_all(&home).unwrap();
            let g = b.join("getent");
            std::fs::write(
                &g,
                format!(
                    "#!/bin/bash\n[ \"$1 $2\" = \"passwd root\" ] || exit 2\necho 'root:x:0:0::{}:/bin/bash'\n",
                    home.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&g, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
            path = format!("{}:{path}", b.display());
        }
        t = t
            .uid(0)
            .sandboxed_at(self.d.path())
            .env("PATH", &path)
            .env("TMPDIR", &self.p("tmp").display().to_string())
            .env("TP_FETCH_STAGE", &self.p("stage").display().to_string())
            .env("TP_FETCH_CACHE", &self.p("cache").display().to_string())
            .env(
                "TP_FS_ROOTS",
                &roots
                    .iter()
                    .map(|r| r.display().to_string())
                    .collect::<Vec<_>>()
                    .join(":"),
            );
        let out = Rc::new(RefCell::new(String::new()));
        let report = run(
            &Playbook::new("fetch", pb_w),
            vec![HostSpec {
                groups: Vec::new(),
                name: "h1".into(),
                target: Target::Local(t),
                vars: w.clone(),
            }],
            RunOpts {
                log: None,
                stdout: Stdout::Capture(out.clone()),
            },
        );
        let text = out.borrow().clone();
        (report, text)
    }
    fn mid(&self, f: &str) -> String {
        std::fs::read_to_string(self.p(f)).unwrap_or_default()
    }
    fn w(&self, sets: Vec<Tgt>) -> W {
        W {
            d: self.d.path().to_path_buf(),
            sets,
            dirs: Vec::new(),
            login: false,
            mid: true,
        }
    }
    /// The archive `pkg.tgz` (a `pkg-1.0/` tree), served; its sum.
    fn serve_tgz(&self) -> String {
        let tree = make_tree(self, "t119");
        assert!(make_archive(&tree, &self.p("www/pkg.tgz"), "tgz"));
        sha256(&std::fs::read(self.p("www/pkg.tgz")).unwrap())
    }
}

fn mode(p: &Path) -> String {
    stat(p, "%a")
}

#[test]
fn a_from_and_an_unpack_under_another_filesystem_download_and_cache_there_and_the_stage_holds_links()
 {
    let e = Env::new();
    let fsb = e.fsb();
    std::fs::create_dir_all(fsb.join("etc")).unwrap();
    std::fs::create_dir_all(fsb.join("opt")).unwrap();
    e.serve("a.txt", FILE.as_bytes());
    let sa = sha256(FILE.as_bytes());
    let st = e.serve_tgz();
    let w = e.w(vec![
        Tgt::from("http://fake/a.txt", Some(&sa), &fsb.join("etc/a.txt")),
        Tgt::unpack("http://fake/pkg.tgz", &st, &fsb.join("opt/pkg")),
    ]);
    let (r, out) = e.go_w(&w, std::slice::from_ref(&fsb));
    assert_eq!(status(&r), &HostStatus::Changed, "{out}\n{}", r.render());
    assert_eq!(
        std::fs::read(fsb.join("etc/a.txt")).unwrap(),
        FILE.as_bytes()
    );
    assert_eq!(
        std::fs::read_to_string(fsb.join("opt/pkg/README")).unwrap(),
        "readme\n"
    );
    // each was downloaded once, onto fsB
    let log = e.log();
    assert_eq!(log.lines().count(), 2, "{log}");
    let down = e.outlog();
    let pre = format!("{}/.telepatch/tmp/", fsb.display());
    assert_eq!(down.len(), 2, "{down:?}");
    for (i, d) in down.iter().enumerate() {
        assert!(
            d.starts_with(&pre) && d.ends_with(&format!("/fetch/{i}.part")),
            "{d}"
        );
    }
    // mid-run: the stage holds links (and its index) and nothing else ...
    let c = format!("{}/.telepatch/cache/sha256", fsb.display());
    let mut want = vec![
        "link f-".to_string() + &sa + " " + &c + "/" + &sa,
        "link f-".to_string() + &st + " " + &c + "/" + &st,
        "other fsroots".to_string(),
    ];
    want.sort();
    let mut got: Vec<String> = e.mid("mid_stage").lines().map(str::to_string).collect();
    got.sort();
    assert_eq!(got, want);
    assert_eq!(e.mid("mid_roots"), format!("{}\n", fsb.display()));
    // ... and fsB holds the chain and the cache entries, root-owned 0644/0755
    let fs = e.mid("mid_fsb");
    for line in [
        "cache d 755",
        "cache/sha256 d 755",
        "tmp d 711",
        "tmp/<run> d 711",
        "tmp/<run>/fetch d 711",
    ] {
        assert!(fs.lines().any(|l| l == line), "{line}\n{fs}");
    }
    for s in [&sa, &st] {
        let line = format!("cache/sha256/{s} f 644");
        assert!(fs.lines().any(|l| l == line), "{line}\n{fs}");
        assert_eq!(mode(&fsb.join(".telepatch/cache/sha256").join(s)), "644");
    }
    assert!(!fs.contains(".part"), "{fs}");
    // after the run: the cache stays on fsB, the main cache was not used,
    // and the run's tmp is gone
    assert_eq!(e.cache(), Vec::<String>::new());
    // (the usage record and its lock, T121, are beside the entries)
    assert_eq!(ls(&fsb.join(".telepatch/cache/sha256")), {
        let mut v = vec![
            sa.clone(),
            st.clone(),
            ".telepatch-usage".to_string(),
            ".telepatch-usage.lock".to_string(),
        ];
        v.sort();
        v
    });
    assert_eq!(
        ls(&fsb.join(".telepatch")),
        ["cache"],
        "tmp goes when empty"
    );
    e.assert_clean();
    // the unpack's scratch tree is a sibling of the target, not under the
    // stage nor the tmp
    let tl = std::fs::read_to_string(e.p("tarlog")).unwrap();
    for l in tl.lines() {
        assert!(
            l.starts_with(&format!("{}/opt/pkg.tp", fsb.display())),
            "scratch tree {l}"
        );
    }
    assert_eq!(tl.lines().count(), 1, "{tl}");
}

#[test]
fn a_target_on_the_stage_s_filesystem_stages_and_caches_as_before() {
    let e = Env::new();
    let fsb = e.fsb();
    std::fs::create_dir_all(&fsb).unwrap();
    e.serve("a.txt", FILE.as_bytes());
    let sa = sha256(FILE.as_bytes());
    let w = e.w(vec![Tgt::from(
        "http://fake/a.txt",
        Some(&sa),
        &e.p("out/a.txt"),
    )]);
    let (r, out) = e.go_w(&w, std::slice::from_ref(&fsb));
    assert_eq!(status(&r), &HostStatus::Changed, "{out}\n{}", r.render());
    assert_eq!(std::fs::read(e.p("out/a.txt")).unwrap(), FILE.as_bytes());
    let down = e.outlog();
    assert_eq!(down.len(), 1, "{down:?}");
    assert!(
        down[0].starts_with(&format!("{}/", e.p("stage").display()))
            && down[0].ends_with("/0.part"),
        "{down:?}"
    );
    assert_eq!(e.cache(), std::slice::from_ref(&sa));
    assert_eq!(e.mid("mid_stage"), "", "no links, no index");
    assert!(!fsb.join(".telepatch").exists(), "fsB is not touched");
}

#[test]
fn a_target_directory_that_does_not_exist_yet_uses_its_nearest_ancestor() {
    let e = Env::new();
    let fsb = e.fsb();
    std::fs::create_dir_all(&fsb).unwrap();
    e.serve("a.txt", FILE.as_bytes());
    let sa = sha256(FILE.as_bytes());
    let t = fsb.join("new/deep/dir/a.txt");
    let mut w = e.w(vec![Tgt::from("http://fake/a.txt", Some(&sa), &t)]);
    w.dirs = vec![fsb.join("new/deep/dir").display().to_string()];
    let (r, out) = e.go_w(&w, std::slice::from_ref(&fsb));
    assert_eq!(status(&r), &HostStatus::Changed, "{out}\n{}", r.render());
    assert_eq!(std::fs::read(&t).unwrap(), FILE.as_bytes());
    let down = e.outlog();
    assert!(
        down.len() == 1 && down[0].starts_with(&format!("{}/.telepatch/tmp/", fsb.display())),
        "{down:?}"
    );
    assert_eq!(e.cache(), Vec::<String>::new());
    assert!(fsb.join(".telepatch/cache/sha256").join(&sa).is_file());
    e.assert_clean();
}

#[test]
fn one_url_for_two_filesystems_is_downloaded_once_on_the_first_consumer_s() {
    let e = Env::new();
    let (fsb, fsc) = (e.fsb(), e.p("fsC"));
    std::fs::create_dir_all(&fsb).unwrap();
    std::fs::create_dir_all(&fsc).unwrap();
    e.serve("a.txt", FILE.as_bytes());
    let sa = sha256(FILE.as_bytes());
    let w = e.w(vec![
        Tgt::from("http://fake/a.txt", Some(&sa), &fsb.join("a.txt")),
        Tgt::from("http://fake/a.txt", Some(&sa), &fsc.join("b.txt")),
    ]);
    let (r, out) = e.go_w(&w, &[fsb.clone(), fsc.clone()]);
    assert_eq!(status(&r), &HostStatus::Changed, "{out}\n{}", r.render());
    assert_eq!(e.log().lines().count(), 1, "{}", e.log());
    assert_eq!(std::fs::read(fsb.join("a.txt")).unwrap(), FILE.as_bytes());
    assert_eq!(std::fs::read(fsc.join("b.txt")).unwrap(), FILE.as_bytes());
    let down = e.outlog();
    assert!(
        down.len() == 1 && down[0].starts_with(&format!("{}/.telepatch/tmp/", fsb.display())),
        "first consumer's filesystem: {down:?}"
    );
    assert!(
        e.mid("mid_stage").contains(&format!(
            "link f-{sa} {}/.telepatch/cache/sha256/{sa}",
            fsb.display()
        )),
        "{}",
        e.mid("mid_stage")
    );
    assert!(!fsc.join(".telepatch").exists(), "nothing staged on fsC");
    // the op that copies across names the move once; the first consumer,
    // whose target is where the file was staged, says nothing
    let note = format!(
        "telepatch: fetch http://fake/a.txt: staged on {}, target now on {}",
        fsb.display(),
        fsc.display()
    );
    assert_eq!(out.matches(&note).count(), 1, "{out}");
    assert_eq!(out.matches("target now on").count(), 1, "{out}");
    assert_eq!(e.cache(), Vec::<String>::new());
    e.assert_clean();
}

#[test]
fn a_second_run_hits_the_cache_on_the_other_filesystem_with_no_download() {
    let e = Env::new();
    let fsb = e.fsb();
    std::fs::create_dir_all(&fsb).unwrap();
    e.serve("a.txt", FILE.as_bytes());
    let sa = sha256(FILE.as_bytes());
    let t = fsb.join("a.txt");
    let w = e.w(vec![Tgt::from("http://fake/a.txt", Some(&sa), &t)]);
    let (r1, out1) = e.go_w(&w, std::slice::from_ref(&fsb));
    assert_eq!(status(&r1), &HostStatus::Changed, "{out1}");
    assert_eq!(e.log().lines().count(), 1);
    // run 2: the target is gone, the cache entry on fsB is what places it
    std::fs::remove_file(&t).unwrap();
    let (r2, out2) = e.go_w(&w, std::slice::from_ref(&fsb));
    assert_eq!(status(&r2), &HostStatus::Changed, "{out2}\n{}", r2.render());
    assert_eq!(e.log(), "", "no curl call");
    assert_eq!(e.outlog(), Vec::<String>::new());
    assert_eq!(std::fs::read(&t).unwrap(), FILE.as_bytes());
    assert_eq!(r2.hosts[0].fetches[0].state, FetchState::Cached);
    assert!(
        e.mid("mid_stage").contains(&format!(
            "link f-{sa} {}/.telepatch/cache/sha256/{sa}",
            fsb.display()
        )),
        "{}",
        e.mid("mid_stage")
    );
    // the cache is never removed by the run's cleanup
    assert!(fsb.join(".telepatch/cache/sha256").join(&sa).is_file());
    assert_eq!(ls(&fsb.join(".telepatch")), ["cache"]);
    // run 3: settled, still no call
    let (r3, out3) = e.go_w(&w, std::slice::from_ref(&fsb));
    assert_eq!(status(&r3), &HostStatus::Ok, "{out3}");
    assert_eq!(e.log(), "");
    e.assert_clean();
}

#[test]
fn an_unverified_download_is_staged_on_the_other_filesystem_and_cleaned() {
    let e = Env::new();
    let fsb = e.fsb();
    std::fs::create_dir_all(&fsb).unwrap();
    e.serve("blob", b"unverified bytes\n");
    let t = fsb.join("blob");
    let w = e.w(vec![Tgt::from("http://fake/blob", None, &t)]);
    let (r, out) = e.go_w(&w, std::slice::from_ref(&fsb));
    assert_eq!(status(&r), &HostStatus::Changed, "{out}\n{}", r.render());
    assert_eq!(std::fs::read(&t).unwrap(), b"unverified bytes\n");
    assert_eq!(r.hosts[0].fetches[0].state, FetchState::Unverified);
    let ms = e.mid("mid_stage");
    let l = ms.lines().find(|l| l.starts_with("link u-")).expect(&ms);
    let (name, to) = l["link ".len()..].split_once(' ').unwrap();
    assert!(
        to.starts_with(&format!("{}/.telepatch/tmp/", fsb.display()))
            && to.ends_with(&format!("/fetch/{name}")),
        "{l}"
    );
    assert_eq!(e.cache(), Vec::<String>::new(), "never cached");
    assert!(
        !fsb.join(".telepatch").exists(),
        "nothing is left on fsB: {:?}",
        ls(&fsb)
    );
    e.assert_clean();
}

#[test]
fn an_unwritable_other_filesystem_blocks_the_host_with_no_fallback_to_the_stage() {
    use std::os::unix::fs::PermissionsExt;
    if Command::new("id").arg("-u").output().unwrap().stdout == b"0\n" {
        eprintln!("skipping: root writes anywhere");
        return;
    }
    let e = Env::new();
    let fsb = e.fsb();
    std::fs::create_dir_all(&fsb).unwrap();
    e.serve("a.txt", FILE.as_bytes());
    let sa = sha256(FILE.as_bytes());
    std::fs::create_dir_all(e.p("out")).unwrap();
    let mut w = e.w(vec![Tgt::from(
        "http://fake/a.txt",
        Some(&sa),
        &fsb.join("a.txt"),
    )]);
    w.mid = false;
    std::fs::set_permissions(&fsb, std::fs::Permissions::from_mode(0o555)).unwrap();
    let (r, out) = e.go_w(&w, std::slice::from_ref(&fsb));
    std::fs::set_permissions(&fsb, std::fs::Permissions::from_mode(0o755)).unwrap();
    let HostStatus::Blocked(why) = status(&r) else {
        panic!("{:?}\n{out}\n{}", status(&r), r.render());
    };
    let pre = format!(
        "fetch http://fake/a.txt: cannot stage on {}: ",
        fsb.display()
    );
    assert!(
        why.starts_with(&pre) && why.ends_with(" (window 1)"),
        "{why}"
    );
    assert_eq!(e.log(), "", "nothing was downloaded");
    assert!(!fsb.join("a.txt").exists());
    assert_eq!(
        e.cache(),
        Vec::<String>::new(),
        "no fallback to the stage's cache"
    );
    assert_eq!(ls(&fsb), Vec::<String>::new());
    e.assert_clean();
}

#[test]
fn an_as_user_root_op_reads_a_file_staged_on_the_other_filesystem() {
    let e = Env::new();
    let fsb = e.fsb();
    std::fs::create_dir_all(&fsb).unwrap();
    e.serve("a.txt", FILE.as_bytes());
    let sa = sha256(FILE.as_bytes());
    let mut w = e.w(vec![Tgt::from(
        "http://fake/a.txt",
        Some(&sa),
        &fsb.join("a.txt"),
    )]);
    w.login = true;
    let (r, out) = e.go_w(&w, std::slice::from_ref(&fsb));
    assert_eq!(status(&r), &HostStatus::Changed, "{out}\n{}", r.render());
    assert_eq!(std::fs::read(fsb.join("a.txt")).unwrap(), FILE.as_bytes());
    // staged files are readable by anyone who can traverse: 0644 files in
    // 0711 and 0755 directories
    let fs = e.mid("mid_fsb");
    assert!(fs.lines().any(|l| l == "tmp/<run>/fetch d 711"), "{fs}");
    assert!(
        fs.lines().any(|l| l == format!("cache/sha256/{sa} f 644")),
        "{fs}"
    );
    e.assert_clean();
}

/// A run of a dead controller: its stage directory (index `fsroots`) and
/// its `tmp` on fsB, with some fetch and undo data.
fn dead_run(e: &Env, name: &str, age: &str) {
    let fsb = e.fsb();
    let t = fsb.join(".telepatch/tmp").join(name);
    std::fs::create_dir_all(t.join("fetch")).unwrap();
    std::fs::create_dir_all(t.join("undo/u/1/old")).unwrap();
    std::fs::write(t.join("fetch/x.part"), "partial").unwrap();
    let s = e.p("stage").join(name);
    std::fs::create_dir_all(&s).unwrap();
    std::fs::write(s.join("fsroots"), format!("{}\n", fsb.display())).unwrap();
    let st = Command::new("touch")
        .args(["-d", age])
        .arg(&s)
        .status()
        .unwrap();
    assert!(st.success());
}

#[test]
fn the_stale_sweep_removes_an_old_run_s_tmp_on_the_other_filesystem_and_nothing_else() {
    let e = Env::new();
    let fsb = e.fsb();
    std::fs::create_dir_all(&fsb).unwrap();
    e.serve("a.txt", FILE.as_bytes());
    let sa = sha256(FILE.as_bytes());
    // fsB's cache holds an entry from some earlier run
    let old_sum = sha256(b"older download");
    let cache = fsb.join(".telepatch/cache/sha256");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join(&old_sum), "older download").unwrap();
    dead_run(&e, "old", "3 days ago");
    dead_run(&e, "young", "10 minutes ago");
    let w = e.w(vec![Tgt::from(
        "http://fake/a.txt",
        Some(&sa),
        &fsb.join("a.txt"),
    )]);
    let (r, out) = e.go_w(&w, std::slice::from_ref(&fsb));
    assert_eq!(status(&r), &HostStatus::Changed, "{out}\n{}", r.render());
    // the old run is gone from both places
    assert!(!fsb.join(".telepatch/tmp/old").exists());
    assert!(!e.p("stage/old").exists());
    // what must stay: another run's tmp younger than the threshold (its
    // fetch and undo data included), the cache entries, and the parents
    let y = fsb.join(".telepatch/tmp/young");
    assert!(y.join("fetch/x.part").is_file());
    assert!(y.join("undo/u/1/old").is_dir());
    assert!(e.p("stage/young/fsroots").is_file());
    assert_eq!(
        std::fs::read(cache.join(&old_sum)).unwrap(),
        b"older download"
    );
    assert!(cache.join(&sa).is_file(), "this run's entry stays too");
    assert_eq!(ls(&fsb.join(".telepatch/tmp")), ["young"]);
    assert!(out.contains("from run old removed"), "{out}");
    assert!(
        out.contains("from run young, less than 24 h old, left"),
        "{out}"
    );
    // this run's own tmp is gone
    assert_eq!(ls(&e.p("stage")), ["young"]);
}
