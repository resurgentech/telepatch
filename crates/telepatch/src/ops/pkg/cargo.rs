//! `Source::Cargo`: crates installed with `cargo install`, per user.
//!
//! The installed set comes from one `cargo install --list` (lines
//! `<name> v<ver>:`, each followed by indented binary names; sources that
//! print `(path)` after the version still count). `Present` installs the
//! missing names with one `cargo install <missing…>` (no `--locked`, as the
//! YAML does; A27). `Pinned(v)` is one name: `cargo install <n> --version
//! <v>` when the installed version differs. `Latest` asks the host's own
//! `cargo search <n> --limit 1` per name (`<n> = "<v>"`) and installs the
//! names whose installed version differs. After an install the list is read
//! again: a name still missing, or a pin or latest not met, fails the op.
//! `cargo` not on PATH fails the op with "cargo not found".

use std::collections::BTreeMap;

use crate::sh::quote;

use super::{Probed, Wanted, will_change};

/// `sed` turning `cargo install --list` into `<name> <version>` lines.
/// Header lines only (`^name vX…:$`); the indented binary lines never match.
const LIST_SED: &str = r"s/^\([^ ][^ ]*\) v\([^ :][^ :]*\)[^:]*:$/\1 \2/p";

/// `awk` reading `cargo search`: the version of exactly `n`.
const SEARCH_AWK: &str = r#"$1==n && $2=="=" {gsub(/"/, "", $3); print $3; exit}"#;

/// The op body (sets `rc`, `changed`, `installed`).
pub fn render(names: &[String], wanted: &Wanted) -> String {
    let want: Vec<String> = names.iter().map(|n| quote(n)).collect();
    let mut o = format!(
        "rc=0; changed=0; installed=()\ntp_want=({})\ndeclare -A tp_before=() tp_after=() tp_wantv=()\ntp_todo=()\ntp_list() {{ cargo install --list 2>/dev/null | sed -n '{LIST_SED}'; }}\n",
        want.join(" ")
    );
    o.push_str(
        "if ! command -v cargo >/dev/null 2>&1; then echo 'telepatch: cargo not found' >&2; rc=127; fi\n\
         if [ \"$rc\" -eq 0 ]; then\n  \
         while read -r tp_n tp_v; do tp_before[$tp_n]=$tp_v; done < <(tp_list)\n",
    );
    let install = match wanted {
        Wanted::Present => {
            o.push_str(
                "  for tp_n in \"${tp_want[@]}\"; do\n    [ -n \"${tp_before[$tp_n]+x}\" ] || tp_todo+=(\"$tp_n\")\n  done\n",
            );
            "cargo install \"${tp_todo[@]}\"".to_string()
        }
        Wanted::Pinned(v) => {
            o.push_str(&format!(
                "  tp_pin={}\n  for tp_n in \"${{tp_want[@]}}\"; do\n    tp_wantv[$tp_n]=$tp_pin\n    [ \"${{tp_before[$tp_n]-}}\" = \"$tp_pin\" ] || tp_todo+=(\"$tp_n\")\n  done\n",
                quote(v)
            ));
            "cargo install \"${tp_todo[0]}\" --version \"$tp_pin\"".to_string()
        }
        Wanted::Latest => {
            o.push_str(&format!(
                "  for tp_n in \"${{tp_want[@]}}\"; do\n    tp_l=$(cargo search \"$tp_n\" --limit 1 2>/dev/null | awk -v n=\"$tp_n\" '{SEARCH_AWK}')\n    if [ -z \"$tp_l\" ]; then echo \"telepatch: cargo $tp_n: could not resolve the latest version\" >&2; rc=1; continue; fi\n    tp_wantv[$tp_n]=$tp_l\n    [ \"${{tp_before[$tp_n]-}}\" = \"$tp_l\" ] || tp_todo+=(\"$tp_n\")\n  done\n"
            ));
            "cargo install \"${tp_todo[@]}\"".to_string()
        }
    };
    o.push_str("fi\n");
    o.push_str(&format!(
        "if [ \"$rc\" -eq 0 ] && [ \"${{#tp_todo[@]}}\" -gt 0 ]; then\n  {install}\n  rc=$?\n  \
         while read -r tp_n tp_v; do tp_after[$tp_n]=$tp_v; done < <(tp_list)\n  \
         for tp_n in \"${{tp_want[@]}}\"; do\n    \
         if [ \"${{tp_after[$tp_n]-}}\" != \"${{tp_before[$tp_n]-}}\" ]; then changed=1; installed+=(\"$tp_n\"); fi\n  done\n  \
         if [ \"$rc\" -eq 0 ]; then\n    for tp_n in \"${{tp_todo[@]}}\"; do\n      \
         tp_got=${{tp_after[$tp_n]-}}; tp_wv=${{tp_wantv[$tp_n]-}}\n      \
         if [ -z \"$tp_got\" ] || {{ [ -n \"$tp_wv\" ] && [ \"$tp_got\" != \"$tp_wv\" ]; }}; then\n        \
         echo \"telepatch: cargo $tp_n: installed ${{tp_got:-absent}}, wanted ${{tp_wv:-present}}\" >&2; rc=1\n      fi\n    done\n  fi\nfi\n"
    ));
    o
}

/// The prepare probe. With `cargo` present and listing: `cargo ok`, then
/// `installed <name> <version>` per installed crate, then, only when
/// `Latest`, `latest <name> <version>` per name that `cargo search` found.
/// Nothing is printed when `cargo` is missing or fails.
pub fn probe(names: &[String], wanted: &Wanted) -> String {
    let mut o = format!(
        "if command -v cargo >/dev/null 2>&1 && tp_l=$(cargo install --list 2>/dev/null); then\n  echo 'cargo ok'\n  printf '%s\\n' \"$tp_l\" | sed -n '{}'\n",
        LIST_SED.replace("\\1 \\2/p", "installed \\1 \\2/p")
    );
    if *wanted == Wanted::Latest {
        let want: Vec<String> = names.iter().map(|n| quote(n)).collect();
        o.push_str(&format!(
            "  for tp_n in {}; do\n    cargo search \"$tp_n\" --limit 1 2>/dev/null | awk -v n=\"$tp_n\" '$1==n && $2==\"=\" {{gsub(/\"/, \"\", $3); print \"latest \" n \" \" $3; exit}}'\n  done\n",
            want.join(" ")
        ));
    }
    o.push_str("fi\n");
    o
}

/// What the prepare probe found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CargoProbe {
    /// The installed crates and versions; `None` when the probe could not
    /// list them (no `cargo`, or the probe failed).
    pub installed: Option<BTreeMap<String, String>>,
    /// The latest version per name `cargo search` resolved.
    pub latest: BTreeMap<String, String>,
}

/// Parse [`probe`]'s output.
pub fn parse_probe(out: &str) -> CargoProbe {
    let mut p = CargoProbe::default();
    for l in out.lines() {
        if l == "cargo ok" {
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
    probed: &CargoProbe,
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

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    const LIST: &str = "cargo-deb v2.1.0:\n    cargo-deb\nripgrep v14.0.3 (/src/ripgrep):\n    rg\nfoo v1.0.0 (https://x/y#abc):\n    foo\n";

    /// The shell `sed` of [`LIST_SED`] on a real-looking list.
    #[test]
    fn list_sed_reads_headers() {
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!("printf '%s' \"$1\" | sed -n '{LIST_SED}'"))
            .arg("bash")
            .arg(LIST)
            .output()
            .unwrap();
        // The URL source has a colon inside the parentheses: not listed.
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            "cargo-deb 2.1.0\nripgrep 14.0.3\n"
        );
    }

    #[test]
    fn search_awk_matches_the_exact_name() {
        let body = "cargo-deb-extra = \"9.9.9\"    # x\ncargo-deb = \"2.2.0\"    # y\n";
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!(
                "printf '%s' \"$1\" | awk -v n=cargo-deb '{SEARCH_AWK}'"
            ))
            .arg("bash")
            .arg(body)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8(out.stdout).unwrap(), "2.2.0\n");
    }

    #[test]
    fn probe_output_parses() {
        let p = parse_probe("cargo ok\ninstalled a 1.0.0\ninstalled b 2.0.0\nlatest a 1.1.0\n");
        assert_eq!(p.installed.as_ref().unwrap().len(), 2);
        assert_eq!(p.latest.get("a").map(String::as_str), Some("1.1.0"));
        // cargo ok with an empty list: nothing installed, not unknown.
        assert_eq!(parse_probe("cargo ok\n").installed, Some(BTreeMap::new()));
        assert_eq!(parse_probe("").installed, None);
    }

    #[test]
    fn will_changes_per_name() {
        let names = s(&["a", "b", "c"]);
        let p = parse_probe(
            "cargo ok\ninstalled a 1.0.0\ninstalled b 1.0.0\nlatest a 1.0.0\nlatest b 2.0.0\n",
        );
        let l = |v: &[(&str, Option<&str>, &str)]| {
            v.iter()
                .map(|(n, i, w)| (n.to_string(), i.map(str::to_string), w.to_string()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            will_changes(&names, &p, &Wanted::Present),
            l(&[("c", None, "present")])
        );
        assert_eq!(
            will_changes(&names, &p, &Wanted::Latest),
            l(&[
                ("b", Some("1.0.0"), "2.0.0 (latest)"),
                ("c", None, "unknown (latest)")
            ])
        );
        // No cargo: unknown -> present for every name.
        assert_eq!(
            will_changes(&s(&["a"]), &CargoProbe::default(), &Wanted::Present),
            l(&[("a", Some("unknown"), "present")])
        );
    }
}
