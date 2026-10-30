//! Prepare: what is checked, in one batch of round trips, before a window
//! is applied. A failure blocks the host with nothing applied.
//!
//! The root shell opening is the permission check (`sudo -n` refusal is
//! [`crate::transport::Limit::SudoRefused`], handled when the host opens).
//! Per window this module checks that every user the window's ops address
//! exists (`getent passwd`), and gathers package state: one query per
//! manager over every name in the window's pkg ops.
//!
//! Gathering versus asserting: package state is *gathered* and reported
//! ("will install: ..."); it is never asserted. The pkg templates already
//! skip what is installed, and an earlier op in the same window may install
//! the tool a later probe needs, so a probe that fails or finds a name
//! missing cannot block. The `exists` / `bin_*` facts the playbook asked
//! are the engine's reads, already batched into one round trip each.

use std::collections::BTreeSet;

use crate::engine::Window;
use crate::ops::pkg::{Manager, PkgMode};
use crate::pool::Shell;
use crate::report::WillInstall;
use crate::sh::quote;
use crate::trace::{OpSpec, Principal};
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
pub fn pkg_names(window: &Window) -> Vec<(Manager, Vec<String>)> {
    let mut out: Vec<(Manager, Vec<String>)> = Vec::new();
    for op in &window.ops {
        let OpSpec::Pkg(p) = &op.node.spec else {
            continue;
        };
        if p.mode == PkgMode::BuildDep {
            continue;
        }
        let i = match out.iter().position(|(m, _)| *m == p.manager) {
            Some(i) => i,
            None => {
                out.push((p.manager, Vec::new()));
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
pub fn pkg_probe(manager: Manager, names: &[String]) -> String {
    let mut s = String::new();
    if manager == Manager::Pip {
        s.push_str(
            "tp_l=$( (python3 -m pip list --format=freeze 2>/dev/null || pip3 list --format=freeze 2>/dev/null) | cut -d= -f1 | tr 'A-Z_' 'a-z-')\n",
        );
    }
    for n in names {
        let q = quote(n);
        let test = match manager {
            Manager::Apt => format!(
                "[ \"$(dpkg-query -W -f='${{db:Status-Status}}' {q} 2>/dev/null)\" = installed ]"
            ),
            Manager::Dnf | Manager::Yum => format!("rpm -q --whatprovides {q} >/dev/null 2>&1"),
            Manager::Pacman => format!("[ -z \"$(pacman -T {q} 2>/dev/null)\" ]"),
            Manager::Pip => format!(
                "printf '%s\\n' \"$tp_l\" | grep -qx -- \"$(printf %s {q} | tr 'A-Z_' 'a-z-')\""
            ),
        };
        s.push_str(&format!("if {test}; then echo \"present \"{q}; fi\n"));
    }
    s
}

pub fn manager_name(m: Manager) -> &'static str {
    match m {
        Manager::Apt => "apt",
        Manager::Dnf => "dnf",
        Manager::Yum => "yum",
        Manager::Pacman => "pacman",
        Manager::Pip => "pip",
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

/// Check `window` on the host. Users already verified are in `users_ok`.
/// `Err` is the reason the host is blocked. On success, the packages the
/// window's pkg ops will install. `seq` hands out probe ids.
pub async fn prepare_window(
    shell: &mut Shell,
    window: &Window,
    users_ok: &mut BTreeSet<String>,
    seq: &mut usize,
) -> Result<Vec<WillInstall>, String> {
    let new_users: Vec<String> = users_in(window)
        .into_iter()
        .filter(|u| !users_ok.contains(u))
        .collect();
    let groups = pkg_names(window);

    // Send every query, then collect: one round trip.
    let mut next = || {
        *seq += 1;
        *seq
    };
    let user_id = if new_users.is_empty() {
        None
    } else {
        let id = next();
        let w = wrap(
            OpKind::Sh,
            &crate::ops::shell::raw_body(&users_script(&new_users)),
            id,
            shell.nonce(),
        );
        shell.send(id, &w).await.map_err(|e| e.to_string())?;
        Some(id)
    };
    let mut pkg_ids = Vec::new();
    for (m, names) in &groups {
        let id = next();
        let w = wrap(
            OpKind::Sh,
            &crate::ops::shell::raw_body(&pkg_probe(*m, names)),
            id,
            shell.nonce(),
        );
        shell.send(id, &w).await.map_err(|e| e.to_string())?;
        pkg_ids.push(id);
    }

    if let Some(id) = user_id {
        let (_, out) = collect(shell, id).await?;
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
    let mut will = Vec::new();
    for ((m, names), id) in groups.iter().zip(pkg_ids) {
        // Gather only: a failed probe means "unknown", not "blocked".
        let missing = match collect(shell, id).await {
            Ok((_, out)) => missing_names(names, &out),
            Err(_) => names.clone(),
        };
        if !missing.is_empty() {
            will.push(WillInstall {
                window: window.number,
                manager: manager_name(*m).to_string(),
                names: missing,
            });
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
        let s = pkg_probe(Manager::Apt, &["it's".to_string()]);
        assert!(s.contains(r"'it'\''s'"), "{s}");
    }
}
