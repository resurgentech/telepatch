//! `svc(unit)`: a systemd unit the host should have enabled and/or started.
//! Wants: `.enabled()` / `.disabled()` / `.masked()` (exclusive) and
//! `.started()` / `.stopped()` (exclusive); at least one is required
//! (`svc(docker): nothing wanted`, and `svc(docker): .enabled() and
//! .masked() are exclusive`, are trace errors). Root only, as `group`.
//! `disabled` disables an enabled unit and leaves a masked, static or
//! missing one alone; `masked` masks a unit that is not masked. Stop runs
//! first, then disable/mask; enable runs before stop, start last.
//!
//! The script sent to the host also records the unit's state before it
//! acts (`was_enabled`, `was_active`, `was_masked` in the trailer: the
//! inverse data; there is no undo log).
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
//! skipped (as is `stopped`). No `systemctl`: the whole op is skipped. A skip is rc 0,
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

/// Run in the script's `else` (systemctl exists), before anything changes:
/// the unit's state, as the three `tp_was_*` words (`true`, `false`; unset
/// when unknown) the trailer reports. Booted: one `systemctl show`. Not
/// booted (`show` fails offline): `is-enabled`, and `active` stays unknown.
const CAPTURE: &str = concat!(
    "  if [ -d \"$tp_run\" ]; then\n",
    "    tp_sh=$(systemctl show -p UnitFileState,ActiveState -- \"$tp_u\" 2>/dev/null)\n",
    "    tp_ue=$(printf '%s\\n' \"$tp_sh\" | sed -n 's/^UnitFileState=//p')\n",
    "    tp_as=$(printf '%s\\n' \"$tp_sh\" | sed -n 's/^ActiveState=//p')\n",
    "  else\n",
    "    tp_ue=$(systemctl is-enabled -- \"$tp_u\" 2>/dev/null | head -n 1)\n",
    "    tp_as=\n",
    "  fi\n",
    "  case \"$tp_ue\" in enabled|enabled-runtime|static|alias) tp_was_e=true;; ?*) tp_was_e=false;; esac\n",
    "  case \"$tp_ue\" in masked|masked-runtime) tp_was_m=true;; ?*) tp_was_m=false;; esac\n",
    "  case \"$tp_as\" in active) tp_was_a=true;; ?*) tp_was_a=false;; esac\n",
);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvcSpec {
    /// The unit as the role wrote it (passed to `systemctl` unchanged).
    pub unit: String,
    pub enabled: bool,
    pub disabled: bool,
    pub masked: bool,
    pub started: bool,
    pub stopped: bool,
}

impl SvcSpec {
    pub fn new(unit: impl Into<String>) -> SvcSpec {
        SvcSpec {
            unit: unit.into(),
            enabled: false,
            disabled: false,
            masked: false,
            started: false,
            stopped: false,
        }
    }

    /// The unit with `.service` appended when it has no unit suffix: the
    /// name in the key, the plan, the report and prepare's line.
    pub fn key_name(&self) -> String {
        key_name(&self.unit)
    }

    /// The op's script as the plan shows it.
    pub fn render(&self) -> String {
        self.script(false)
    }

    /// The script as sent to the host: [`SvcSpec::render`] plus the capture
    /// of the unit's state before anything changes (the trailer's
    /// `was_enabled`, `was_active` and `was_masked`: the inverse data).
    pub fn render_exec(&self) -> String {
        self.script(true)
    }

    fn script(&self, capture: bool) -> String {
        let mut s = format!(
            "tp_u={}\ntp_run=\"${{TP_SVC_RUNDIR:-/run/systemd/system}}\"\ntp_skip=0\nif ! command -v systemctl >/dev/null 2>&1; then\n  skipped=1\nelse\n",
            quote(&self.unit)
        );
        if capture {
            s.push_str(CAPTURE);
        }
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
        if self.stopped {
            s.push_str(concat!(
                "  if [ \"$rc\" -eq 0 ]; then\n",
                "    if [ -d \"$tp_run\" ]; then\n",
                "      if systemctl is-active --quiet -- \"$tp_u\"; then\n",
                "        systemctl stop -- \"$tp_u\"\n",
                "        rc=$?\n",
                "        if [ \"$rc\" -eq 0 ]; then changed=1; fi\n",
                "      fi\n",
                "    else\n",
                "      tp_skip=1\n",
                "    fi\n",
                "  fi\n",
            ));
        }
        if self.disabled {
            s.push_str(concat!(
                "  if [ \"$rc\" -eq 0 ]; then\n",
                "    case \"$(systemctl is-enabled -- \"$tp_u\" 2>/dev/null)\" in\n",
                "      enabled|enabled-runtime)\n",
                "        systemctl disable -- \"$tp_u\"\n",
                "        rc=$?\n",
                "        if [ \"$rc\" -eq 0 ]; then changed=1; fi\n",
                "        ;;\n",
                "    esac\n",
                "  fi\n",
            ));
        }
        if self.masked {
            s.push_str(concat!(
                "  if [ \"$rc\" -eq 0 ]; then\n",
                "    case \"$(systemctl is-enabled -- \"$tp_u\" 2>/dev/null)\" in\n",
                "      masked|masked-runtime) ;;\n",
                "      *)\n",
                "        systemctl mask -- \"$tp_u\"\n",
                "        rc=$?\n",
                "        if [ \"$rc\" -eq 0 ]; then changed=1; fi\n",
                "        ;;\n",
                "    esac\n",
                "  fi\n",
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
            ));
        }
        if self.started || self.stopped {
            s.push_str("  if [ \"$rc\" -eq 0 ] && [ \"$changed\" -eq 0 ] && [ \"$tp_skip\" -eq 1 ]; then skipped=1; fi\n");
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
    #[track_caller]
    pub fn enabled(&self) -> &Self {
        self.0.edit(|op| {
            if let OpSpec::Svc(s) = &mut op.spec {
                s.enabled = true;
            }
        });
        self
    }

    /// Want the unit disabled (`systemctl disable`): an enabled unit is
    /// disabled; a masked, static or missing one is left as it is.
    #[track_caller]
    pub fn disabled(&self) -> &Self {
        self.0.edit(|op| {
            if let OpSpec::Svc(s) = &mut op.spec {
                s.disabled = true;
            }
        });
        self
    }

    /// Want the unit masked (`systemctl mask`).
    #[track_caller]
    pub fn masked(&self) -> &Self {
        self.0.edit(|op| {
            if let OpSpec::Svc(s) = &mut op.spec {
                s.masked = true;
            }
        });
        self
    }

    /// Want the unit stopped (`systemctl stop`).
    #[track_caller]
    pub fn stopped(&self) -> &Self {
        self.0.edit(|op| {
            if let OpSpec::Svc(s) = &mut op.spec {
                s.stopped = true;
            }
        });
        self
    }

    /// Want the unit started (`systemctl start`).
    #[track_caller]
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
    /// A systemd unit: chain `.enabled()` / `.disabled()` / `.masked()` and
    /// `.started()` / `.stopped()`. Root only.
    #[track_caller]
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
/// nothing, or wants two states that exclude each other (enabled, disabled
/// and masked; started and stopped). Derived from the nodes, so a chain
/// split over statements is fine as long as it is finished by the time the
/// trace is read.
pub fn nothing_wanted(trace: &Trace) -> Vec<TraceError> {
    let mut out = Vec::new();
    for (i, n) in trace.nodes.iter().enumerate() {
        let crate::trace::Node::Op(op) = n else {
            continue;
        };
        let OpSpec::Svc(s) = &op.spec else {
            continue;
        };
        let mut errs = Vec::new();
        if !(s.enabled || s.disabled || s.masked || s.started || s.stopped) {
            errs.push(format!("svc({}): nothing wanted", s.unit));
        }
        let names = |v: &[(bool, &str)]| -> Vec<String> {
            v.iter()
                .filter(|(on, _)| *on)
                .map(|(_, n)| format!(".{n}()"))
                .collect()
        };
        for set in [
            names(&[
                (s.enabled, "enabled"),
                (s.disabled, "disabled"),
                (s.masked, "masked"),
            ]),
            names(&[(s.started, "started"), (s.stopped, "stopped")]),
        ] {
            if set.len() > 1 {
                errs.push(format!(
                    "svc({}): {} are exclusive",
                    s.unit,
                    set.join(" and ")
                ));
            }
        }
        for m in errs {
            let mut e = TraceError::new(m);
            e.site = None;
            out.push(trace.locate(crate::trace::NodeId(i), e));
        }
    }
    out
}

/// What one unit of a window wants, merged over the window's ops that name
/// the same key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Want {
    /// The first spelling, passed to `systemctl`.
    pub unit: String,
    pub key: String,
    pub enabled: bool,
    pub disabled: bool,
    pub masked: bool,
    pub started: bool,
    pub stopped: bool,
}

/// Merge specs by key, first-seen order.
pub fn wants<'a>(specs: impl IntoIterator<Item = &'a SvcSpec>) -> Vec<Want> {
    let mut out: Vec<Want> = Vec::new();
    for s in specs {
        let key = s.key_name();
        match out.iter_mut().find(|w| w.key == key) {
            Some(w) => {
                w.enabled |= s.enabled;
                w.disabled |= s.disabled;
                w.masked |= s.masked;
                w.started |= s.started;
                w.stopped |= s.stopped;
            }
            None => out.push(Want {
                unit: s.unit.clone(),
                key,
                enabled: s.enabled,
                disabled: s.disabled,
                masked: s.masked,
                started: s.started,
                stopped: s.stopped,
            }),
        }
    }
    out
}

/// A prepare probe, one round trip. Prints `systemctl yes|no` and `booted
/// yes|no`; then, booted, one `systemctl show -p
/// Id,UnitFileState,ActiveState,LoadState` for every unit (its blocks, in
/// argument order, are parsed by [`findings_after`]). Offline `show` fails,
/// and a booted `show` that answers nothing falls back to the per-unit
/// lines `enabled <state> <unit>` and (booted only) `active <state>
/// <unit>`; `-` is an empty answer.
pub fn probe(wants: &[Want]) -> String {
    let mut s = String::from(
        "tp_run=\"${TP_SVC_RUNDIR:-/run/systemd/system}\"\nif command -v systemctl >/dev/null 2>&1; then echo \"systemctl yes\"; else echo \"systemctl no\"; fi\nif [ -d \"$tp_run\" ]; then echo \"booted yes\"; else echo \"booted no\"; fi\nif command -v systemctl >/dev/null 2>&1; then\n  tp_o=\n",
    );
    let units: Vec<String> = wants.iter().map(|w| quote(&w.unit)).collect();
    s.push_str(&format!(
        "  if [ -d \"$tp_run\" ]; then tp_o=$(systemctl show -p Id,UnitFileState,ActiveState,LoadState -- {} 2>/dev/null); fi\n  if [ -n \"$tp_o\" ]; then\n    printf '%s\\n' \"$tp_o\"\n  else\n",
        units.join(" ")
    ));
    for w in wants {
        let q = quote(&w.unit);
        s.push_str(&format!(
            "    tp_s=$(systemctl is-enabled -- {q} 2>/dev/null | head -n 1); echo \"enabled ${{tp_s:--}} \"{q}\n"
        ));
        if w.started || w.stopped {
            s.push_str(&format!(
                "    if [ -d \"$tp_run\" ]; then tp_s=$(systemctl is-active -- {q} 2>/dev/null | head -n 1); echo \"active ${{tp_s:--}} \"{q}; fi\n"
            ));
        }
    }
    s.push_str("  fi\nfi\n");
    s
}

fn state<'a>(v: &'a [(String, String)], u: &str) -> Option<&'a str> {
    v.iter()
        .find(|(n, _)| n == u)
        .map(|(_, s)| s.as_str())
        .filter(|s| *s != "-")
}

/// The unit-file state, `enabled <state> <unit>` style, from one `show`
/// block: `UnitFileState`, else `masked` / `not-found` from `LoadState`.
fn block_enabled(unit_file: &str, load: &str) -> String {
    if !unit_file.is_empty() {
        unit_file.to_string()
    } else if load == "not-found" {
        "not-found".to_string()
    } else {
        "-".to_string()
    }
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
    let mut en: Vec<(String, String)> = Vec::new();
    let mut act: Vec<(String, String)> = Vec::new();
    // `show` blocks: property lines, separated by a blank line.
    let mut blocks: Vec<[String; 3]> = Vec::new();
    let mut cur: Option<[String; 3]> = None;
    for l in output.lines() {
        if l.is_empty() {
            blocks.extend(cur.take());
        } else if let Some(v) = l.strip_prefix("systemctl ") {
            systemctl = Some(v == "yes");
        } else if let Some(v) = l.strip_prefix("booted ") {
            booted = Some(v == "yes");
        } else if let Some(r) = l.strip_prefix("enabled ")
            && let Some((st, u)) = r.split_once(' ')
        {
            en.push((u.to_string(), st.to_string()));
        } else if let Some(r) = l.strip_prefix("active ")
            && let Some((st, u)) = r.split_once(' ')
        {
            act.push((u.to_string(), st.to_string()));
        } else if let Some((k, v)) = l.split_once('=') {
            let slot = match k {
                "UnitFileState" => 0,
                "ActiveState" => 1,
                "LoadState" => 2,
                "Id" => {
                    cur.get_or_insert_with(Default::default);
                    continue;
                }
                _ => continue,
            };
            cur.get_or_insert_with(Default::default)[slot] = v.to_string();
        }
    }
    blocks.extend(cur.take());
    // One block per unit, in the probe's argument order; any other count
    // is not trusted.
    if blocks.len() == wants.len() {
        for (w, [uf, active, load]) in wants.iter().zip(&blocks) {
            en.push((w.unit.clone(), block_enabled(uf, load)));
            act.push((
                w.unit.clone(),
                if active.is_empty() { "-" } else { active }.to_string(),
            ));
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
        let e = state(&en, &w.unit);
        if earlier && e == Some("not-found") {
            out.push((
                w.key.clone(),
                "unit not found yet: decided at run time, after this window's earlier ops"
                    .to_string(),
            ));
            continue;
        }
        let a = state(&act, &w.unit);
        let enabled_now = |s: &str| matches!(s, "enabled" | "enabled-runtime");
        // In the order the script runs them.
        let mut parts = Vec::new();
        if w.enabled
            && e.is_some_and(|s| !matches!(s, "enabled" | "enabled-runtime" | "static" | "alias"))
        {
            parts.push("will enable");
        }
        if w.stopped && booted && a == Some("active") {
            parts.push("will stop");
        }
        if w.disabled && e.is_some_and(enabled_now) {
            parts.push("will disable");
        }
        if w.masked && e.is_some_and(|s| !matches!(s, "masked" | "masked-runtime")) {
            parts.push("will mask");
        }
        if w.started && booted && a.is_some_and(|s| s != "active") {
            parts.push("will start");
        }
        if parts.is_empty() {
            continue;
        }
        let acts = parts
            .iter()
            .any(|p| matches!(*p, "will enable" | "will disable" | "will mask"));
        let mut detail = parts.join(", ");
        if acts && !booted {
            if w.started {
                detail.push_str("; no running systemd: start skipped");
            }
            if w.stopped {
                detail.push_str("; no running systemd: stop skipped");
            }
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
            enabled: true,
            started: true,
            ..SvcSpec::new("docker")
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
            enabled: true,
            started: true,
            ..SvcSpec::new("docker")
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
            enabled: true,
            started: true,
            ..SvcSpec::new("docker")
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

    #[test]
    fn findings_read_show_blocks_in_unit_order() {
        let w = wants(&[
            SvcSpec {
                masked: true,
                stopped: true,
                ..SvcSpec::new("a")
            },
            SvcSpec {
                disabled: true,
                ..SvcSpec::new("b")
            },
            SvcSpec {
                enabled: true,
                ..SvcSpec::new("tigervncserver@:1")
            },
        ]);
        let out = "systemctl yes\nbooted yes\n\
            Id=a.service\nLoadState=loaded\nActiveState=active\nUnitFileState=enabled\n\n\
            Id=b.service\nLoadState=masked\nActiveState=inactive\nUnitFileState=masked\n\n\
            Id=tigervncserver@:1.service\nLoadState=not-found\nActiveState=inactive\nUnitFileState=\n";
        assert_eq!(
            findings(&w, out),
            [
                ("a.service".into(), "will stop, will mask".into()),
                ("tigervncserver@:1.service".into(), "will enable".into()),
            ]
        );
        // a block count that does not match the units is not trusted
        assert!(findings(&w, "systemctl yes\nbooted yes\nId=a.service\n").is_empty());
    }

    #[test]
    fn exclusive_wants_are_trace_errors_only_per_op() {
        let w = wants(&[
            SvcSpec {
                enabled: true,
                ..SvcSpec::new("x")
            },
            SvcSpec {
                masked: true,
                ..SvcSpec::new("x")
            },
        ]);
        assert_eq!(w.len(), 1);
        assert!(w[0].enabled && w[0].masked);
    }
}
