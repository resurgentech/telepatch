//! Policy errors: trace errors for things that would work but the project
//! forbids. Each has a name, and one way to permit it on the op that trips
//! it: `.allow(Policy::Name, "reason")`. The reason is required; the plan
//! and the run report list every allowance with its reason.
//!
//! Checked so far: [`Policy::UnitViaFile`], [`Policy::UnguardedRestart`]
//! (an `svc` `.restart()` / `.reload()` with no guard when the trace is
//! read, so a `.when(g)` later in the chain counts) and
//! [`Policy::Unverified`] (a `file` op that places or unpacks a fetch with
//! no `.sha256(..)`: the fetch is not an op, so the file op that takes it
//! carries the error and the allowance).

use std::fmt;

use crate::host::TraceError;
use crate::trace::{Node, NodeId, OpNode, OpSpec, Trace};

/// A rule an op may break only with an allowance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Policy {
    /// `h.file` under `/etc/systemd/system/`: unit files go through `h.unit`.
    UnitViaFile,
    /// A restart or reload with no guard.
    UnguardedRestart,
    /// A download with no checksum source.
    Unverified,
}

impl Policy {
    /// The kebab name printed in errors, plans and reports.
    pub fn name(self) -> &'static str {
        match self {
            Policy::UnitViaFile => "unit-via-file",
            Policy::UnguardedRestart => "unguarded-restart",
            Policy::Unverified => "unverified",
        }
    }

    /// The Rust path of the variant, as an author writes it.
    pub fn variant(self) -> &'static str {
        match self {
            Policy::UnitViaFile => "UnitViaFile",
            Policy::UnguardedRestart => "UnguardedRestart",
            Policy::Unverified => "Unverified",
        }
    }
}

impl fmt::Display for Policy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// One `.allow(policy, reason)` on an op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allowance {
    pub policy: Policy,
    pub reason: String,
}

impl fmt::Display for Allowance {
    /// `allowed unit-via-file: <reason>`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "allowed {}: {}", self.policy, self.reason)
    }
}

/// The policy errors of every op in `trace` that trips a policy it does
/// not allow. Derived from the nodes, so an `.allow(..)` later in the chain
/// (or a later statement on the handle) counts as long as it is there when
/// the trace is read; the engine reads it after every poll, before
/// anything is applied.
pub fn violations(trace: &Trace) -> Vec<TraceError> {
    trace
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| match n {
            Node::Op(op) => Some(
                trips(op)
                    .into_iter()
                    .map(|(p, msg)| (i, p, msg, op))
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .flatten()
        .filter(|(_, p, _, op)| !op.allowed.iter().any(|a| a.policy == *p))
        .map(|(i, p, msg, _)| trace.locate(NodeId(i), TraceError::policy(p, msg)))
        .collect()
}

/// The policies `op` trips, with the error messages.
fn trips(op: &OpNode) -> Vec<(Policy, String)> {
    let mut out = Vec::new();
    match &op.spec {
        OpSpec::File(f) => {
            if under_etc_systemd_system(&f.path) {
                out.push((
                    Policy::UnitViaFile,
                    format!("file {}: unit files go through h.unit", f.path),
                ));
            }
            if let Some(src) = &f.src
                && src.fetched.sha256.is_none()
                && src.fetched.problem().is_none()
            {
                out.push((
                    Policy::Unverified,
                    format!("file {}: fetch {} has no sha256", f.path, src.fetched.url),
                ));
            }
        }
        OpSpec::Svc(sv) if op.guard.is_none() => {
            if let Some(a) = sv.action() {
                out.push((
                    Policy::UnguardedRestart,
                    format!("svc {}: .{}() has no guard", sv.key_name(), a.name()),
                ));
            }
        }
        _ => {}
    }
    out
}

/// `path` is strictly below `/etc/systemd/system`, after collapsing
/// repeated `/` and `.` segments and resolving `..` lexically. The
/// package-owned `/lib/systemd` and `/usr/lib/systemd` are not covered.
fn under_etc_systemd_system(path: &str) -> bool {
    if !path.starts_with('/') {
        return false;
    }
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.len() > 3 && parts[..3] == ["etc", "systemd", "system"]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_etc_systemd_system_is_lexical() {
        for p in [
            "/etc/systemd/system/a.service",
            "/etc/systemd/system/a.service.d/override.conf",
            "//etc//systemd/system/a",
            "/etc/systemd/./system/a",
            "/etc/x/../systemd/system/a",
        ] {
            assert!(under_etc_systemd_system(p), "{p}");
        }
        for p in [
            "/etc/systemd/system",
            "/etc/systemd/system/",
            "/etc/systemd/system/..",
            "/lib/systemd/system/a",
            "/usr/lib/systemd/system/a",
            "/etc/systemd/systemx/a",
            "etc/systemd/system/a",
            "~/etc/systemd/system/a",
        ] {
            assert!(!under_etc_systemd_system(p), "{p}");
        }
    }
}
