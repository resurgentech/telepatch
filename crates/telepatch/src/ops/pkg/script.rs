//! `Source::Script`: a vendor installer (`curl … | sh`) with the five
//! answers of a source spelled out by the role: the binary that proves
//! presence (`bin`), the command printing its version (`version`), where
//! the latest version comes from (`latest`), and how to install and update.
//!
//! Builder strings are shell commands and are inserted verbatim, each in
//! its own subshell; nothing in them is templated. The wanted version
//! reaches the install / update command as `TP_WANT` (empty for
//! `Present`). "Latest" is resolved on the host, never on the controller.

use crate::sh::quote;

use super::Wanted;

/// The version token of A19: first match of `[0-9]+(\.[0-9A-Za-z+~-]+)*`.
pub const VERSION_RE: &str = r"[0-9]+(\.[0-9A-Za-z+~-]+)*";

/// The first version token in `s` (the Rust side of [`VERSION_RE`]).
pub fn version_token(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let start = b.iter().position(u8::is_ascii_digit)?;
    let mut end = start;
    while end < b.len() && b[end].is_ascii_digit() {
        end += 1;
    }
    let part = |c: u8| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'~' | b'-');
    while end + 1 < b.len() && b[end] == b'.' && part(b[end + 1]) {
        end += 1;
        while end < b.len() && part(b[end]) {
            end += 1;
        }
    }
    Some(s[start..end].to_string())
}

/// `p` as a shell word: quoted, except a leading `~`/`~user`, which must
/// expand on the host (the rule of `FileSpec`'s path, `ops/file.rs:55-74`).
pub(super) fn path_word(p: &str) -> String {
    if let Some(rest) = p.strip_prefix('~') {
        let (user, tail) = match rest.find('/') {
            Some(i) => (&rest[..i], Some(&rest[i + 1..])),
            None => (rest, None),
        };
        if user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
        {
            return match tail {
                Some(t) => format!("~{user}/{}", quote(t)),
                None => format!("~{user}"),
            };
        }
    }
    quote(p)
}

/// Shell printing the body of the (already quoted) url on stdout: `curl`
/// when the host has it, else `wget`.
fn fetch_stdout(u: &str) -> String {
    format!(
        "if command -v curl >/dev/null 2>&1; then curl -fsSL -- {u}; else wget -q -O - -- {u}; fi"
    )
}

/// Where the latest version comes from. Rendered into a host-side command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Latest {
    /// A command run on the host that prints the version.
    Command(String),
    /// `owner/repo` on GitHub: the `tag_name` of its latest release, read
    /// with `curl`, else `wget`, on the host (a leading `v` dropped).
    GithubRelease(String),
    /// A url whose body names the latest version (go.dev's
    /// `VERSION?m=text`: `go1.27.3` then a `time` line), read with `curl`,
    /// else `wget`, on the host. Callers take the body's first version
    /// token, so `go1.27.3` and `v1.27.3` are both `1.27.3`.
    Text(String),
}

impl Latest {
    pub fn command(c: impl Into<String>) -> Latest {
        Latest::Command(c.into())
    }

    pub fn github_release(repo: impl Into<String>) -> Latest {
        Latest::GithubRelease(repo.into())
    }

    /// The body of `url` names the latest version ([`Latest::Text`]).
    pub fn text(url: impl Into<String>) -> Latest {
        Latest::Text(url.into())
    }

    /// Shell printing the latest version (raw; callers take the token).
    pub fn render(&self) -> String {
        match self {
            Latest::Command(c) => c.clone(),
            Latest::GithubRelease(repo) => {
                let u = quote(&format!(
                    "https://api.github.com/repos/{repo}/releases/latest"
                ));
                format!(
                    "{} | {}",
                    fetch_stdout(&u),
                    "sed -n 's/.*\"tag_name\"[[:space:]]*:[[:space:]]*\"v\\{0,1\\}\\([^\"]*\\)\".*/\\1/p' | head -n1"
                )
            }
            Latest::Text(url) => fetch_stdout(&quote(url)),
        }
    }
}

/// A vendor installer. `bin`, `version` and `install` are required;
/// `update` defaults to `install`; `latest` is required for `.latest()`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallerScript {
    pub bin: String,
    pub version: String,
    pub latest: Option<Latest>,
    pub install: String,
    pub update: Option<String>,
}

impl InstallerScript {
    pub fn new() -> InstallerScript {
        InstallerScript::default()
    }

    /// The binary whose presence proves the install (`[ -x bin ]`).
    pub fn bin(mut self, p: impl Into<String>) -> Self {
        self.bin = p.into();
        self
    }

    /// A command printing the installed version; its first version token
    /// is the installed version.
    pub fn version(mut self, c: impl Into<String>) -> Self {
        self.version = c.into();
        self
    }

    pub fn latest(mut self, l: Latest) -> Self {
        self.latest = Some(l);
        self
    }

    pub fn install(mut self, c: impl Into<String>) -> Self {
        self.install = c.into();
        self
    }

    pub fn update(mut self, c: impl Into<String>) -> Self {
        self.update = Some(c.into());
        self
    }

    /// The update command (`install` when none was given).
    pub fn update_cmd(&self) -> &str {
        self.update.as_deref().unwrap_or(&self.install)
    }

    /// The missing required parts, as trace error messages.
    pub fn problems(&self, name: &str, wanted: &Wanted) -> Vec<String> {
        let mut v = Vec::new();
        for (part, val) in [
            ("bin", &self.bin),
            ("version", &self.version),
            ("install", &self.install),
        ] {
            if val.trim().is_empty() {
                v.push(format!("script {name}: {part} is required"));
            }
        }
        if *wanted == Wanted::Latest {
            v.extend(self.latest_problem(name));
        }
        v
    }

    pub fn latest_problem(&self, name: &str) -> Option<String> {
        self.latest
            .is_none()
            .then(|| format!("script {name}: latest is required for .latest()"))
    }

    /// Shell defining `tp_tok` (stdin -> version token) and `tp_probe`
    /// (sets `tp_have` 0/1 and `tp_v`, the installed version or empty).
    fn probe_fns(&self) -> String {
        format!(
            "tp_tok() {{ grep -oE '{VERSION_RE}' | head -n1; }}\n\
             tp_probe() {{\n  tp_have=0; tp_v=\n  if [ -x {bin} ]; then\n    tp_have=1\n    tp_v=$( (\n{version}\n) 2>/dev/null | tp_tok)\n  fi\n}}\n",
            bin = path_word(&self.bin),
            version = self.version,
        )
    }

    /// Shell setting `tp_latest` to the resolved latest version (or empty).
    fn latest_block(&self) -> String {
        let cmd = self.latest.as_ref().map(Latest::render).unwrap_or_default();
        format!("tp_latest=$( (\n{cmd}\n) 2>/dev/null | tp_tok)\n")
    }

    /// The op body (sets `rc`, `changed`, `installed`).
    pub fn render(&self, name: &str, wanted: &Wanted) -> String {
        let n = quote(name);
        let mut o = format!("rc=0; changed=0; installed=()\ntp_name={n}\n");
        o.push_str(&self.probe_fns());
        match wanted {
            Wanted::Present => o.push_str("tp_want_v=\n"),
            Wanted::Pinned(v) => o.push_str(&format!("tp_want_v={}\n", quote(v))),
            Wanted::Latest => {
                o.push_str(&self.latest_block());
                o.push_str("tp_want_v=$tp_latest\n");
                o.push_str(&format!(
                    "if [ -z \"$tp_want_v\" ]; then echo \"telepatch: script \"{n}\": could not resolve the latest version\" >&2; rc=1; fi\n"
                ));
            }
            Wanted::Absent => o.push_str(
                "echo 'telepatch: .absent() is only available on the distro manager' >&2; rc=1\n",
            ),
        }
        o.push_str(
            "tp_probe\ntp_had=$tp_have; tp_before=$tp_v; tp_act=\n\
             if [ \"$rc\" -eq 0 ]; then\n  if [ \"$tp_have\" -eq 0 ]; then tp_act=install\n  \
             elif [ -n \"$tp_want_v\" ] && [ \"$tp_v\" != \"$tp_want_v\" ]; then tp_act=update\n  fi\nfi\n",
        );
        o.push_str(&format!(
            "if [ \"$tp_act\" = install ]; then\n  ( export TP_WANT=\"$tp_want_v\"\n{}\n  )\n  rc=$?\nelif [ \"$tp_act\" = update ]; then\n  ( export TP_WANT=\"$tp_want_v\"\n{}\n  )\n  rc=$?\nfi\n",
            self.install,
            self.update_cmd()
        ));
        o.push_str(
            "if [ -n \"$tp_act\" ]; then\n  tp_probe\n  \
             if [ \"$tp_have\" -ne \"$tp_had\" ] || [ \"$tp_v\" != \"$tp_before\" ]; then changed=1; installed=(\"$tp_name\"); fi\n  \
             if [ \"$rc\" -eq 0 ]; then\n    \
             if [ \"$tp_have\" -eq 0 ]; then tp_got=absent; else tp_got=${tp_v:-unknown}; fi\n    \
             if [ \"$tp_have\" -eq 0 ] || { [ -n \"$tp_want_v\" ] && [ \"$tp_v\" != \"$tp_want_v\" ]; }; then\n      \
             echo \"telepatch: script $tp_name: installed $tp_got, wanted ${tp_want_v:-present}\" >&2; rc=1\n    fi\n  fi\nfi\n",
        );
        o
    }

    /// The prepare probe: prints `absent` or `installed <v>` (`<v>` may be
    /// empty), then, only when `latest`, `latest <v>`.
    pub fn probe(&self, wanted: &Wanted) -> String {
        let mut o = self.probe_fns();
        o.push_str(
            "tp_probe\nif [ \"$tp_have\" -eq 1 ]; then echo \"installed $tp_v\"; else echo absent; fi\n",
        );
        if *wanted == Wanted::Latest {
            o.push_str(&self.latest_block());
            o.push_str("echo \"latest $tp_latest\"\n");
        }
        o
    }
}

/// What the prepare probe found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probed {
    /// The probe failed or printed nothing recognisable.
    Unknown,
    Absent,
    /// Present; the version token, empty when none was found.
    Installed(String),
}

/// Parse [`InstallerScript::probe`]'s output: what is installed, and the latest
/// version (`None` when not asked or not resolved).
pub fn parse_probe(out: &str) -> (Probed, Option<String>) {
    let mut have = Probed::Unknown;
    let mut latest = None;
    for l in out.lines() {
        if l == "absent" {
            have = Probed::Absent;
        } else if let Some(v) = l.strip_prefix("installed ") {
            have = Probed::Installed(v.trim().to_string());
        } else if l == "installed" {
            have = Probed::Installed(String::new());
        } else if let Some(v) = l.strip_prefix("latest ") {
            let v = v.trim();
            if !v.is_empty() {
                latest = Some(v.to_string());
            }
        }
    }
    (have, latest)
}

/// `(installed, wanted)` as the report prints them, or `None` when nothing
/// will change. `installed` is `None` for absent and `"unknown"` when the
/// probe failed or found no version; `wanted` is `present`, the pin, or
/// `<v> (latest)` (`unknown (latest)` when the lookup failed).
pub fn will_change(
    have: &Probed,
    latest: Option<&str>,
    wanted: &Wanted,
) -> Option<(Option<String>, String)> {
    let want = match wanted {
        Wanted::Present => "present".to_string(),
        Wanted::Pinned(v) => v.clone(),
        Wanted::Latest => format!("{} (latest)", latest.unwrap_or("unknown")),
        Wanted::Absent => return None,
    };
    let target = match wanted {
        Wanted::Present => None,
        Wanted::Pinned(v) => Some(v.as_str()),
        Wanted::Latest => Some(latest.unwrap_or("")),
        Wanted::Absent => return None,
    };
    let installed = match have {
        Probed::Absent => None,
        Probed::Unknown => Some("unknown".to_string()),
        Probed::Installed(v) => {
            match target {
                // Present and present: nothing to do.
                None => return None,
                Some(t) if !t.is_empty() && v == t => return None,
                _ => {}
            }
            Some(if v.is_empty() {
                "unknown".to_string()
            } else {
                v.clone()
            })
        }
    };
    Some((installed, want))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_tokens() {
        for (s, want) in [
            ("rustc 1.99.1 (abc 2026-01-01)", Some("1.99.1")),
            ("uv 0.13.6", Some("0.13.6")),
            ("0.9.5\n", Some("0.9.5")),
            ("v1.2.3-rc.1+build", Some("1.2.3-rc.1+build")),
            ("tool 2.", Some("2")),
            ("x 1.0~beta.2 y", Some("1.0~beta.2")),
            ("go1.22.3 linux", Some("1.22.3")),
            ("no digits", None),
            ("", None),
        ] {
            assert_eq!(version_token(s).as_deref(), want, "{s:?}");
        }
    }

    /// The shell `tp_tok` agrees with [`version_token`].
    #[test]
    fn shell_token_matches_rust() {
        let inputs = [
            "rustc 1.99.1 (abc 2026-01-01)",
            "uv 0.13.6",
            "v1.2.3-rc.1+build",
            "tool 2.",
            "x 1.0~beta.2 y",
            "go1.22.3 linux",
            "no digits",
        ];
        for s in inputs {
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(format!(
                    "printf '%s\\n' \"$1\" | grep -oE '{VERSION_RE}' | head -n1"
                ))
                .arg("bash")
                .arg(s)
                .output()
                .unwrap();
            let got = String::from_utf8(out.stdout).unwrap();
            let got = got.trim_end();
            assert_eq!(
                version_token(s).unwrap_or_default(),
                got,
                "{s:?} shell vs rust"
            );
        }
    }

    #[test]
    fn github_release_parses_tag_name_on_the_host_side() {
        // The sed half of the rendering, fed a release body.
        let r = Latest::github_release("astral-sh/uv").render();
        assert!(
            r.starts_with(
                "if command -v curl >/dev/null 2>&1; then curl -fsSL -- 'https://api.github.com/repos/astral-sh/uv/releases/latest'; else wget -q -O - -- 'https://api.github.com/repos/astral-sh/uv/releases/latest'; fi | "
            ),
            "{r}"
        );
        let sed = r.split_once(" | ").unwrap().1;
        for (body, want) in [
            (
                "{\n  \"tag_name\": \"0.9.5\",\n  \"name\": \"x\"\n}\n",
                "0.9.5",
            ),
            ("{\"url\":\"u\",\"tag_name\":\"v1.2.3\",\"x\":1}", "1.2.3"),
        ] {
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(format!("printf '%s' \"$1\" | {sed}"))
                .arg("bash")
                .arg(body)
                .output()
                .unwrap();
            assert_eq!(String::from_utf8(out.stdout).unwrap().trim_end(), want);
        }
    }

    #[test]
    fn text_reads_the_body_with_curl_else_wget() {
        let r = Latest::text("https://go.dev/VERSION?m=text").render();
        assert_eq!(
            r,
            "if command -v curl >/dev/null 2>&1; then curl -fsSL -- 'https://go.dev/VERSION?m=text'; else wget -q -O - -- 'https://go.dev/VERSION?m=text'; fi"
        );
        // the caller's token of go.dev's body
        assert_eq!(
            version_token("go1.27.3\ntime 2026-11-01T00:00:00Z\n").as_deref(),
            Some("1.27.3")
        );
    }

    #[test]
    fn probe_output_parses() {
        assert_eq!(parse_probe("absent\n"), (Probed::Absent, None));
        assert_eq!(
            parse_probe("installed 0.9.2\nlatest 0.9.5\n"),
            (Probed::Installed("0.9.2".into()), Some("0.9.5".into()))
        );
        assert_eq!(
            parse_probe("installed \nlatest \n"),
            (Probed::Installed(String::new()), None)
        );
        assert_eq!(parse_probe("garbage"), (Probed::Unknown, None));
    }

    #[test]
    fn will_change_lines() {
        let p = |v: &str| Wanted::Pinned(v.into());
        let inst = |v: &str| Probed::Installed(v.into());
        let s = |i: Option<&str>, w: &str| Some((i.map(str::to_string), w.to_string()));
        assert_eq!(
            will_change(&Probed::Absent, None, &Wanted::Present),
            s(None, "present")
        );
        assert_eq!(will_change(&inst("0.9.2"), None, &Wanted::Present), None);
        assert_eq!(
            will_change(&inst("0.9.2"), None, &p("0.9.5")),
            s(Some("0.9.2"), "0.9.5")
        );
        assert_eq!(will_change(&inst("0.9.5"), None, &p("0.9.5")), None);
        assert_eq!(
            will_change(&inst("0.9.2"), Some("0.9.5"), &Wanted::Latest),
            s(Some("0.9.2"), "0.9.5 (latest)")
        );
        assert_eq!(
            will_change(&inst("0.9.5"), Some("0.9.5"), &Wanted::Latest),
            None
        );
        assert_eq!(
            will_change(&inst("0.9.2"), None, &Wanted::Latest),
            s(Some("0.9.2"), "unknown (latest)")
        );
        assert_eq!(
            will_change(&Probed::Unknown, None, &Wanted::Present),
            s(Some("unknown"), "present")
        );
    }
}
