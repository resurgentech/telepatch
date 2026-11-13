//! The `binary_release` pkg source (`h.pkg([n]).binary_release(..)`): the
//! trace (shapes, trace errors, the `Unverified` policy error and its
//! allowance) and the executor over the local transport; and allowances
//! carried through the optimizer's pkg merge.
//!
//! No test touches the network. A PATH-fake `curl` / `wget` serves
//! `<d>/www/<path>` for a URL `http://fake/<path>` (to `-o`/`-O <file>` or
//! to stdout) and logs every call to `<d>/log`, so "no download" is a log
//! without the release's url. A fake `uname` answers `-m` from `<d>/arch`.
//! The staging root and the cache are `TP_FETCH_STAGE` and `TP_FETCH_CACHE`,
//! and `TMPDIR` is an empty directory that must stay empty.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use telepatch::engine::{Member, WindowOp};
use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::facts::os::Os;
use telepatch::ops::pkg::{Arch, BinaryRelease, Latest, Sum};
use telepatch::optimize::{Item, optimize};
use telepatch::report::{HostStatus, Report};
use telepatch::testing::FixtureHost;
use telepatch::transport::local::LocalTransport;
use telepatch::{Host, Key, Node, NodeId, OpSpec, Playbook, PlaybookFuture, Policy, Value};

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

fn go_spec() -> BinaryRelease {
    BinaryRelease::new()
        .url("https://go.dev/dl/go{version}.linux-{arch}.tar.gz")
        .sha256(Sum::json(
            "https://go.dev/dl/?mode=json",
            "filename",
            "sha256",
        ))
        .unpack_to("/usr/local/go")
        .strip(1)
        .bin("/usr/local/go/bin/go")
        .version("go version")
        .latest(Latest::text("https://go.dev/VERSION?m=text"))
}

// --- the trace ---

#[test]
fn the_decided_examples_trace_with_one_name_kind_keys_and_post() {
    let h = host();
    let go = h.pkg(["go"]).binary_release(go_spec()).latest().id();
    let mc = h
        .pkg(["mc"])
        .binary_release(
            BinaryRelease::new()
                .url("https://dl.min.io/client/mc/release/linux-{arch}/mc")
                .sha256(Sum::sidecar(".sha256sum"))
                .bin("/usr/local/bin/mc"),
        )
        .latest()
        .id();
    let cmake_sum = SUM.to_string();
    let cmake = h
        .pkg(["cmake"])
        .binary_release(
            BinaryRelease::new()
                .url("https://github.com/Kitware/CMake/releases/download/v{version}/cmake-{version}-linux-{arch}.tar.gz")
                .arch_names(Arch::UNAME)
                .sha256(Sum::value(&cmake_sum))
                .unpack_to("/opt/cmake")
                .strip(1)
                .bin("/opt/cmake/bin/cmake")
                .version("cmake --version"),
        )
        .pin("3.23.2")
        .id();
    assert!(errors(&h).is_empty(), "{:?}", errors(&h));
    for (id, name, bin) in [
        (go, "go", "/usr/local/go/bin/go"),
        (mc, "mc", "/usr/local/bin/mc"),
        (cmake, "cmake", "/opt/cmake/bin/cmake"),
    ] {
        let o = op(&h, id);
        assert_eq!(o.spec.kind(), "pkg[binary_release]");
        assert_eq!(
            o.writes.iter().cloned().collect::<Vec<_>>(),
            vec![Key::Any],
            "writes * unless narrowed"
        );
        let post: Vec<(Key, Value)> = o.post.clone().into_iter().collect();
        assert!(
            post.contains(&(
                Key::pkg(format!("binary_release:root:{name}")),
                Value::Installed
            )),
            "{post:?}"
        );
        assert!(
            post.contains(&(Key::file(bin), Value::Exists(true))),
            "{post:?}"
        );
        let OpSpec::Pkg(p) = &o.spec else { panic!() };
        assert!(!p.mergeable(), "a binary release never merges");
    }
}

#[test]
fn trace_errors() {
    let h = host();
    let ok = || {
        BinaryRelease::new()
            .url("http://x/t-{version}.tgz")
            .sha256(Sum::value(SUM))
            .unpack_to("/opt/t")
            .bin("/opt/t/bin/t")
            .version("t --version")
    };
    h.pkg(["a", "b"]).binary_release(ok());
    h.pkg(["c"])
        .binary_release(BinaryRelease::new().bin("/x").sha256(Sum::value(SUM)));
    h.pkg(["d"]).binary_release(
        BinaryRelease::new()
            .url("http://x/d")
            .sha256(Sum::value(SUM)),
    );
    h.pkg(["e"]).binary_release(
        BinaryRelease::new()
            .url("http://x/e")
            .sha256(Sum::value(SUM))
            .bin("/x/e")
            .strip(1),
    );
    h.pkg(["f"]).binary_release(
        BinaryRelease::new()
            .url("http://x/f")
            .sha256(Sum::value("nothex"))
            .bin("/x/f"),
    );
    h.pkg(["g"]).binary_release(
        BinaryRelease::new()
            .url("http://x/{os}/g")
            .sha256(Sum::value(SUM))
            .bin("/x/g"),
    );
    h.pkg(["i"])
        .binary_release(
            BinaryRelease::new()
                .url("http://x/i")
                .sha256(Sum::value(SUM))
                .bin("/x/i"),
        )
        .pin("1.0");
    h.pkg(["j"]).binary_release(ok()).latest();
    h.pkg(["k"])
        .binary_release(BinaryRelease::new().url("http://x/k").bin("/x/k"))
        .allow(Policy::Unverified, "no sums")
        .latest();
    h.pkg(["l"]).binary_release(
        BinaryRelease::new()
            .url("http://x/l")
            .sha256(Sum::value(SUM))
            .bin("/x/l")
            .latest(Latest::text("http://x/VERSION")),
    );
    h.pkg(["m"]).binary_release(
        BinaryRelease::new()
            .url("http://x/m-{version}")
            .sha256(Sum::value(SUM))
            .bin("/x/m"),
    );
    h.pkg(["n"]).binary_release(
        BinaryRelease::new()
            .url("http://x/n")
            .sha256(Sum::json("http://x/j", "file name", "sha256"))
            .bin("/x/n"),
    );
    h.as_user("bot").pkg(["o"]).binary_release(ok());
    h.pkg(["p"]).pip().binary_release(ok());
    assert_eq!(
        errors(&h),
        [
            "binary_release a: exactly one package name per binary_release op",
            "binary_release c: url is required",
            "binary_release d: bin is required",
            "binary_release e: .strip(1) needs .unpack_to(..)",
            "binary_release f: sha256 must be 64 hex digits, got \"nothex\"",
            "binary_release g: unknown placeholder {os} in the url (only {version} and {arch})",
            "binary_release i: .pin(v) needs {version} in the url",
            "binary_release j: .latest() needs .latest(Latest::..) when the url has {version}",
            "binary_release k: .latest() with no {version} in the url needs a checksum source",
            "binary_release l: .latest(Latest::..) needs {version} in the url",
            "binary_release m: version is required when the url has {version}",
            "binary_release n: Sum::json keys are letters, digits, _ and -; got \"file name\"",
            "binary_release source is system-wide (root only)",
            "binary_release(): this pkg op's source is already pip; one source call per op",
        ]
    );
}

#[test]
fn no_checksum_is_the_unverified_policy_error_and_an_allowance_permits_it() {
    let h = host();
    h.pkg(["t"]).binary_release(
        BinaryRelease::new()
            .url("https://example.com/t")
            .bin("/usr/local/bin/t"),
    );
    let t = h.snapshot();
    assert_eq!(t.errors.len(), 1, "{:?}", t.errors);
    assert_eq!(t.errors[0].policy, Some(Policy::Unverified));
    assert_eq!(
        t.errors[0].message(),
        "pkg[binary_release] t: no checksum source (.sha256(Sum::..))"
    );

    let h = host();
    h.pkg(["t"])
        .binary_release(
            BinaryRelease::new()
                .url("https://example.com/t")
                .bin("/usr/local/bin/t"),
        )
        .allow(Policy::Unverified, "the vendor publishes no sums");
    assert!(errors(&h).is_empty(), "{:?}", errors(&h));
}

// --- the executor over the local transport ---

#[derive(Clone, Debug)]
enum Want {
    Present,
    Pin(String),
    Latest,
}

#[derive(Clone)]
struct V {
    name: String,
    br: BinaryRelease,
    want: Want,
    allow: bool,
    /// A plain file written first, to prove a blocked host applies nothing.
    first: Option<String>,
}

fn pb(h: Host<V>) -> PlaybookFuture {
    Box::pin(async move {
        let v = h.vars.clone();
        if let Some(p) = &v.first {
            h.file(p).content("first");
        }
        let o = h.pkg([v.name.as_str()]).binary_release(v.br.clone());
        match &v.want {
            Want::Present => {}
            Want::Pin(p) => {
                o.pin(p);
            }
            Want::Latest => {
                o.latest();
            }
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
        for s in ["www", "out", "tmp", "stage", "src", "bin"] {
            std::fs::create_dir_all(e.p(s)).unwrap();
        }
        e.arch("x86_64");
        e.fakes();
        e
    }
    fn p(&self, s: &str) -> PathBuf {
        self.d.path().join(s)
    }
    fn arch(&self, a: &str) {
        std::fs::write(self.p("arch"), a).unwrap();
    }
    fn serve(&self, name: &str, bytes: &[u8]) {
        let p = self.p("www").join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, bytes).unwrap();
    }
    fn fakes(&self) {
        let dd = self.d.path().display();
        let curl = format!(
            r#"D={dd}
out=-; url=
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out=$2; shift 2;;
    --) url=$2; shift 2;;
    -*) shift;;
    *) url=$1; shift;;
  esac
done
echo "curl $url" >> $D/log
n=${{url#http://fake/}}
if [ ! -f "$D/www/$n" ]; then echo "curl: (22) The requested URL returned error: 404" >&2; exit 22; fi
if [ "$out" = - ]; then cat "$D/www/$n"; else cp "$D/www/$n" "$out"; fi"#
        );
        let wget = format!(
            r#"D={dd}
out=; url=
while [ $# -gt 0 ]; do
  case "$1" in
    -O) out=$2; shift 2;;
    -O*) out=${{1#-O}}; shift;;
    --) url=$2; shift 2;;
    -*) shift;;
    *) url=$1; shift;;
  esac
done
echo "wget $url" >> $D/log
n=${{url#http://fake/}}
if [ ! -f "$D/www/$n" ]; then echo "wget: server returned error: HTTP/1.1 404 Not Found" >&2; exit 8; fi
if [ "$out" = - ]; then cat "$D/www/$n"; else cp "$D/www/$n" "$out"; fi"#
        );
        let uname = format!(
            "if [ \"$1\" = -m ]; then cat {dd}/arch; echo; else exec /usr/bin/uname \"$@\"; fi"
        );
        for (t, body) in [("curl", curl), ("wget", wget), ("uname", uname)] {
            exe(&self.p("bin").join(t), &format!("#!/bin/bash\n{body}\n"));
        }
    }
    fn log(&self) -> String {
        std::fs::read_to_string(self.p("log")).unwrap_or_default()
    }
    fn go(&self, v: &V) -> (Report, String) {
        self.go_path(
            v,
            &format!(
                "{}:{}",
                self.p("bin").display(),
                std::env::var("PATH").unwrap()
            ),
        )
    }
    fn go_path(&self, v: &V, path: &str) -> (Report, String) {
        let _ = std::fs::remove_file(self.p("log"));
        let pb = Playbook::new("binary_release", pb);
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
    /// A go-like release `go<v>.linux-<arch>.tar.gz` with a top directory
    /// `go/` holding `bin/go` (prints `go version go<v> linux/amd64`) and a
    /// `VERSION` file; returns its sha256.
    fn go_release(&self, v: &str, arch: &str) -> String {
        let src = self.p("src").join(v);
        let _ = std::fs::remove_dir_all(&src);
        std::fs::create_dir_all(src.join("go/bin")).unwrap();
        exe(
            &src.join("go/bin/go"),
            &format!("#!/bin/sh\necho \"go version go{v} linux/amd64\"\n"),
        );
        std::fs::write(src.join("go/VERSION"), format!("go{v}\n")).unwrap();
        let name = format!("go{v}.linux-{arch}.tar.gz");
        let st = Command::new("tar")
            .arg("-czf")
            .arg(self.p("www").join(&name))
            .arg("-C")
            .arg(&src)
            .arg("go")
            .status()
            .unwrap();
        assert!(st.success());
        sha256(&std::fs::read(self.p("www").join(&name)).unwrap())
    }
    /// go.dev's `?mode=json` layout: releases, each with `files[]`.
    fn go_json(&self, files: &[(&str, &str)]) {
        let mut s = String::from(
            "[\n {\n  \"version\": \"go9.9.9\",\n  \"stable\": true,\n  \"files\": [\n",
        );
        let entries: Vec<String> = files
            .iter()
            .map(|(f, sum)| {
                format!(
                    "   {{\n    \"filename\": \"{f}\",\n    \"os\": \"linux\",\n    \"arch\": \"amd64\",\n    \"version\": \"go9.9.9\",\n    \"sha256\" : \"{sum}\",\n    \"size\": 12,\n    \"kind\": \"archive\"\n   }}"
                )
            })
            .collect();
        s.push_str(&entries.join(",\n"));
        s.push_str("\n  ]\n }\n]\n");
        self.serve("dl.json", s.as_bytes());
    }
    fn go_v(&self, want: Want) -> V {
        V {
            name: "go".into(),
            br: BinaryRelease::new()
                .url("http://fake/go{version}.linux-{arch}.tar.gz")
                .sha256(Sum::json("http://fake/dl.json", "filename", "sha256"))
                .unpack_to(self.p("out/go").display().to_string())
                .strip(1)
                .bin(self.p("out/go/bin/go").display().to_string())
                .version("go version")
                .latest(Latest::text("http://fake/VERSION")),
            want,
            allow: false,
            first: None,
        }
    }
}

fn exe(p: &Path, body: &str) {
    std::fs::write(p, body).unwrap();
    std::fs::set_permissions(p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
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

fn status(r: &Report) -> &HostStatus {
    &r.hosts[0].status
}

fn run_bin(p: &Path, arg: &str) -> String {
    let o = Command::new(p).arg(arg).output().unwrap();
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

#[test]
fn a_tarball_with_a_json_sum_and_latest_text_installs_settles_and_upgrades() {
    let e = Env::new();
    let s2 = e.go_release("1.27.2", "amd64");
    e.go_json(&[
        ("go1.27.2.linux-amd64.tar.gz", &s2),
        ("go1.27.2.linux-arm64.tar.gz", SUM),
    ]);
    e.serve("VERSION", b"go1.27.2\ntime 2026-10-01T00:00:00Z\n");
    // an older tree is there, not ours
    std::fs::create_dir_all(e.p("out/go/old")).unwrap();
    let v = e.go_v(Want::Latest);

    // run 1: not installed
    let (r1, out1) = e.go(&v);
    assert_eq!(status(&r1), &HostStatus::Changed, "{out1}\n{}", r1.render());
    assert!(
        out1.contains(
            "== h1 [prepare] pkg[binary_release] go: not installed, 1.27.2 wanted; fetched, sha256 ok =="
        ),
        "{out1}"
    );
    assert_eq!(
        e.log(),
        "curl http://fake/VERSION\ncurl http://fake/dl.json\ncurl http://fake/go1.27.2.linux-amd64.tar.gz\n"
    );
    assert_eq!(
        run_bin(&e.p("out/go/bin/go"), "version"),
        "go version go1.27.2 linux/amd64"
    );
    assert!(!e.p("out/go/old").exists(), "the tree is replaced");
    assert_eq!(e.cache(), std::slice::from_ref(&s2));
    assert_eq!(ls(&e.p("out")), ["go"], "nothing left beside the target");
    let rec = &r1.hosts[0].ops[0];
    assert_eq!(rec.desc, "pkg[binary_release] latest go");
    assert!(rec.changed);
    e.assert_clean();

    // run 2: settled; resolving latest is the only call
    let (r2, out2) = e.go(&v);
    assert_eq!(status(&r2), &HostStatus::Ok, "{out2}\n{}", r2.render());
    assert_eq!(e.log(), "curl http://fake/VERSION\n", "no download");
    assert!(!out2.contains("[prepare] pkg[binary_release]"), "{out2}");
    assert!(r2.hosts[0].ops.iter().all(|o| !o.changed));
    e.assert_clean();

    // run 3: a newer release is published
    let s3 = e.go_release("1.27.3", "amd64");
    e.go_json(&[
        ("go1.27.3.linux-amd64.tar.gz", &s3),
        ("go1.27.2.linux-amd64.tar.gz", &s2),
    ]);
    e.serve("VERSION", b"go1.27.3\ntime 2026-11-01T00:00:00Z\n");
    let (r3, out3) = e.go(&v);
    assert_eq!(status(&r3), &HostStatus::Changed, "{out3}\n{}", r3.render());
    assert!(
        out3.contains(
            "== h1 [prepare] pkg[binary_release] go: 1.27.2 installed, 1.27.3 wanted; fetched, sha256 ok =="
        ),
        "{out3}"
    );
    assert_eq!(
        run_bin(&e.p("out/go/bin/go"), "version"),
        "go version go1.27.3 linux/amd64"
    );
    assert_eq!(ls(&e.p("out")), ["go"]);
    e.assert_clean();

    // a pin back to the cached 1.27.2: the json is read, the tarball is not
    let (r4, out4) = e.go(&e.go_v(Want::Pin("1.27.2".into())));
    assert_eq!(status(&r4), &HostStatus::Changed, "{out4}\n{}", r4.render());
    assert!(
        out4.contains(
            "== h1 [prepare] pkg[binary_release] go: 1.27.3 installed, 1.27.2 wanted; cached =="
        ),
        "{out4}"
    );
    assert_eq!(e.log(), "curl http://fake/dl.json\n");
    assert_eq!(
        run_bin(&e.p("out/go/bin/go"), "version"),
        "go version go1.27.2 linux/amd64"
    );
    e.assert_clean();

    // present with something installed: no network call at all
    let (r5, out5) = e.go(&e.go_v(Want::Present));
    assert_eq!(status(&r5), &HostStatus::Ok, "{out5}\n{}", r5.render());
    assert_eq!(e.log(), "");
    e.assert_clean();
}

#[test]
fn wget_reads_latest_the_json_and_the_release_when_curl_is_absent() {
    let e = Env::new();
    let s2 = e.go_release("1.27.2", "amd64");
    e.go_json(&[("go1.27.2.linux-amd64.tar.gz", &s2)]);
    e.serve("VERSION", b"go1.27.2\n");
    // a PATH of /usr/bin without curl and wget, then the fakes but curl
    let farm = e.p("farm");
    std::fs::create_dir_all(&farm).unwrap();
    for ent in std::fs::read_dir("/usr/bin").unwrap().flatten() {
        let n = ent.file_name().to_string_lossy().to_string();
        if n != "curl" && n != "wget" {
            let _ = std::os::unix::fs::symlink(ent.path(), farm.join(&n));
        }
    }
    std::fs::remove_file(e.p("bin/curl")).unwrap();
    let (r, out) = e.go_path(
        &e.go_v(Want::Latest),
        &format!("{}:{}", e.p("bin").display(), farm.display()),
    );
    assert_eq!(status(&r), &HostStatus::Changed, "{out}\n{}", r.render());
    assert_eq!(
        e.log(),
        "wget http://fake/VERSION\nwget http://fake/dl.json\nwget http://fake/go1.27.2.linux-amd64.tar.gz\n"
    );
    assert_eq!(
        run_bin(&e.p("out/go/bin/go"), "version"),
        "go version go1.27.2 linux/amd64"
    );
    e.assert_clean();
}

#[derive(Clone)]
struct G {
    dir: PathBuf,
}

fn pb_github(h: Host<G>) -> PlaybookFuture {
    Box::pin(async move {
        let d = h.vars.dir.display().to_string();
        h.pkg(["tool"])
            .installer_script(
                telepatch::ops::pkg::InstallerScript::new()
                    .bin(format!("{d}/tool"))
                    .version(format!("{d}/tool --version"))
                    .latest(Latest::github_release("acme/tool"))
                    .install(format!(
                        "printf '#!/bin/sh\\necho tool %s\\n' \"$TP_WANT\" > {d}/tool && chmod 755 {d}/tool"
                    )),
            )
            .latest();
        Ok(())
    })
}

#[test]
fn wget_resolves_a_github_release_latest_when_curl_is_absent() {
    let d = tempfile::tempdir().unwrap();
    let out_dir = d.path().join("out");
    let bin = d.path().join("bin");
    let farm = d.path().join("farm");
    for p in [&out_dir, &bin, &farm] {
        std::fs::create_dir_all(p).unwrap();
    }
    // a PATH of /usr/bin without curl and wget, then a fake wget that
    // answers only the releases/latest url
    for ent in std::fs::read_dir("/usr/bin").unwrap().flatten() {
        let n = ent.file_name().to_string_lossy().to_string();
        if n != "curl" && n != "wget" {
            let _ = std::os::unix::fs::symlink(ent.path(), farm.join(&n));
        }
    }
    let log = d.path().join("log");
    exe(
        &bin.join("wget"),
        &format!(
            r#"#!/bin/bash
args="$*"
echo "wget $args" >> {log}
if [ "$args" != "-q -O - -- https://api.github.com/repos/acme/tool/releases/latest" ]; then
  echo "wget: unexpected: $args" >&2; exit 8
fi
printf '{{\n  "url": "u",\n  "tag_name": "v4.5.6",\n  "name": "x"\n}}\n'
"#,
            log = log.display()
        ),
    );
    let pb = Playbook::new("github", pb_github);
    let out = Rc::new(RefCell::new(String::new()));
    let r = run(
        &pb,
        vec![HostSpec {
            groups: Vec::new(),
            name: "h1".into(),
            target: Target::Local(
                LocalTransport::new()
                    .uid(0)
                    .sandboxed_at(d.path())
                    .env("PATH", &format!("{}:{}", bin.display(), farm.display())),
            ),
            vars: G {
                dir: out_dir.clone(),
            },
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    let out = out.borrow().clone();
    assert!(!out.contains("could not resolve"), "{out}\n{}", r.render());
    assert_eq!(status(&r), &HostStatus::Changed, "{out}\n{}", r.render());
    // read in prepare and again in apply, each the same wget call
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(!calls.is_empty(), "wget was called");
    for l in calls.lines() {
        assert_eq!(
            l,
            "wget -q -O - -- https://api.github.com/repos/acme/tool/releases/latest"
        );
    }
    assert_eq!(run_bin(&out_dir.join("tool"), "--version"), "tool 4.5.6");
}

#[test]
fn a_single_binary_with_a_sidecar_and_no_version_follows_the_published_checksum() {
    let e = Env::new();
    let body1 = b"#!/bin/sh\necho mc one\n";
    let s1 = sha256(body1);
    e.serve("linux-amd64/mc", body1);
    e.serve("linux-amd64/mc.sha256sum", format!("{s1}  mc\n").as_bytes());
    let bin = e.p("out/bin/mc");
    let v = V {
        name: "mc".into(),
        br: BinaryRelease::new()
            .url("http://fake/linux-{arch}/mc")
            .sha256(Sum::sidecar(".sha256sum"))
            .bin(bin.display().to_string()),
        want: Want::Latest,
        allow: false,
        first: None,
    };
    let (r1, out1) = e.go(&v);
    assert_eq!(status(&r1), &HostStatus::Changed, "{out1}\n{}", r1.render());
    let l = format!(
        "== h1 [prepare] pkg[binary_release] mc: not installed, sha256 {} wanted; fetched, sha256 ok ==",
        &s1[..12]
    );
    assert!(out1.contains(&l), "{l}\n{out1}");
    assert_eq!(
        e.log(),
        "curl http://fake/linux-amd64/mc.sha256sum\ncurl http://fake/linux-amd64/mc\n"
    );
    assert_eq!(std::fs::read(&bin).unwrap(), body1);
    assert_eq!(run_bin(&bin, ""), "mc one");
    let mode = std::fs::metadata(&bin).unwrap().permissions();
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
        0o755
    );
    assert_eq!(ls(&e.p("out/bin")), ["mc"], "the candidate was renamed");
    e.assert_clean();

    // run 2: the sidecar is the only call
    let (r2, out2) = e.go(&v);
    assert_eq!(status(&r2), &HostStatus::Ok, "{out2}\n{}", r2.render());
    assert_eq!(e.log(), "curl http://fake/linux-amd64/mc.sha256sum\n");
    e.assert_clean();

    // run 3: the vendor publishes a new build under the same url
    let body2 = b"#!/bin/sh\necho mc two\n";
    let s2 = sha256(body2);
    e.serve("linux-amd64/mc", body2);
    e.serve("linux-amd64/mc.sha256sum", s2.as_bytes());
    let (r3, out3) = e.go(&v);
    assert_eq!(status(&r3), &HostStatus::Changed, "{out3}\n{}", r3.render());
    let l = format!(
        "== h1 [prepare] pkg[binary_release] mc: installed, sha256 {} wanted; fetched, sha256 ok ==",
        &s2[..12]
    );
    assert!(out3.contains(&l), "{l}\n{out3}");
    assert_eq!(run_bin(&bin, ""), "mc two");
    assert_eq!(ls(&e.p("out/bin")), ["mc"]);
    e.assert_clean();

    // a known sum and no version: latest is "present with that sum", no call
    let v4 = V {
        br: BinaryRelease::new()
            .url("http://fake/linux-{arch}/mc")
            .sha256(Sum::value(&s2))
            .bin(bin.display().to_string()),
        ..v.clone()
    };
    let (r4, out4) = e.go(&v4);
    assert_eq!(status(&r4), &HostStatus::Ok, "{out4}\n{}", r4.render());
    assert_eq!(e.log(), "");
    e.assert_clean();
}

#[test]
fn arch_maps_by_default_by_uname_and_by_a_table() {
    for (machine, arch, want) in [
        ("x86_64", None, "amd64"),
        ("aarch64", None, "arm64"),
        ("x86_64", Some(Arch::UNAME), "x86_64"),
        ("aarch64", Some(Arch::UNAME), "aarch64"),
        ("x86_64", Some(Arch::from(&[("x86_64", "x64")][..])), "x64"),
        ("riscv64", None, "riscv64"),
    ] {
        let e = Env::new();
        e.arch(machine);
        let body = format!("#!/bin/sh\necho {want}\n");
        e.serve(&format!("{want}/t"), body.as_bytes());
        let mut br = BinaryRelease::new()
            .url("http://fake/{arch}/t")
            .sha256(Sum::value(sha256(body.as_bytes())))
            .bin(e.p("out/t").display().to_string());
        if let Some(a) = arch.clone() {
            br = br.arch_names(a);
        }
        let v = V {
            name: "t".into(),
            br,
            want: Want::Present,
            allow: false,
            first: None,
        };
        let (r, out) = e.go(&v);
        assert_eq!(
            status(&r),
            &HostStatus::Changed,
            "{machine} {arch:?}: {out}\n{}",
            r.render()
        );
        assert_eq!(
            e.log(),
            format!("curl http://fake/{want}/t\n"),
            "{machine} {arch:?}"
        );
        e.assert_clean();
    }
}

#[test]
fn a_version_in_the_url_with_nothing_to_resolve_it_blocks_when_absent() {
    let e = Env::new();
    let mut v = e.go_v(Want::Present);
    v.br = BinaryRelease::new()
        .url("http://fake/go{version}.linux-{arch}.tar.gz")
        .sha256(Sum::json("http://fake/dl.json", "filename", "sha256"))
        .unpack_to(e.p("out/go").display().to_string())
        .strip(1)
        .bin(e.p("out/go/bin/go").display().to_string())
        .version("go version");
    let (r, out) = e.go(&v);
    let HostStatus::Blocked(why) = status(&r) else {
        panic!("{:?}\n{out}", status(&r));
    };
    assert_eq!(
        why,
        "pkg[binary_release] go: not installed, and the url needs a {version}: give .latest(Latest::..) or .pin(v) (window 1)"
    );
    assert_eq!(e.log(), "");
    e.assert_clean();
}

#[test]
fn a_checksum_mismatch_blocks_before_apply_with_no_cache_entry() {
    let e = Env::new();
    let s2 = e.go_release("1.27.2", "amd64");
    e.go_json(&[("go1.27.2.linux-amd64.tar.gz", SUM)]);
    let mut v = e.go_v(Want::Pin("1.27.2".into()));
    v.first = Some(e.p("out/first").display().to_string());
    let (r, out) = e.go(&v);
    let HostStatus::Blocked(why) = status(&r) else {
        panic!("{:?}\n{out}\n{}", status(&r), r.render());
    };
    assert_eq!(
        why,
        &format!(
            "pkg[binary_release] go: fetch http://fake/go1.27.2.linux-amd64.tar.gz: sha256 mismatch: want {SUM} got {s2} (window 1)"
        )
    );
    assert!(
        !e.p("out/first").exists() && !e.p("out/go").exists(),
        "nothing was applied"
    );
    assert_eq!(e.cache(), Vec::<String>::new());
    e.assert_clean();

    // a json that does not list the file blocks too
    e.go_json(&[("go1.27.9.linux-amd64.tar.gz", SUM)]);
    let (r, out) = e.go(&v);
    let HostStatus::Blocked(why) = status(&r) else {
        panic!("{:?}\n{out}", status(&r));
    };
    assert_eq!(
        why,
        "pkg[binary_release] go: no sha256 for go1.27.2.linux-amd64.tar.gz in http://fake/dl.json (window 1)"
    );
    e.assert_clean();
}

#[test]
fn no_checksum_downloads_unverified_with_the_allowance_and_cleans_up() {
    let e = Env::new();
    let body = b"#!/bin/sh\necho blob\n";
    e.serve("blob", body);
    let v = V {
        name: "blob".into(),
        br: BinaryRelease::new()
            .url("http://fake/blob")
            .bin(e.p("out/blob").display().to_string()),
        want: Want::Present,
        allow: true,
        first: None,
    };
    let (r, out) = e.go(&v);
    assert_eq!(status(&r), &HostStatus::Changed, "{out}\n{}", r.render());
    assert!(
        out.contains(
            "== h1 [prepare] pkg[binary_release] blob: not installed, present wanted; fetched, not verified =="
        ),
        "{out}"
    );
    assert_eq!(std::fs::read(e.p("out/blob")).unwrap(), body);
    assert_eq!(e.cache(), Vec::<String>::new(), "never cached");
    assert!(
        r.render()
            .contains("allowed unverified: the test server publishes no sums"),
        "{}",
        r.render()
    );
    e.assert_clean();
    let (r2, out2) = e.go(&v);
    assert_eq!(status(&r2), &HostStatus::Ok, "{out2}");
    assert_eq!(e.log(), "");
    e.assert_clean();
}

// --- allowances through the optimizer's merge ---

fn pb_merge(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        h.pkg(["tp-a"]);
        h.pkg(["tp-b"])
            .allow(Policy::Unverified, "the second member's reason");
        Ok(())
    })
}

#[test]
fn a_merged_member_s_allowance_is_listed_in_the_plan_and_the_report() {
    let pb = Playbook::new("merge", pb_merge);
    let plan = telepatch::testing::plan(&pb, &FixtureHost::new("h1", os(), Rc::new(())));
    assert!(plan.contains("merged from (none) #0, (none) #1"), "{plan}");
    let line = plan
        .lines()
        .find(|l| l.contains("allowed unverified"))
        .unwrap_or_else(|| panic!("no allowance in the plan:\n{plan}"));
    assert!(
        line.starts_with("      allowed unverified: the second member's reason (for #1 at ")
            && line.contains("binary_release.rs:"),
        "{line}"
    );

    // the run report: every manager reports both names installed
    let d = tempfile::tempdir().unwrap();
    let bin = d.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    exe(&bin.join("dpkg-query"), "#!/bin/sh\necho installed\n");
    exe(&bin.join("rpm"), "#!/bin/sh\nexit 0\n");
    exe(&bin.join("pacman"), "#!/bin/sh\nexit 0\n");
    let r = run(
        &pb,
        vec![HostSpec {
            groups: Vec::new(),
            name: "h1".into(),
            target: Target::Local(LocalTransport::new().uid(0).sandboxed_at(d.path()).env(
                "PATH",
                &format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )),
            vars: (),
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Capture(Rc::new(RefCell::new(String::new()))),
        },
    );
    assert_eq!(r.exit_code(), 0, "{}", r.render());
    let ops = &r.hosts[0].ops;
    assert_eq!(ops.len(), 1, "the two ops merged: {ops:?}");
    assert_eq!(ops[0].allowed.len(), 1, "{ops:?}");
    let text = r.render();
    assert!(
        text.contains(": allowed unverified: the second member's reason (for #1 at "),
        "{text}"
    );
}

#[test]
fn an_allowance_on_one_member_does_not_permit_another_member_s_policy_error() {
    let h = host();
    let a = h.pkg(["tp-a"]).id();
    let b = h
        .pkg(["tp-b"])
        .allow(Policy::Unverified, "b's own reason")
        .id();
    let t = h.snapshot();
    let items: Vec<Item> = t
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| match n {
            Node::Op(o) => Some(Item::Op(WindowOp {
                id: NodeId(i),
                node: o.clone(),
                members: vec![Member::of(NodeId(i), "", o)],
            })),
            _ => None,
        })
        .collect();
    let ops = optimize(items).schedule();
    assert_eq!(ops.len(), 1, "merged");
    let merged = &ops[0];
    assert_eq!(merged.node.allowed.len(), 1);
    assert_eq!(
        merged.node.allowed[0].member.as_ref().map(|m| m.id),
        Some(b)
    );

    // a trip of member a is not permitted by b's allowance
    let trip = |id: NodeId| vec![(id, Policy::Unverified, format!("member {id} trips"))];
    let left = telepatch::policy::unpermitted(&merged.node.allowed, trip(a));
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(left[0].0, a);
    // the same trip on b is
    assert!(telepatch::policy::unpermitted(&merged.node.allowed, trip(b)).is_empty());
    // and another policy on b is not
    let other = vec![(b, Policy::UnitViaFile, "x".to_string())];
    assert_eq!(
        telepatch::policy::unpermitted(&merged.node.allowed, other).len(),
        1
    );
}

/// An earlier op fails, so the binary_release op prepare made ready never
/// runs: the host's stage directory (the release record) is still removed
/// at the end of the host (FOREMAN, from the implementer's A1).
#[test]
fn a_host_that_halts_before_the_op_leaves_nothing_staged() {
    let e = Env::new();
    let s2 = e.go_release("1.27.2", "amd64");
    e.go_json(&[("go1.27.2.linux-amd64.tar.gz", &s2)]);
    let mut v = e.go_v(Want::Pin("1.27.2".into()));
    // a file under a directory that does not exist: the op fails
    v.first = Some(e.p("no/such/dir/first").display().to_string());
    let (r, out) = e.go(&v);
    assert_eq!(r.exit_code(), 1, "{out}\n{}", r.render());
    assert!(!e.p("out/go").exists(), "{out}");
    e.assert_clean();
}

// --- a binary release after the op that installs curl (T83a) ---

/// `h.pkg(["curl"])` (when `curl`) then the binary release of `v`.
#[derive(Clone)]
struct VC {
    v: V,
    curl: bool,
    /// `h.pkg(["wget"])` in place of curl's op.
    wget: bool,
}

fn pb_curl(h: Host<VC>) -> PlaybookFuture {
    Box::pin(async move {
        let vc = h.vars.clone();
        if vc.curl {
            h.pkg(["curl"]);
        }
        if vc.wget {
            h.pkg(["wget"]);
        }
        let v = &vc.v;
        let o = h.pkg([v.name.as_str()]).binary_release(v.br.clone());
        match &v.want {
            Want::Present => {}
            Want::Pin(p) => {
                o.pin(p);
            }
            Want::Latest => {
                o.latest();
            }
        }
        Ok(())
    })
}

impl Env {
    /// No curl or wget on the PATH, and a fake package manager for every
    /// distro family in `<d>/pm` whose install of `curl` puts the fake
    /// curl into `<d>/inst`, already on the PATH (each install logged to
    /// `<d>/pmlog`). Returns the PATH: `<d>/pm:<d>/inst:<d>/bin:<farm>`.
    fn pm_path(&self) -> String {
        std::fs::rename(self.p("bin/curl"), self.p("curl.pkg")).unwrap();
        std::fs::rename(self.p("bin/wget"), self.p("wget.pkg")).unwrap();
        let pm = self.p("pm");
        let inst = self.p("inst");
        let farm = self.p("farm");
        for d in [&pm, &inst, &farm] {
            std::fs::create_dir_all(d).unwrap();
        }
        let dd = self.d.path().display();
        let body = format!(
            r#"#!/bin/bash
D={dd}
me=${{0##*/}}
inst() {{ for n; do case "$n" in curl|wget) cp "$D/$n.pkg" "$D/inst/$n"; echo "$me install $n" >> "$D/pmlog";; esac; done; }}
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
            exe(&pm.join(t), &body);
        }
        for ent in std::fs::read_dir("/usr/bin").unwrap().flatten() {
            let n = ent.file_name().to_string_lossy().to_string();
            if n != "curl" && n != "wget" {
                let _ = std::os::unix::fs::symlink(ent.path(), farm.join(&n));
            }
        }
        format!(
            "{}:{}:{}:{}",
            pm.display(),
            inst.display(),
            self.p("bin").display(),
            farm.display()
        )
    }
    fn pmlog(&self) -> String {
        std::fs::read_to_string(self.p("pmlog")).unwrap_or_default()
    }
    fn go_c(&self, v: &VC, path: &str) -> (Report, String, Vec<serde_json::Value>) {
        let _ = std::fs::remove_file(self.p("log"));
        let pb = Playbook::new("curl_binary_release", pb_curl);
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
    /// A go 1.27.2 release, its json and VERSION, served; `go_v(want)`.
    fn go_served(&self, want: Want) -> V {
        let s2 = self.go_release("1.27.2", "amd64");
        self.go_json(&[("go1.27.2.linux-amd64.tar.gz", &s2)]);
        self.serve("VERSION", b"go1.27.2\n");
        self.go_v(want)
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

#[test]
fn a_binary_release_after_the_pkg_op_that_installs_curl_runs_after_it() {
    let e = Env::new();
    let path = e.pm_path();
    let v = VC {
        v: e.go_served(Want::Latest),
        curl: true,
        wget: false,
    };
    let (r, out, evs) = e.go_c(&v, &path);
    assert_eq!(status(&r), &HostStatus::Changed, "{out}\n{}", r.render());
    assert_eq!(e.pmlog().lines().count(), 1, "{}", e.pmlog());
    assert_eq!(
        e.log(),
        "curl http://fake/VERSION\ncurl http://fake/dl.json\ncurl http://fake/go1.27.2.linux-amd64.tar.gz\n"
    );
    assert_eq!(
        run_bin(&e.p("out/go/bin/go"), "version"),
        "go version go1.27.2 linux/amd64"
    );
    let split = out
        .find("== h1 [prepare] window 1: neither curl nor wget is on the host; split after #0 (pkg curl): #1 runs in window 2 ==")
        .unwrap_or_else(|| panic!("no split line:\n{out}"));
    let prep = out
        .find("== h1 [prepare] pkg[binary_release] go: not installed, 1.27.2 wanted; fetched, sha256 ok ==")
        .unwrap_or_else(|| panic!("no prepare line:\n{out}"));
    assert!(split < prep, "{out}");
    assert_eq!(windows(&evs), [(1, vec![0]), (2, vec![1])], "{evs:#?}");
    assert_eq!(
        r.hosts[0]
            .ops
            .iter()
            .map(|o| (o.id, o.changed))
            .collect::<Vec<_>>(),
        [(0, true), (1, true)]
    );
    e.assert_clean();
}

/// The same through wget: the version, the checksum list and the release
/// are all read with the wget the pkg op installs.
#[test]
fn a_binary_release_after_the_pkg_op_that_installs_wget_runs_after_it_through_wget() {
    let e = Env::new();
    let path = e.pm_path();
    let v = VC {
        v: e.go_served(Want::Latest),
        curl: false,
        wget: true,
    };
    let (r, out, evs) = e.go_c(&v, &path);
    assert_eq!(status(&r), &HostStatus::Changed, "{out}\n{}", r.render());
    let pm = e.pmlog();
    assert!(
        pm.lines().count() == 1 && pm.ends_with(" install wget\n"),
        "wget installed once: {pm}"
    );
    assert_eq!(
        e.log(),
        "wget http://fake/VERSION\nwget http://fake/dl.json\nwget http://fake/go1.27.2.linux-amd64.tar.gz\n"
    );
    assert_eq!(
        run_bin(&e.p("out/go/bin/go"), "version"),
        "go version go1.27.2 linux/amd64"
    );
    let split = out
        .find("== h1 [prepare] window 1: neither curl nor wget is on the host; split after #0 (pkg wget): #1 runs in window 2 ==")
        .unwrap_or_else(|| panic!("no split line:\n{out}"));
    let prep = out
        .find("== h1 [prepare] pkg[binary_release] go: not installed, 1.27.2 wanted; fetched, sha256 ok ==")
        .unwrap_or_else(|| panic!("no prepare line:\n{out}"));
    assert!(split < prep, "{out}");
    assert_eq!(windows(&evs), [(1, vec![0]), (2, vec![1])], "{evs:#?}");
    e.assert_clean();
}

#[test]
fn a_binary_release_with_curl_on_the_host_is_not_split() {
    let e = Env::new();
    let path = e.pm_path();
    std::fs::copy(e.p("curl.pkg"), e.p("inst/curl")).unwrap();
    let v = VC {
        v: e.go_served(Want::Latest),
        curl: true,
        wget: false,
    };
    let (r, out, evs) = e.go_c(&v, &path);
    assert_eq!(status(&r), &HostStatus::Changed, "{out}\n{}", r.render());
    assert_eq!(e.pmlog(), "");
    assert!(!out.contains("split"), "{out}");
    assert_eq!(windows(&evs), [(1, vec![0, 1])], "{evs:#?}");
    e.assert_clean();
}

#[test]
fn a_binary_release_with_no_downloader_and_none_installed_blocks_naming_them() {
    for want in [Want::Latest, Want::Pin("1.27.2".into())] {
        let e = Env::new();
        let path = e.pm_path();
        let v = VC {
            v: e.go_served(want.clone()),
            curl: false,
            wget: false,
        };
        let (r, out, _) = e.go_c(&v, &path);
        let HostStatus::Blocked(why) = status(&r) else {
            panic!("{want:?}: {:?}\n{out}", status(&r));
        };
        assert_eq!(
            why, "pkg[binary_release] go: neither curl nor wget is on the host (window 1)",
            "{want:?}"
        );
        assert!(!out.contains("split"), "{out}");
        assert!(!e.p("out/go").exists());
        e.assert_clean();
    }
}

// --- the plan is byte-stable ---

fn pb_plan(h: Host<()>) -> PlaybookFuture {
    Box::pin(async move {
        h.pkg(["go"]).binary_release(go_spec()).latest();
        let f = h.fetch("https://example.invalid/tool.tar.gz");
        h.file("/opt/tool/tool.bin")
            .from(&f)
            .allow(Policy::Unverified, "the test server publishes no sums");
        Ok(())
    })
}

/// The plan of a binary release and a no-sum fetch carries no per-process
/// value: the snapshot is written once and compared in every later process.
#[test]
fn binary_release_plan() {
    let pb = Playbook::new("br_plan", pb_plan);
    let plan = telepatch::testing::plan_for("debian12", &pb, ());
    assert!(plan.contains("$__tp_run"), "{plan}");

    insta::assert_snapshot!("binary_release_plan", plan);
}

/// A login op that names `$__tp_run` reads it from its login shell, where
/// the executor set it at open: the op is rendered exactly like a root op
/// with the same script, nothing passed on a command line (`tests/
/// login_shell.rs` runs it).
#[test]
fn a_login_op_names_the_run_variable_and_renders_like_a_root_op() {
    let h = host();
    h.as_user("root").raw("echo \"$__tp_run\"");
    h.raw("echo \"$__tp_run\"");
    let nodes = h.snapshot().nodes;
    let Node::Op(a) = &nodes[0] else { panic!() };
    let Node::Op(b) = &nodes[1] else { panic!() };
    assert!(a.run_as.login && !b.run_as.login);
    let a = telepatch::trailer::render_for(a, 1, "n0nce", &h.os, "root").unwrap();
    let b = telepatch::trailer::render_for(b, 1, "n0nce", &h.os, "root").unwrap();
    assert_eq!(a, b);
    assert!(!a.contains("env -i"), "{a}");
}
