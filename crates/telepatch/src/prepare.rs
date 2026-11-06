//! Prepare: what is checked, in one batch of round trips, before a window
//! is applied. A failure blocks the host with nothing applied.
//!
//! The root shell opening is the permission check (`sudo -n` refusal is
//! [`crate::transport::Limit::SudoRefused`], handled when the host opens).
//! Per window this module checks that every user the window's ops address
//! exists (`getent passwd`), opens each such user's shell (on SSH, `sudo -n
//! -iu u bash`: the per-user permission check), and gathers package state:
//! one query per manager over every name in the window's pkg ops, and one
//! probe per non-distro op (script, cargo: installed version, and latest
//! when wanted), sent in that op's `RunAs` on its principal's shell.
//!
//! Gathering versus asserting: package state is *gathered* and reported
//! ("will install: ..."); it is never asserted. The pkg templates already
//! skip what is installed, and an earlier op in the same window may install
//! the tool a later probe needs, so a probe that fails or finds a name
//! missing cannot block. The `exists` / `bin_*` facts the playbook asked
//! are the engine's reads, already batched into one round trip each.

use std::collections::BTreeSet;

use crate::engine::Window;
use crate::facts::os::Os;
use crate::ops::pkg::{Manager, PkgMode, PkgSpec, Source, Wanted};
use crate::pool::{Shell, ShellError};
use crate::report::{WillChange, WillCreate, WillInstall};
use crate::sh::quote;
use crate::trace::{OpNode, OpSpec, Principal};
use crate::trailer::{OpKind, wrap};

/// Non-root users the window's ops address (`as_user(u)`).
pub fn users_in(window: &Window) -> BTreeSet<String> {
    window
        .ops
        .iter()
        .filter_map(|o| match &o.node.run_as.principal {
            Principal::User(u) => Some(u.clone()),
            Principal::Root => None,
        })
        .collect()
}

/// A script printing `ok <u>` or `missing <u>` per user.
pub fn users_script<'a>(users: impl IntoIterator<Item = &'a String>) -> String {
    let mut s = String::new();
    for u in users {
        let q = quote(u);
        s.push_str(&format!(
            "if command -v getent >/dev/null 2>&1; then getent passwd {q} >/dev/null 2>&1; else grep -q \"^\"{q}\":\" /etc/passwd; fi && echo \"ok \"{q} || echo \"missing \"{q}\n"
        ));
    }
    s
}

/// The users `users_script`'s output says are missing.
pub fn missing_users(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|l| l.strip_prefix("missing "))
        .map(str::to_string)
        .collect()
}

/// Package names per manager that the window asks to install, in
/// first-seen order. `build-dep` ops are skipped: what they would install
/// is not known up front. `@group` names are skipped too.
pub fn pkg_names(window: &Window) -> Vec<(Source, Vec<String>)> {
    let mut out: Vec<(Source, Vec<String>)> = Vec::new();
    for op in &window.ops {
        let OpSpec::Pkg(p) = &op.node.spec else {
            continue;
        };
        if p.mode == PkgMode::BuildDep {
            continue;
        }
        // Only distro and pip names go through `pkg_probe`; the other
        // sources have their own probe ([`source_probe`]).
        if !matches!(p.source, Source::Distro(_) | Source::Pip) {
            continue;
        }
        let i = match out.iter().position(|(s, _)| *s == p.source) {
            Some(i) => i,
            None => {
                out.push((p.source.clone(), Vec::new()));
                out.len() - 1
            }
        };
        for n in &p.names {
            if !n.starts_with('@') && !out[i].1.contains(n) {
                out[i].1.push(n.clone());
            }
        }
    }
    out.retain(|(_, v)| !v.is_empty());
    out
}

/// A script printing `present <name>` for every name already installed.
pub fn pkg_probe(source: &Source, names: &[String]) -> String {
    let mut s = String::new();
    if *source == Source::Pip {
        s.push_str(
            "tp_l=$( (python3 -m pip list --format=freeze 2>/dev/null || pip3 list --format=freeze 2>/dev/null) | cut -d= -f1 | tr 'A-Z_' 'a-z-')\n",
        );
    }
    for n in names {
        let q = quote(n);
        let test = match source {
            Source::Distro(Manager::Apt) => format!(
                "[ \"$(dpkg-query -W -f='${{db:Status-Status}}' {q} 2>/dev/null)\" = installed ]"
            ),
            Source::Distro(Manager::Dnf | Manager::Yum) => {
                format!("rpm -q --whatprovides {q} >/dev/null 2>&1")
            }
            Source::Distro(Manager::Pacman) => format!("[ -z \"$(pacman -T {q} 2>/dev/null)\" ]"),
            Source::Pip => format!(
                "printf '%s\\n' \"$tp_l\" | grep -qx -- \"$(printf %s {q} | tr 'A-Z_' 'a-z-')\""
            ),
            // Not probed here (see `pkg_names`): report nothing as present.
            Source::Cargo { .. } | Source::Mise | Source::Rustup(_) | Source::Script(_) => {
                "false".to_string()
            }
        };
        s.push_str(&format!("if {test}; then echo \"present \"{q}; fi\n"));
    }
    s
}

pub fn manager_name(s: &Source) -> &'static str {
    match s {
        Source::Distro(Manager::Apt) => "apt",
        Source::Distro(Manager::Dnf) => "dnf",
        Source::Distro(Manager::Yum) => "yum",
        Source::Distro(Manager::Pacman) => "pacman",
        Source::Pip => "pip",
        Source::Cargo { .. } => "cargo",
        Source::Mise => "mise",
        Source::Rustup(_) => "rustup",
        Source::Script(_) => "script",
    }
}

/// Names `pkg_probe`'s output does not list as present.
pub fn missing_names(names: &[String], output: &str) -> Vec<String> {
    let present: BTreeSet<&str> = output
        .lines()
        .filter_map(|l| l.strip_prefix("present "))
        .collect();
    names
        .iter()
        .filter(|n| !present.contains(n.as_str()))
        .cloned()
        .collect()
}

/// The probe script of a non-distro, non-pip pkg op, or `None` when its
/// source has no such probe (distro and pip go through [`pkg_probe`]).
pub fn source_probe(p: &PkgSpec) -> Option<String> {
    match &p.source {
        Source::Script(s) => Some(s.probe(&p.wanted)),
        Source::Cargo { .. } => Some(crate::ops::pkg::cargo_probe(&p.names, &p.wanted)),
        Source::Mise => Some(crate::ops::pkg::mise_probe(&p.names, &p.wanted)),
        Source::Rustup(r) => {
            let tc = p.names.first().map(String::as_str).unwrap_or("");
            Some(crate::ops::pkg::rustup_probe(tc, &p.wanted, r))
        }
        // A pacman pin is checked against the repo here; other distro
        // wants are not probed (pkg_probe covers presence).
        Source::Distro(Manager::Pacman) => match (&p.wanted, p.names.first()) {
            (Wanted::Pinned(_), Some(n)) => {
                let q = quote(n);
                Some(format!(
                    "tp_i=$(pacman -Q {q} 2>/dev/null | cut -d' ' -f2)\ntp_r=$(pacman -Si {q} 2>/dev/null | sed -n 's/^Version *: //p' | head -n1)\necho \"installed ${{tp_i:--}}\"\necho \"repo ${{tp_r:--}}\"\n"
                ))
            }
            _ => None,
        },
        Source::Distro(_) | Source::Pip => None,
    }
}

/// The reason a pkg op blocks its host, from its [`source_probe`] output.
/// A pacman pin that is neither the repo's version nor the installed one
/// cannot be met (Arch repos hold one version). A failed probe, or a repo
/// that does not list the package, is unknown, never blocked.
pub fn source_blocker(window: usize, p: &PkgSpec, out: Option<&str>) -> Option<String> {
    let (Source::Distro(Manager::Pacman), Wanted::Pinned(pin), Some(out), Some(name)) =
        (&p.source, &p.wanted, out, p.names.first())
    else {
        return None;
    };
    let field = |k: &str| {
        out.lines()
            .find_map(|l| l.strip_prefix(k))
            .map(str::trim)
            .filter(|v| !v.is_empty() && *v != "-")
    };
    let repo = field("repo ")?;
    if repo == pin || field("installed ") == Some(pin.as_str()) {
        return None;
    }
    Some(format!(
        "pacman: cannot pin {name} to {pin} (window {window}): the repo has {repo}, and Arch repos hold one version per package"
    ))
}

/// Parse a [`source_probe`]'s output (`None` when the probe failed) into
/// what will change. A failed probe means "unknown", never blocked.
pub fn source_changes(window: usize, p: &PkgSpec, out: Option<&str>) -> Vec<WillChange> {
    match &p.source {
        Source::Script(_) => {
            let (have, latest) = match out {
                Some(o) => crate::ops::pkg::parse_probe(o),
                None => (crate::ops::pkg::Probed::Unknown, None),
            };
            let name = p.names.first().cloned().unwrap_or_default();
            crate::ops::pkg::will_change(&have, latest.as_deref(), &p.wanted)
                .map(|(installed, wanted)| WillChange {
                    window,
                    source: manager_name(&p.source).to_string(),
                    user: p.user.clone(),
                    name,
                    installed,
                    wanted,
                })
                .into_iter()
                .collect()
        }
        Source::Cargo { .. } => {
            let probed = out
                .map(crate::ops::pkg::parse_cargo_probe)
                .unwrap_or_default();
            crate::ops::pkg::cargo_will_changes(&p.names, &probed, &p.wanted)
                .into_iter()
                .map(|(name, installed, wanted)| WillChange {
                    window,
                    source: manager_name(&p.source).to_string(),
                    user: p.user.clone(),
                    name,
                    installed,
                    wanted,
                })
                .collect()
        }
        Source::Mise => {
            let probed = out
                .map(crate::ops::pkg::parse_mise_probe)
                .unwrap_or_default();
            crate::ops::pkg::mise_will_changes(&p.names, &probed, &p.wanted)
                .into_iter()
                .map(|(name, installed, wanted)| WillChange {
                    window,
                    source: manager_name(&p.source).to_string(),
                    user: p.user.clone(),
                    name,
                    installed,
                    wanted,
                })
                .collect()
        }
        Source::Rustup(r) => {
            // An empty output is a failed probe (a working one prints
            // "rustup ok" or "rustup absent" at least).
            let probed = out
                .map(crate::ops::pkg::parse_rustup_probe)
                .unwrap_or_default();
            let name = p.names.first().cloned().unwrap_or_default();
            crate::ops::pkg::rustup_will_change(r, &probed, &p.wanted)
                .map(|(installed, wanted)| WillChange {
                    window,
                    source: manager_name(&p.source).to_string(),
                    user: p.user.clone(),
                    name,
                    installed,
                    wanted,
                })
                .into_iter()
                .collect()
        }
        Source::Distro(_) | Source::Pip => Vec::new(),
    }
}

/// What prepare found a window will do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Prepared {
    pub will_install: Vec<WillInstall>,
    pub will_change: Vec<WillChange>,
    pub will_create: Vec<WillCreate>,
}

/// Names of the groups the window's `group` ops ensure, first-seen order,
/// without duplicates.
pub fn group_names(window: &Window) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for op in &window.ops {
        if let OpSpec::Group(g) = &op.node.spec
            && !out.contains(&g.name)
        {
            out.push(g.name.clone());
        }
    }
    out
}

/// What the window's `svc` ops want, merged per unit key.
pub fn svc_wants(window: &Window) -> Vec<crate::ops::svc::Want> {
    crate::ops::svc::wants(window.ops.iter().filter_map(|op| match &op.node.spec {
        OpSpec::Svc(s) => Some(s),
        _ => None,
    }))
}

/// True when an op that can install software (a package of any source, a
/// `sh`, a file) comes before the window's first `svc` op: the svc probe
/// runs before it, so what it finds missing may exist by run time.
pub fn svc_after_installers(window: &Window) -> bool {
    for op in &window.ops {
        match &op.node.spec {
            // A restart or reload is not probed (not a want).
            OpSpec::Svc(s) if s.action().is_some() => {}
            OpSpec::Svc(_) => return false,
            // A unit op writes the unit a later svc op names.
            OpSpec::Pkg(_)
            | OpSpec::Shell(_)
            | OpSpec::Raw { .. }
            | OpSpec::File(_)
            | OpSpec::Unit(_) => {
                return true;
            }
            OpSpec::Group(_) | OpSpec::User(_) | OpSpec::Repo(_) => {}
        }
    }
    false
}

/// The window's `unit` ops, in window order.
pub fn unit_specs(window: &Window) -> Vec<&crate::ops::unit::UnitSpec> {
    window
        .ops
        .iter()
        .filter_map(|op| match &op.node.spec {
            OpSpec::Unit(u) => Some(u),
            _ => None,
        })
        .collect()
}

/// True when an op that can install software comes before the window's
/// first `unit` op: a missing `systemd-analyze` may exist by run time.
pub fn unit_after_installers(window: &Window) -> bool {
    for op in &window.ops {
        match &op.node.spec {
            OpSpec::Unit(_) => return false,
            OpSpec::Pkg(_) | OpSpec::Shell(_) | OpSpec::Raw { .. } | OpSpec::File(_) => {
                return true;
            }
            _ => {}
        }
    }
    false
}

/// Run `script` in `shell` as a plain op with id `id`; its rc and output.
pub async fn probe(shell: &mut Shell, id: usize, script: &str) -> Result<(i32, String), String> {
    let wrapped = wrap(
        OpKind::Sh,
        &crate::ops::shell::raw_body(script),
        id,
        shell.nonce(),
    );
    shell.send(id, &wrapped).await.map_err(|e| e.to_string())?;
    collect(shell, id).await
}

/// Wait for the op `id` already sent.
pub async fn collect(shell: &mut Shell, id: usize) -> Result<(i32, String), String> {
    let out = shell.wait(id).await.map_err(|e| e.to_string())?;
    let rc = out
        .trailer
        .typed::<crate::trailer::ShellResult>()
        .map_err(|e| e.to_string())?
        .rc;
    Ok((rc, String::from_utf8_lossy(&out.output).into_owned()))
}

/// A host's shells as prepare sees them: the root shell, the shells of the
/// users opened so far, and a way to open one more.
#[allow(async_fn_in_trait)]
pub trait ShellSet {
    /// The shell ops of `who` run in: `Root` is the root shell, `User(u)`
    /// is `u`'s shell once [`ShellSet::open_user`] has opened it.
    fn shell(&mut self, who: &Principal) -> Option<&mut Shell>;
    /// Open `u`'s shell unless it is open. `Err` is the reason the host is
    /// blocked.
    async fn open_user(&mut self, u: &str) -> Result<(), String>;
}

fn shell_of<'a, S: ShellSet>(shells: &'a mut S, who: &Principal) -> Result<&'a mut Shell, String> {
    shells.shell(who).ok_or_else(|| match who {
        Principal::Root => "no root shell".to_string(),
        Principal::User(u) => format!("no shell open for user {u}"),
    })
}

/// Check `window` on the host. Users already verified are in `users_ok`.
/// `Err` is the reason the host is blocked. On success, the packages the
/// window's pkg ops will install or change. `seq` hands out probe ids.
///
/// Order: the `getent` check for users not yet verified (root shell, one
/// round trip); then the shell of every user the window addresses that is
/// not open yet ([`ShellSet::open_user`]); then the package probes, sent
/// all at once and collected (distro and pip on the root shell, each
/// non-distro probe on its op's principal's shell).
pub async fn prepare_window<S: ShellSet>(
    shells: &mut S,
    window: &Window,
    os: &Os,
    users_ok: &mut BTreeSet<String>,
    seq: &mut usize,
) -> Result<Prepared, String> {
    let users = users_in(window);
    let new_users: Vec<String> = users
        .iter()
        .filter(|u| !users_ok.contains(*u))
        .cloned()
        .collect();
    let groups = pkg_names(window);

    let mut next = || {
        *seq += 1;
        *seq
    };
    if !new_users.is_empty() {
        let id = next();
        let root = shell_of(shells, &Principal::Root)?;
        let (_, out) = probe(root, id, &users_script(&new_users)).await?;
        let missing = missing_users(&out);
        if !missing.is_empty() {
            return Err(format!(
                "user {} does not exist on the host (as_user in window {})",
                missing.join(", "),
                window.number
            ));
        }
        users_ok.extend(new_users);
    }
    for u in &users {
        shells.open_user(u).await?;
    }

    // Send every query, then collect: one round trip.
    let mut pkg_ids = Vec::new();
    for (m, names) in &groups {
        let id = next();
        let root = shell_of(shells, &Principal::Root)?;
        let w = wrap(
            OpKind::Sh,
            &crate::ops::shell::raw_body(&pkg_probe(m, names)),
            id,
            root.nonce(),
        );
        root.send(id, &w).await.map_err(|e| e.to_string())?;
        pkg_ids.push(id);
    }
    // Group existence, one probe for the window, on the root shell (a
    // group op is root only).
    let group_list = group_names(window);
    let mut group_id = None;
    if !group_list.is_empty() {
        let id = next();
        let root = shell_of(shells, &Principal::Root)?;
        let w = wrap(
            OpKind::Sh,
            &crate::ops::shell::raw_body(&crate::ops::group::probe(&group_list)),
            id,
            root.nonce(),
        );
        root.send(id, &w).await.map_err(|e| e.to_string())?;
        group_id = Some(id);
    }
    // Accounts, one probe for the window, on the root shell (a user op is
    // root only).
    let mut user_id = None;
    let user_specs: Vec<&crate::ops::user::UserSpec> = window
        .ops
        .iter()
        .filter_map(|op| match &op.node.spec {
            OpSpec::User(u) => Some(u),
            _ => None,
        })
        .collect();
    if !user_specs.is_empty() {
        let id = next();
        let root = shell_of(shells, &Principal::Root)?;
        let w = wrap(
            OpKind::Sh,
            &crate::ops::shell::raw_body(&crate::ops::user::probe(&user_specs)),
            id,
            root.nonce(),
        );
        root.send(id, &w).await.map_err(|e| e.to_string())?;
        user_id = Some(id);
    }
    // Repositories, one probe for the window, on the root shell (a repo op
    // is root only).
    let mut repo_id = None;
    let repo_specs: Vec<&crate::ops::repo::RepoSpec> = window
        .ops
        .iter()
        .filter_map(|op| match &op.node.spec {
            OpSpec::Repo(r) => Some(r),
            _ => None,
        })
        .collect();
    if !repo_specs.is_empty() {
        let id = next();
        let root = shell_of(shells, &Principal::Root)?;
        let w = wrap(
            OpKind::Sh,
            &crate::ops::shell::raw_body(&crate::ops::repo::probe(&repo_specs)),
            id,
            root.nonce(),
        );
        root.send(id, &w).await.map_err(|e| e.to_string())?;
        repo_id = Some(id);
    }
    // Units, one probe for the window, on the root shell (a svc op is root
    // only), after the repo probe. Gathered, never asserted: the package
    // may arrive earlier in the same window.
    let mut svc_id = None;
    let svc_list = svc_wants(window);
    if !svc_list.is_empty() {
        let id = next();
        let root = shell_of(shells, &Principal::Root)?;
        let w = wrap(
            OpKind::Sh,
            &crate::ops::shell::raw_body(&crate::ops::svc::probe(&svc_list)),
            id,
            root.nonce(),
        );
        root.send(id, &w).await.map_err(|e| e.to_string())?;
        svc_id = Some(id);
    }
    // Unit files, one probe for the window, on the root shell (a unit op
    // is root only). Gathered, never asserted.
    let mut unit_id = None;
    let unit_list = unit_specs(window);
    if !unit_list.is_empty() {
        let id = next();
        let root = shell_of(shells, &Principal::Root)?;
        let w = wrap(
            OpKind::Sh,
            &crate::ops::shell::raw_body(&crate::ops::unit::probe(&unit_list)),
            id,
            root.nonce(),
        );
        root.send(id, &w).await.map_err(|e| e.to_string())?;
        unit_id = Some(id);
    }
    // Non-distro probes, each in its op's RunAs and principal's shell (as
    // `read_facts` does).
    let mut src_ids: Vec<(&PkgSpec, &Principal, Option<usize>)> = Vec::new();
    for op in &window.ops {
        let OpSpec::Pkg(p) = &op.node.spec else {
            continue;
        };
        let Some(script) = source_probe(p) else {
            continue;
        };
        let id = next();
        let node = OpNode {
            spec: OpSpec::Raw { script },
            run_as: op.node.run_as.clone(),
            reads: Default::default(),
            writes: Default::default(),
            guard: None,
            post: Default::default(),
            allow_fail: false,
            site: None,
            allowed: Vec::new(),
            at_end: false,
        };
        let who = &op.node.run_as.principal;
        let sh = shell_of(shells, who)?;
        match sh.send_op(&node, id, os).await {
            Ok(()) => src_ids.push((p, who, Some(id))),
            // Not renderable for this shell: unknown, not blocked.
            Err(ShellError::Render(_)) => src_ids.push((p, who, None)),
            Err(e) => return Err(e.to_string()),
        }
    }

    let mut will = Prepared::default();
    for ((m, names), id) in groups.iter().zip(pkg_ids) {
        // Gather only: a failed probe means "unknown", not "blocked".
        let root = shell_of(shells, &Principal::Root)?;
        let missing = match collect(root, id).await {
            Ok((_, out)) => missing_names(names, &out),
            Err(_) => names.clone(),
        };
        if !missing.is_empty() {
            will.will_install.push(WillInstall {
                window: window.number,
                manager: manager_name(m).to_string(),
                names: missing,
            });
        }
    }
    for (p, who, id) in src_ids {
        let out = match id {
            Some(id) => match collect(shell_of(shells, who)?, id).await {
                Ok((0, out)) => Some(out),
                _ => None,
            },
            None => None,
        };
        if let Some(why) = source_blocker(window.number, p, out.as_deref()) {
            return Err(why);
        }
        will.will_change
            .extend(source_changes(window.number, p, out.as_deref()));
    }
    if let Some(id) = group_id {
        // Gather only: a failed probe reports nothing.
        let root = shell_of(shells, &Principal::Root)?;
        if let Ok((_, out)) = collect(root, id).await {
            for name in crate::ops::group::missing(&out) {
                will.will_create.push(WillCreate {
                    window: window.number,
                    kind: "group".to_string(),
                    name,
                    detail: None,
                });
            }
        }
    }
    if let Some(id) = user_id {
        let root = shell_of(shells, &Principal::Root)?;
        if let Ok((_, out)) = collect(root, id).await {
            for (name, detail) in crate::ops::user::findings(&out) {
                will.will_create.push(WillCreate {
                    window: window.number,
                    kind: "user".to_string(),
                    name,
                    detail,
                });
            }
        }
    }
    if let Some(id) = repo_id {
        // Gather only: a failed probe reports nothing.
        let root = shell_of(shells, &Principal::Root)?;
        if let Ok((_, out)) = collect(root, id).await {
            for name in crate::ops::repo::findings(&out) {
                will.will_create.push(WillCreate {
                    window: window.number,
                    kind: "repo".to_string(),
                    name,
                    detail: Some("will add".to_string()),
                });
            }
        }
    }
    if let Some(id) = unit_id {
        // Gather only: a failed probe reports nothing.
        let root = shell_of(shells, &Principal::Root)?;
        if let Ok((_, out)) = collect(root, id).await {
            let earlier = unit_after_installers(window);
            for (name, detail) in crate::ops::unit::findings(&unit_list, &out, earlier) {
                will.will_create.push(WillCreate {
                    window: window.number,
                    kind: "unit".to_string(),
                    name,
                    detail: Some(detail),
                });
            }
        }
    }
    if let Some(id) = svc_id {
        // Gather only: a failed probe reports nothing.
        let root = shell_of(shells, &Principal::Root)?;
        if let Ok((_, out)) = collect(root, id).await {
            let earlier = svc_after_installers(window);
            for (name, detail) in crate::ops::svc::findings_after(&svc_list, &out, earlier) {
                will.will_create.push(WillCreate {
                    window: window.number,
                    kind: "svc".to_string(),
                    name,
                    detail: Some(detail),
                });
            }
        }
    }
    Ok(will)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_names_and_users_parse() {
        let names = vec!["a".to_string(), "b".to_string()];
        assert_eq!(missing_names(&names, "present a\n"), vec!["b".to_string()]);
        assert_eq!(missing_users("ok x\nmissing y\n"), vec!["y".to_string()]);
    }

    #[test]
    fn probes_quote_names() {
        let s = pkg_probe(&Source::Distro(Manager::Apt), &["it's".to_string()]);
        assert!(s.contains(r"'it'\''s'"), "{s}");
    }

    #[test]
    fn a_restart_before_the_installers_is_not_the_first_svc_want() {
        use crate::engine::{Member, WindowKind, WindowOp};
        use crate::ops::svc::SvcSpec;
        use crate::trace::{NodeId, RunAs};
        let op = |id: usize, spec: OpSpec| WindowOp {
            id: NodeId(id),
            members: vec![Member {
                id: NodeId(id),
                role: String::new(),
                pkg_names: Vec::new(),
            }],
            node: OpNode {
                writes: spec.default_writes(),
                post: spec.default_post(),
                spec,
                run_as: RunAs::root_shell(),
                reads: Default::default(),
                guard: None,
                allow_fail: false,
                site: None,
                allowed: Vec::new(),
                at_end: false,
            },
        };
        let restart = SvcSpec {
            restart: true,
            ..SvcSpec::new("cron")
        };
        let started = SvcSpec {
            started: true,
            ..SvcSpec::new("docker")
        };
        let w = Window {
            number: 1,
            kind: WindowKind::Final,
            ops: vec![
                op(0, OpSpec::Svc(restart)),
                op(
                    1,
                    OpSpec::Pkg(PkgSpec::new(Manager::Apt, vec!["docker".into()])),
                ),
                op(2, OpSpec::Svc(started)),
            ],
        };
        assert!(svc_after_installers(&w));
        assert_eq!(svc_wants(&w).len(), 1, "a restart is not probed");
    }

    fn pacman_pin(name: &str, v: &str) -> PkgSpec {
        let mut p = PkgSpec::new(Manager::Pacman, vec![name.into()]);
        p.wanted = Wanted::Pinned(v.into());
        p
    }

    #[test]
    fn only_a_pacman_pin_has_a_distro_probe() {
        let probe = source_probe(&pacman_pin("jq", "1.7-1")).unwrap();
        assert!(probe.contains("pacman -Si 'jq'"), "{probe}");
        assert!(probe.contains("pacman -Q 'jq'"), "{probe}");
        let mut p = PkgSpec::new(Manager::Pacman, vec!["jq".into()]);
        assert!(source_probe(&p).is_none());
        p.wanted = Wanted::Latest;
        assert!(source_probe(&p).is_none());
        let mut a = pacman_pin("jq", "1");
        a.source = Source::Distro(Manager::Apt);
        assert!(source_probe(&a).is_none());
    }

    #[test]
    fn a_pacman_pin_not_the_repo_version_blocks() {
        let p = pacman_pin("jq", "1.6-1");
        let msg = source_blocker(2, &p, Some("installed -\nrepo 1.7.1-3\n")).unwrap();
        assert!(
            msg.contains("jq") && msg.contains("1.6-1") && msg.contains("1.7.1-3"),
            "{msg}"
        );
        assert!(msg.contains("window 2"), "{msg}");
        // The repo's version, or what is installed already, is fine.
        assert!(
            source_blocker(
                2,
                &pacman_pin("jq", "1.7.1-3"),
                Some("installed -\nrepo 1.7.1-3\n")
            )
            .is_none()
        );
        assert!(source_blocker(2, &p, Some("installed 1.6-1\nrepo 1.7.1-3\n")).is_none());
        // Unknown is never blocked: failed probe, or no repo version.
        assert!(source_blocker(2, &p, None).is_none());
        assert!(source_blocker(2, &p, Some("installed -\nrepo -\n")).is_none());
    }
}
