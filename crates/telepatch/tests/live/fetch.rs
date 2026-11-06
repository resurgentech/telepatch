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

use std::cell::RefCell;
use std::process::{Command, Stdio};
use std::rc::Rc;

use telepatch::executor::{HostSpec, RunOpts, Stdout, Target, run};
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
    let pb = Playbook { name, run: f };
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
