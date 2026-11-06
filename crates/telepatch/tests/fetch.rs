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
        let pb = Playbook {
            name: "fetch",
            run: pb_fetch,
        };
        let out = Rc::new(RefCell::new(String::new()));
        let report = run(
            &pb,
            vec![HostSpec {
                name: "h1".into(),
                target: Target::Local(
                    LocalTransport::new()
                        .uid(0)
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
    fn cache(&self) -> Vec<String> {
        ls(&self.p("cache"))
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
    assert_eq!(e.cache().len(), 2, "both archives are cached by sum");
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
        &Playbook {
            name: "two",
            run: pb,
        },
        vec![HostSpec {
            name: "h1".into(),
            target: Target::Local(
                LocalTransport::new()
                    .uid(0)
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
