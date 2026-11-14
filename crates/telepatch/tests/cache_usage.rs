//! T135: the cache without locks or records. An item's mtime is its last
//! access, an empty marker `owners/<owner_dir(playbook)>/<sum>` beside `sha256` says
//! which playbook uses it, and the end of a run expires what went 90 days
//! unaccessed and drops what an entitled run's playbook no longer fetches.
//!
//! Everything runs on the local transport inside a sandbox directory `<d>`
//! (T124): the main cache is `<d>/cache`, so the registry is `<d>/caches` and
//! its owner markers are under `<d>/owners`. `TP_CACHE_NOW` is the clock. A
//! fake `curl` serves `http://fake/<name>` from `<d>/www/<name>`. `<d>/fsB`
//! is a filesystem of its own (`TP_FS_ROOTS`), whose cache is
//! `<d>/fsB/.telepatch/cache/sha256` with markers under
//! `<d>/fsB/.telepatch/cache/owners`. A test that removes anything also
//! asserts what must stay.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use telepatch::executor::{
    CheckOpts, HostSpec, RunControls, RunOpts, Stdout, Target, run_check, run_with,
};
use telepatch::ops::fetch::{owner_dir, owner_name};
use telepatch::report::{HostStatus, Report};
use telepatch::transport::local::LocalTransport;
use telepatch::{Host, Playbook, PlaybookFuture};

const DAY: u64 = 86_400;
const NOW: u64 = 1_800_000_000;
const FILE_A: &str = "alpha file contents\n";
const FILE_B: &str = "bravo file contents\n";

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

/// A made-up 64-hex name.
fn hex(n: u64) -> String {
    format!("{n:064x}")
}

#[derive(Clone, Default)]
struct V {
    /// `(url, sum, target)` placed by `.from`.
    items: Vec<(String, String, String)>,
    /// Kill the root shell after the fetch ops.
    kill: bool,
    /// An op that fails (T122).
    fail: bool,
    /// A directory for a pending reboot barrier after the fetches (T122).
    stop_dir: Option<String>,
}

fn pb(h: Host<V>) -> PlaybookFuture {
    Box::pin(async move {
        let v = h.vars.clone();
        for (url, sum, target) in &v.items {
            h.file(target).from(h.fetch(url).sha256(sum));
        }
        if let Some(d) = &v.stop_dir {
            h.file(&format!("{d}/marker")).content("1\n").needs_reboot();
            h.reboot().if_pending();
            h.raw(format!("touch {d}/after"));
        }
        if v.fail {
            h.raw("false");
        }
        if v.kill {
            h.raw("kill -9 $$ $BASHPID");
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
        for s in ["www", "out", "tmp", "stage", "fsB", "bin"] {
            std::fs::create_dir_all(e.p(s)).unwrap();
        }
        e.fake_curl();
        e.serve("a.txt", FILE_A.as_bytes());
        e.serve("b.txt", FILE_B.as_bytes());
        e
    }
    fn p(&self, s: &str) -> PathBuf {
        self.d.path().join(s)
    }
    fn serve(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.p("www").join(name), bytes).unwrap();
    }
    fn fake_curl(&self) {
        let dd = self.d.path().display();
        let body = format!(
            r#"#!/bin/bash
D={dd}
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
if [ -f "$D/www/$n" ]; then cp "$D/www/$n" "$out"; else echo "curl: (22) 404" >&2; exit 22; fi
"#
        );
        let p = self.p("bin/curl");
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }
    fn cache(&self) -> PathBuf {
        self.p("cache")
    }
    /// The main cache's owner markers: `<d>/owners` (beside `<d>/cache`).
    fn owners(&self) -> PathBuf {
        self.p("owners")
    }
    fn fsb_cache(&self) -> PathBuf {
        self.p("fsB/.telepatch/cache/sha256")
    }
    fn fsb_owners(&self) -> PathBuf {
        self.p("fsB/.telepatch/cache/owners")
    }
    fn item(&self, name: &str, contents: &str) -> (String, String, String) {
        (
            format!("http://fake/{name}"),
            sha256(contents.as_bytes()),
            self.p("out").join(name).display().to_string(),
        )
    }
    fn a(&self) -> (String, String, String) {
        self.item("a.txt", FILE_A)
    }
    fn b(&self) -> (String, String, String) {
        self.item("b.txt", FILE_B)
    }
    fn transport(&self, now: u64) -> LocalTransport {
        LocalTransport::new()
            .uid(0)
            .sandboxed_at(self.d.path())
            .env(
                "PATH",
                &format!(
                    "{}:{}",
                    self.p("bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("TMPDIR", &self.p("tmp").display().to_string())
            .env("TP_FETCH_STAGE", &self.p("stage").display().to_string())
            .env("TP_FETCH_CACHE", &self.cache().display().to_string())
            .env("TP_FS_ROOTS", &self.p("fsB").display().to_string())
            .env("TP_CACHE_NOW", &now.to_string())
            .env("TP_RUN_DIR", &self.p("run").display().to_string())
    }
    fn spec(&self, v: &V, now: u64) -> Vec<HostSpec<V>> {
        vec![HostSpec {
            groups: Vec::new(),
            name: "h1".into(),
            target: Target::Local(self.transport(now)),
            vars: v.clone(),
        }]
    }
    /// A run of the playbook named `name` under `controls`.
    fn go_as(
        &self,
        name: &'static str,
        v: &V,
        now: u64,
        controls: RunControls,
    ) -> (Report, String) {
        let out = Rc::new(RefCell::new(String::new()));
        let r = run_with(
            &Playbook::new(name, pb),
            self.spec(v, now),
            RunOpts {
                log: None,
                stdout: Stdout::Capture(out.clone()),
            },
            None,
            controls,
        );
        let t = out.borrow().clone();
        (r, t)
    }
    fn as_pb(&self, name: &'static str, v: &V, now: u64) -> (Report, String) {
        self.go_as(name, v, now, RunControls::default())
    }
    fn go(&self, v: &V, now: u64) -> (Report, String) {
        self.as_pb("cu", v, now)
    }
    fn check(&self, name: &'static str, v: &V, now: u64) -> String {
        let out = Rc::new(RefCell::new(String::new()));
        let _ = run_check(
            &Playbook::new(name, pb),
            self.spec(v, now),
            RunOpts {
                log: None,
                stdout: Stdout::Capture(out.clone()),
            },
            CheckOpts::default(),
        );
        out.borrow().clone()
    }
    /// A file in `dir` with this name, its mtime `t` (epoch seconds).
    fn put(&self, dir: &Path, name: &str, t: u64) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, format!("data of {name}\n")).unwrap();
        set_mtime(&p, t);
        p
    }
    /// An owner marker `<owners>/<playbook>/<sum>`.
    fn mark(&self, owners: &Path, playbook: &str, sum: &str) -> PathBuf {
        let d = owners.join(playbook);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(sum);
        std::fs::write(&p, "").unwrap();
        p
    }
    fn write_registry(&self, dirs: &[&Path]) {
        let s: String = dirs.iter().map(|d| format!("{}\n", d.display())).collect();
        std::fs::write(self.p("caches"), s).unwrap();
    }
    fn registry(&self) -> Vec<String> {
        std::fs::read_to_string(self.p("caches"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
    fn log(&self) -> String {
        std::fs::read_to_string(self.p("log")).unwrap_or_default()
    }
}

fn set_mtime(p: &Path, t: u64) {
    let st = Command::new("touch")
        .args(["-h", "-m", "-d", &format!("@{t}")])
        .arg(p)
        .status()
        .unwrap();
    assert!(st.success());
}

fn mtime(p: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(p)
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
        .mtime() as u64
}

/// The playbooks with a marker for `sum` under `owners`, sorted.
fn owners_of(owners: &Path, sum: &str) -> Vec<String> {
    let mut v: Vec<String> = names(owners)
        .into_iter()
        .filter(|p| owners.join(p).join(sum).symlink_metadata().is_ok())
        .collect();
    v.sort();
    v
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|r| {
            r.flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn stat(p: &Path, fmt: &str) -> String {
    let o = Command::new("stat")
        .args(["-c", fmt])
        .arg(p)
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn ok(r: &Report, out: &str) {
    assert!(
        matches!(r.hosts[0].status, HostStatus::Ok | HostStatus::Changed),
        "{:?}\n{out}\n{}",
        r.hosts[0].status,
        r.render()
    );
}

fn only(item: &(String, String, String)) -> V {
    V {
        items: vec![item.clone()],
        ..V::default()
    }
}

fn two(a: &(String, String, String), b: &(String, String, String)) -> V {
    V {
        items: vec![a.clone(), b.clone()],
        ..V::default()
    }
}

fn sum12(s: &str) -> &str {
    &s[..12]
}

// ---- touch on access, at the moment of access ----

#[test]
fn a_hit_touches_the_item_and_marks_it_for_the_playbook() {
    let e = Env::new();
    let a = e.a();
    let main = e.cache();
    std::fs::create_dir_all(&main).unwrap();
    std::fs::write(main.join(&a.1), FILE_A).unwrap();
    set_mtime(&main.join(&a.1), NOW - 10 * DAY);
    // Another item no run names keeps its time.
    let other = hex(9);
    e.put(&main, &other, NOW - 10 * DAY);
    let (r, out) = e.as_pb("P", &only(&a), NOW);
    ok(&r, &out);
    assert_eq!(e.log(), "", "a hit downloads nothing");
    assert_eq!(mtime(&main.join(&a.1)), NOW, "the hit is an access");
    assert_eq!(mtime(&main.join(&other)), NOW - 10 * DAY);
    assert_eq!(owners_of(&e.owners(), &a.1), ["P"]);
    assert_eq!(owners_of(&e.owners(), &other), Vec::<String>::new());
    let m = e.owners().join("P").join(&a.1);
    assert_eq!(std::fs::read(&m).unwrap(), b"", "an empty marker");
    assert_eq!(stat(&m, "%a"), "644");
    assert_eq!(stat(&e.owners(), "%a"), "755");
    assert_eq!(stat(&e.owners().join("P"), "%a"), "755");
    assert_eq!(e.registry(), [main.display().to_string()]);
    // A second run advances it.
    let (r, out) = e.as_pb("P", &only(&a), NOW + 1000);
    ok(&r, &out);
    assert_eq!(mtime(&main.join(&a.1)), NOW + 1000);
    assert_eq!(e.registry(), [main.display().to_string()], "one line");
}

#[test]
fn a_download_touches_and_marks_it_and_nothing_else_is_created_in_the_cache() {
    let e = Env::new();
    let a = e.a();
    let (r, out) = e.as_pb("P", &only(&a), NOW);
    ok(&r, &out);
    assert_eq!(e.log(), "curl http://fake/a.txt\n");
    assert_eq!(mtime(&e.cache().join(&a.1)), NOW, "{out}");
    assert_eq!(owners_of(&e.owners(), &a.1), ["P"]);
    assert_eq!(e.registry(), [e.cache().display().to_string()]);
    assert_eq!(
        names(&e.cache()),
        std::slice::from_ref(&a.1),
        "only the item"
    );
    assert_eq!(names(&e.owners()), ["P"]);
    assert_eq!(names(&e.owners().join("P")), std::slice::from_ref(&a.1));
    // No lock and no record anywhere in the sandbox's cache parent.
    assert_eq!(
        names(e.d.path())
            .into_iter()
            .filter(|n| n.starts_with('.'))
            .collect::<Vec<_>>(),
        Vec::<String>::new()
    );
}

#[test]
fn a_settled_run_still_touches_it() {
    let e = Env::new();
    let a = e.a();
    let v = only(&a);
    let (r, out) = e.go(&v, NOW);
    assert_eq!(r.hosts[0].status, HostStatus::Changed, "{out}");
    let (r, out) = e.go(&v, NOW + 5 * DAY);
    assert_eq!(r.hosts[0].status, HostStatus::Ok, "settled: {out}");
    assert_eq!(
        mtime(&e.cache().join(&a.1)),
        NOW + 5 * DAY,
        "a settled host still reads the entry in prepare"
    );
}

#[test]
fn a_run_that_fails_after_prepare_keeps_the_touch_and_the_marker() {
    for (kill, fail) in [(true, false), (false, true)] {
        let e = Env::new();
        let main = e.cache();
        // An old item: a dead shell runs no end-of-run probe, so it stays.
        let old = hex(1);
        e.put(&main, &old, NOW - 400 * DAY);
        e.write_registry(&[&main]);
        let a = e.a();
        let v = V {
            kill,
            fail,
            ..only(&a)
        };
        let (r, out) = e.as_pb("P", &v, NOW);
        assert!(
            matches!(r.hosts[0].status, HostStatus::Failed { .. }),
            "{:?}\n{out}",
            r.hosts[0].status
        );
        assert_eq!(mtime(&main.join(&a.1)), NOW, "kill={kill}: {out}");
        assert_eq!(owners_of(&e.owners(), &a.1), ["P"], "kill={kill}");
        if kill {
            assert!(
                main.join(&old).exists(),
                "a 400-day item survives a dead shell"
            );
            assert!(!out.contains("removed"), "{out}");
        }
    }
}

#[test]
fn a_check_run_touches_and_creates_nothing() {
    let e = Env::new();
    let a = e.a();
    let main = e.cache();
    std::fs::create_dir_all(&main).unwrap();
    std::fs::write(main.join(&a.1), FILE_A).unwrap();
    set_mtime(&main.join(&a.1), NOW - 10 * DAY);
    let old = hex(1);
    e.put(&main, &old, NOW - 400 * DAY);
    let _ = e.check("P", &only(&a), NOW);
    assert_eq!(mtime(&main.join(&a.1)), NOW - 10 * DAY, "not touched");
    assert!(!e.owners().exists(), "no marker");
    assert!(!e.p("caches").exists(), "no registry line");
    assert!(main.join(&old).exists(), "nothing expired");
    assert_eq!(names(&main), {
        let mut v = vec![a.1.clone(), old.clone()];
        v.sort();
        v
    });
}

// ---- expiry ----

#[test]
fn an_item_at_91_days_is_removed_with_its_markers_and_one_at_89_stays() {
    let e = Env::new();
    let main = e.cache();
    let (old, young, edge) = (hex(1), hex(2), hex(3));
    e.put(&main, &old, NOW - 91 * DAY);
    e.put(&main, &young, NOW - 89 * DAY);
    e.put(&main, &edge, NOW - 90 * DAY);
    for p in ["P", "Q"] {
        e.mark(&e.owners(), p, &old);
        e.mark(&e.owners(), p, &young);
    }
    e.write_registry(&[&main]);
    let a = e.a();
    let (r, out) = e.as_pb("R", &only(&a), NOW);
    ok(&r, &out);
    assert!(!main.join(&old).exists(), "91 days: removed");
    assert!(main.join(&young).exists(), "89 days: stays");
    assert!(main.join(&edge).exists(), "exactly 90 days: stays");
    assert_eq!(owners_of(&e.owners(), &old), Vec::<String>::new());
    assert_eq!(owners_of(&e.owners(), &young), ["P", "Q"]);
    assert_eq!(mtime(&main.join(&young)), NOW - 89 * DAY);
    assert!(
        out.contains(&format!(
            "cache {}: removed 1 items (0.0 MB) unused for 90 days",
            main.display()
        )),
        "{out}"
    );
}

#[test]
fn a_per_filesystem_cache_is_registered_marked_and_expired() {
    let e = Env::new();
    std::fs::create_dir_all(e.p("fsB/etc")).unwrap();
    let a = e.a();
    let v = V {
        items: vec![(
            a.0.clone(),
            a.1.clone(),
            e.p("fsB/etc/a.txt").display().to_string(),
        )],
        ..V::default()
    };
    let (r, out) = e.as_pb("P", &v, NOW);
    ok(&r, &out);
    let fsb = e.fsb_cache();
    assert!(fsb.join(&a.1).exists(), "downloaded onto fsB: {out}");
    assert!(
        e.registry().contains(&fsb.display().to_string()),
        "registered: {:?}",
        e.registry()
    );
    assert_eq!(mtime(&fsb.join(&a.1)), NOW);
    assert_eq!(owners_of(&e.fsb_owners(), &a.1), ["P"]);
    assert_eq!(names(&fsb), std::slice::from_ref(&a.1));
    // An entry nobody names, 91 days unaccessed, in that cache.
    let old = hex(7);
    e.put(&fsb, &old, NOW - 91 * DAY);
    e.mark(&e.fsb_owners(), "Q", &old);
    // And a registered directory that is gone.
    let gone = e.p("fsC/.telepatch/cache/sha256");
    let mut reg = e.registry();
    reg.push(gone.display().to_string());
    std::fs::write(e.p("caches"), reg.join("\n") + "\n").unwrap();
    let (r, out) = e.as_pb("P", &v, NOW + 10);
    ok(&r, &out);
    assert!(
        !fsb.join(&old).exists(),
        "expired in the per-filesystem cache"
    );
    assert_eq!(owners_of(&e.fsb_owners(), &old), Vec::<String>::new());
    assert!(fsb.join(&a.1).exists());
    assert_eq!(mtime(&fsb.join(&a.1)), NOW + 10);
    assert!(
        out.contains(&format!(
            "cache {}: removed 1 items (0.0 MB) unused for 90 days",
            fsb.display()
        )),
        "{out}"
    );
    // Runs never rewrite the registry: the dead line stays, and creates
    // nothing.
    assert!(e.registry().contains(&gone.display().to_string()));
    assert!(!e.p("fsC").exists(), "a dead line creates nothing");
}

#[test]
fn only_hex_named_files_in_registered_cache_dirs_are_ever_removed() {
    let e = Env::new();
    let main = e.cache();
    let a = e.a();
    let v = only(&a);
    let ancient = NOW - 400 * DAY;
    // What may go: one registered 91-day-old item.
    let doomed = hex(1);
    e.put(&main, &doomed, NOW - 91 * DAY);
    // What must stay, in the registered cache directory.
    let keep_names = [
        "A".repeat(64),
        "a".repeat(63),
        "a".repeat(65),
        format!("{}.part", hex(2)),
        format!(".{}.part", hex(3)),
        format!(".{}", hex(4)),
        "notes.txt".to_string(),
    ];
    for n in &keep_names {
        e.put(&main, n, ancient);
    }
    // A directory and a symlink with a 64-hex name, both ancient.
    std::fs::create_dir_all(main.join(hex(5))).unwrap();
    std::fs::write(main.join(hex(5)).join("inner"), "x").unwrap();
    set_mtime(&main.join(hex(5)), ancient);
    let outside = e.p("outside-target");
    std::fs::write(&outside, "target of a link").unwrap();
    set_mtime(&outside, ancient);
    std::os::unix::fs::symlink(&outside, main.join(hex(6))).unwrap();
    set_mtime(&main.join(hex(6)), ancient);
    e.write_registry(&[&main]);
    // Other data: another run's stage, a tmp tree and an undo directory
    // with 64-hex names, an unregistered cache-shaped directory.
    let other_run = e.p("stage/other-run");
    e.put(&other_run, &format!("f-{}", hex(8)), ancient);
    e.put(&other_run, &hex(8), ancient);
    let tmp = e.p("fsB/.telepatch/tmp/oldrun/fetch");
    e.put(&tmp, &hex(9), ancient);
    let undo = e.p("fsB/.telepatch/undo/root/abc");
    e.put(&undo, &hex(10), ancient);
    let stray = e.fsb_cache();
    e.put(&stray, &hex(11), ancient);
    e.mark(&e.fsb_owners(), "cu", &hex(11));

    let (r, out) = e.go(&v, NOW);
    ok(&r, &out);
    assert!(!main.join(&doomed).exists(), "the one thing that may go");
    for n in &keep_names {
        assert!(main.join(n).exists(), "{n} must stay: {:?}", names(&main));
    }
    assert!(
        main.join(hex(5)).join("inner").exists(),
        "a directory stays"
    );
    assert!(
        main.join(hex(6)).symlink_metadata().is_ok() && outside.exists(),
        "a symlink and its target stay"
    );
    assert!(other_run.join(hex(8)).exists());
    assert!(other_run.join(format!("f-{}", hex(8))).exists());
    assert!(tmp.join(hex(9)).exists(), ".telepatch/tmp is untouched");
    assert!(undo.join(hex(10)).exists(), "undo data is untouched");
    assert!(
        stray.join(hex(11)).exists(),
        "an unregistered directory stays"
    );
    assert_eq!(owners_of(&e.fsb_owners(), &hex(11)), ["cu"], "{out}");
    assert!(!out.contains(&stray.display().to_string()), "{out}");
}

#[test]
fn a_registry_line_outside_the_sandbox_is_refused_and_untouched() {
    let e = Env::new();
    // A real directory outside the sandbox, in a tempdir of its own.
    // Shaped like a per-filesystem cache, so only the sandbox check stops it.
    let outside = tempfile::tempdir().unwrap();
    let od = outside.path().join("fs/.telepatch/cache/sha256");
    std::fs::create_dir_all(&od).unwrap();
    let old = hex(1);
    let p = e.put(&od, &old, NOW - 400 * DAY);
    std::fs::write(od.join(".telepatch-usage"), "v1\n").unwrap();
    let ow = outside.path().join("fs/.telepatch/cache/owners");
    e.mark(&ow, "cu", &old);
    let snap = |d: &Path| -> Vec<(String, Vec<u8>)> {
        names(d)
            .into_iter()
            .map(|n| (n.clone(), std::fs::read(d.join(&n)).unwrap_or_default()))
            .collect()
    };
    let before = snap(&od);
    // The registry (in the sandbox) lists it, besides the main cache.
    let main = e.cache();
    std::fs::create_dir_all(&main).unwrap();
    e.write_registry(&[&main, &od]);
    let a = e.a();
    let (r, out) = e.go(&V::default(), NOW);
    ok(&r, &out);
    let (r, out) = e.go(&only(&a), NOW);
    ok(&r, &out);
    assert_eq!(snap(&od), before, "nothing outside the sandbox changed");
    assert_eq!(mtime(&p), NOW - 400 * DAY);
    assert!(ow.join("cu").join(&old).exists());
    // The sandbox's own cache is still touched.
    assert_eq!(mtime(&main.join(&a.1)), NOW);
    assert!(
        e.registry().contains(&od.display().to_string()),
        "{:?}",
        e.registry()
    );
}

#[test]
fn two_concurrent_local_runs_both_touch_and_mark_and_the_registry_has_no_torn_line() {
    let e = Env::new();
    let (a, b) = (e.a(), e.b());
    for round in 0..4u64 {
        let now = NOW + round * 100;
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|s| {
            for (name, item) in [("P", &a), ("Q", &b)] {
                let (e, barrier) = (&e, &barrier);
                s.spawn(move || {
                    let v = only(item);
                    barrier.wait();
                    let (r, out) = e.as_pb(name, &v, now);
                    ok(&r, &out);
                });
            }
        });
        assert_eq!(mtime(&e.cache().join(&a.1)), now, "round {round}");
        assert_eq!(mtime(&e.cache().join(&b.1)), now, "round {round}");
        assert_eq!(owners_of(&e.owners(), &a.1), ["P"], "round {round}");
        assert_eq!(owners_of(&e.owners(), &b.1), ["Q"], "round {round}");
        let reg = e.registry();
        assert!(!reg.is_empty());
        for l in &reg {
            assert_eq!(
                l,
                &e.cache().display().to_string(),
                "round {round}: {reg:?}"
            );
        }
    }
}

#[test]
fn a_registry_line_that_is_not_a_cache_directory_is_ignored() {
    let e = Env::new();
    // Inside the sandbox, but not the main cache nor `*/.telepatch/cache/sha256`.
    let od = e.p("etc");
    let old = hex(1);
    let p = e.put(&od, &old, NOW - 400 * DAY);
    let main = e.cache();
    std::fs::create_dir_all(&main).unwrap();
    e.write_registry(&[&main, &od]);
    let a = e.a();
    let (r, out) = e.go(&only(&a), NOW);
    ok(&r, &out);
    assert!(p.exists(), "a file in a non-cache directory stays");
    assert_eq!(names(&od), vec![old.clone()], "nothing was written there");
    assert!(out.contains("not a telepatch cache directory"), "{out}");
    assert_eq!(mtime(&main.join(&a.1)), NOW);
}

// ---- legacy files (T121's record and locks, never shipped) ----

#[test]
fn the_legacy_files_are_removed_and_their_directory_s_items_survive_the_run() {
    let e = Env::new();
    let main = e.cache();
    let fsb = e.fsb_cache();
    let (m1, f1, plain) = (hex(1), hex(2), hex(3));
    e.put(&main, &m1, NOW - 400 * DAY);
    e.put(&fsb, &f1, NOW - 400 * DAY);
    let legacy = [
        ".telepatch-usage",
        ".telepatch-usage.tmp.123",
        ".telepatch-usage.bad-5",
        ".telepatch-usage.lock",
    ];
    for d in [&main, &fsb] {
        for n in legacy {
            std::fs::write(d.join(n), "v1\n").unwrap();
        }
    }
    std::fs::write(e.p(".caches.lock"), "").unwrap();
    std::fs::write(e.p(".lock"), "").unwrap();
    // A registered directory with no record: its old item expires as usual.
    let fsc = e.p("fsC/.telepatch/cache/sha256");
    e.put(&fsc, &plain, NOW - 400 * DAY);
    // An unregistered cache-shaped directory keeps its record.
    let stray = e.p("fsD/.telepatch/cache/sha256");
    e.put(&stray, &hex(4), NOW - 400 * DAY);
    std::fs::write(stray.join(".telepatch-usage"), "v1\n").unwrap();
    e.write_registry(&[&main, &fsb, &fsc]);
    let (r, out) = e.go(&V::default(), NOW);
    ok(&r, &out);
    for d in [&main, &fsb] {
        for n in legacy {
            assert!(!d.join(n).exists(), "{n} in {}", d.display());
        }
    }
    assert!(!e.p(".caches.lock").exists());
    assert!(!e.p(".lock").exists());
    assert!(main.join(&m1).exists(), "{out}");
    assert!(fsb.join(&f1).exists(), "{out}");
    assert_eq!(mtime(&main.join(&m1)), NOW, "counted as accessed now");
    assert_eq!(mtime(&fsb.join(&f1)), NOW);
    assert!(
        !fsc.join(&plain).exists(),
        "no record there: expiry as usual"
    );
    assert!(
        stray.join(".telepatch-usage").exists(),
        "unregistered: kept"
    );
    assert!(stray.join(hex(4)).exists());
    // Once: a second run much later expires them like any other item.
    let (r, out) = e.go(&V::default(), NOW + 91 * DAY);
    ok(&r, &out);
    assert!(!main.join(&m1).exists(), "{out}");
}

// ---- ownership ----

/// P has fetched A and B (both cached, both marked by P).
fn p_has_a_and_b(e: &Env) -> ((String, String, String), (String, String, String)) {
    let (a, b) = (e.a(), e.b());
    let (r, out) = e.as_pb("P", &two(&a, &b), NOW);
    ok(&r, &out);
    assert_eq!(owners_of(&e.owners(), &a.1), ["P"], "{out}");
    assert_eq!(owners_of(&e.owners(), &b.1), ["P"], "{out}");
    (a, b)
}

#[test]
fn a_full_run_removes_what_the_playbook_no_longer_fetches() {
    let e = Env::new();
    let (a, b) = p_has_a_and_b(&e);
    let (r, out) = e.as_pb("P", &only(&a), NOW + DAY);
    ok(&r, &out);
    assert!(!e.cache().join(&b.1).exists(), "B is removed\n{out}");
    assert!(e.cache().join(&a.1).exists());
    assert_eq!(owners_of(&e.owners(), &b.1), Vec::<String>::new());
    assert_eq!(owners_of(&e.owners(), &a.1), ["P"]);
    assert!(
        out.contains(&format!(
            "cache {}: removed {} (0.0 MB): no longer fetched by P",
            e.cache().display(),
            sum12(&b.1)
        )),
        "{out}"
    );
    assert!(!out.contains("ownership not checked"), "{out}");
    // P drops everything: its owner directory goes with its last marker.
    let (r, out) = e.as_pb("P", &V::default(), NOW + 2 * DAY);
    ok(&r, &out);
    assert!(!e.cache().join(&a.1).exists(), "{out}");
    assert!(!e.owners().join("P").exists(), "empty owners/P is removed");
}

/// After `p_has_a_and_b`, a run that fetches only A but is not entitled:
/// no marker is removed, the stream says why once.
fn assert_kept(e: &Env, b: &(String, String, String), out: &str, why: &str) {
    assert!(e.cache().join(&b.1).exists(), "B stays\n{out}");
    assert_eq!(owners_of(&e.owners(), &b.1), ["P"], "{out}");
    assert_eq!(
        out.matches("cache: ownership not checked (").count(),
        1,
        "once\n{out}"
    );
    assert!(
        out.contains(&format!("cache: ownership not checked ({why}")),
        "{out}"
    );
    assert!(!out.contains("no longer fetched"), "{out}");
}

#[test]
fn an_only_run_removes_no_marker_and_says_why() {
    let e = Env::new();
    let (a, b) = p_has_a_and_b(&e);
    let (_, out) = e.go_as(
        "P",
        &only(&a),
        NOW + DAY,
        RunControls {
            only: vec!["nosuch".into()],
            ..RunControls::default()
        },
    );
    assert_kept(&e, &b, &out, "--only");
}

#[test]
fn a_check_run_says_ownership_was_not_checked_and_removes_nothing() {
    let e = Env::new();
    let (a, b) = p_has_a_and_b(&e);
    let out = e.check("P", &only(&a), NOW + DAY);
    assert_kept(&e, &b, &out, "--check");
}

#[test]
fn a_failing_op_removes_no_marker_and_says_why() {
    let e = Env::new();
    let (a, b) = p_has_a_and_b(&e);
    let mut v = only(&a);
    v.fail = true;
    let (r, out) = e.as_pb("P", &v, NOW + DAY);
    assert!(
        matches!(r.hosts[0].status, HostStatus::Failed { .. }),
        "{out}"
    );
    assert_kept(&e, &b, &out, "the host ended failed");
    assert_eq!(mtime(&e.cache().join(&a.1)), NOW + DAY, "A still touched");
}

#[test]
fn a_reboot_pending_stop_removes_no_marker_and_says_why() {
    let e = Env::new();
    let (a, b) = p_has_a_and_b(&e);
    let mut v = only(&a);
    v.stop_dir = Some(e.p("out").display().to_string());
    let (r, out) = e.as_pb("P", &v, NOW + DAY);
    assert!(
        matches!(r.hosts[0].status, HostStatus::RebootPending(_)),
        "{out}"
    );
    assert!(!e.p("out/after").exists(), "stopped at the barrier");
    assert_kept(&e, &b, &out, "the host ");
}

#[test]
fn an_item_another_playbook_marks_stays_and_loses_only_p_s_marker() {
    let e = Env::new();
    let (a, b) = (e.a(), e.b());
    let (r, out) = e.as_pb("Q", &only(&b), NOW);
    ok(&r, &out);
    let (r, out) = e.as_pb("P", &two(&a, &b), NOW + DAY);
    ok(&r, &out);
    assert_eq!(owners_of(&e.owners(), &b.1), ["P", "Q"]);
    let (r, out) = e.as_pb("P", &only(&a), NOW + 2 * DAY);
    ok(&r, &out);
    assert!(e.cache().join(&b.1).exists(), "Q still fetches B\n{out}");
    assert_eq!(owners_of(&e.owners(), &b.1), ["Q"]);
    assert_eq!(owners_of(&e.owners(), &a.1), ["P"]);
    assert!(!out.contains("no longer fetched"), "{out}");
    // Q stops fetching B: now it goes.
    let (r, out) = e.as_pb("Q", &V::default(), NOW + 3 * DAY);
    ok(&r, &out);
    assert!(!e.cache().join(&b.1).exists(), "{out}");
    assert!(e.cache().join(&a.1).exists(), "P's A stays");
    assert_eq!(owners_of(&e.owners(), &a.1), ["P"], "P's marker stays");
    assert!(out.contains("no longer fetched by Q"), "{out}");
}

#[test]
fn an_item_with_no_marker_is_left_to_expiry_on_an_entitled_run() {
    let e = Env::new();
    let main = e.cache();
    let unmarked = hex(7);
    e.put(&main, &unmarked, NOW - DAY);
    e.write_registry(&[&main]);
    let (r, out) = e.as_pb("P", &V::default(), NOW);
    ok(&r, &out);
    assert!(main.join(&unmarked).exists(), "{out}");
    assert!(!e.owners().join("P").exists(), "no marker was made for it");
}

#[test]
fn two_playbooks_on_one_host_each_mark_what_they_use() {
    let e = Env::new();
    let (a, b) = (e.a(), e.b());
    let (r, out) = e.as_pb("P", &only(&a), NOW);
    ok(&r, &out);
    let (r, out) = e.as_pb("Q", &two(&a, &b), NOW + DAY);
    ok(&r, &out);
    assert_eq!(owners_of(&e.owners(), &a.1), ["P", "Q"]);
    assert_eq!(owners_of(&e.owners(), &b.1), ["Q"]);
}

#[test]
fn a_playbook_name_that_cannot_be_an_owner_is_a_trace_error() {
    // The encoded form is at most 200 bytes: 67 `/` encode to 201.
    let long: &'static str = Box::leak("n".repeat(201).into_boxed_str());
    let slashes: &'static str = Box::leak("/".repeat(67).into_boxed_str());
    for name in ["", "a,b", "a\tb", "a\nb", "a\0b", long, slashes] {
        let e = Env::new();
        let a = e.a();
        let (r, out) = e.as_pb(name, &only(&a), NOW);
        match &r.hosts[0].status {
            HostStatus::Error(m) => assert!(m.contains("playbook name"), "{m}"),
            s => panic!("{name:?}: {s:?}\n{out}"),
        }
        assert!(!e.owners().exists(), "{name:?}: no owner directory");
        assert!(!e.cache().exists(), "{name:?}: nothing fetched");
    }
    // Encoded at exactly 200 bytes is a name: 66 `/` (198) and `nn`.
    let n200: &'static str = Box::leak(format!("{}nn", "/".repeat(66)).into_boxed_str());
    assert_eq!(owner_dir(n200).len(), 200);
    let e = Env::new();
    let a = e.a();
    let (r, out) = e.as_pb(n200, &only(&a), NOW);
    ok(&r, &out);
    assert_eq!(owners_of(&e.owners(), &a.1), [owner_dir(n200)]);
}

// ---- owner directory names (T135 amendment: percent-encoded) ----

#[test]
fn owner_dir_encodes_every_byte_but_the_safe_set_and_a_leading_dot() {
    for (name, dir) in [
        ("dev_desktop", "dev_desktop"),
        ("container/dev_base", "container%2Fdev_base"),
        ("a/b", "a%2Fb"),
        ("a%2Fb", "a%252Fb"),
        (".x", "%2Ex"),
        (".", "%2E"),
        ("..", "%2E."),
        ("a.b-c_D9", "a.b-c_D9"),
        ("a b", "a%20b"),
        ("\u{e9}", "%C3%A9"),
    ] {
        assert_eq!(owner_dir(name), dir, "{name:?}");
        assert_eq!(owner_name(dir).as_deref(), Some(name), "{dir:?}");
    }
    // Only what `owner_dir` makes decodes: not a raw `/`, a lower-case or
    // short escape, an escaped safe byte, a bare leading `.` or bad UTF-8.
    for dir in ["", "a/b", "a%2fb", "a%2", "%41", ".x", "..", "%zz", "%FF"] {
        assert_eq!(owner_name(dir), None, "{dir:?}");
    }
}

#[test]
fn a_playbook_with_slashes_marks_under_its_encoded_directory() {
    let e = Env::new();
    let a = e.a();
    let (r, out) = e.as_pb("container/dev_base", &only(&a), NOW);
    ok(&r, &out);
    assert!(
        e.owners().join("container%2Fdev_base").join(&a.1).is_file(),
        "{out}"
    );
    assert_eq!(names(&e.owners()), ["container%2Fdev_base"]);
}

#[test]
fn names_that_encode_alike_do_not_and_none_is_hidden_or_a_parent() {
    let e = Env::new();
    let (a, b) = (e.a(), e.b());
    for name in ["a/b", "a%2Fb", ".x", ".", ".."] {
        let (r, out) = e.as_pb(name, &only(&a), NOW);
        ok(&r, &out);
    }
    assert_eq!(
        names(&e.owners()),
        ["%2E", "%2E.", "%2Ex", "a%252Fb", "a%2Fb"]
    );
    // `a/b` takes B too, then drops A: `a%2Fb`'s marker on A keeps A.
    let (r, out) = e.as_pb("a/b", &two(&a, &b), NOW + DAY);
    ok(&r, &out);
    let (r, out) = e.as_pb("a/b", &only(&b), NOW + 2 * DAY);
    ok(&r, &out);
    assert!(e.cache().join(&a.1).exists(), "{out}");
    assert!(!e.owners().join("a%2Fb").join(&a.1).exists(), "{out}");
    assert!(e.owners().join("a%252Fb").join(&a.1).exists(), "{out}");
    assert!(!out.contains("no longer fetched"), "{out}");
    // `a/b` drops B, which only it marked: the line names the playbook, not
    // its directory, and only `a%2Fb` goes.
    let (r, out) = e.as_pb("a/b", &V::default(), NOW + 3 * DAY);
    ok(&r, &out);
    assert!(!e.cache().join(&b.1).exists(), "{out}");
    assert!(out.contains("no longer fetched by a/b"), "{out}");
    assert_eq!(names(&e.owners()), ["%2E", "%2E.", "%2Ex", "a%252Fb"]);
}

#[test]
fn ownership_removal_touches_only_p_s_markers_and_regular_hex_files_of_registered_caches() {
    let e = Env::new();
    let (a, b) = p_has_a_and_b(&e);
    let main = e.cache();
    // Entries P marks that are not regular hex files.
    let outside = e.p("tmp/outside");
    std::fs::write(&outside, "keep me").unwrap();
    let link = hex(0x11);
    std::os::unix::fs::symlink(&outside, main.join(&link)).unwrap();
    let dir = hex(0x12);
    std::fs::create_dir_all(main.join(&dir)).unwrap();
    std::fs::write(main.join(&dir).join("inner"), "keep").unwrap();
    std::fs::write(main.join("not-a-sum"), "keep").unwrap();
    for s in [&link, &dir] {
        e.mark(&e.owners(), "P", s);
    }
    // P's directory holds a non-hex name: not a marker, left alone.
    std::fs::write(e.owners().join("P").join("notes"), "keep").unwrap();
    // Another playbook's item and directory, an unmarked item.
    let other = hex(0x13);
    e.put(&main, &other, NOW);
    let qm = e.mark(&e.owners(), "Q", &other);
    let unmarked = hex(0x14);
    e.put(&main, &unmarked, NOW);
    // An unregistered per-filesystem cache whose markers name P, tmp and
    // undo data, and another run's stage.
    let fsb = e.fsb_cache();
    let unreg = hex(0x15);
    e.put(&fsb, &unreg, NOW);
    let um = e.mark(&e.fsb_owners(), "P", &unreg);
    let tmpf = e.p("fsB/.telepatch/tmp/oldrun/fetch");
    std::fs::create_dir_all(&tmpf).unwrap();
    std::fs::write(tmpf.join(hex(0x16)), "keep").unwrap();
    let undo = e.p("fsB/.telepatch/undo");
    std::fs::create_dir_all(&undo).unwrap();
    std::fs::write(undo.join(hex(0x17)), "keep").unwrap();
    let other_run = e.p("stage/otherrun");
    std::fs::create_dir_all(&other_run).unwrap();
    std::fs::write(other_run.join(hex(0x18)), "keep").unwrap();
    e.write_registry(&[&main]);

    // P keeps only A.
    let (r, out) = e.as_pb("P", &only(&a), NOW + DAY);
    ok(&r, &out);
    assert!(!main.join(&b.1).exists(), "B goes\n{out}");
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep me");
    assert!(std::fs::symlink_metadata(main.join(&link)).is_ok(), "link");
    assert!(main.join(&dir).join("inner").exists(), "dir");
    assert!(main.join("not-a-sum").exists());
    assert!(main.join(&other).exists(), "Q's item");
    assert!(qm.exists(), "Q's marker");
    assert!(e.owners().join("P").join("notes").exists());
    assert!(main.join(&unmarked).exists(), "an unmarked item");
    assert!(main.join(&a.1).exists(), "A is in F");
    assert!(fsb.join(&unreg).exists(), "an unregistered dir is not read");
    assert!(um.exists(), "nor are its markers");
    assert!(tmpf.join(hex(0x16)).exists(), ".telepatch/tmp");
    assert!(undo.join(hex(0x17)).exists(), "undo data");
    assert!(other_run.join(hex(0x18)).exists(), "another run's stage");
    assert_eq!(out.matches("no longer fetched").count(), 1, "{out}");
}

#[test]
fn an_item_in_f_stays_even_when_another_registered_dir_holds_a_copy() {
    let e = Env::new();
    let (a, b) = (e.a(), e.b());
    let (r, out) = e.as_pb("P", &two(&a, &b), NOW);
    ok(&r, &out);
    // A stray twin of B in a registered per-filesystem cache, marked by P.
    let fsb = e.fsb_cache();
    std::fs::create_dir_all(&fsb).unwrap();
    std::fs::write(fsb.join(&b.1), FILE_B).unwrap();
    e.mark(&e.fsb_owners(), "P", &b.1);
    e.write_registry(&[&e.cache(), &fsb]);
    let (r, out) = e.as_pb("P", &two(&a, &b), NOW + DAY);
    ok(&r, &out);
    assert!(fsb.join(&b.1).exists(), "the sum is in F\n{out}");
    assert!(e.cache().join(&b.1).exists());
}

#[test]
fn a_playbook_name_with_shell_metacharacters_is_data_in_the_script() {
    let e = Env::new();
    let (a, b) = (e.a(), e.b());
    // The canary is `<d>/stagex`.
    let canary = e.p("stagex");
    let name: &'static str =
        "w\"i 'r' $(touch \"$TP_FETCH_STAGE\"x) `touch \"$TP_FETCH_STAGE\"x` * ;";
    let (r, out) = e.as_pb(name, &two(&a, &b), NOW);
    ok(&r, &out);
    assert_eq!(owners_of(&e.owners(), &a.1), [owner_dir(name)], "{out}");
    // A second playbook's item must not match the name as a glob.
    let (r, out) = e.as_pb("other", &only(&b), NOW + DAY);
    ok(&r, &out);
    let (r, out) = e.as_pb(name, &only(&a), NOW + 2 * DAY);
    ok(&r, &out);
    assert!(e.cache().join(&b.1).exists(), "other still marks B\n{out}");
    assert_eq!(owners_of(&e.owners(), &b.1), ["other"]);
    let (r, out) = e.as_pb(name, &V::default(), NOW + 3 * DAY);
    ok(&r, &out);
    assert!(!e.cache().join(&a.1).exists(), "{out}");
    assert!(
        out.contains(&format!("no longer fetched by {name}")),
        "{out}"
    );
    assert!(!canary.exists(), "the name was never run");
}
