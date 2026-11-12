//! Where a container's published port is, for the tests that connect to one.
//! `just docker-vm` (`../tp_basic/scripts/docker-vm.sh`) exports three variables:
//!
//! - `TP_DOCKER_VM_PUBLISH`: the address in the VM a container publishes on;
//! - `TP_DOCKER_VM_ADDR`: the address this machine connects to;
//! - `TP_DOCKER_VM_PORTS`: `first-last`, a fixed range of TCP ports, each with
//!   the same number at both ends.
//!
//! On a remote VM host both addresses are `127.0.0.1` and an ssh forward carries
//! each port; on `TP_VM_HOST=local` both are the VM's address. A loopback
//! `PUBLISH` is therefore fine. The test picks a port from the range, publishes
//! `<PUBLISH>:P:<container port>` and dials `<ADDR>:P`.

use std::net::Ipv4Addr;
use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::container::DOCKER_VM_MSG;

#[derive(Debug, Clone, PartialEq)]
pub struct Endpoint {
    publish: Ipv4Addr,
    addr: Ipv4Addr,
    first: u16,
    last: u16,
}

fn ipv4(name: &str, v: Option<&str>) -> Result<Ipv4Addr, String> {
    let v = v
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{DOCKER_VM_MSG} ({name} is not set)"))?;
    match v.parse::<Ipv4Addr>() {
        Ok(a) if !a.is_unspecified() => Ok(a),
        _ => Err(format!(
            "{DOCKER_VM_MSG} ({name}={v} is not an IPv4 address)"
        )),
    }
}

impl Endpoint {
    /// From the three variables' values. Fails closed, naming the variable.
    pub fn parse(
        publish: Option<&str>,
        addr: Option<&str>,
        ports: Option<&str>,
    ) -> Result<Endpoint, String> {
        let publish = ipv4("TP_DOCKER_VM_PUBLISH", publish)?;
        let addr = ipv4("TP_DOCKER_VM_ADDR", addr)?;
        let r = ports
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("{DOCKER_VM_MSG} (TP_DOCKER_VM_PORTS is not set)"))?;
        let bad = || {
            format!(
                "{DOCKER_VM_MSG} (TP_DOCKER_VM_PORTS={r} is not first-last, \
                 1024-65535, first <= last)"
            )
        };
        let (f, l) = r.split_once('-').ok_or_else(bad)?;
        let num = |s: &str| {
            if s.is_empty() || s.len() > 5 || !s.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            s.parse::<u16>().ok().filter(|p| *p >= 1024)
        };
        let (first, last) = (num(f).ok_or_else(bad)?, num(l).ok_or_else(bad)?);
        if first > last {
            return Err(bad());
        }
        Ok(Endpoint {
            publish,
            addr,
            first,
            last,
        })
    }

    pub fn from_env() -> Result<Endpoint, String> {
        let get = |n: &str| std::env::var(n).ok();
        Endpoint::parse(
            get("TP_DOCKER_VM_PUBLISH").as_deref(),
            get("TP_DOCKER_VM_ADDR").as_deref(),
            get("TP_DOCKER_VM_PORTS").as_deref(),
        )
    }

    pub fn ports(&self) -> RangeInclusive<u16> {
        self.first..=self.last
    }

    /// `-p` argument publishing `container` on host port `port`.
    pub fn publish_arg(&self, port: u16, container: u16) -> String {
        format!("{}:{port}:{container}", self.publish)
    }

    /// Where this machine connects for host port `port`.
    pub fn dial(&self, port: u16) -> String {
        format!("{}:{port}", self.addr)
    }

    pub fn addr(&self) -> String {
        self.addr.to_string()
    }

    pub fn range(&self) -> String {
        format!("{}-{}", self.first, self.last)
    }
}

/// The outcome of one attempt to start a container on a port.
pub enum Tried<T> {
    Ok(T),
    /// The port is taken in the VM: try the next one.
    Taken,
    /// Anything else: stop.
    Failed(String),
}

/// Docker's words for a host port that is already bound.
pub fn is_port_taken(stderr: &str) -> bool {
    stderr.contains("port is already allocated") || stderr.contains("address already in use")
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// Pick a port of the range and run `attempt` on it, moving to the next on
/// `Taken`, starting `start` ports into the range and wrapping. Returns the
/// port and the attempt's value.
pub fn alloc_port<T>(
    ep: &Endpoint,
    start: usize,
    mut attempt: impl FnMut(u16) -> Tried<T>,
) -> Result<(u16, T), String> {
    let n = ep.ports().count();
    for i in 0..n {
        let port = ep.first + ((start + i) % n) as u16;
        match attempt(port) {
            Tried::Ok(v) => return Ok((port, v)),
            Tried::Taken => continue,
            Tried::Failed(e) => return Err(e),
        }
    }
    Err(format!(
        "every port of TP_DOCKER_VM_PORTS={} is taken in the VM",
        ep.range()
    ))
}

/// [`alloc_port`] from a process-wide rotating start, so containers alive at
/// once do not begin on the same port.
pub fn alloc_next<T>(
    ep: &Endpoint,
    attempt: impl FnMut(u16) -> Tried<T>,
) -> Result<(u16, T), String> {
    alloc_port(ep, NEXT.fetch_add(1, Ordering::SeqCst), attempt)
}
