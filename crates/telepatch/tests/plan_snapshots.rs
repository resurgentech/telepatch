//! Plan-text snapshots for a two-role toy playbook on two fixture hosts.

use std::rc::Rc;

use telepatch::facts::os::{Family, Os};
use telepatch::testing::{FixtureHost, trace, trace_all};
use telepatch::{Host, Key, Playbook, PlaybookFuture, TraceError};

struct Vars {
    admin: String,
}

fn os_fixture(name: &str) -> Os {
    let p = format!(
        "{}/tests/fixtures/os-release/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    Os::parse(&std::fs::read_to_string(p).unwrap()).unwrap()
}

async fn web(h: &Host<Vars>) -> Result<(), TraceError> {
    let _role = h.role_enter("web");
    h.raw("echo hi");
    match h.os.family {
        Family::Debian => h.raw("apt-get update"),
        _ => h.raw("dnf makecache"),
    }
    .writes([Key::file("/var/cache/pkgmeta")]);
    let s = h.as_user(&h.vars.admin);
    s.raw("echo \"$HOME\"")
        .writes([Key::file("/home/x/.profile")]);
    h.as_user("root").raw("id -un").reads([Key::user("root")]);
    h.barrier();
    Ok(())
}

async fn monitoring(h: &Host<Vars>) -> Result<(), TraceError> {
    let _role = h.role_enter("monitoring");
    h.raw("systemctl enable node_exporter")
        .reads([Key::pkg("node-exporter")])
        .writes([Key::svc("node_exporter")]);
    Ok(())
}

fn run(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move {
        web(&h).await?;
        monitoring(&h).await
    })
}

fn playbook() -> Playbook<Vars> {
    Playbook::new("toy", run)
}

fn host(name: &str, fixture: &str) -> FixtureHost<Vars> {
    FixtureHost::new(
        name,
        os_fixture(fixture),
        Rc::new(Vars {
            admin: "deploy".into(),
        }),
    )
}

#[test]
fn plan_debian() {
    insta::assert_snapshot!(trace(&playbook(), &host("web1", "ubuntu2404")));
}

#[test]
fn plan_redhat() {
    insta::assert_snapshot!(trace(&playbook(), &host("web2", "rockylinux9")));
}

#[test]
fn plan_two_hosts_in_order() {
    let hosts = [host("a", "debian12"), host("b", "amazonlinux2023")];
    let all = trace_all(&playbook(), &hosts);
    assert!(all.find("host a").unwrap() < all.find("host b").unwrap());
}

#[test]
fn plan_is_deterministic() {
    let h = host("web1", "arch");
    assert_eq!(trace(&playbook(), &h), trace(&playbook(), &h));
}

fn relogin_run(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move {
        let _role = h.role_enter("profile");
        let u = h.as_user(&h.vars.admin);
        u.raw("echo one");
        u.relogin();
        u.raw("echo two");
        h.as_user("root").relogin();
        Ok(())
    })
}

#[test]
fn plan_shows_relogin() {
    let pb = Playbook::new("relogin", relogin_run);
    insta::assert_snapshot!(trace(&pb, &host("web1", "ubuntu2404")));
}
