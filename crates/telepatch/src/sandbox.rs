//! The test switch (T124): `TP_SANDBOX_REQUIRED=1`, set for every process
//! cargo runs by `.cargo/config.toml`, makes a shell refuse the real stage,
//! cache, run directory and filesystem roots. [`SBX`] is the shell function
//! every script family that resolves one of those paths includes.
//!
//! `tp_sbx PATH...` returns 0 when the switch is off. With it on, each PATH
//! must be absolute, hold no `..` component and lie under `$TP_SANDBOX`
//! (a directory that exists; an empty or unset one matches nothing);
//! otherwise it prints the refusal on stderr and returns 1. A real run over
//! ssh never sees the variable, so it is unaffected.

/// The text of [`SBX`], as a macro so it can sit inside `concat!`.
macro_rules! sbx_def {
    () => {
        r#"tp_sbx() { [ "${TP_SANDBOX_REQUIRED-}" = 1 ] || return 0; local p s=${TP_SANDBOX-}; s=${s%/}; for p; do case $p/ in */../*) ;; /*) [ -n "$s" ] && [ -d "$s" ] && case $p/ in "$s"/*) continue;; esac;; esac; echo "telepatch: sandbox: refusing $p (TP_SANDBOX_REQUIRED is set; tests must use telepatch::testing::sandbox)" >&2; return 1; done; }
"#
    };
}
pub(crate) use sbx_def;

/// The `tp_sbx` function definition, ending in a newline.
pub(crate) const SBX: &str = sbx_def!();

/// The refusal line for `path` (what `tp_sbx` prints).
pub(crate) fn refusal(path: &str) -> String {
    format!(
        "telepatch: sandbox: refusing {path} (TP_SANDBOX_REQUIRED is set; tests must use telepatch::testing::sandbox)"
    )
}

/// The shell words `tp_sbx` is called with at a shell's open: the stage,
/// the cache, the run directory and every `TP_FS_ROOTS` entry.
pub(crate) const INIT_CHECK: &str = r#"tp_a=(); IFS=: read -ra tp_r <<<"${TP_FS_ROOTS-}"
for tp_x in "${tp_r[@]}"; do [ -z "$tp_x" ] || tp_a+=("$tp_x"); done
tp_sbx "${TP_FETCH_STAGE:-/var/tmp/telepatch}" "${TP_FETCH_CACHE:-/var/cache/telepatch/sha256}" "${TP_RUN_DIR:-/run/telepatch}" "${tp_a[@]}"
"#;

/// The variables a shell hands on to a login shell it starts with `env -i`
/// (T128): the test switch, the sandbox and every engine path seam. A
/// production shell has none of them set, so nothing is carried there.
/// Their values are paths (or a number), never a script.
pub(crate) const CARRY: &[&str] = &[
    "TP_SANDBOX_REQUIRED",
    "TP_SANDBOX",
    "TP_FETCH_STAGE",
    "TP_FETCH_CACHE",
    "TP_RUN_DIR",
    "TP_FS_ROOTS",
    "TP_UNIT_DIR",
    "TP_SVC_RUNDIR",
    "TP_BOOT_ID",
    "TP_CACHE_NOW",
];

/// The shell words, for an `env -i` command line, that pass each [`CARRY`]
/// variable set in the shell that runs it as `NAME=value`, and nothing for
/// one that is unset: `${NAME+NAME="$NAME"}` each, space-separated. The
/// value is expanded by that shell, so it stays one word whatever it holds.
pub(crate) fn carry_words() -> String {
    CARRY
        .iter()
        .map(|n| format!("${{{n}+{n}=\"${n}\"}}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Is this process one cargo started (`TP_SANDBOX_REQUIRED=1`)?
pub(crate) fn required() -> bool {
    std::env::var("TP_SANDBOX_REQUIRED").is_ok_and(|v| v == "1")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(env: &[(&str, &str)], paths: &[&str]) -> (bool, String) {
        let mut c = std::process::Command::new("bash");
        c.arg("-c")
            .arg(format!("{SBX}tp_sbx \"$@\""))
            .arg("sh")
            .args(paths)
            .env_remove("TP_SANDBOX_REQUIRED")
            .env_remove("TP_SANDBOX");
        for (k, v) in env {
            c.env(k, v);
        }
        let o = c.output().unwrap();
        (
            o.status.success(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    }

    /// The words `carry_words` gives in a shell with `env` set, one per line.
    fn carried(env: &[(&str, &str)]) -> String {
        let mut c = std::process::Command::new("bash");
        c.arg("-c")
            .arg(format!("printf '%s\\n' {}", carry_words()))
            .env_clear()
            .env("PATH", "/usr/bin:/bin");
        for (k, v) in env {
            c.env(k, v);
        }
        String::from_utf8(c.output().unwrap().stdout).unwrap()
    }

    #[test]
    fn a_production_shell_carries_nothing_and_a_value_stays_one_word() {
        // printf with no arguments prints one empty line.
        assert_eq!(carried(&[]), "\n");
        assert_eq!(
            carried(&[("TP_SANDBOX", "/a b*"), ("TP_CACHE_NOW", "")]),
            "TP_SANDBOX=/a b*\nTP_CACHE_NOW=\n"
        );
    }

    #[test]
    fn the_switch_off_allows_anything() {
        assert!(run(&[], &["/var/tmp/telepatch"]).0);
    }

    #[test]
    fn a_path_must_lie_under_an_existing_sandbox() {
        let d = tempfile::tempdir().unwrap();
        let sb = d.path().display().to_string();
        let on = [("TP_SANDBOX_REQUIRED", "1"), ("TP_SANDBOX", sb.as_str())];
        assert!(run(&on, &[&format!("{sb}/a"), &sb]).0);
        let (ok, err) = run(&on, &[&format!("{sb}/a"), "/var/tmp/telepatch"]);
        assert!(!ok);
        assert_eq!(err.trim(), refusal("/var/tmp/telepatch"));
        // A sibling that shares the prefix, a `..`, a relative path.
        assert!(!run(&on, &[&format!("{sb}x/a")]).0);
        assert!(!run(&on, &[&format!("{sb}/../a")]).0);
        assert!(!run(&on, &["a/b"]).0);
    }

    #[test]
    fn an_unset_empty_or_missing_sandbox_matches_nothing() {
        assert!(!run(&[("TP_SANDBOX_REQUIRED", "1")], &["/tmp/a"]).0);
        assert!(
            !run(
                &[("TP_SANDBOX_REQUIRED", "1"), ("TP_SANDBOX", "")],
                &["/tmp/a"]
            )
            .0
        );
        assert!(
            !run(
                &[("TP_SANDBOX_REQUIRED", "1"), ("TP_SANDBOX", "/")],
                &["/tmp/a"]
            )
            .0
        );
        assert!(
            !run(
                &[
                    ("TP_SANDBOX_REQUIRED", "1"),
                    ("TP_SANDBOX", "/nonexistent-sbx")
                ],
                &["/nonexistent-sbx/a"]
            )
            .0
        );
    }
}
