//! T121: the cache registry, the usage record and the 90-day expiry.
//!
//! Everything runs on the local transport inside a sandbox directory `<d>`
//! (T124): the main cache is `<d>/cache`, so the registry is `<d>/caches`,
//! the record lock `<d>/cache/.telepatch-usage.lock` and the run lock
//! `<d>/.lock`. `TP_CACHE_NOW` is the clock. A fake `curl` serves
//! `http://fake/<name>` from `<d>/www/<name>`. `<d>/fsB` is a filesystem of
//! its own (`TP_FS_ROOTS`). A test that removes anything also asserts what
//! must stay.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use telepatch::executor::{CheckOpts, HostSpec, RunOpts, Stdout, Target, run, run_check};
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
    /// Write whether the run lock is free, at `<dir>/lockstate`, from an op.
    probe_lock: Option<String>,
}

fn pb(h: Host<V>) -> PlaybookFuture {
    Box::pin(async move {
        let v = h.vars.clone();
        for (url, sum, target) in &v.items {
            h.file(target).from(h.fetch(url).sha256(sum));
        }
        if let Some(d) = &v.probe_lock {
            h.raw(format!(
                "if flock -x -n {d}/.lock true; then echo free > {d}/lockstate; else echo held > {d}/lockstate; fi\ntrue"
            ));
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
    fn fsb_cache(&self) -> PathBuf {
        self.p("fsB/.telepatch/cache/sha256")
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
    fn transport(&self, now: u64, without_flock: bool) -> LocalTransport {
        let base = if without_flock {
            let farm = self.p("farm");
            std::fs::create_dir_all(&farm).unwrap();
            for e in std::fs::read_dir("/usr/bin").unwrap().flatten() {
                let n = e.file_name();
                if n == "flock" {
                    continue;
                }
                let _ = std::os::unix::fs::symlink(e.path(), farm.join(&n));
            }
            farm.display().to_string()
        } else {
            std::env::var("PATH").unwrap()
        };
        LocalTransport::new()
            .uid(0)
            .sandboxed_at(self.d.path())
            .env("PATH", &format!("{}:{base}", self.p("bin").display()))
            .env("TMPDIR", &self.p("tmp").display().to_string())
            .env("TP_FETCH_STAGE", &self.p("stage").display().to_string())
            .env("TP_FETCH_CACHE", &self.cache().display().to_string())
            .env("TP_FS_ROOTS", &self.p("fsB").display().to_string())
            .env("TP_CACHE_NOW", &now.to_string())
    }
    fn spec(&self, v: &V, now: u64, without_flock: bool) -> Vec<HostSpec<V>> {
        vec![HostSpec {
            groups: Vec::new(),
            name: "h1".into(),
            target: Target::Local(self.transport(now, without_flock)),
            vars: v.clone(),
        }]
    }
    fn go_with(&self, v: &V, now: u64, without_flock: bool) -> (Report, String) {
        let out = Rc::new(RefCell::new(String::new()));
        let r = run(
            &Playbook::new("cu", pb),
            self.spec(v, now, without_flock),
            RunOpts {
                log: None,
                stdout: Stdout::Capture(out.clone()),
            },
        );
        let t = out.borrow().clone();
        (r, t)
    }
    fn go(&self, v: &V, now: u64) -> (Report, String) {
        self.go_with(v, now, false)
    }
    /// A file in `dir` with this name, `age_days` old by mtime.
    fn put(&self, dir: &Path, name: &str, age_days: u64) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, format!("data of {name}\n")).unwrap();
        let st = Command::new("touch")
            .args(["-d", &format!("{age_days} days ago")])
            .arg(&p)
            .status()
            .unwrap();
        assert!(st.success());
        p
    }
    /// Write a record by hand: `(sum, time, owners)`.
    fn write_record(&self, dir: &Path, lines: &[(String, u64, &str)]) {
        std::fs::create_dir_all(dir).unwrap();
        let mut s = String::from("v1\n");
        for (sum, t, o) in lines {
            s.push_str(&format!("{sum}\t{t}\t{o}\n"));
        }
        std::fs::write(dir.join(".telepatch-usage"), s).unwrap();
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
}

/// The record in `dir`, parsed strictly: `v1`, then `<64 hex> TAB <digits>
/// TAB <owners>` per line. Panics on anything else.
fn record(dir: &Path) -> Vec<(String, u64, String)> {
    let s = std::fs::read_to_string(dir.join(".telepatch-usage"))
        .unwrap_or_else(|e| panic!("no record in {}: {e}", dir.display()));
    let mut it = s.lines();
    assert_eq!(it.next(), Some("v1"), "{s:?}");
    let mut out = Vec::new();
    for l in it {
        let f: Vec<&str> = l.split('\t').collect();
        assert_eq!(f.len(), 3, "{l:?} in {s:?}");
        assert!(
            f[0].len() == 64 && f[0].bytes().all(|b| b.is_ascii_hexdigit()),
            "{l:?}"
        );
        out.push((f[0].to_string(), f[1].parse().unwrap(), f[2].to_string()));
    }
    out
}

fn time_of(dir: &Path, sum: &str) -> Option<u64> {
    record(dir).into_iter().find(|r| r.0 == sum).map(|r| r.1)
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

#[test]
fn a_hit_records_the_item_and_a_second_run_advances_its_time() {
    let e = Env::new();
    let a = e.a();
    let sum = a.1.clone();
    // The entry is in the cache already, with a record that holds it, and
    // another entry, at an old time, that no run names.
    let main = e.cache();
    std::fs::create_dir_all(&main).unwrap();
    std::fs::write(main.join(&sum), FILE_A).unwrap();
    let other = hex(9);
    e.put(&main, &other, 1);
    e.write_record(
        &main,
        &[
            (sum.clone(), NOW - 10 * DAY, ""),
            (other.clone(), NOW - 10 * DAY, ""),
        ],
    );
    let v = only(&a);
    let (r, out) = e.go(&v, NOW);
    ok(&r, &out);
    assert_eq!(std::fs::read_to_string(e.p("log")).unwrap_or_default(), "");
    assert_eq!(time_of(&main, &sum), Some(NOW), "the hit is a use");
    assert_eq!(
        time_of(&main, &other),
        Some(NOW - 10 * DAY),
        "an unused item keeps its time"
    );
    let (r, out) = e.go(&v, NOW + 1000);
    ok(&r, &out);
    assert_eq!(time_of(&main, &sum), Some(NOW + 1000));
    assert_eq!(time_of(&main, &other), Some(NOW - 10 * DAY));
    // The record is 0644 and the directory is registered.
    let mode = Command::new("stat")
        .args(["-c", "%a"])
        .arg(main.join(".telepatch-usage"))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&mode.stdout).trim(), "644");
    assert_eq!(e.registry(), [main.display().to_string()]);
}

#[test]
fn a_download_records_it() {
    let e = Env::new();
    let a = e.a();
    let sum = a.1.clone();
    let (r, out) = e.go(&only(&a), NOW);
    ok(&r, &out);
    assert_eq!(
        std::fs::read_to_string(e.p("log")).unwrap(),
        "curl http://fake/a.txt\n"
    );
    assert_eq!(
        record(&e.cache()),
        [(sum.clone(), NOW, String::new())],
        "{out}"
    );
    assert_eq!(e.registry(), [e.cache().display().to_string()]);
    assert_eq!(
        names(&e.cache()),
        [
            ".telepatch-usage".to_string(),
            ".telepatch-usage.lock".to_string(),
            sum
        ]
    );
}

#[test]
fn a_settled_run_records_it() {
    let e = Env::new();
    let a = e.a();
    let v = only(&a);
    let (r, out) = e.go(&v, NOW);
    assert_eq!(r.hosts[0].status, HostStatus::Changed, "{out}");
    let (r, out) = e.go(&v, NOW + 5 * DAY);
    assert_eq!(r.hosts[0].status, HostStatus::Ok, "settled: {out}");
    assert_eq!(
        time_of(&e.cache(), &a.1),
        Some(NOW + 5 * DAY),
        "a settled host still reads the entry in prepare"
    );
}

#[test]
fn an_item_at_91_days_is_removed_and_one_at_89_stays() {
    let e = Env::new();
    let main = e.cache();
    let (old, young, edge) = (hex(1), hex(2), hex(3));
    for n in [&old, &young, &edge] {
        e.put(&main, n, 0);
    }
    e.write_record(
        &main,
        &[
            (old.clone(), NOW - 91 * DAY, ""),
            (young.clone(), NOW - 89 * DAY, ""),
            (edge.clone(), NOW - 90 * DAY, ""),
        ],
    );
    e.write_registry(&[&main]);
    let a = e.a();
    let (r, out) = e.go(&only(&a), NOW);
    ok(&r, &out);
    assert!(!main.join(&old).exists(), "91 days: removed");
    assert!(main.join(&young).exists(), "89 days: stays");
    assert!(main.join(&edge).exists(), "exactly 90 days: stays");
    assert_eq!(time_of(&main, &old), None, "its line goes with it");
    assert_eq!(time_of(&main, &young), Some(NOW - 89 * DAY));
    assert_eq!(time_of(&main, &a.1), Some(NOW));
    assert!(
        out.contains(&format!(
            "cache {}: removed 1 items (0.0 MB) unused for 90 days",
            main.display()
        )),
        "{out}"
    );
}

#[test]
fn a_pre_existing_file_is_adopted_not_removed() {
    let e = Env::new();
    let main = e.cache();
    // A file from before this step: no record at all, very old by mtime.
    let old = hex(1);
    e.put(&main, &old, 400);
    let a = e.a();
    let v = only(&a);
    let (r, out) = e.go(&v, NOW);
    ok(&r, &out);
    assert!(main.join(&old).exists());
    assert_eq!(time_of(&main, &old), Some(NOW), "adopted at now");
    // The same with a record that lacks the file.
    let old2 = hex(2);
    e.put(&main, &old2, 400);
    let (r, out) = e.go(&v, NOW + 200 * DAY);
    ok(&r, &out);
    assert!(main.join(&old2).exists());
    assert_eq!(time_of(&main, &old2), Some(NOW + 200 * DAY));
    // The first one is now 200 days unused: it is the first to go.
    assert!(!main.join(&old).exists(), "200 days unused: expired");
}

#[test]
fn a_corrupt_record_is_set_aside_and_nothing_is_removed() {
    let e = Env::new();
    let main = e.cache();
    let ancient = hex(1);
    e.put(&main, &ancient, 0);
    // A header that is not v1, over an otherwise good line that says ancient.
    std::fs::write(
        main.join(".telepatch-usage"),
        format!("v2\n{ancient}\t5\t\n"),
    )
    .unwrap();
    e.write_registry(&[&main]);
    let a = e.a();
    let v = only(&a);
    let (r, out) = e.go(&v, NOW);
    ok(&r, &out);
    assert!(
        main.join(&ancient).exists(),
        "nothing removed on a corrupt record"
    );
    let bad = main.join(format!(".telepatch-usage.bad-{NOW}"));
    assert_eq!(
        std::fs::read_to_string(&bad).unwrap(),
        format!("v2\n{ancient}\t5\t\n"),
        "the old record is kept, byte for byte"
    );
    assert_eq!(
        time_of(&main, &ancient),
        Some(NOW),
        "every present file is now"
    );
    assert_eq!(time_of(&main, &a.1), Some(NOW));
    assert!(out.contains("usage record unreadable"), "{out}");

    // A bad line below a good header counts too.
    std::fs::write(
        main.join(".telepatch-usage"),
        format!("v1\n{ancient}\t5\t\nnot a record line\n"),
    )
    .unwrap();
    let (r, out) = e.go(&v, NOW + 1);
    ok(&r, &out);
    assert!(main.join(&ancient).exists());
    assert!(
        main.join(format!(".telepatch-usage.bad-{}", NOW + 1))
            .exists()
    );
    assert_eq!(time_of(&main, &ancient), Some(NOW + 1));
}

#[test]
fn a_killed_root_shell_records_nothing_and_removes_nothing() {
    let e = Env::new();
    let main = e.cache();
    let old = hex(1);
    e.put(&main, &old, 0);
    e.write_record(&main, &[(old.clone(), NOW - 400 * DAY, "")]);
    e.write_registry(&[&main]);
    let before = std::fs::read(main.join(".telepatch-usage")).unwrap();
    let reg_before = std::fs::read(e.p("caches")).unwrap();
    let a = e.a();
    let v = V {
        kill: true,
        ..only(&a)
    };
    let (r, out) = e.go(&v, NOW);
    assert!(
        matches!(r.hosts[0].status, HostStatus::Failed { .. }),
        "{:?}\n{out}",
        r.hosts[0].status
    );
    // Prepare downloaded the item; the dead shell recorded nothing.
    assert!(main.join(&a.1).exists());
    assert_eq!(
        std::fs::read(main.join(".telepatch-usage")).unwrap(),
        before
    );
    assert_eq!(std::fs::read(e.p("caches")).unwrap(), reg_before);
    assert!(
        main.join(&old).exists(),
        "a 400-day item survives a dead shell"
    );
    assert!(!out.contains("removed"), "{out}");
}

#[test]
fn a_per_filesystem_cache_is_registered_and_expired() {
    let e = Env::new();
    std::fs::create_dir_all(e.p("fsB/etc")).unwrap();
    let a = e.a();
    let sum = a.1.clone();
    let v = V {
        items: vec![(
            a.0.clone(),
            sum.clone(),
            e.p("fsB/etc/a.txt").display().to_string(),
        )],
        ..V::default()
    };
    let (r, out) = e.go(&v, NOW);
    ok(&r, &out);
    let fsb = e.fsb_cache();
    assert!(fsb.join(&sum).exists(), "downloaded onto fsB: {out}");
    assert!(
        e.registry().contains(&fsb.display().to_string()),
        "registered: {:?}",
        e.registry()
    );
    assert_eq!(time_of(&fsb, &sum), Some(NOW));
    // An entry nobody names, 91 days unused, in that cache.
    let old = hex(7);
    e.put(&fsb, &old, 0);
    let mut lines: Vec<(String, u64, &str)> =
        record(&fsb).into_iter().map(|l| (l.0, l.1, "")).collect();
    lines.push((old.clone(), NOW - 91 * DAY, ""));
    e.write_record(&fsb, &lines);
    // And a registered directory that is gone.
    let gone = e.p("fsC/.telepatch/cache/sha256");
    let mut reg = e.registry();
    reg.push(gone.display().to_string());
    std::fs::write(e.p("caches"), reg.join("\n") + "\n").unwrap();
    let (r, out) = e.go(&v, NOW + 10);
    ok(&r, &out);
    assert!(
        !fsb.join(&old).exists(),
        "expired in the per-filesystem cache"
    );
    assert!(fsb.join(&sum).exists());
    assert_eq!(time_of(&fsb, &sum), Some(NOW + 10));
    assert!(
        out.contains(&format!(
            "cache {}: removed 1 items (0.0 MB) unused for 90 days",
            fsb.display()
        )),
        "{out}"
    );
    let reg = e.registry();
    assert!(reg.contains(&fsb.display().to_string()), "{reg:?}");
    assert!(
        !reg.contains(&gone.display().to_string()),
        "dropped: {reg:?}"
    );
    assert!(!e.p("fsC").exists(), "a dropped line creates nothing");
}

#[test]
fn only_hex_named_files_in_registered_cache_dirs_are_ever_removed() {
    let e = Env::new();
    let main = e.cache();
    let a = e.a();
    let v = only(&a);
    // What may go: one registered, recorded, 91-day-old item.
    let doomed = hex(1);
    e.put(&main, &doomed, 0);
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
        e.put(&main, n, 400);
    }
    // A directory and a symlink with a 64-hex name.
    std::fs::create_dir_all(main.join(hex(5))).unwrap();
    std::fs::write(main.join(hex(5)).join("inner"), "x").unwrap();
    let outside = e.p("outside-target");
    std::fs::write(&outside, "target of a link").unwrap();
    std::os::unix::fs::symlink(&outside, main.join(hex(6))).unwrap();
    // Records name some of them: a line for the directory or the link is
    // no licence to remove it.
    e.write_record(
        &main,
        &[
            (doomed.clone(), NOW - 91 * DAY, ""),
            (hex(5), NOW - 400 * DAY, ""),
            (hex(6), NOW - 400 * DAY, ""),
        ],
    );
    e.write_registry(&[&main]);
    // Other data: another run's stage, a tmp tree and an undo directory
    // with 64-hex names, an unregistered cache-shaped directory with an old
    // record.
    let other_run = e.p("stage/other-run");
    e.put(&other_run, &format!("f-{}", hex(8)), 400);
    e.put(&other_run, &hex(8), 400);
    let tmp = e.p("fsB/.telepatch/tmp/oldrun/fetch");
    e.put(&tmp, &hex(9), 400);
    let undo = e.p("fsB/.telepatch/undo/root/abc");
    e.put(&undo, &hex(10), 400);
    let stray = e.p("stray-cache");
    e.put(&stray, &hex(11), 400);
    e.write_record(&stray, &[(hex(11), NOW - 400 * DAY, "")]);
    let stray_rec = std::fs::read(stray.join(".telepatch-usage")).unwrap();

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
    assert_eq!(
        std::fs::read(stray.join(".telepatch-usage")).unwrap(),
        stray_rec,
        "and so does its record"
    );
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
    let p = e.put(&od, &old, 400);
    e.write_record(&od, &[(old.clone(), NOW - 400 * DAY, "")]);
    let snap = |d: &Path| -> Vec<(String, Vec<u8>)> {
        names(d)
            .into_iter()
            .map(|n| (n.clone(), std::fs::read(d.join(&n)).unwrap_or_default()))
            .collect()
    };
    let before = snap(&od);
    let mtime = std::fs::metadata(&p).unwrap().modified().unwrap();
    // The registry (in the sandbox) lists it, besides the main cache.
    let main = e.cache();
    std::fs::create_dir_all(&main).unwrap();
    e.write_registry(&[&main, &od]);
    let a = e.a();
    let (r, out) = e.go(&only(&a), NOW);
    ok(&r, &out);
    assert_eq!(snap(&od), before, "nothing outside the sandbox changed");
    assert_eq!(std::fs::metadata(&p).unwrap().modified().unwrap(), mtime);
    assert!(
        !names(&od)
            .iter()
            .any(|n| n.contains("lock") || n.contains("tmp")),
        "no lock or temp file there: {:?}",
        names(&od)
    );
    // The sandbox's own cache is still recorded.
    assert_eq!(time_of(&main, &a.1), Some(NOW));
    // The line is kept as it was, not dropped.
    assert!(
        e.registry().contains(&od.display().to_string()),
        "{:?}",
        e.registry()
    );
}

#[test]
fn two_concurrent_local_runs_both_record_and_the_record_parses() {
    let e = Env::new();
    let (a, b) = (e.a(), e.b());
    let seeded = hex(1);
    e.put(&e.cache(), &seeded, 0);
    for round in 0..4u64 {
        let now = NOW + round * 100;
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|s| {
            for item in [&a, &b] {
                let (e, barrier) = (&e, &barrier);
                s.spawn(move || {
                    let v = only(item);
                    barrier.wait();
                    let (r, out) = e.go(&v, now);
                    ok(&r, &out);
                });
            }
        });
        let rec = record(&e.cache());
        for it in [&a, &b] {
            assert_eq!(
                rec.iter().find(|r| r.0 == it.1).map(|r| r.1),
                Some(now),
                "round {round}: {rec:?}"
            );
        }
        assert!(rec.iter().any(|r| r.0 == seeded), "{rec:?}");
        let mut sums: Vec<&String> = rec.iter().map(|r| &r.0).collect();
        let n = sums.len();
        sums.sort();
        sums.dedup();
        assert_eq!(sums.len(), n, "one line per item: {rec:?}");
        assert_eq!(
            e.registry(),
            [e.cache().display().to_string()],
            "one registry line"
        );
        for n in names(&e.cache()) {
            assert!(!n.contains(".tmp."), "no temp file left: {n}");
        }
    }
}

#[test]
fn the_root_shell_holds_the_run_lock_for_the_whole_run_and_lets_go() {
    let e = Env::new();
    let v = V {
        probe_lock: Some(e.d.path().display().to_string()),
        ..only(&e.a())
    };
    let (r, out) = e.go(&v, NOW);
    ok(&r, &out);
    assert_eq!(
        std::fs::read_to_string(e.p("lockstate")).unwrap().trim(),
        "held",
        "an exclusive lock is refused during the run"
    );
    let free = Command::new("flock")
        .args(["-x", "-n"])
        .arg(e.p(".lock"))
        .arg("true")
        .status()
        .unwrap();
    assert!(free.success(), "released when the run ends");
}

#[test]
fn without_flock_nothing_is_recorded_or_removed_and_a_warning_says_why() {
    let e = Env::new();
    let main = e.cache();
    let old = hex(1);
    e.put(&main, &old, 0);
    e.write_record(&main, &[(old.clone(), NOW - 400 * DAY, "")]);
    e.write_registry(&[&main]);
    let before = std::fs::read(main.join(".telepatch-usage")).unwrap();
    let (r, out) = e.go_with(&only(&e.a()), NOW, true);
    ok(&r, &out);
    assert_eq!(
        std::fs::read(main.join(".telepatch-usage")).unwrap(),
        before
    );
    assert!(main.join(&old).exists());
    assert!(out.contains("flock is not on the host"), "{out}");
    assert!(!e.p(".lock").exists(), "no run lock without flock");
}

#[test]
fn a_check_run_records_and_removes_nothing() {
    let e = Env::new();
    let main = e.cache();
    let old = hex(1);
    e.put(&main, &old, 0);
    e.write_record(&main, &[(old.clone(), NOW - 400 * DAY, "")]);
    e.write_registry(&[&main]);
    let before = std::fs::read(main.join(".telepatch-usage")).unwrap();
    let out = Rc::new(RefCell::new(String::new()));
    let _ = run_check(
        &Playbook::new("cu", pb),
        e.spec(&only(&e.a()), NOW, false),
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
        CheckOpts::default(),
    );
    assert_eq!(
        std::fs::read(main.join(".telepatch-usage")).unwrap(),
        before
    );
    assert!(main.join(&old).exists());
}

#[test]
fn a_registry_line_that_is_not_a_cache_directory_is_ignored() {
    let e = Env::new();
    // Inside the sandbox, but not the main cache nor `*/.telepatch/cache/sha256`.
    let od = e.p("etc");
    std::fs::create_dir_all(&od).unwrap();
    let old = hex(1);
    let p = e.put(&od, &old, 400);
    let main = e.cache();
    std::fs::create_dir_all(&main).unwrap();
    e.write_registry(&[&main, &od]);
    let a = e.a();
    let (r, out) = e.go(&only(&a), NOW);
    ok(&r, &out);
    assert!(p.exists(), "a file in a non-cache directory stays");
    assert_eq!(names(&od), vec![old.clone()], "nothing was written there");
    assert!(out.contains("not a telepatch cache directory"), "{out}");
    assert_eq!(time_of(&main, &a.1), Some(NOW));
}
