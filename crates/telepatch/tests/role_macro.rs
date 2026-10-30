//! `#[role]` bracketing.

use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use telepatch::facts::os::Os;
use telepatch::{Host, Node, TraceError, role, sh};

fn host<V>(v: V) -> Host<V> {
    Host::new(
        "t",
        Os::parse("ID=debian\nVERSION_ID=12\n").unwrap(),
        Rc::new(v),
    )
}

fn block_on<T>(f: impl std::future::Future<Output = T>) -> T {
    let mut f = Box::pin(f);
    match f.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("pending"),
    }
}

#[role]
async fn unit_role<V>(h: &Host<V>) {
    sh!(h, "echo a");
}

#[role]
async fn fallible(h: &Host<String>) -> Result<(), TraceError> {
    let v = &h.vars;
    sh!(h, "echo {v}");
    Ok(())
}

#[role]
async fn early(h: &Host<bool>) -> Result<(), TraceError> {
    if *h.vars {
        Err(TraceError::new("boom"))?;
    }
    sh!(h, "echo late");
    Ok(())
}

fn marks<V>(h: &Host<V>) -> Vec<String> {
    h.snapshot()
        .nodes
        .iter()
        .map(|n| match n {
            Node::RoleBegin(s) => format!("begin {s}"),
            Node::RoleEnd(s) => format!("end {s}"),
            Node::Op(_) => "op".into(),
            _ => "other".into(),
        })
        .collect()
}

#[test]
fn unit_generic_role() {
    let h = host(5u8);
    block_on(unit_role(&h));
    assert_eq!(marks(&h), ["begin unit_role", "op", "end unit_role"]);
}

#[test]
fn fallible_concrete_role() {
    let h = host("x".to_string());
    block_on(fallible(&h)).unwrap();
    assert_eq!(marks(&h), ["begin fallible", "op", "end fallible"]);
}

#[test]
fn early_return_still_ends_role() {
    let h = host(true);
    assert!(block_on(early(&h)).is_err());
    assert_eq!(marks(&h), ["begin early", "end early"]);
}

#[test]
fn name_is_registered() {
    assert_eq!(__tp_role_name_fallible, "fallible");
}
