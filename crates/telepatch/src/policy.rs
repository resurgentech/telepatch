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
//! carries the error and the allowance; and a `pkg` op of the
//! `binary_release` source with no checksum source).
//!
//! An allowance belongs to the trace op that wrote it. When the optimizer
//! merges ops, the merged op carries every member's allowances, each tagged
//! with its member ([`Allowance::member`]); a policy check on a merged op
//! ([`unpermitted`]) lets a member's trip pass only by that member's own
//! allowance.

use std::fmt;

use crate::host::TraceError;
use crate::ops::pkg::Source;
use crate::trace::{Node, NodeId, OpNode, OpSpec, Site, Trace};

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
    /// The member of a merged op this allowance came from; `None` on a
    /// trace op (it is the op's own) and on an op that merged with nothing.
    pub member: Option<AllowedFor>,
}

/// The trace op an allowance of a merged op belongs to: its id and the
/// site of its builder call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllowedFor {
    pub id: NodeId,
    pub site: Option<Site>,
}

impl Allowance {
    /// An allowance of the op itself.
    pub fn new(policy: Policy, reason: impl Into<String>) -> Allowance {
        Allowance {
            policy,
            reason: reason.into(),
            member: None,
        }
    }

    /// Does this allowance permit `policy` for the trace op `member`? An
    /// untagged allowance is the op's own and permits it for the op; a
    /// tagged one only for its member.
    pub fn permits(&self, member: NodeId, policy: Policy) -> bool {
        self.policy == policy && self.member.is_none_or(|m| m.id == member)
    }
}

impl fmt::Display for Allowance {
    /// `allowed unit-via-file: <reason>`; on a merged op `... (for #3 at
    /// <file>:<line>)`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "allowed {}: {}", self.policy, self.reason)?;
        match self.member {
            Some(AllowedFor {
                id,
                site: Some(site),
            }) => write!(f, " (for {id} at {site})"),
            Some(AllowedFor { id, site: None }) => write!(f, " (for {id})"),
            None => Ok(()),
        }
    }
}

/// The trips `(member, policy, message)` of a (possibly merged) op that its
/// allowances do not permit: each member's trip passes only by an
/// allowance of that member ([`Allowance::permits`]).
pub fn unpermitted(
    allowed: &[Allowance],
    trips: Vec<(NodeId, Policy, String)>,
) -> Vec<(NodeId, Policy, String)> {
    trips
        .into_iter()
        .filter(|(m, p, _)| !allowed.iter().any(|a| a.permits(*m, *p)))
        .collect()
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
            Node::Op(op) => {
                let id = NodeId(i);
                let t = trips(op).into_iter().map(|(p, m)| (id, p, m)).collect();
                Some(unpermitted(&op.allowed, t))
            }
            _ => None,
        })
        .flatten()
        .map(|(id, p, msg)| trace.locate(id, TraceError::policy(p, msg)))
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
        OpSpec::Pkg(p) => {
            if let Source::BinaryRelease(b) = &p.source
                && b.sha256.is_none()
            {
                let name = p.names.first().map(String::as_str).unwrap_or("");
                out.push((
                    Policy::Unverified,
                    format!("pkg[binary_release] {name}: no checksum source (.sha256(Sum::..))"),
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
