//! `svc(unit)`: a systemd unit the host should have enabled and/or started.
//! `.enabled()` and `.started()` are the only wants built; at least one is
//! required (`svc(docker): nothing wanted` is a trace error otherwise). Root
//! only, as `group`.
//!
//! The unit is passed to `systemctl` as given; the key is `svc:<unit>` with
//! `.service` appended when the name has no unit suffix, so `svc("docker")`
//! and `svc("docker.service")` share one key.
//!
//! # Hosts without a running systemd
//!
//! "Booted" is `[ -d /run/systemd/system ]` (the `sd_booted` test; the path
//! can be overridden by `TP_SVC_RUNDIR` so tests do not depend on the machine
//! running them). Probed by FOREMAN in init-less `ubuntu:24.04` (+systemd,
//! cron) and `rockylinux/rockylinux:10` (+systemd, cronie) containers:
//!
//! * the directory is absent; `systemctl is-enabled` works offline
//!   (`enabled` rc 0, `disabled` rc 1, `not-found` rc 4, `masked` rc 1);
//! * plain `systemctl enable` / `disable` work offline, rc 0, with no
//!   `--root=/` needed (Ubuntu also prints the systemd-sysv-install lines);
//!   enabling a masked unit prints `Failed to enable unit: Unit ... is
//!   masked`, rc 1;
//! * `systemctl is-active` and `systemctl show -p NeedDaemonReload` fail
//!   offline ("System has not been booted with systemd as init system"), so
//!   they are only called when booted;
//! * `cron` and `cronie` are enabled by their package on install.
//!
//! So: `systemctl` present, not booted: `enabled` is applied (a unit-file
//! operation, so an image booted later runs the service), `started` is
//! skipped. No `systemctl`: the whole op is skipped. A skip is rc 0,
//! `skipped=1`, never `changed`. On a booted host `start` is preceded by
//! `systemctl daemon-reload` when systemd says `NeedDaemonReload=yes` for the
//! unit. The decision is made by the script at run time from the host's real
//! state (the package may arrive earlier in the same window).

use std::ops::Deref;

use crate::TraceError;
use crate::host::{Host, OpHandle};
use crate::sh::quote;
use crate::trace::{OpSpec, Principal, Trace};

/// Unit-name suffixes systemd knows.
const SUFFIXES: [&str; 11] = [
    ".service",
    ".socket",
    ".device",
    ".mount",
    ".automount",
    ".swap",
    ".target",
    ".path",
    ".timer",
    ".slice",
    ".scope",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvcSpec {
    /// The unit as the role wrote it (passed to `systemctl` unchanged).
    pub unit: String,
    pub enabled: bool,
    pub started: bool,
}

impl SvcSpec {
    pub fn new(unit: impl Into<String>) -> SvcSpec {
        SvcSpec {
            unit: unit.into(),
            enabled: false,
            started: false,
        }
    }

    /// The unit with `.service` appended when it has no unit suffix: the
    /// name in the key, the plan, the report and prepare's line.
    pub fn key_name(&self) -> String {
        key_name(&self.unit)
    }

    pub fn render(&self) -> String {
        let mut s = format!(
            "tp_u={}\ntp_run=\"${{TP_SVC_RUNDIR:-/run/systemd/system}}\"\ntp_skip=0\nif ! command -v systemctl >/dev/null 2>&1; then\n  skipped=1\nelse\n",
            quote(&self.unit)
        );
        if self.enabled {
            s.push_str(concat!(
                "  case \"$(systemctl is-enabled -- \"$tp_u\" 2>/dev/null)\" in\n",
                "    enabled|enabled-runtime|static|alias) ;;\n",
                "    *)\n",
                "      systemctl enable -- \"$tp_u\"\n",
                "      rc=$?\n",
                "      if [ \"$rc\" -eq 0 ]; then changed=1; fi\n",
                "      ;;\n",
                "  esac\n",
            ));
        }
        if self.started {
            s.push_str(concat!(
                "  if [ \"$rc\" -eq 0 ]; then\n",
                "    if [ -d \"$tp_run\" ]; then\n",
                "      if ! systemctl is-active --quiet -- \"$tp_u\"; then\n",
                "        if [ \"$(systemctl show -p NeedDaemonReload --value -- \"$tp_u\" 2>/dev/null)\" = yes ]; then\n",
                "          systemctl daemon-reload\n",
                "          rc=$?\n",
                "        fi\n",
                "        if [ \"$rc\" -eq 0 ]; then\n",
                "          systemctl start -- \"$tp_u\"\n",
                "          rc=$?\n",
                "          if [ \"$rc\" -eq 0 ]; then changed=1; fi\n",
                "        fi\n",
                "      fi\n",
                "    else\n",
                "      tp_skip=1\n",
                "    fi\n",
                "  fi\n",
                "  if [ \"$rc\" -eq 0 ] && [ \"$changed\" -eq 0 ] && [ \"$tp_skip\" -eq 1 ]; then skipped=1; fi\n",
            ));
        }
        s.push_str("fi\n");
        s
    }
}

/// See [`SvcSpec::key_name`].
pub fn key_name(unit: &str) -> String {
    if SUFFIXES.iter().any(|s| unit.ends_with(s)) {
        unit.to_string()
    } else {
        format!("{unit}.service")
    }
}

/// Handle for a `svc` op.
#[derive(Clone)]
pub struct SvcHandle(OpHandle);

impl Deref for SvcHandle {
    type Target = OpHandle;
    fn deref(&self) -> &OpHandle {
        &self.0
    }
}

impl SvcHandle {
    /// Want the unit enabled (`systemctl enable`).
    pub fn enabled(&self) -> &Self {
        self.0.edit(|op| {
            if let OpSpec::Svc(s) = &mut op.spec {
                s.enabled = true;
            }
        });
        self
    }

    /// Want the unit started (`systemctl start`).
    pub fn started(&self) -> &Self {
        self.0.edit(|op| {
            if let OpSpec::Svc(s) = &mut op.spec {
                s.started = true;
            }
        });
        self
    }
}

impl<V> Host<V> {
    /// A systemd unit: chain `.enabled()` and/or `.started()`. Root only.
    pub fn svc(&self, unit: &str) -> SvcHandle {
        let h = self.op(OpSpec::Svc(SvcSpec::new(unit)));
        if let Principal::User(u) = &self.run_as().principal {
            h.record_error(TraceError::new(format!(
                "svc({unit}) is root only: as_user({u}) cannot manage units"
            )));
        }
        SvcHandle(h)
    }
}

/// Trace errors found only once an op is complete: an `svc` that wants
/// nothing. Derived from the nodes, so a chain split over statements is
/// fine as long as it is finished by the time the trace is read.
pub fn nothing_wanted(trace: &Trace) -> Vec<TraceError> {
    trace
        .nodes
        .iter()
        .filter_map(|n| match n {
            crate::trace::Node::Op(op) => match &op.spec {
                OpSpec::Svc(s) if !s.enabled && !s.started => {
                    Some(TraceError::new(format!("svc({}): nothing wanted", s.unit)))
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// What one unit of a window wants, merged over the window's ops that name
/// the same key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Want {
    /// The first spelling, passed to `systemctl`.
    pub unit: String,
    pub key: String,
    pub enabled: bool,
    pub started: bool,
}

/// Merge specs by key, first-seen order.
pub fn wants<'a>(specs: impl IntoIterator<Item = &'a SvcSpec>) -> Vec<Want> {
    let mut out: Vec<Want> = Vec::new();
    for s in specs {
        let key = s.key_name();
        match out.iter_mut().find(|w| w.key == key) {
            Some(w) => {
                w.enabled |= s.enabled;
                w.started |= s.started;
            }
            None => out.push(Want {
                unit: s.unit.clone(),
                key,
                enabled: s.enabled,
                started: s.started,
            }),
        }
    }
    out
}

/// A prepare probe. Prints `systemctl yes|no`, `booted yes|no`, and per
/// unit `enabled <state> <unit>` and (booted only) `active <state> <unit>`;
/// `-` is an empty answer.
pub fn probe(wants: &[Want]) -> String {
    let mut s = String::from(
        "tp_run=\"${TP_SVC_RUNDIR:-/run/systemd/system}\"\nif command -v systemctl >/dev/null 2>&1; then echo \"systemctl yes\"; else echo \"systemctl no\"; fi\nif [ -d \"$tp_run\" ]; then echo \"booted yes\"; else echo \"booted no\"; fi\nif command -v systemctl >/dev/null 2>&1; then\n",
    );
    for w in wants {
        let q = quote(&w.unit);
        s.push_str(&format!(
            "  tp_s=$(systemctl is-enabled -- {q} 2>/dev/null | head -n 1); echo \"enabled ${{tp_s:--}} \"{q}\n"
        ));
        if w.started {
            s.push_str(&format!(
                "  if [ -d \"$tp_run\" ]; then tp_s=$(systemctl is-active -- {q} 2>/dev/null | head -n 1); echo \"active ${{tp_s:--}} \"{q}; fi\n"
            ));
        }
    }
    s.push_str("fi\n");
    s
}

fn state<'a>(v: &[(&'a str, &'a str)], u: &str) -> Option<&'a str> {
    v.iter()
        .find(|(n, _)| *n == u)
        .map(|(_, s)| *s)
        .filter(|s| *s != "-")
}

/// `(key, detail)` per unit that has something to say, from [`probe`]'s
/// output, for a window where nothing that could install `systemctl` runs
/// before the svc ops. A unit whose answers are missing or empty is left out.
pub fn findings(wants: &[Want], output: &str) -> Vec<(String, String)> {
    findings_after(wants, output, false)
}

/// As [`findings`]; `earlier` says an op that can install software (a
/// package, a `sh`, a file) runs before the window's first svc op. Then a
/// missing `systemctl` is not a promised skip: the probe ran before those
/// ops, and the script decides at run time from what the host has then.
pub fn findings_after(wants: &[Want], output: &str, earlier: bool) -> Vec<(String, String)> {
    let mut systemctl = None;
    let mut booted = None;
    let mut en: Vec<(&str, &str)> = Vec::new();
    let mut act: Vec<(&str, &str)> = Vec::new();
    for l in output.lines() {
        if let Some(v) = l.strip_prefix("systemctl ") {
            systemctl = Some(v == "yes");
        } else if let Some(v) = l.strip_prefix("booted ") {
            booted = Some(v == "yes");
        } else if let Some(r) = l.strip_prefix("enabled ")
            && let Some((st, u)) = r.split_once(' ')
        {
            en.push((u, st));
        } else if let Some(r) = l.strip_prefix("active ")
            && let Some((st, u)) = r.split_once(' ')
        {
            act.push((u, st));
        }
    }
    let (Some(systemctl), Some(booted)) = (systemctl, booted) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for w in wants {
        if !systemctl {
            let detail = if earlier {
                "no systemctl yet: decided at run time, after this window's earlier ops"
            } else {
                "no systemctl: skipped"
            };
            out.push((w.key.clone(), detail.to_string()));
            continue;
        }
        if earlier && state(&en, &w.unit) == Some("not-found") {
            out.push((
                w.key.clone(),
                "unit not found yet: decided at run time, after this window's earlier ops"
                    .to_string(),
            ));
            continue;
        }
        let en_needed = w.enabled
            && state(&en, &w.unit)
                .is_some_and(|s| !matches!(s, "enabled" | "enabled-runtime" | "static" | "alias"));
        let start_needed =
            w.started && booted && state(&act, &w.unit).is_some_and(|s| s != "active");
        let mut parts = Vec::new();
        if en_needed {
            parts.push("will enable");
        }
        if start_needed {
            parts.push("will start");
        }
        if parts.is_empty() {
            continue;
        }
        let mut detail = parts.join(", ");
        if en_needed && w.started && !booted {
            detail.push_str("; no running systemd: start skipped");
        }
        out.push((w.key.clone(), detail));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names() {
        assert_eq!(key_name("docker"), "docker.service");
        assert_eq!(key_name("docker.service"), "docker.service");
        assert_eq!(key_name("ssh.socket"), "ssh.socket");
        assert_eq!(key_name("a.b"), "a.b.service");
    }

    #[test]
    fn findings_parse_probe_output() {
        let w = wants(&[SvcSpec {
            unit: "docker".into(),
            enabled: true,
            started: true,
        }]);
        let f = findings(
            &w,
            "systemctl yes\nbooted yes\nenabled disabled docker\nactive inactive docker\n",
        );
        assert_eq!(
            f,
            [("docker.service".into(), "will enable, will start".into())]
        );
        assert!(findings(&w, "").is_empty());
        let f = findings(
            &w,
            "systemctl yes\nbooted yes\nenabled - docker\nactive - docker\n",
        );
        assert!(f.is_empty(), "{f:?}");
    }

    #[test]
    fn findings_unit_not_found_after_earlier_ops_is_decided_at_run_time() {
        let w = wants(&[SvcSpec {
            unit: "docker".into(),
            enabled: true,
            started: true,
        }]);
        let out = "systemctl yes\nbooted yes\nenabled not-found docker\nactive inactive docker\n";
        assert_eq!(
            findings_after(&w, out, true),
            [(
                "docker.service".into(),
                "unit not found yet: decided at run time, after this window's earlier ops".into()
            )]
        );
        assert_eq!(
            findings_after(&w, out, false),
            [("docker.service".into(), "will enable, will start".into())]
        );
    }

    #[test]
    fn findings_no_systemctl_after_earlier_ops_is_not_a_skip() {
        let w = wants(&[SvcSpec {
            unit: "docker".into(),
            enabled: true,
            started: true,
        }]);
        let out = "systemctl no\nbooted no\n";
        assert_eq!(
            findings_after(&w, out, false),
            [("docker.service".into(), "no systemctl: skipped".into())]
        );
        assert_eq!(
            findings_after(&w, out, true),
            [(
                "docker.service".into(),
                "no systemctl yet: decided at run time, after this window's earlier ops".into()
            )]
        );
    }
}
