//! Live: the `fetch` value and `.from(..)` / `.unpack(..).strip(1)` on
//! ubuntu:24.04 and rockylinux/rockylinux:10, plain containers with no init,
//! through the executor over `docker exec`. Run with `just test-live-fetch`.
//!
//! No network beyond what the distro's manager needs to install `python3`
//! (and `curl` where the image has none): the files are served by
//! `python3 -m http.server` on 127.0.0.1 inside the container, written
//! there by the test (a plain file and a tar.gz with one top directory).
//!
//! * run 1 downloads both through the real `curl` (prepare says `fetched,
//!   sha256 ok`), places the file with mode 0640, unpacks the archive with
//!   `strip(1)` into a directory that held an older tree, caches both by
//!   sha256, and leaves nothing in `/tmp` or `/var/tmp/telepatch`;
//! * the server is then stopped, so run 2 can only succeed without any
//!   network call: it is `CHANGED 0`, prepare says `cached` for both, and
//!   the server's log still holds run 1's two requests;
//! * a checksum that is not the file's blocks the host before apply, with
//!   the mismatch named and no cache entry for it (run between the two, with
//!   the server still up).
//!
//! And the `binary_release` pkg source (`fetch_*_binary_release`): a
//! tarball with a JSON checksum and a single binary with a sidecar, served
//! the same way (see [`fetch_binary_release_from_a_local_server`]).
//!
//! And a host with no `curl`, `wget` or `cmp` (`fetch_*_after_the_op_that_installs_curl`):
//! a `pkg` op installing `curl`, then a `.from` and a binary release in the
//! same window; the window splits after the pkg op (see
//! [`fetch_after_the_op_that_installs_curl`]).

use std::cell::RefCell;
use std::process::{Command, Stdio};
use std::rc::Rc;

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
use telepatch::ops::pkg::{BinaryRelease, Latest, Sum};
use telepatch::report::{FetchState, HostStatus, Report};
use telepatch::transport::docker::DockerTransport;
use telepatch::{Host, Playbook, PlaybookFuture};

use crate::container::Container;

const PORT: u16 = 8765;

#[derive(Clone)]
struct V {
    install: Vec<String>,
    plain_sum: String,
    tgz_sum: String,
}

fn pb_install(h: Host<V>) -> PlaybookFuture {
    Box::pin(async move {
        h.pkg(h.vars.install.clone());
        Ok(())
    })
}

fn pb_fetch(h: Host<V>) -> PlaybookFuture {
    Box::pin(async move {
        let plain = h
            .fetch(format!("http://127.0.0.1:{PORT}/plain.txt"))
            .sha256(&h.vars.plain_sum);
        h.file("/etc/tp-fetch.txt").from(&plain).mode(0o640);
        let tgz = h
            .fetch(format!("http://127.0.0.1:{PORT}/dataset.tgz"))
            .sha256(&h.vars.tgz_sum);
        h.file("/srv/dataset").unpack(tgz).strip(1);
        Ok(())
    })
}

fn pb_bad(h: Host<V>) -> PlaybookFuture {
    Box::pin(async move {
        // a sum that is not the file's; the file exists on the server
        let bad = h
            .fetch(format!("http://127.0.0.1:{PORT}/plain.txt"))
            .sha256("0".repeat(64));
        h.file("/etc/tp-bad.txt").from(bad);
        Ok(())
    })
}

fn go(
    ctr: &Container,
    name: &'static str,
    v: &V,
    f: fn(Host<V>) -> PlaybookFuture,
) -> (Report, String) {
    let pb = Playbook::new(name, f);
    let out = Rc::new(RefCell::new(String::new()));
    let r = run(
        &pb,
        vec![HostSpec {
            name: "c".into(),
            target: Target::Docker(DockerTransport::new().container("c", &ctr.name)),
            vars: v.clone(),
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    let text = out.borrow().clone();
    println!("{text}\n{}", r.render());
    (r, text)
}

/// `docker exec <ctr> sh -c <script>`: (rc, stdout).
fn exec(ctr: &Container, script: &str) -> (i32, String) {
    let o = Command::new("docker")
        .args(["exec", &ctr.name, "sh", "-c", script])
        .stdin(Stdio::null())
        .output()
        .expect("docker exec");
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).trim().to_string(),
    )
}

fn fetch_from_a_local_server(image: &str) {
    let ctr = Container::start(image);
    // what the image lacks: python3 for the server, curl for the engine
    let (rc, _) = exec(&ctr, "command -v curl");
    let mut v = V {
        install: vec!["python3".into()],
        plain_sum: String::new(),
        tgz_sum: String::new(),
    };
    if rc != 0 {
        v.install.push("curl".into());
    }
    let (r, out) = go(&ctr, "install", &v, pb_install);
    assert_eq!(r.exit_code(), 0, "install:\n{out}\n{}", r.render());
    for t in ["curl", "python3", "tar", "sha256sum"] {
        let (rc, _) = exec(&ctr, &format!("command -v {t}"));
        assert_eq!(rc, 0, "{image}: {t} is on the host");
    }

    // the served files, and a server on 127.0.0.1
    let (rc, o) = exec(
        &ctr,
        "mkdir -p /srv/www/ds/pkg-1.0/bin && printf 'served by the test\\n' > /srv/www/plain.txt \
         && printf 'readme\\n' > /srv/www/ds/pkg-1.0/README && printf '#!/bin/sh\\n' > /srv/www/ds/pkg-1.0/bin/tool \
         && chmod 755 /srv/www/ds/pkg-1.0/bin/tool && tar -czf /srv/www/dataset.tgz -C /srv/www/ds pkg-1.0",
    );
    assert_eq!(rc, 0, "writing the served files: {o}");
    let (_, plain) = exec(&ctr, "sha256sum /srv/www/plain.txt | cut -d' ' -f1");
    let (_, tgz) = exec(&ctr, "sha256sum /srv/www/dataset.tgz | cut -d' ' -f1");
    v.plain_sum = plain.clone();
    v.tgz_sum = tgz.clone();
    let o = Command::new("docker")
        .args([
            "exec",
            "-d",
            &ctr.name,
            "sh",
            "-c",
            &format!(
                "cd /srv/www && echo $$ > /srv/httpd.pid && exec python3 -m http.server {PORT} --bind 127.0.0.1 > /srv/httpd.log 2>&1"
            ),
        ])
        .stdin(Stdio::null())
        .output()
        .expect("docker exec -d");
    assert!(o.status.success(), "starting the server");
    let (rc, o) = exec(
        &ctr,
        &format!(
            "for i in $(seq 1 50); do curl -fsS -o /dev/null http://127.0.0.1:{PORT}/plain.txt && exit 0; sleep 0.2; done; exit 1"
        ),
    );
    assert_eq!(rc, 0, "the server did not come up: {o}");
    // that readiness probe is one request; count from here
    let requests = |ctr: &Container| {
        exec(ctr, "grep -c '\"GET /' /srv/httpd.log")
            .1
            .parse::<usize>()
            .unwrap_or(0)
    };
    let before = requests(&ctr);

    // an older tree is there; /tmp and the staging dir start empty
    let (_, _) = exec(
        &ctr,
        "mkdir -p /srv/dataset/old && echo x > /srv/dataset/old/leftover && echo stale > /srv/dataset/README",
    );
    let (_, tmp_before) = exec(&ctr, "ls -A /tmp");

    // run 1
    let (r1, out1) = go(&ctr, "fetch", &v, pb_fetch);
    assert_eq!(r1.exit_code(), 0, "run 1:\n{out1}\n{}", r1.render());
    assert_eq!(r1.hosts[0].status, HostStatus::Changed, "{}", r1.render());
    for u in ["plain.txt", "dataset.tgz"] {
        let l = format!("== c [prepare] fetch http://127.0.0.1:{PORT}/{u}: fetched, sha256 ok ==");
        assert!(out1.contains(&l), "{l}\n{out1}");
    }
    assert!(
        r1.hosts[0]
            .fetches
            .iter()
            .all(|f| f.state == FetchState::Fetched),
        "{}",
        r1.render()
    );
    assert_eq!(requests(&ctr) - before, 2, "run 1 made two requests");
    let (_, body) = exec(&ctr, "cat /etc/tp-fetch.txt");
    assert_eq!(body, "served by the test");
    let (_, m) = exec(&ctr, "stat -c %a /etc/tp-fetch.txt");
    assert_eq!(m, "640");
    let (rc, _) = exec(
        &ctr,
        "test -f /srv/dataset/README && test -x /srv/dataset/bin/tool && ! test -e /srv/dataset/old && ! test -e /srv/dataset/pkg-1.0",
    );
    assert_eq!(rc, 0, "unpacked with strip(1), replacing the old tree");
    let (_, body) = exec(&ctr, "cat /srv/dataset/README");
    assert_eq!(body, "readme");
    let (_, cache) = exec(&ctr, "ls /var/cache/telepatch/sha256");
    let mut want = [plain.clone(), tgz.clone()];
    want.sort();
    assert_eq!(cache, want.join("\n"), "cached by sha256");
    let (_, stage) = exec(&ctr, "ls -A /var/tmp/telepatch 2>/dev/null");
    assert_eq!(stage, "", "the staging dir is cleaned");
    let (_, tmp_after) = exec(&ctr, "ls -A /tmp");
    assert_eq!(tmp_after, tmp_before, "nothing left in /tmp");
    let (_, near) = exec(&ctr, "ls -A /etc /srv | grep '\\.tp' || true");
    assert_eq!(
        near, "",
        "no candidate or sibling temp left beside a target"
    );

    // a wrong checksum blocks before apply, with no cache entry (the server
    // is still up: the download works and the sum is what fails)
    let (r3, out3) = go(&ctr, "bad", &v, pb_bad);
    let HostStatus::Blocked(why) = &r3.hosts[0].status else {
        panic!("{:?}\n{out3}", r3.hosts[0].status);
    };
    assert_eq!(
        why,
        &format!(
            "fetch http://127.0.0.1:{PORT}/plain.txt: sha256 mismatch: want {} got {plain} (window 1)",
            "0".repeat(64)
        )
    );
    let (rc, _) = exec(&ctr, "test -e /etc/tp-bad.txt");
    assert_ne!(rc, 0, "nothing was applied");
    let (_, cache) = exec(&ctr, "ls /var/cache/telepatch/sha256");
    assert_eq!(cache, want.join("\n"), "no entry for the wrong sum");
    let (_, stage) = exec(&ctr, "ls -A /var/tmp/telepatch 2>/dev/null");
    assert_eq!(stage, "", "the staging dir is cleaned");

    // the server goes away: run 2 can only pass with no network call
    let (rc, o) = exec(&ctr, "kill $(cat /srv/httpd.pid)");
    assert_eq!(rc, 0, "stopping the server: {o}");
    let (rc, _) = exec(
        &ctr,
        &format!(
            "sleep 1; ! curl -fsS -o /dev/null --max-time 2 http://127.0.0.1:{PORT}/plain.txt"
        ),
    );
    assert_eq!(rc, 0, "the server is down");
    let (r2, out2) = go(&ctr, "fetch", &v, pb_fetch);
    assert_eq!(r2.exit_code(), 0, "run 2:\n{out2}\n{}", r2.render());
    assert_eq!(r2.hosts[0].status, HostStatus::Ok, "{}", r2.render());
    for o in &r2.hosts[0].ops {
        assert!(!o.changed, "run 2 changed {}: {}", o.id, o.desc);
    }
    for u in ["plain.txt", "dataset.tgz"] {
        let l = format!("== c [prepare] fetch http://127.0.0.1:{PORT}/{u}: cached ==");
        assert!(out2.contains(&l), "{l}\n{out2}");
    }
    assert_eq!(requests(&ctr) - before, 3, "run 2 made no request");
}

/// The `binary_release` pkg source against the same kind of server: a
/// tarball (`tool-<v>.linux-<arch>.tar.gz`, top directory `tool-<v>/`
/// holding `bin/tool`, which prints `tool version <v>`) checked by a JSON
/// document in go.dev's `?mode=json` layout, with `{version}` from a
/// `VERSION` text file; and a single binary (`linux-<arch>/one`) checked by a
/// `.sha256sum` sidecar, with no `{version}`, so latest follows the
/// published checksum. Run 1 installs both, run 2 is `CHANGED 0` with no
/// download (only `VERSION` and the sidecar are read), run 3, after the
/// server publishes 1.2.4 and a new `one`, upgrades both.
#[derive(Clone)]
struct B;

fn pb_binary_release(h: Host<B>) -> PlaybookFuture {
    Box::pin(async move {
        let base = format!("http://127.0.0.1:{PORT}");
        h.pkg(["tool"])
            .binary_release(
                BinaryRelease::new()
                    .url(format!("{base}/tool-{{version}}.linux-{{arch}}.tar.gz"))
                    .sha256(Sum::json(
                        format!("{base}/releases.json"),
                        "filename",
                        "sha256",
                    ))
                    .unpack_to("/opt/tool")
                    .strip(1)
                    .bin("/opt/tool/bin/tool")
                    .version("tool --version")
                    .latest(Latest::text(format!("{base}/VERSION"))),
            )
            .latest();
        h.pkg(["one"])
            .binary_release(
                BinaryRelease::new()
                    .url(format!("{base}/linux-{{arch}}/one"))
                    .sha256(Sum::sidecar(".sha256sum"))
                    .bin("/usr/local/bin/tp-one"),
            )
            .latest();
        Ok(())
    })
}

fn go_b(ctr: &Container) -> (Report, String) {
    let pb = Playbook::new("binary_release", pb_binary_release);
    let out = Rc::new(RefCell::new(String::new()));
    let r = run(
        &pb,
        vec![HostSpec {
            name: "c".into(),
            target: Target::Docker(DockerTransport::new().container("c", &ctr.name)),
            vars: B,
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    let text = out.borrow().clone();
    println!("{text}\n{}", r.render());
    (r, text)
}

/// Publish tool `<v>` and a `one` printing `one <n>` on the server; the
/// sums of the tarball and of `one`.
fn publish(ctr: &Container, arch: &str, v: &str, n: u32) -> (String, String) {
    let (rc, o) = exec(
        ctr,
        &format!(
            "set -e; d=/srv/src/tool-{v}; mkdir -p $d/bin /srv/www/linux-{arch}; \
             printf '#!/bin/sh\\necho \"tool version {v}\"\\n' > $d/bin/tool; chmod 755 $d/bin/tool; \
             tar -czf /srv/www/tool-{v}.linux-{arch}.tar.gz -C /srv/src tool-{v}; \
             printf 'v{v}\\n' > /srv/www/VERSION; \
             s=$(sha256sum /srv/www/tool-{v}.linux-{arch}.tar.gz | cut -d' ' -f1); \
             printf '[\\n {{\\n  \"version\": \"tool{v}\",\\n  \"stable\": true,\\n  \"files\": [\\n   {{\\n    \"filename\": \"tool-{v}.linux-{arch}.tar.gz\",\\n    \"os\": \"linux\",\\n    \"sha256\": \"%s\",\\n    \"kind\": \"archive\"\\n   }}\\n  ]\\n }}\\n]\\n' \"$s\" > /srv/www/releases.json; \
             printf '#!/bin/sh\\necho one {n}\\n' > /srv/www/linux-{arch}/one; \
             (cd /srv/www/linux-{arch} && sha256sum one > one.sha256sum)"
        ),
    );
    assert_eq!(rc, 0, "publishing {v}: {o}");
    let (_, t) = exec(
        ctr,
        &format!("sha256sum /srv/www/tool-{v}.linux-{arch}.tar.gz | cut -d' ' -f1"),
    );
    let (_, one) = exec(
        ctr,
        &format!("sha256sum /srv/www/linux-{arch}/one | cut -d' ' -f1"),
    );
    (t, one)
}

fn fetch_binary_release_from_a_local_server(image: &str) {
    let ctr = Container::start(image);
    let (rc, _) = exec(&ctr, "command -v curl");
    let mut v = V {
        install: vec!["python3".into()],
        plain_sum: String::new(),
        tgz_sum: String::new(),
    };
    if rc != 0 {
        v.install.push("curl".into());
    }
    let (r, out) = go(&ctr, "install", &v, pb_install);
    assert_eq!(r.exit_code(), 0, "install:\n{out}\n{}", r.render());
    let (_, machine) = exec(&ctr, "uname -m");
    let arch = match machine.as_str() {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        m => panic!("no default {{arch}} for {m}"),
    }
    .to_string();
    let (t1, o1) = publish(&ctr, &arch, "1.2.3", 1);
    let o = Command::new("docker")
        .args([
            "exec",
            "-d",
            &ctr.name,
            "sh",
            "-c",
            &format!(
                "cd /srv/www && exec python3 -m http.server {PORT} --bind 127.0.0.1 > /srv/httpd.log 2>&1"
            ),
        ])
        .stdin(Stdio::null())
        .output()
        .expect("docker exec -d");
    assert!(o.status.success(), "starting the server");
    let (rc, o) = exec(
        &ctr,
        &format!(
            "for i in $(seq 1 50); do curl -fsS -o /dev/null http://127.0.0.1:{PORT}/VERSION && exit 0; sleep 0.2; done; exit 1"
        ),
    );
    assert_eq!(rc, 0, "the server did not come up: {o}");
    let gets = |ctr: &Container, path: &str| {
        exec(ctr, &format!("grep -c '\"GET {path} ' /srv/httpd.log"))
            .1
            .parse::<usize>()
            .unwrap_or(0)
    };
    let (_, tmp_before) = exec(&ctr, "ls -A /tmp");
    let tarball = |v: &str| format!("/tool-{v}.linux-{arch}.tar.gz");
    let one = format!("/linux-{arch}/one");

    // run 1: both installed
    let (r1, out1) = go_b(&ctr);
    assert_eq!(
        r1.hosts[0].status,
        HostStatus::Changed,
        "run 1:\n{out1}\n{}",
        r1.render()
    );
    for l in [
        "== c [prepare] pkg[binary_release] tool: not installed, 1.2.3 wanted; fetched, sha256 ok =="
            .to_string(),
        format!(
            "== c [prepare] pkg[binary_release] one: not installed, sha256 {} wanted; fetched, sha256 ok ==",
            &o1[..12]
        ),
    ] {
        assert!(out1.contains(&l), "{l}\n{out1}");
    }
    assert_eq!(exec(&ctr, "/opt/tool/bin/tool").1, "tool version 1.2.3");
    assert_eq!(exec(&ctr, "/usr/local/bin/tp-one").1, "one 1");
    assert_eq!(exec(&ctr, "stat -c %a /usr/local/bin/tp-one").1, "755");
    let (_, cache) = exec(&ctr, "ls /var/cache/telepatch/sha256");
    assert!(
        cache.contains(&t1) && cache.contains(&o1),
        "cached: {cache}"
    );
    assert_eq!((gets(&ctr, &tarball("1.2.3")), gets(&ctr, &one)), (1, 1));

    // run 2: settled; only VERSION and the sidecar are read
    let (r2, out2) = go_b(&ctr);
    assert_eq!(
        r2.hosts[0].status,
        HostStatus::Ok,
        "run 2:\n{out2}\n{}",
        r2.render()
    );
    for o in &r2.hosts[0].ops {
        assert!(!o.changed, "run 2 changed {}: {}", o.id, o.desc);
    }
    assert!(!out2.contains("[prepare] pkg[binary_release]"), "{out2}");
    assert_eq!(
        (gets(&ctr, &tarball("1.2.3")), gets(&ctr, &one)),
        (1, 1),
        "run 2 downloaded nothing"
    );
    assert_eq!(gets(&ctr, "/releases.json"), 1, "nor read the checksums");

    // run 3: 1.2.4 and a new `one` are published
    let (_, o2) = publish(&ctr, &arch, "1.2.4", 2);
    let (r3, out3) = go_b(&ctr);
    assert_eq!(
        r3.hosts[0].status,
        HostStatus::Changed,
        "run 3:\n{out3}\n{}",
        r3.render()
    );
    for l in [
        "== c [prepare] pkg[binary_release] tool: 1.2.3 installed, 1.2.4 wanted; fetched, sha256 ok =="
            .to_string(),
        format!(
            "== c [prepare] pkg[binary_release] one: installed, sha256 {} wanted; fetched, sha256 ok ==",
            &o2[..12]
        ),
    ] {
        assert!(out3.contains(&l), "{l}\n{out3}");
    }
    assert_eq!(exec(&ctr, "/opt/tool/bin/tool").1, "tool version 1.2.4");
    assert_eq!(exec(&ctr, "/usr/local/bin/tp-one").1, "one 2");
    let (rc, _) = exec(
        &ctr,
        "test -f /opt/tool/.telepatch-fetch && ! test -e /opt/tool/tool-1.2.4",
    );
    assert_eq!(rc, 0, "unpacked with strip(1)");

    let (_, stage) = exec(&ctr, "ls -A /var/tmp/telepatch 2>/dev/null");
    assert_eq!(stage, "", "the staging dir is cleaned");
    let (_, tmp_after) = exec(&ctr, "ls -A /tmp");
    assert_eq!(tmp_after, tmp_before, "nothing left in /tmp");
    let (_, near) = exec(&ctr, "ls -A /opt /usr/local/bin | grep '\\.tp' || true");
    assert_eq!(near, "", "no candidate or old tree left beside a target");
}

#[test]
#[ignore = "needs docker"]
fn fetch_ubuntu_binary_release() {
    fetch_binary_release_from_a_local_server("ubuntu:24.04");
}

#[test]
#[ignore = "needs docker"]
fn fetch_rocky_binary_release() {
    fetch_binary_release_from_a_local_server("rockylinux/rockylinux:10");
}

#[test]
#[ignore = "needs docker"]
fn fetch_ubuntu_from_a_local_server() {
    fetch_from_a_local_server("ubuntu:24.04");
}

#[test]
#[ignore = "needs docker"]
fn fetch_rocky_from_a_local_server() {
    fetch_from_a_local_server("rockylinux/rockylinux:10");
}

/// A host with no `curl`, `wget` or `cmp` (T83a): one window holds a `pkg`
/// op installing `curl`, then a `.from` and a binary release served from
/// the container. Prepare finds no downloader, so the window splits after
/// the pkg op: `curl` is installed as window 1, and the download and the
/// release run as window 2 with the curl just installed.
#[derive(Clone)]
struct T {
    plain_sum: String,
}

fn pb_after_curl(h: Host<T>) -> PlaybookFuture {
    Box::pin(async move {
        let base = format!("http://127.0.0.1:{PORT}");
        h.pkg(["curl"]);
        let plain = h
            .fetch(format!("{base}/plain.txt"))
            .sha256(&h.vars.plain_sum);
        h.file("/etc/tp-after-curl.txt").from(&plain);
        h.pkg(["tool"])
            .binary_release(
                BinaryRelease::new()
                    .url(format!("{base}/tool-{{version}}.linux-{{arch}}.tar.gz"))
                    .sha256(Sum::json(
                        format!("{base}/releases.json"),
                        "filename",
                        "sha256",
                    ))
                    .unpack_to("/opt/tool")
                    .strip(1)
                    .bin("/opt/tool/bin/tool")
                    .version("tool --version")
                    .latest(Latest::text(format!("{base}/VERSION"))),
            )
            .latest();
        Ok(())
    })
}

fn fetch_after_the_op_that_installs_curl(image: &str) {
    let ctr = Container::start(image);
    // python3 for the server, installed while the image's own curl (if
    // any) is still there
    let v = V {
        install: vec!["python3".into()],
        plain_sum: String::new(),
        tgz_sum: String::new(),
    };
    let (r, out) = go(&ctr, "install", &v, pb_install);
    assert_eq!(r.exit_code(), 0, "install:\n{out}\n{}", r.render());
    // then no curl, wget or cmp: the packages removed (Rocky's
    // curl-minimal provides `curl`, so it goes too, without its libcurl
    // that dnf needs), and the cmp binary deleted (diffutils is essential
    // on Ubuntu)
    let (rc, o) = exec(
        &ctr,
        "if command -v dpkg >/dev/null 2>&1; then \
           for p in curl wget; do if dpkg -s $p >/dev/null 2>&1; then dpkg --purge --force-depends $p || exit 1; fi; done; \
         else \
           for p in curl curl-minimal wget wget2 wget2-wget; do if rpm -q $p >/dev/null 2>&1; then rpm -e --nodeps $p || exit 1; fi; done; \
         fi; rm -f /usr/bin/cmp /bin/cmp",
    );
    assert_eq!(rc, 0, "removing curl, wget and cmp: {o}");
    for t in ["curl", "wget", "cmp"] {
        let (rc, _) = exec(&ctr, &format!("command -v {t}"));
        assert_ne!(rc, 0, "{image}: {t} is gone");
    }

    let (_, machine) = exec(&ctr, "uname -m");
    let arch = match machine.as_str() {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        m => panic!("no default {{arch}} for {m}"),
    }
    .to_string();
    let (rc, o) = exec(
        &ctr,
        "mkdir -p /srv/www && printf 'served by the test\\n' > /srv/www/plain.txt",
    );
    assert_eq!(rc, 0, "writing the served file: {o}");
    let (_, plain) = exec(&ctr, "sha256sum /srv/www/plain.txt | cut -d' ' -f1");
    let (t1, _) = publish(&ctr, &arch, "1.2.3", 1);
    let o = Command::new("docker")
        .args([
            "exec",
            "-d",
            &ctr.name,
            "sh",
            "-c",
            &format!(
                "cd /srv/www && exec python3 -m http.server {PORT} --bind 127.0.0.1 > /srv/httpd.log 2>&1"
            ),
        ])
        .stdin(Stdio::null())
        .output()
        .expect("docker exec -d");
    assert!(o.status.success(), "starting the server");
    // readiness without curl: python3's urllib
    let (rc, o) = exec(
        &ctr,
        &format!(
            "for i in $(seq 1 50); do python3 -c 'import urllib.request; urllib.request.urlopen(\"http://127.0.0.1:{PORT}/VERSION\")' 2>/dev/null && exit 0; sleep 0.2; done; exit 1"
        ),
    );
    assert_eq!(rc, 0, "the server did not come up: {o}");
    let (_, tmp_before) = exec(&ctr, "ls -A /tmp");

    let pb = Playbook::new("after_curl", pb_after_curl);
    let out = Rc::new(RefCell::new(String::new()));
    let r = run(
        &pb,
        vec![HostSpec {
            name: "c".into(),
            target: Target::Docker(DockerTransport::new().container("c", &ctr.name)),
            vars: T {
                plain_sum: plain.clone(),
            },
        }],
        RunOpts {
            log: None,
            stdout: Stdout::Capture(out.clone()),
        },
    );
    let out = out.borrow().clone();
    println!("{out}\n{}", r.render());
    assert_eq!(
        r.hosts[0].status,
        HostStatus::Changed,
        "{out}\n{}",
        r.render()
    );
    let split = "== c [prepare] window 1: neither curl nor wget is on the host; split after #0 (pkg curl): #1 runs in window 2 ==";
    let at = out.find(split).unwrap_or_else(|| panic!("{split}\n{out}"));
    for l in [
        format!("== c [prepare] fetch http://127.0.0.1:{PORT}/plain.txt: fetched, sha256 ok =="),
        "== c [prepare] pkg[binary_release] tool: not installed, 1.2.3 wanted; fetched, sha256 ok =="
            .to_string(),
    ] {
        let i = out.find(&l).unwrap_or_else(|| panic!("{l}\n{out}"));
        assert!(i > at, "{l} comes after the split:\n{out}");
    }
    assert!(
        r.hosts[0].fetches.iter().all(|f| f.window == 2),
        "{}",
        r.render()
    );
    assert_eq!(
        r.hosts[0]
            .ops
            .iter()
            .map(|o| (o.id, o.changed))
            .collect::<Vec<_>>(),
        [(0, true), (1, true), (2, true)],
        "{}",
        r.render()
    );
    let (rc, _) = exec(&ctr, "command -v curl");
    assert_eq!(rc, 0, "the pkg op installed curl");
    assert_eq!(
        exec(&ctr, "cat /etc/tp-after-curl.txt").1,
        "served by the test"
    );
    assert_eq!(exec(&ctr, "/opt/tool/bin/tool").1, "tool version 1.2.3");
    let (_, cache) = exec(&ctr, "ls /var/cache/telepatch/sha256");
    assert!(
        cache.contains(&plain) && cache.contains(&t1),
        "cached: {cache}"
    );
    let (_, stage) = exec(&ctr, "ls -A /var/tmp/telepatch 2>/dev/null");
    assert_eq!(stage, "", "the staging dir is cleaned");
    let (_, tmp_after) = exec(&ctr, "ls -A /tmp");
    assert_eq!(tmp_after, tmp_before, "nothing left in /tmp");
    assert!(!out.contains("command not found"), "{out}");
}

#[test]
#[ignore = "needs docker"]
fn fetch_ubuntu_after_the_op_that_installs_curl() {
    fetch_after_the_op_that_installs_curl("ubuntu:24.04");
}

#[test]
#[ignore = "needs docker"]
fn fetch_rocky_after_the_op_that_installs_curl() {
    fetch_after_the_op_that_installs_curl("rockylinux/rockylinux:10");
}
