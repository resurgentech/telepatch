//! `Source::Mise`: tools installed by `mise` into its system layer
//! (`/usr/local/share/mise`, config `/etc/mise/config.toml`), root only, so
//! every user reaches them through the shims directory
//! `/usr/local/share/mise/shims` on `PATH`.
//!
//! The op runs as root and never calls `sudo`. Each name gets an entry in
//! the `[tools]` table of the system config (`"latest"` for `Present` and
//! `Latest`, the version for `Pinned`), set with `mise config set --system`
//! only when it differs, so a settled host leaves the file (mtime too) and
//! its other entries alone. The installed set is `mise ls --json
//! --installed` filtered to the system data dir (root's own
//! `~/.local/share/mise` is not read), which needs no network. `Present`
//! installs the missing names with one `mise install --system <names>`;
//! `Pinned(v)` is one name and installs `<n>@<v>` when `v` is not among its
//! installed versions; `Latest` asks the host's `mise latest <n>` first (a
//! failed lookup fails the op before the config or anything else changes)
//! and installs `<n>@<latest>` when that version is not installed. After an
//! install `mise reshim --system` refreshes the system shims (`install
//! --system` alone writes them to root's own dir) and the installs are read
//! again: a name still missing, or a pin or latest not met, fails the op.
//! `mise` not on PATH (nor at `/usr/local/bin/mise`) fails the op with
//! "mise not found".
//!
//! `MISE_SYSTEM_CONFIG_DIR` and `MISE_SYSTEM_DATA_DIR` are read, never
//! exported; they are mise's own overrides and default to `/etc/mise` and
//! `/usr/local/share/mise` (tests point them at a temp dir).

use std::collections::BTreeMap;

use crate::sh::quote;

use super::{Probed, Wanted, will_change};

/// Shell: find mise (`tp_mise`, empty when absent) and the system dirs.
const FIND: &str = r#"tp_cfgdir=${MISE_SYSTEM_CONFIG_DIR:-/etc/mise}; tp_data=${MISE_SYSTEM_DATA_DIR:-/usr/local/share/mise}
tp_mise=$(command -v mise 2>/dev/null) || tp_mise=
[ -n "$tp_mise" ] || { [ -x /usr/local/bin/mise ] && tp_mise=/usr/local/bin/mise; }
"#;

/// `awk` over `mise ls --json --installed`: `<tool> <version>` for every
/// install whose path is under `pre` (the system data dir's `installs/`).
/// Tool keys are the two-space-indented `"name": [` lines.
const LIST_AWK: &str = r#"/^  "[^"]+": \[/ { t=$0; sub(/^  "/, "", t); sub(/": \[.*/, "", t); next }
/"version":/ { v=$0; sub(/.*"version": "/, "", v); sub(/".*/, "", v) }
/"install_path":/ { p=$0; sub(/.*"install_path": "/, "", p); sub(/".*/, "", p); if (index(p, pre) == 1) print t, v }"#;

fn list_fn() -> String {
    format!("tp_list() {{ awk -v pre=\"$tp_data/installs/\" '{LIST_AWK}'; }}\n")
}

/// The op body (sets `rc`, `changed`, `installed`).
pub fn render(names: &[String], wanted: &Wanted) -> String {
    let want: Vec<String> = names.iter().map(|n| quote(n)).collect();
    let mut o = format!(
        "rc=0; changed=0; installed=()\ntp_want=({})\ndeclare -A tp_have=() tp_before=() tp_spec=() tp_wantv=()\ntp_todo=(); tp_args=()\n",
        want.join(" ")
    );
    o.push_str(FIND);
    o.push_str(&list_fn());
    o.push_str(
        "tp_read() { local n v; tp_have=(); while read -r n v; do tp_have[$n]+=\" $v\"; done < <(\"$tp_mise\" ls --json --installed 2>/dev/null | tp_list); }\n\
         tp_has() { case \"${tp_have[$1]-} \" in *\" $2 \"*) return 0;; esac; return 1; }\n\
         if [ -z \"$tp_mise\" ]; then echo 'telepatch: mise not found' >&2; rc=127; fi\n\
         if [ \"$rc\" -eq 0 ]; then\n  tp_read\n  for tp_k in \"${!tp_have[@]}\"; do tp_before[$tp_k]=${tp_have[$tp_k]}; done\n",
    );
    match wanted {
        Wanted::Present => o.push_str(
            "  for tp_n in \"${tp_want[@]}\"; do\n    tp_spec[$tp_n]=latest\n    [ -n \"${tp_have[$tp_n]-}\" ] || { tp_todo+=(\"$tp_n\"); tp_args+=(\"$tp_n\"); }\n  done\n",
        ),
        Wanted::Pinned(v) => o.push_str(&format!(
            "  tp_pin={}\n  for tp_n in \"${{tp_want[@]}}\"; do\n    tp_spec[$tp_n]=$tp_pin; tp_wantv[$tp_n]=$tp_pin\n    tp_has \"$tp_n\" \"$tp_pin\" || {{ tp_todo+=(\"$tp_n\"); tp_args+=(\"$tp_n@$tp_pin\"); }}\n  done\n",
            quote(v)
        )),
        Wanted::Latest => o.push_str(
            "  for tp_n in \"${tp_want[@]}\"; do\n    tp_l=$(\"$tp_mise\" latest \"$tp_n\" 2>/dev/null) || tp_l=\n    tp_l=${tp_l%%$'\\n'*}\n    if [ -z \"$tp_l\" ]; then echo \"telepatch: mise $tp_n: could not resolve the latest version\" >&2; rc=1; continue; fi\n    tp_spec[$tp_n]=latest; tp_wantv[$tp_n]=$tp_l\n    tp_has \"$tp_n\" \"$tp_l\" || { tp_todo+=(\"$tp_n\"); tp_args+=(\"$tp_n@$tp_l\"); }\n  done\n",
        ),
    }
    o.push_str("fi\n");
    // The config entries, only where they differ.
    o.push_str(
        "if [ \"$rc\" -eq 0 ]; then\n  mkdir -p \"$tp_cfgdir\" && { [ -e \"$tp_cfgdir/config.toml\" ] || : > \"$tp_cfgdir/config.toml\"; } || rc=1\n  for tp_n in \"${tp_want[@]}\"; do\n    [ \"$rc\" -eq 0 ] || break\n    tp_cur=$(\"$tp_mise\" config get --system \"tools.$tp_n\" 2>/dev/null) || tp_cur=\n    if [ \"$tp_cur\" != \"${tp_spec[$tp_n]}\" ]; then\n      if \"$tp_mise\" config set --system \"tools.$tp_n\" \"${tp_spec[$tp_n]}\"; then changed=1; else rc=1; fi\n    fi\n  done\nfi\n",
    );
    // Install, shim, read again, verify.
    o.push_str(
        "if [ \"$rc\" -eq 0 ] && [ \"${#tp_todo[@]}\" -gt 0 ]; then\n  \"$tp_mise\" install --system \"${tp_args[@]}\"\n  rc=$?\n  if [ \"$rc\" -eq 0 ]; then \"$tp_mise\" reshim --system; rc=$?; fi\n  tp_read\n  for tp_n in \"${tp_want[@]}\"; do\n    if [ \"${tp_have[$tp_n]-}\" != \"${tp_before[$tp_n]-}\" ]; then changed=1; installed+=(\"$tp_n\"); fi\n  done\n  if [ \"$rc\" -eq 0 ]; then\n    for tp_n in \"${tp_todo[@]}\"; do\n      tp_wv=${tp_wantv[$tp_n]-}\n      if [ -n \"$tp_wv\" ]; then tp_has \"$tp_n\" \"$tp_wv\" && continue; else [ -n \"${tp_have[$tp_n]-}\" ] && continue; fi\n      tp_got=${tp_have[$tp_n]-}; tp_got=${tp_got# }\n      echo \"telepatch: mise $tp_n: installed ${tp_got:-absent}, wanted ${tp_wv:-present}\" >&2; rc=1\n    done\n  fi\nfi\n",
    );
    o
}

/// The prepare probe. With `mise` present and listing: `mise ok`, then
/// `installed <name> <version>` per wanted name that has a system install
/// (the pinned version when installed, else the highest), then, only when
/// `Latest`, `latest <name> <version>` per name `mise latest` resolved.
/// Nothing is printed when `mise` is missing or fails. `Present` makes no
/// network call.
pub fn probe(names: &[String], wanted: &Wanted) -> String {
    let want: Vec<String> = names.iter().map(|n| quote(n)).collect();
    let pin = match wanted {
        Wanted::Pinned(v) => quote(v),
        _ => "''".to_string(),
    };
    let mut o = String::new();
    o.push_str(FIND);
    o.push_str(&list_fn());
    o.push_str(&format!(
        "if [ -n \"$tp_mise\" ] && tp_j=$(\"$tp_mise\" ls --json --installed 2>/dev/null); then\n  tp_l=$(printf '%s\\n' \"$tp_j\" | tp_list)\n  echo 'mise ok'\n  tp_pin={pin}\n  for tp_n in {}; do\n    tp_vs=$(printf '%s\\n' \"$tp_l\" | awk -v n=\"$tp_n\" '$1==n {{print $2}}' | sort -V)\n    [ -n \"$tp_vs\" ] || continue\n    if [ -n \"$tp_pin\" ] && printf '%s\\n' \"$tp_vs\" | grep -qxF -- \"$tp_pin\"; then tp_v=$tp_pin; else tp_v=$(printf '%s\\n' \"$tp_vs\" | tail -n 1); fi\n    echo \"installed $tp_n $tp_v\"\n  done\n",
        want.join(" ")
    ));
    if *wanted == Wanted::Latest {
        o.push_str(&format!(
            "  for tp_n in {}; do\n    tp_x=$(\"$tp_mise\" latest \"$tp_n\" 2>/dev/null | head -n 1) && [ -n \"$tp_x\" ] && echo \"latest $tp_n $tp_x\"\n  done\n",
            want.join(" ")
        ));
    }
    o.push_str("fi\n");
    o
}

/// What the prepare probe found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MiseProbe {
    /// The installed (system) tools and versions; `None` when the probe
    /// could not list them (no `mise`, or the probe failed).
    pub installed: Option<BTreeMap<String, String>>,
    /// The latest version per name `mise latest` resolved.
    pub latest: BTreeMap<String, String>,
}

/// Parse [`probe`]'s output.
pub fn parse_probe(out: &str) -> MiseProbe {
    let mut p = MiseProbe::default();
    for l in out.lines() {
        if l == "mise ok" {
            p.installed.get_or_insert_with(BTreeMap::new);
        } else if let Some(r) = l.strip_prefix("installed ") {
            if let Some((n, v)) = r.split_once(' ') {
                p.installed
                    .get_or_insert_with(BTreeMap::new)
                    .insert(n.to_string(), v.trim().to_string());
            }
        } else if let Some(r) = l.strip_prefix("latest ")
            && let Some((n, v)) = r.split_once(' ')
            && !v.trim().is_empty()
        {
            p.latest.insert(n.to_string(), v.trim().to_string());
        }
    }
    p
}

/// `(name, installed, wanted)` per name that will change, in `names` order
/// (see [`will_change`] for the two strings).
pub fn will_changes(
    names: &[String],
    probed: &MiseProbe,
    wanted: &Wanted,
) -> Vec<(String, Option<String>, String)> {
    names
        .iter()
        .filter_map(|n| {
            let have = match &probed.installed {
                None => Probed::Unknown,
                Some(m) => match m.get(n) {
                    None => Probed::Absent,
                    Some(v) => Probed::Installed(v.clone()),
                },
            };
            will_change(&have, probed.latest.get(n).map(String::as_str), wanted)
                .map(|(i, w)| (n.clone(), i, w))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LS: &str = r#"{
  "fd": [
    {
      "version": "10.5.0",
      "requested_version": "latest",
      "install_path": "/root/.local/share/mise/installs/fd/10.5.0",
      "installed": true,
      "active": false
    }
  ],
  "just": [
    {
      "version": "1.36.0",
      "install_path": "/usr/local/share/mise/installs/just/1.36.0",
      "installed": true,
      "active": false
    },
    {
      "version": "1.59.0",
      "requested_version": "latest",
      "install_path": "/usr/local/share/mise/installs/just/1.59.0",
      "source": {
        "type": "mise.toml",
        "path": "/etc/mise/config.toml"
      },
      "installed": true,
      "active": false
    }
  ]
}
"#;

    /// The shell `awk` keeps system installs only, one line per version.
    #[test]
    fn list_awk_reads_system_installs_only() {
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!(
                "printf '%s' \"$1\" | awk -v pre=/usr/local/share/mise/installs/ '{LIST_AWK}'"
            ))
            .arg("bash")
            .arg(LS)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            "just 1.36.0\njust 1.59.0\n"
        );
    }

    #[test]
    fn probe_output_parses() {
        let p = parse_probe("mise ok\ninstalled a 1.0.0\ninstalled b 2.0.0\nlatest a 1.1.0\n");
        assert_eq!(p.installed.as_ref().unwrap().len(), 2);
        assert_eq!(p.latest.get("a").map(String::as_str), Some("1.1.0"));
        assert_eq!(parse_probe("mise ok\n").installed, Some(BTreeMap::new()));
        assert_eq!(parse_probe("").installed, None);
    }

    #[test]
    fn will_changes_per_name() {
        let names: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let p = parse_probe(
            "mise ok\ninstalled a 1.0.0\ninstalled b 1.0.0\nlatest a 1.0.0\nlatest b 2.0.0\n",
        );
        assert_eq!(
            will_changes(&names, &p, &Wanted::Present),
            [("c".to_string(), None, "present".to_string())]
        );
        assert_eq!(will_changes(&names, &p, &Wanted::Latest).len(), 2);
        assert_eq!(
            will_changes(&names[..1], &MiseProbe::default(), &Wanted::Present),
            [(
                "a".to_string(),
                Some("unknown".to_string()),
                "present".to_string()
            )]
        );
    }
}
