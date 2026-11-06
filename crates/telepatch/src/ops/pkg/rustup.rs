//! `Source::Rustup`: one rust toolchain with its targets and components,
//! per user (`h.pkg(["stable"]).rustup()`; the pkg name is the toolchain).
//!
//! The op, in order, stopping at the first failure:
//!
//! 1. `rustup` absent: the installer (default `curl --proto '=https'
//!    --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y`) with
//!    `--default-toolchain <tc> --profile <p>` appended, run in a subshell
//!    under `pipefail`. rustup present but the toolchain missing (including
//!    "proxies, no toolchain"): `rustup toolchain install <tc> --profile
//!    <p>`.
//! 2. `Latest`, toolchain already there: `rustup check`; `rustup update
//!    <tc>` when it reports an update for `<tc>` or for rustup itself.
//!    `rustup check` exits 100 when updates exist; any other non-zero exit
//!    fails the op (G4-1).
//! 3. `.set_default()`: `rustup default <tc>` unless `rustup default`
//!    already names it.
//! 4. Missing targets: one `rustup target add --toolchain <tc> …`; missing
//!    components: one `rustup component add --toolchain <tc> …`.
//!
//! The installer puts rustup in `${CARGO_HOME:-$HOME/.cargo}/bin`, which
//! need not be on the op shell's PATH; the op prepends that directory
//! (when it exists) before probing and again after the installer.
//!
//! Matching rules, in awk (`tcm`): a listing line's first word names
//! `<name>` when it equals it or is `<name>-<triple>`, where a triple
//! is a known architecture plus two more `-` parts
//! (`x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`). So `nightly` does not match
//! `nightly-2026-10-01-x86_64-…` (`2026` is no architecture), `rust` does not match `rust-std-x86_64-…`, and
//! `1.94.0` does not match `1.94.0-beta.1-x86_64-…` (`beta.1` has a dot).
//! The same rule matches components (`rustfmt` against
//! `rustfmt-x86_64-unknown-linux-gnu`; `rust-src` has no suffix). Targets
//! are whole-line matches. The installed version is the first version
//! token of `rustup run <tc> rustc --version`.
//!
//! `changed` iff the installer or `toolchain install` ran, an update moved
//! the version, or a default, target or component was set.

use std::fmt::Write;

use crate::sh::quote;

use super::{VERSION_RE, Wanted};

pub const DEFAULT_INSTALLER: &str =
    "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y";

/// The options of a rustup op (set with the builders on the handle).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RustupSpec {
    pub targets: Vec<String>,
    pub components: Vec<String>,
    /// `None` is `default`.
    pub profile: Option<String>,
    pub set_default: bool,
    /// `None` is [`DEFAULT_INSTALLER`].
    pub installer: Option<String>,
}

impl RustupSpec {
    pub fn profile(&self) -> &str {
        self.profile.as_deref().unwrap_or("default")
    }

    pub fn installer(&self) -> &str {
        self.installer.as_deref().unwrap_or(DEFAULT_INSTALLER)
    }

    /// Add `v` to `list`, skipping duplicates.
    pub fn add(list: &mut Vec<String>, v: impl IntoIterator<Item = String>) {
        for x in v {
            if !list.contains(&x) {
                list.push(x);
            }
        }
    }
}

/// awk function `tcm(field, name)`: the matching rule above.
const AWK_TCM: &str = "function tcm(f, t) { return f == t || (index(f, t \"-\") == 1 && substr(f, length(t) + 2) ~ /^(x86_64|i[3-6]86|aarch64|arm[0-9a-z_]*|riscv[0-9a-z]*|powerpc[0-9a-z_]*|s390x|loongarch64|mips[0-9a-z_]*|sparc[0-9a-z_]*|thumb[0-9a-z_]*|wasm[0-9]+)-[a-z0-9_]+-[a-z]/) }";

/// Shell functions shared by the op and the probe. `tp_tc` is the
/// toolchain.
fn prelude() -> String {
    let tcm = AWK_TCM;
    format!(
        "tp_path() {{\n  tp_cb=${{CARGO_HOME:-$HOME/.cargo}}/bin\n  if [ -d \"$tp_cb\" ]; then case \":$PATH:\" in *\":$tp_cb:\"*) ;; *) PATH=$tp_cb:$PATH;; esac; fi\n}}\n\
         tp_match() {{ awk -v t=\"$1\" '{tcm} {{ if (tcm($1, t)) m=1 }} END {{ exit m ? 0 : 1 }}'; }}\n\
         tp_ver() {{ rustup run \"$tp_tc\" rustc --version 2>/dev/null | grep -oE '{VERSION_RE}' | head -n1; }}\n\
         tp_has_tc() {{ rustup toolchain list 2>/dev/null | tp_match \"$tp_tc\"; }}\n\
         tp_updates() {{ awk -v t=\"$tp_tc\" '{tcm} /[Uu]pdate available/ {{ if ($1 == \"rustup\") print \"self\"; else if (tcm($1, t)) {{ v=$0; sub(/.*-> */, \"\", v); split(v, a, \" \"); print \"tc \" a[1] }} }}'; }}\n\
         tp_check() {{ tp_chk=$(rustup check 2>&1); tp_crc=$?; [ \"$tp_crc\" -eq 0 ] || [ \"$tp_crc\" -eq 100 ]; }}\n"
    )
}

fn words(v: &[String]) -> String {
    v.iter().map(|x| quote(x)).collect::<Vec<_>>().join(" ")
}

/// The op body (sets `rc`, `changed`, `installed`).
pub fn render(tc: &str, wanted: &Wanted, spec: &RustupSpec) -> String {
    let mut o = format!(
        "rc=0; changed=0; installed=()\ntp_tc={}\ntp_prof={}\n",
        quote(tc),
        quote(spec.profile())
    );
    o.push_str(&prelude());
    o.push_str("tp_path\ntp_fresh=0\n");
    o.push_str(&format!(
        "if ! command -v rustup >/dev/null 2>&1; then\n  ( set -o pipefail; {} --default-toolchain \"$tp_tc\" --profile \"$tp_prof\" )\n  rc=$?\n  tp_path\n  if [ \"$rc\" -eq 0 ] && ! command -v rustup >/dev/null 2>&1; then echo 'telepatch: rustup not found after the installer' >&2; rc=1; fi\n  if [ \"$rc\" -eq 0 ]; then tp_fresh=1; fi\n\
         elif ! tp_has_tc; then\n  rustup toolchain install \"$tp_tc\" --profile \"$tp_prof\"\n  rc=$?\n  if [ \"$rc\" -eq 0 ]; then tp_fresh=1; fi\nfi\n\
         if [ \"$tp_fresh\" -eq 1 ]; then\n  if tp_has_tc; then changed=1; installed+=(\"$tp_tc\"); else echo \"telepatch: rustup toolchain $tp_tc: not installed afterwards\" >&2; rc=1; fi\nfi\n",
        spec.installer()
    ));
    if *wanted == Wanted::Latest {
        o.push_str(
            "if [ \"$rc\" -eq 0 ] && [ \"$tp_fresh\" -eq 0 ]; then\n  tp_v0=$(tp_ver)\n  if ! tp_check; then\n    echo \"telepatch: rustup check failed (rc $tp_crc): $tp_chk\" >&2; rc=1\n  elif [ -n \"$(printf '%s\\n' \"$tp_chk\" | tp_updates)\" ]; then\n    rustup update \"$tp_tc\"\n    rc=$?\n    if [ \"$(tp_ver)\" != \"$tp_v0\" ]; then changed=1; installed+=(\"$tp_tc\"); fi\n  fi\nfi\n",
        );
    }
    if spec.set_default {
        o.push_str(
            "if [ \"$rc\" -eq 0 ] && ! rustup default 2>/dev/null | tp_match \"$tp_tc\"; then\n  rustup default \"$tp_tc\"\n  rc=$?\n  if [ \"$rc\" -eq 0 ]; then changed=1; fi\nfi\n",
        );
    }
    if !spec.targets.is_empty() {
        o.push_str(&format!(
            "if [ \"$rc\" -eq 0 ]; then\n  tp_have=$(rustup target list --installed --toolchain \"$tp_tc\" 2>/dev/null)\n  tp_miss=()\n  for tp_x in {}; do\n    printf '%s\\n' \"$tp_have\" | grep -qxF -- \"$tp_x\" || tp_miss+=(\"$tp_x\")\n  done\n  if [ \"${{#tp_miss[@]}}\" -gt 0 ]; then\n    rustup target add --toolchain \"$tp_tc\" \"${{tp_miss[@]}}\"\n    rc=$?\n    if [ \"$rc\" -eq 0 ]; then\n      changed=1\n      for tp_x in \"${{tp_miss[@]}}\"; do installed+=(\"$tp_tc/target/$tp_x\"); done\n    fi\n  fi\nfi\n",
            words(&spec.targets)
        ));
    }
    if !spec.components.is_empty() {
        o.push_str(&format!(
            "if [ \"$rc\" -eq 0 ]; then\n  tp_have=$(rustup component list --installed --toolchain \"$tp_tc\" 2>/dev/null)\n  tp_miss=()\n  for tp_x in {}; do\n    printf '%s\\n' \"$tp_have\" | tp_match \"$tp_x\" || tp_miss+=(\"$tp_x\")\n  done\n  if [ \"${{#tp_miss[@]}}\" -gt 0 ]; then\n    rustup component add --toolchain \"$tp_tc\" \"${{tp_miss[@]}}\"\n    rc=$?\n    if [ \"$rc\" -eq 0 ]; then\n      changed=1\n      for tp_x in \"${{tp_miss[@]}}\"; do installed+=(\"$tp_tc/component/$tp_x\"); done\n    fi\n  fi\nfi\n",
            words(&spec.components)
        ));
    }
    o
}

/// The prepare probe. Prints `rustup absent`, or `rustup ok` then
/// `toolchain absent` or `toolchain <version>` and, for an installed
/// toolchain, `default missing` (when `.set_default()`), `target missing
/// <t>`, `component missing <c>` and, only for `Latest`, `update tc
/// <version>` / `update self` (or `check failed`). Nothing is printed when
/// the probe itself cannot run.
pub fn probe(tc: &str, wanted: &Wanted, spec: &RustupSpec) -> String {
    let mut o = format!("tp_tc={}\n", quote(tc));
    o.push_str(&prelude());
    o.push_str("tp_path\nif ! command -v rustup >/dev/null 2>&1; then echo 'rustup absent'; else\n  echo 'rustup ok'\n  if ! tp_has_tc; then echo 'toolchain absent'; else\n    echo \"toolchain $(tp_ver)\"\n");
    if spec.set_default {
        o.push_str(
            "    rustup default 2>/dev/null | tp_match \"$tp_tc\" || echo 'default missing'\n",
        );
    }
    if !spec.targets.is_empty() {
        let _ = write!(
            o,
            "    tp_have=$(rustup target list --installed --toolchain \"$tp_tc\" 2>/dev/null)\n    for tp_x in {}; do\n      printf '%s\\n' \"$tp_have\" | grep -qxF -- \"$tp_x\" || echo \"target missing $tp_x\"\n    done\n",
            words(&spec.targets)
        );
    }
    if !spec.components.is_empty() {
        let _ = write!(
            o,
            "    tp_have=$(rustup component list --installed --toolchain \"$tp_tc\" 2>/dev/null)\n    for tp_x in {}; do\n      printf '%s\\n' \"$tp_have\" | tp_match \"$tp_x\" || echo \"component missing $tp_x\"\n    done\n",
            words(&spec.components)
        );
    }
    if *wanted == Wanted::Latest {
        o.push_str(
            "    if tp_check; then printf '%s\\n' \"$tp_chk\" | tp_updates | sed 's/^/update /'; else echo 'check failed'; fi\n",
        );
    }
    o.push_str("  fi\nfi\n");
    o
}

/// What the prepare probe found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RustupProbe {
    /// `None` when the probe printed nothing recognisable.
    pub state: Option<State>,
    pub default_missing: bool,
    pub targets_missing: Vec<String>,
    pub components_missing: Vec<String>,
    /// `rustup check` reports an update for the toolchain; the new version
    /// (empty when the line had none).
    pub update: Option<String>,
    /// `rustup check` reports an update for rustup itself.
    pub self_update: bool,
    pub check_failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    NoRustup,
    NoToolchain,
    /// The version (empty when `rustc` printed none).
    Installed(String),
}

/// Parse [`probe`]'s output.
pub fn parse_probe(out: &str) -> RustupProbe {
    let mut p = RustupProbe::default();
    for l in out.lines() {
        let l = l.trim_end();
        if l == "rustup absent" {
            p.state = Some(State::NoRustup);
        } else if l == "toolchain absent" {
            p.state = Some(State::NoToolchain);
        } else if l == "toolchain" {
            p.state = Some(State::Installed(String::new()));
        } else if let Some(v) = l.strip_prefix("toolchain ") {
            p.state = Some(State::Installed(v.trim().to_string()));
        } else if l == "default missing" {
            p.default_missing = true;
        } else if let Some(t) = l.strip_prefix("target missing ") {
            p.targets_missing.push(t.to_string());
        } else if let Some(c) = l.strip_prefix("component missing ") {
            p.components_missing.push(c.to_string());
        } else if l == "update self" {
            p.self_update = true;
        } else if let Some(v) = l.strip_prefix("update tc") {
            p.update = Some(v.trim().to_string());
        } else if l == "check failed" {
            p.check_failed = true;
        }
    }
    p
}

/// `(installed, wanted)` as the report prints them, or `None` when
/// nothing will change. `installed` is `None` when the toolchain is absent
/// and `"unknown"` when the probe failed or found no version.
///
/// `wanted` is `present` (also for an absent toolchain under `Latest`,
/// whose version cannot be looked up before rustup exists), or
/// `<v> (latest)` (`unknown (latest)` when the check failed), followed by
/// ` (+target a, b)`, ` (+component c)`, ` (+default)` and, when only
/// rustup itself updates, ` (+rustup update)`. An installed, current
/// toolchain with only extras to add reads `1.99.1 -> present (+target x)`
/// (`1.99.1 (latest)` under `Latest`).
pub fn will_change(
    spec: &RustupSpec,
    probed: &RustupProbe,
    wanted: &Wanted,
) -> Option<(Option<String>, String)> {
    let latest = *wanted == Wanted::Latest;
    let Some(state) = &probed.state else {
        let mut w = if latest {
            "unknown (latest)"
        } else {
            "present"
        }
        .to_string();
        suffixes(&mut w, &spec.targets, &spec.components, spec.set_default);
        return Some((Some("unknown".to_string()), w));
    };
    let State::Installed(v) = state else {
        // Absent: everything asked for is added.
        let mut w = "present".to_string();
        suffixes(&mut w, &spec.targets, &spec.components, spec.set_default);
        return Some((None, w));
    };
    let have = if v.is_empty() { "unknown" } else { v };
    let mut want = if !latest {
        "present".to_string()
    } else if probed.check_failed {
        "unknown (latest)".to_string()
    } else if let Some(new) = &probed.update {
        format!("{} (latest)", if new.is_empty() { "unknown" } else { new })
    } else {
        format!("{have} (latest)")
    };
    let moved = latest && (probed.update.is_some() || probed.check_failed);
    let mut tail = String::new();
    suffixes(
        &mut tail,
        &probed.targets_missing,
        &probed.components_missing,
        probed.default_missing,
    );
    if latest && probed.self_update && probed.update.is_none() {
        tail.push_str(" (+rustup update)");
    }
    if !moved && tail.is_empty() {
        return None;
    }
    want.push_str(&tail);
    Some((Some(have.to_string()), want))
}

fn suffixes(o: &mut String, targets: &[String], components: &[String], default: bool) {
    if !targets.is_empty() {
        let _ = write!(o, " (+target {})", targets.join(", "));
    }
    if !components.is_empty() {
        let _ = write!(o, " (+component {})", components.join(", "));
    }
    if default {
        o.push_str(" (+default)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str, stdin: &str) -> (bool, String) {
        let mut c = std::process::Command::new("bash")
            .arg("-c")
            .arg(script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        c.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        let o = c.wait_with_output().unwrap();
        (o.status.success(), String::from_utf8(o.stdout).unwrap())
    }

    /// Does `name` match a line of `listing` by the shell's `tp_match`?
    fn matches(name: &str, listing: &str) -> bool {
        sh(&format!("{}tp_match {}", prelude(), quote(name)), listing).0
    }

    #[test]
    fn toolchain_names_match_the_listing() {
        let list = "stable-x86_64-unknown-linux-gnu (default)\nnightly-2026-10-01-x86_64-unknown-linux-gnu\n1.94.0-beta.1-x86_64-unknown-linux-gnu\nmine\n";
        assert!(matches("stable", list));
        assert!(matches("mine", list));
        assert!(matches("nightly-2026-10-01", list));
        // `nightly` is not `nightly-2026-10-01-…`; `1.94.0` is not a beta.
        assert!(!matches("nightly", list));
        assert!(!matches("1.94.0", list));
        assert!(matches("1.94.0-beta.1", list));
        assert!(!matches("stable", "no installed toolchains\n"));
        assert!(!matches("stable", ""));
        assert!(matches("stable", "stable-aarch64-apple-darwin\n"));
    }

    #[test]
    fn components_match_with_or_without_a_triple() {
        let list = "rust-src\nrustfmt-x86_64-unknown-linux-gnu\nrust-std-x86_64-unknown-linux-gnu\nrust-analyzer-x86_64-unknown-linux-gnu\n";
        for c in ["rust-src", "rustfmt", "rust-std", "rust-analyzer"] {
            assert!(matches(c, list), "{c}");
        }
        for c in ["rust", "clippy", "rustc"] {
            assert!(!matches(c, list), "{c}");
        }
    }

    #[test]
    fn updates_are_read_from_check() {
        let chk = "stable-x86_64-unknown-linux-gnu - Update available : 1.99.1 (abc 2026-01-01) -> 1.100.0 (def 2026-02-01)\nnightly-x86_64-unknown-linux-gnu - Up to date : 1.101.0\nrustup - Update available : 1.27.1 -> 1.28.0\n";
        // rustup 1.29 prints lower case: `- update available: a -> b` (observed 2026-10-31).
        let chk129 = "stable-x86_64-unknown-linux-gnu - update available: 1.98.1 (48a229cea 2026-09-01) -> 1.99.1 (58cf8f9c3 2026-10-13)\nrustup - update available: 1.29.1 -> 1.29.2\n";
        let (_, out) = sh(&format!("{}tp_tc=stable\ntp_updates", prelude()), chk129);
        assert_eq!(out, "tc 1.99.1\nself\n");
        for (tc, want) in [
            ("stable", "tc 1.100.0\nself\n"),
            ("nightly", "self\n"),
            ("beta", "self\n"),
        ] {
            let (_, out) = sh(
                &format!("{}tp_tc={}\ntp_updates", prelude(), quote(tc)),
                chk,
            );
            assert_eq!(out, want, "{tc}");
        }
        let (_, out) = sh(
            &format!("{}tp_tc=stable\ntp_updates", prelude()),
            "stable-x86_64-unknown-linux-gnu - Up to date : 1.99.1 (abc 2026-01-01)\nrustup - Up to date : 1.28.0\n",
        );
        assert_eq!(out, "");
    }

    #[test]
    fn rendered_scripts_are_valid_bash() {
        let spec = RustupSpec {
            targets: vec!["a".into()],
            components: vec!["b".into()],
            set_default: true,
            ..Default::default()
        };
        for w in [Wanted::Present, Wanted::Latest] {
            for s in [render("stable", &w, &spec), probe("stable", &w, &spec)] {
                let st = std::process::Command::new("bash")
                    .args(["-n", "-c", &s])
                    .status()
                    .unwrap();
                assert!(st.success(), "{s}");
            }
        }
    }

    #[test]
    fn probe_output_parses() {
        let p = parse_probe(
            "rustup ok\ntoolchain 1.99.1\ndefault missing\ntarget missing t1\ncomponent missing c\nupdate tc 1.100.0\nupdate self\n",
        );
        assert_eq!(p.state, Some(State::Installed("1.99.1".into())));
        assert!(p.default_missing && p.self_update);
        assert_eq!(p.targets_missing, ["t1"]);
        assert_eq!(p.components_missing, ["c"]);
        assert_eq!(p.update.as_deref(), Some("1.100.0"));
        assert_eq!(parse_probe("").state, None);
        assert_eq!(parse_probe("rustup absent\n").state, Some(State::NoRustup));
    }

    fn spec(targets: &[&str], components: &[&str], default: bool) -> RustupSpec {
        RustupSpec {
            targets: targets.iter().map(|s| s.to_string()).collect(),
            components: components.iter().map(|s| s.to_string()).collect(),
            set_default: default,
            ..Default::default()
        }
    }

    fn wc(spec: &RustupSpec, out: &str, w: &Wanted) -> Option<(Option<String>, String)> {
        will_change(spec, &parse_probe(out), w)
    }

    #[test]
    fn will_change_lines() {
        let s = spec(&["m"], &[], false);
        let some = |i: Option<&str>, w: &str| Some((i.map(str::to_string), w.to_string()));
        // Absent: everything asked for is added.
        assert_eq!(
            wc(&s, "rustup absent\n", &Wanted::Present),
            some(None, "present (+target m)")
        );
        assert_eq!(
            wc(
                &spec(&[], &["c"], true),
                "rustup ok\ntoolchain absent\n",
                &Wanted::Latest
            ),
            some(None, "present (+component c) (+default)")
        );
        // Installed and current: nothing, or only the missing extras.
        assert_eq!(
            wc(&s, "rustup ok\ntoolchain 1.99.1\n", &Wanted::Present),
            None
        );
        assert_eq!(
            wc(
                &s,
                "rustup ok\ntoolchain 1.99.1\ntarget missing m\n",
                &Wanted::Present
            ),
            some(Some("1.99.1"), "present (+target m)")
        );
        assert_eq!(
            wc(&s, "rustup ok\ntoolchain 1.99.1\n", &Wanted::Latest),
            None
        );
        assert_eq!(
            wc(
                &s,
                "rustup ok\ntoolchain 1.99.1\nupdate tc 1.100.0\n",
                &Wanted::Latest
            ),
            some(Some("1.99.1"), "1.100.0 (latest)")
        );
        assert_eq!(
            wc(
                &s,
                "rustup ok\ntoolchain 1.99.1\nupdate self\n",
                &Wanted::Latest
            ),
            some(Some("1.99.1"), "1.99.1 (latest) (+rustup update)")
        );
        assert_eq!(
            wc(
                &s,
                "rustup ok\ntoolchain 1.99.1\ncheck failed\n",
                &Wanted::Latest
            ),
            some(Some("1.99.1"), "unknown (latest)")
        );
        // The probe failed: unknown, with everything asked for.
        assert_eq!(
            wc(&s, "", &Wanted::Present),
            some(Some("unknown"), "present (+target m)")
        );
        assert_eq!(
            wc(&s, "", &Wanted::Latest),
            some(Some("unknown"), "unknown (latest) (+target m)")
        );
    }
}
