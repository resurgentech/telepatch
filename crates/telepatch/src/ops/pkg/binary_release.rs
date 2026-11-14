//! `Source::BinaryRelease`: a tarball or a single binary a vendor
//! publishes, `h.pkg([n]).binary_release(BinaryRelease::new()..)`.
//!
//! The url may hold `{version}` and `{arch}`. `{arch}` is the host's
//! `uname -m` through the spec's [`Arch`] table (by default `x86_64` is
//! `amd64` and `aarch64` is `arm64`; a name the table lacks passes through).
//! Everything is resolved on the host, in prepare ([`BinaryRelease::prepare`]):
//!
//! * installed: `bin` is executable; its version is the first version
//!   token ([`super::VERSION_RE`]) of the `.version(cmd)` output, run with
//!   `bin`'s directory first on `PATH` (so `go version` is that `go`);
//! * wanted, with `{version}` in the url: present is any installed version
//!   (nothing installed: the latest source's version, and with none the
//!   host is blocked); `.pin(v)` is `v`; `.latest()` is the first version
//!   token of the [`Latest`] source's output (`go1.27.3` is `1.27.3`). A
//!   settled host (installed equals wanted) makes no network call beyond
//!   resolving latest;
//! * wanted, with no `{version}`: present is "installed at all"; latest
//!   means the published checksum differs from the installed file's (the
//!   `sha256sum` of `bin`, or for a tarball the archive sum in T81's marker
//!   `<unpack_to>/.telepatch-fetch`). With a [`Sum::Value`] that needs no
//!   network: latest is present with that sum;
//! * the checksum: [`Sum::Value`], the first 64-hex word of
//!   `<url><suffix>` ([`Sum::Sidecar`]), or the `sum_key` of the object in
//!   a JSON document whose `filename_key` is the url's last segment
//!   ([`Sum::Json`]; see [`json_sum_shell`] for what that assumes). None is
//!   the policy error `Unverified`.
//!
//! When a change is wanted the release goes through T81's staging and
//! cache ([`crate::ops::fetch::prelude`]'s `tp_one`): a cache hit makes no
//! download; a mismatch or a failed download blocks the host before apply.
//! Prepare then records `<sum> <wanted version or -> <file>` and, on a
//! second line, the url, in `<stage>/<run>/br-<hash of the name>`, which
//! the op reads and removes. A cached file gone by then (a `cache purge`
//! during the run, T135) fails the op before anything is placed.
//! A tarball with an `unpack_to` lands on that directory's filesystem (T119,
//! [`crate::ops::fetch::prelude`]): on another one the download and its cache
//! entry are under `<root>/.telepatch/`, the record names the stage's link
//! `f-<sum>` to it, and a single binary (no install directory) stays on the
//! stage's filesystem.
//! With no record the op changes nothing. The swap is T81's: a single
//! binary is copied to a candidate beside `bin`, given mode 0755 and
//! renamed over `bin` (one rename); a tarball is unpacked (`.strip(n)`) into
//! a sibling of `unpack_to` and renamed in (one rename when `unpack_to` is
//! absent; when it exists, the old tree is renamed aside first and removed
//! after, two renames, and it comes back if the second fails). Root only.

use crate::ops::fetch::{self, Found, Item};
use crate::ops::file::FileSpec;
use crate::report::{FetchState, WillCreate};
use crate::sh::quote;

use super::script::path_word;
use super::{Latest, VERSION_RE, Wanted};

/// Where the checksum of a release comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sum {
    /// A JSON document listing files (go.dev's `?mode=json`): the object
    /// whose `filename_key` is the release's file name holds the sum under
    /// `sum_key`.
    Json {
        url: String,
        filename_key: String,
        sum_key: String,
    },
    /// `<url><suffix>` holds the sum (`<hex>  <name>`, or the hex alone).
    Sidecar(String),
    /// The sum itself (64 hex digits; lower-cased when valid).
    Value(String),
}

impl Sum {
    pub fn json(
        url: impl Into<String>,
        filename_key: impl Into<String>,
        sum_key: impl Into<String>,
    ) -> Sum {
        Sum::Json {
            url: url.into(),
            filename_key: filename_key.into(),
            sum_key: sum_key.into(),
        }
    }

    pub fn sidecar(suffix: impl Into<String>) -> Sum {
        Sum::Sidecar(suffix.into())
    }

    pub fn value(hex: impl AsRef<str>) -> Sum {
        let s = hex.as_ref();
        Sum::Value(if fetch::valid_sum(s) {
            s.to_ascii_lowercase()
        } else {
            s.to_string()
        })
    }
}

/// How `uname -m` becomes `{arch}`: a table of `(uname, url name)`; a
/// machine the table lacks keeps uname's name. [`Arch::default`] maps
/// `x86_64` to `amd64` and `aarch64` to `arm64`; [`Arch::UNAME`] is the
/// empty table (uname's names throughout). A custom table:
/// `.arch_names(&[("x86_64", "x64"), ("aarch64", "arm64")][..])`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arch(pub Vec<(String, String)>);

impl Arch {
    /// uname's own names (`x86_64`, `aarch64`).
    pub const UNAME: Arch = Arch(Vec::new());
}

impl Default for Arch {
    fn default() -> Arch {
        Arch::from(&[("x86_64", "amd64"), ("aarch64", "arm64")][..])
    }
}

impl From<&[(&str, &str)]> for Arch {
    fn from(t: &[(&str, &str)]) -> Arch {
        Arch(
            t.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
        )
    }
}

impl<const N: usize> From<[(&str, &str); N]> for Arch {
    fn from(t: [(&str, &str); N]) -> Arch {
        Arch::from(&t[..])
    }
}

impl From<Vec<(String, String)>> for Arch {
    fn from(t: Vec<(String, String)>) -> Arch {
        Arch(t)
    }
}

/// A vendor's binary release. `url` and `bin` are required; `version` is
/// required when the url has `{version}`; `unpack_to` makes it a tarball
/// (else the download is the binary itself).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BinaryRelease {
    pub url: String,
    pub sha256: Option<Sum>,
    pub arch: Arch,
    pub unpack_to: Option<String>,
    pub strip: Option<u32>,
    pub bin: String,
    pub version: Option<String>,
    pub latest: Option<Latest>,
}

impl BinaryRelease {
    pub fn new() -> BinaryRelease {
        BinaryRelease::default()
    }

    /// The download url; `{version}` and `{arch}` are filled in on the host.
    pub fn url(mut self, u: impl Into<String>) -> Self {
        self.url = u.into();
        self
    }

    pub fn sha256(mut self, s: Sum) -> Self {
        self.sha256 = Some(s);
        self
    }

    /// The `{arch}` table ([`Arch`]).
    pub fn arch_names(mut self, a: impl Into<Arch>) -> Self {
        self.arch = a.into();
        self
    }

    /// The release is a tarball (gz, xz, zst, plain tar or zip) unpacked
    /// into this directory, replacing it.
    pub fn unpack_to(mut self, dir: impl Into<String>) -> Self {
        self.unpack_to = Some(dir.into());
        self
    }

    /// Drop `n` leading path components when unpacking.
    pub fn strip(mut self, n: u32) -> Self {
        self.strip = Some(n);
        self
    }

    /// The binary whose presence proves the install; for a single-binary
    /// release, where it is placed.
    pub fn bin(mut self, p: impl Into<String>) -> Self {
        self.bin = p.into();
        self
    }

    /// A command printing the installed version (its first version token).
    pub fn version(mut self, c: impl Into<String>) -> Self {
        self.version = Some(c.into());
        self
    }

    /// Where the latest version comes from (with `{version}` in the url).
    pub fn latest(mut self, l: Latest) -> Self {
        self.latest = Some(l);
        self
    }

    /// The url names a version.
    pub fn versioned(&self) -> bool {
        self.url.contains("{version}")
    }

    /// The trace errors of the spec itself.
    pub fn problems(&self, name: &str) -> Vec<String> {
        let mut v = Vec::new();
        let p = |m: String| format!("binary_release {name}: {m}");
        if self.url.trim().is_empty() {
            v.push(p("url is required".into()));
        }
        if self.bin.trim().is_empty() {
            v.push(p("bin is required".into()));
        }
        if let Some(ph) = unknown_placeholder(&self.url) {
            v.push(p(format!(
                "unknown placeholder {{{ph}}} in the url (only {{version}} and {{arch}})"
            )));
        }
        if let (Some(n), None) = (self.strip, &self.unpack_to) {
            v.push(p(format!(".strip({n}) needs .unpack_to(..)")));
        }
        match &self.sha256 {
            Some(Sum::Value(s)) if !fetch::valid_sum(s) => {
                v.push(p(format!("sha256 must be 64 hex digits, got {s:?}")));
            }
            Some(Sum::Json {
                filename_key,
                sum_key,
                ..
            }) => {
                for k in [filename_key, sum_key] {
                    if k.is_empty()
                        || !k
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    {
                        v.push(p(format!(
                            "Sum::json keys are letters, digits, _ and -; got {k:?}"
                        )));
                    }
                }
            }
            _ => {}
        }
        if self.latest.is_some() && !self.versioned() {
            v.push(p(".latest(Latest::..) needs {version} in the url".into()));
        }
        if self.versioned() && self.version.as_deref().is_none_or(|c| c.trim().is_empty()) {
            v.push(p("version is required when the url has {version}".into()));
        }
        v
    }

    /// The trace error of wanting `w`, if any.
    pub fn want_problem(&self, name: &str, w: &Wanted) -> Option<String> {
        let m = match w {
            Wanted::Pinned(_) if !self.versioned() => ".pin(v) needs {version} in the url",
            Wanted::Latest if self.versioned() && self.latest.is_none() => {
                ".latest() needs .latest(Latest::..) when the url has {version}"
            }
            Wanted::Latest if !self.versioned() && self.sha256.is_none() => {
                ".latest() with no {version} in the url needs a checksum source"
            }
            _ => return None,
        };
        Some(format!("binary_release {name}: {m}"))
    }

    /// What the op replaces: `unpack_to` for a tarball, else `bin`.
    fn target(&self) -> &str {
        self.unpack_to.as_deref().unwrap_or(&self.bin)
    }

    /// Shell setting `tp_have` (0/1) and `tp_v` (the installed version
    /// token, or empty).
    fn probe_shell(&self) -> String {
        let bin = path_word(&self.bin);
        let mut s = format!("tp_have=0; tp_v=\nif [ -x {bin} ]; then\n  tp_have=1\n");
        if let Some(c) = &self.version {
            s.push_str(&format!(
                "  tp_bd=$(dirname -- {bin})\n  tp_v=$( ( PATH=\"$tp_bd:$PATH\"\n{c}\n) 2>/dev/null </dev/null | tp_tok)\n"
            ));
        }
        s.push_str("fi\n");
        s
    }

    /// Shell setting `tp_arch` from `uname -m` through the table.
    fn arch_shell(&self) -> String {
        let mut s = String::from("tp_m=$(uname -m 2>/dev/null)\ncase \"$tp_m\" in\n");
        for (a, b) in &self.arch.0 {
            s.push_str(&format!("  {}) tp_arch={};;\n", quote(a), quote(b)));
        }
        s.push_str("  *) tp_arch=$tp_m;;\nesac\n");
        s
    }

    /// Shell setting `var` to `tpl` with `{arch}` and `{version}` filled in
    /// from `tp_arch` and `tp_want`.
    fn subst_shell(var: &str, tpl: &str) -> String {
        format!(
            "{var}={}\n{var}=${{{var}//'{{arch}}'/\"$tp_arch\"}}\n{var}=${{{var}//'{{version}}'/\"$tp_want\"}}\n",
            quote(tpl)
        )
    }

    /// Shell setting `tp_sum` (empty with no checksum source), or printing
    /// `br block ..` and returning.
    fn sum_shell(&self) -> String {
        match &self.sha256 {
            None => "tp_sum=\n".to_string(),
            Some(Sum::Value(h)) => format!("tp_sum={}\n", quote(h)),
            Some(Sum::Sidecar(sfx)) => format!(
                "tp_su=\"$tp_url\"{sfx}\n{NO_DL}if ! tp_body=$(tp_get \"$tp_su\" 2>/dev/null); then echo \"br block cannot read the checksum $tp_su\"; return 0; fi\ntp_sum=$(printf '%s\\n' \"$tp_body\" | grep -oE '[0-9a-fA-F]{{64}}' | head -n1 | tr A-F a-f)\nif [ -z \"$tp_sum\" ]; then echo \"br block no sha256 in $tp_su\"; return 0; fi\n",
                sfx = quote(sfx)
            ),
            Some(Sum::Json {
                url,
                filename_key,
                sum_key,
            }) => {
                let mut s = Self::subst_shell("tp_ju", url);
                s.push_str("tp_file=${tp_url%%[?#]*}\ntp_file=${tp_file##*/}\n");
                s.push_str(NO_DL);
                s.push_str("if ! tp_body=$(tp_get \"$tp_ju\" 2>/dev/null); then echo \"br block cannot read the checksums $tp_ju\"; return 0; fi\n");
                s.push_str(&format!(
                    "tp_sum=$(printf '%s' \"$tp_body\" | {})\n",
                    json_sum_shell(filename_key, sum_key)
                ));
                s.push_str("if [ -z \"$tp_sum\" ]; then echo \"br block no sha256 for $tp_file in $tp_ju\"; return 0; fi\n");
                s
            }
        }
    }

    /// The prepare script: prints `br have <0|1> <v>`, then `br settled`,
    /// `br block <why>`, or `br want <v>`, `br url <u>`, `br sum <s>`, one
    /// `fetch 0 <word>` line from T81's `tp_one` and, when the release is
    /// cached or staged, `br change` (after writing the record).
    pub fn prepare(&self, name: &str, wanted: &Wanted) -> String {
        let mut s = fetch::prelude();
        s.push_str(&format!(
            "tp_tok() {{ grep -oE '{VERSION_RE}' | head -n1; }}\ntp_get() {{\n  case \"$tp_dl\" in\n    curl) curl -fsSL -- \"$1\";;\n    wget) wget -q -O - -- \"$1\";;\n    *) return 127;;\n  esac\n}}\n"
        ));
        s.push_str("tp_br() {\n");
        s.push_str(&format!(
            "local tp_m tp_arch tp_have tp_v tp_bd tp_want= tp_url tp_su tp_ju tp_file tp_sum tp_body tp_o tp_f tp_got tp_dec\ntp_dec=\"$tp_run/br-$(printf %s {} | sha256sum | cut -c1-32)\"\nrm -f -- \"$tp_dec\"\n",
            quote(name)
        ));
        s.push_str(&self.arch_shell());
        s.push_str(&self.probe_shell());
        s.push_str("echo \"br have $tp_have $tp_v\"\n");
        const SETTLED: &str = "echo \"br settled\"; return 0";
        let resolve_latest = |s: &mut String, l: &Latest| {
            // A latest read over the network needs a downloader: without
            // one, say so (a split can then lift it), not "could not
            // resolve".
            if matches!(l, Latest::Text(_) | Latest::GithubRelease(_)) {
                s.push_str(NO_DL);
            }
            s.push_str(&format!(
                "tp_want=$( (\n{}\n) 2>/dev/null </dev/null | tp_tok)\nif [ -z \"$tp_want\" ]; then echo \"br block could not resolve the latest version\"; return 0; fi\n",
                l.render()
            ));
        };
        let installed_is_wanted = format!(
            "if [ \"$tp_have\" = 1 ] && [ \"$tp_v\" = \"$tp_want\" ]; then {SETTLED}; fi\n"
        );
        if self.versioned() {
            match wanted {
                Wanted::Present => {
                    s.push_str(&format!("if [ \"$tp_have\" = 1 ]; then {SETTLED}; fi\n"));
                    match &self.latest {
                        Some(l) => resolve_latest(&mut s, l),
                        None => s.push_str(
                            "echo \"br block not installed, and the url needs a {version}: give .latest(Latest::..) or .pin(v)\"; return 0\n",
                        ),
                    }
                }
                Wanted::Pinned(v) => {
                    s.push_str(&format!("tp_want={}\n", quote(v)));
                    s.push_str(&installed_is_wanted);
                }
                Wanted::Latest => {
                    if let Some(l) = &self.latest {
                        resolve_latest(&mut s, l);
                    }
                    s.push_str(&installed_is_wanted);
                }
                Wanted::Absent => {
                    s.push_str(
                        "echo \"br block .absent() is only available on the distro manager\"; return 0\n",
                    );
                }
            }
            s.push_str("echo \"br want $tp_want\"\n");
        } else if *wanted != Wanted::Latest {
            s.push_str(&format!("if [ \"$tp_have\" = 1 ]; then {SETTLED}; fi\n"));
        }
        s.push_str(&Self::subst_shell("tp_url", &self.url));
        s.push_str("echo \"br url $tp_url\"\n");
        s.push_str(&self.sum_shell());
        s.push_str("echo \"br sum $tp_sum\"\n");
        if !self.versioned() && *wanted == Wanted::Latest {
            match &self.unpack_to {
                Some(d) => s.push_str(&format!(
                    "if [ \"$(cat {}/.telepatch-fetch 2>/dev/null)\" = \"$tp_sum strip={}\" ]; then {SETTLED}; fi\n",
                    path_word(d),
                    self.strip.unwrap_or(0)
                )),
                None => s.push_str(&format!(
                    "if [ -f {b} ] && [ \"$(sha256sum < {b} | cut -d' ' -f1)\" = \"$tp_sum\" ]; then {SETTLED}; fi\n",
                    b = path_word(&self.bin)
                )),
            }
        }
        s.push_str(&format!(
            "tp_o=$(tp_one 0 \"$tp_url\" \"$tp_sum\" {}{})\necho \"$tp_o\"\n",
            if self.unpack_to.is_some() { 1 } else { 0 },
            self.unpack_to
                .as_deref()
                .map(|d| format!(" {}", path_word(d)))
                .unwrap_or_default()
        ));
        s.push_str(
            "case \"${tp_o%%$'\\n'*}\" in\n  'fetch 0 cached'|'fetch 0 fetched'|'fetch 0 staged') ;;\n  *) rmdir -- \"$tp_run\" 2>/dev/null; return 0;;\nesac\n\
             if [ -n \"$tp_sum\" ]; then tp_f=\"$tp_cd/$tp_sum\"; if [ -L \"$tp_run/f-$tp_sum\" ]; then tp_f=\"$tp_run/f-$tp_sum\"; fi; tp_got=$tp_sum\n\
             else tp_f=\"$tp_run/u-$(printf %s \"$tp_url\" | sha256sum | cut -c1-32)\"; tp_got=$(sha256sum \"$tp_f\" | cut -d' ' -f1); fi\n\
             if ! (umask 077; mkdir -p -- \"$tp_run\" && printf '%s %s %s\\n%s\\n' \"$tp_got\" \"${tp_want:--}\" \"$tp_f\" \"$tp_url\" > \"$tp_dec\"); then echo \"br block cannot write $tp_dec\"; return 0; fi\n\
             echo \"br change\"\n}\ntp_br\n",
        );
        s
    }

    /// The op body (sets `rc`, `changed`, `installed`): the swap of what
    /// prepare recorded, or nothing when it recorded nothing.
    pub fn render(&self, name: &str) -> String {
        let mut o = format!("rc=0; changed=0; installed=()\ntp_name={}\n", quote(name));
        o.push_str(fetch::LOCATE);
        o.push_str(fetch::TP_RUN);
        o.push_str(&format!(
            "tp_dec=\"$tp_run/br-$(printf %s \"$tp_name\" | sha256sum | cut -c1-32)\"\ntp_tok() {{ grep -oE '{VERSION_RE}' | head -n1; }}\n",
        ));
        o.push_str("if [ -f \"$tp_dec\" ]; then\n");
        o.push_str(
            "{ read -r tp_s tp_want tp_f; IFS= read -r tp_u; } < \"$tp_dec\"\nrm -f -- \"$tp_dec\"\n\
             if [ ! -f \"$tp_f\" ]; then case $tp_f in \"$tp_run\"/u-*) echo \"telepatch: binary_release $tp_name: $tp_f is missing (prepare did not fetch it)\" >&2;; *) echo \"telepatch: the cached download ${tp_s:0:12} for $tp_u is gone (a cache purge during the run?); run again to download it\" >&2;; esac; rc=1; fi\n",
        );
        o.push_str(&format!("tp_p={}\n", path_word(self.target())));
        o.push_str(
            "if [ \"$rc\" -eq 0 ]; then mkdir -p -- \"$(dirname -- \"$tp_p\")\" || rc=$?; fi\n",
        );
        let mut f = FileSpec::new(self.target());
        match &self.unpack_to {
            Some(_) => fetch::render_unpack(&f, self.strip.unwrap_or(0), &mut o),
            None => {
                f.mode = Some(0o755);
                fetch::render_place(&f, &mut o);
            }
        }
        let bin = path_word(&self.bin);
        o.push_str("case \"$tp_f\" in \"$tp_run\"/*) rm -f -- \"$tp_f\";; esac\nrmdir -- \"$tp_run\" 2>/dev/null\n");
        o.push_str(&format!(
            "if [ \"$rc\" -eq 0 ] && [ ! -x {bin} ]; then echo \"telepatch: binary_release $tp_name: \"{bin}\" is not an executable after the install\" >&2; rc=1; fi\n"
        ));
        if let Some(c) = &self.version {
            o.push_str(&format!(
                "if [ \"$rc\" -eq 0 ] && [ \"$tp_want\" != - ]; then\n  tp_bd=$(dirname -- {bin})\n  tp_v=$( ( PATH=\"$tp_bd:$PATH\"\n{c}\n) 2>/dev/null </dev/null | tp_tok)\n  if [ \"$tp_v\" != \"$tp_want\" ]; then echo \"telepatch: binary_release $tp_name: installed ${{tp_v:-unknown}}, wanted $tp_want\" >&2; rc=1; fi\nfi\n"
            ));
        }
        o.push_str("if [ \"$changed\" = 1 ]; then installed=(\"$tp_name\"); fi\nfi\n");
        o
    }
}

/// The first `{word}` in `url` other than `{version}` and `{arch}`.
fn unknown_placeholder(url: &str) -> Option<&str> {
    let mut rest = url;
    while let Some(i) = rest.find('{') {
        let after = &rest[i + 1..];
        let Some(j) = after.find('}') else { break };
        let w = &after[..j];
        if !w.is_empty()
            && w.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            && w != "version"
            && w != "arch"
        {
            return Some(w);
        }
        rest = &after[j + 1..];
    }
    None
}

/// The pipeline (stdin: the JSON; `$tp_file`: the file name; stdout: the
/// lower-cased sum or nothing) that reads [`Sum::Json`] with no `jq` or
/// python: newlines dropped, the text split at every `{` and `}` so each
/// flat object is one line, `"key" : "value"` spacing normalised, the
/// first line holding `"<filename_key>":"<file>"` kept, and the 64-hex
/// value of `"<sum_key>"` on it printed. It assumes what go.dev's
/// `?mode=json` (and the usual release listings) give: the file objects
/// hold no nested object, and no string value holds `{`, `}`, or an
/// escaped quote next to the two keys. The keys are checked at trace time
/// (letters, digits, `_`, `-`).
pub fn json_sum_shell(filename_key: &str, sum_key: &str) -> String {
    format!(
        "tr -d '\\r\\n' | tr '{{}}' '\\n\\n' | sed 's/\"[[:space:]]*:[[:space:]]*\"/\":\"/g' | grep -F -- \"\\\"{filename_key}\\\":\\\"$tp_file\\\"\" | head -n1 | sed -n 's/.*\"{sum_key}\":\"\\([0-9a-fA-F]\\{{64\\}}\\)\".*/\\1/p' | tr A-F a-f"
    )
}

/// The `br block` line of a missing downloader ([`fetch::NO_DOWNLOADER`]).
const NO_DL: &str = "  if [ -z \"$tp_dl\" ]; then echo \"br block neither curl nor wget is on the host\"; return 0; fi\n";

/// Why prepare blocks the host over a binary release: `why` is the
/// reason, and `no_downloader` says it is only that a download was needed
/// and neither curl nor wget is on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked {
    pub why: String,
    pub no_downloader: bool,
}

impl From<&str> for Blocked {
    fn from(why: &str) -> Blocked {
        Blocked {
            why: why.to_string(),
            no_downloader: false,
        }
    }
}

/// What prepare found for one binary-release op ([`BinaryRelease::prepare`]'s
/// output; `None` when the probe failed): settled (`Ok(None)`), the
/// prepare line (`pkg[binary_release] go: 1.27.2 installed, 1.27.3
/// wanted; fetched, sha256 ok`), or the reason the host is blocked.
pub fn prepared(
    window: usize,
    name: &str,
    out: Option<&str>,
) -> Result<Option<WillCreate>, Blocked> {
    let blocked = |why: &str| {
        Blocked::from(format!("pkg[binary_release] {name}: {why} (window {window})").as_str())
    };
    let no_dl = |why: &str| Blocked {
        no_downloader: true,
        ..blocked(why)
    };
    let Some(out) = out else {
        return Err(blocked("the host gave no answer"));
    };
    let field = |k: &str| {
        out.lines()
            .rev()
            .find_map(|l| l.strip_prefix(k))
            .map(str::trim)
    };
    if let Some(why) = field("br block ") {
        if why == fetch::NO_DOWNLOADER {
            return Err(no_dl(why));
        }
        return Err(blocked(why));
    }
    if out.lines().any(|l| l == "br settled") {
        return Ok(None);
    }
    let have = out
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("br have "))
        .unwrap_or("");
    let (flag, v) = have.split_once(' ').unwrap_or((have, ""));
    let installed = match (flag, v.trim()) {
        ("1", "") => "installed".to_string(),
        ("1", v) => format!("{v} installed"),
        _ => "not installed".to_string(),
    };
    let want = field("br want ").unwrap_or("");
    let sum = field("br sum ").unwrap_or("");
    let url = field("br url ").unwrap_or("");
    let item = Item {
        url: url.to_string(),
        sum: (!sum.is_empty()).then(|| sum.to_string()),
        unpack: false,
        retry: 0,
        timeout: None,
        targets: Vec::new(),
    };
    let state = match fetch::parse(std::slice::from_ref(&item), out).remove(0) {
        Found::Blocked(why) => return Err(blocked(&why)),
        Found::NoDownloader(why) => return Err(no_dl(&why)),
        Found::Cached => FetchState::Cached,
        Found::Fetched => FetchState::Fetched,
        Found::Staged => FetchState::Unverified,
    };
    if !out.lines().any(|l| l == "br change") {
        return Err(blocked("the host gave no answer"));
    }
    let wanted = if !want.is_empty() {
        format!("{want} wanted")
    } else if sum.len() >= 12 {
        format!("sha256 {} wanted", &sum[..12])
    } else {
        "present wanted".to_string()
    };
    Ok(Some(WillCreate {
        window,
        kind: "pkg[binary_release]".to_string(),
        name: name.to_string(),
        detail: Some(format!("{installed}, {wanted}; {}", state.words())),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str, stdin: &str) -> String {
        use std::io::Write;
        let mut c = std::process::Command::new("bash")
            .arg("-c")
            .arg(script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        c.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        String::from_utf8(c.wait_with_output().unwrap().stdout).unwrap()
    }

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

    #[test]
    fn the_json_sum_is_the_one_of_the_named_file_in_any_layout() {
        let pretty = format!(
            "[\n {{\n  \"version\": \"go1.27.3\",\n  \"files\": [\n   {{\n    \"filename\": \"go1.27.3.linux-arm64.tar.gz\",\n    \"sha256\": \"{A}\"\n   }},\n   {{\n    \"sha256\" : \"{B}\",\n    \"filename\" : \"go1.27.3.linux-amd64.tar.gz\",\n    \"size\": 1\n   }}\n  ]\n }}\n]\n"
        );
        let compact = format!(
            "[{{\"version\":\"go1.27.3\",\"files\":[{{\"filename\":\"go1.27.3.linux-amd64.tar.gz\",\"sha256\":\"{A}\"}},{{\"filename\":\"go1.27.3.linux-amd64.tar.gz.asc\",\"sha256\":\"{B}\"}}]}}]"
        );
        let run = |json: &str, file: &str| {
            sh(
                &format!(
                    "tp_file={}; {}",
                    quote(file),
                    json_sum_shell("filename", "sha256")
                ),
                json,
            )
            .trim()
            .to_string()
        };
        assert_eq!(
            run(&pretty, "go1.27.3.linux-amd64.tar.gz"),
            B.to_lowercase()
        );
        assert_eq!(run(&pretty, "go1.27.3.linux-arm64.tar.gz"), A);
        assert_eq!(
            run(&compact, "go1.27.3.linux-amd64.tar.gz"),
            A,
            "not the .asc"
        );
        assert_eq!(run(&compact, "go1.27.3.linux-amd64"), "", "an exact name");
        assert_eq!(run(&pretty, "go1.27.9.linux-amd64.tar.gz"), "");
    }

    #[test]
    fn placeholders() {
        assert_eq!(unknown_placeholder("http://x/{version}/{arch}"), None);
        assert_eq!(unknown_placeholder("http://x/{os}-{arch}"), Some("os"));
        assert_eq!(unknown_placeholder("http://x/{ not one }"), None);
        assert_eq!(unknown_placeholder("http://x/{"), None);
    }

    #[test]
    fn the_url_is_filled_in_on_the_host() {
        let s = format!(
            "tp_arch=arm64; tp_want=1.2.3\n{}echo \"$tp_url\"",
            BinaryRelease::subst_shell("tp_url", "http://x/v{version}/t-{version}-{arch}.tgz")
        );
        assert_eq!(sh(&s, "").trim(), "http://x/v1.2.3/t-1.2.3-arm64.tgz");
    }

    #[test]
    fn prepared_lines() {
        let ok = |out: &str| prepared(1, "go", Some(out)).unwrap().map(|w| w.line());
        assert_eq!(
            ok(
                "br have 1 1.27.2\nbr want 1.27.3\nbr url u\nbr sum s\nfetch 0 fetched\nbr change\n"
            ),
            Some(
                "pkg[binary_release] go: 1.27.2 installed, 1.27.3 wanted; fetched, sha256 ok"
                    .into()
            )
        );
        assert_eq!(
            ok(&format!(
                "br have 0 \nbr url u\nbr sum {A}\nfetch 0 cached\nbr change\n"
            )),
            Some(
                "pkg[binary_release] go: not installed, sha256 aaaaaaaaaaaa wanted; cached".into()
            )
        );
        assert_eq!(
            ok("br have 1 \nbr url u\nbr sum \nfetch 0 staged\nbr change\n"),
            Some("pkg[binary_release] go: installed, present wanted; fetched, not verified".into())
        );
        assert_eq!(ok("br have 1 1.0\nbr settled\n"), None);
        assert_eq!(
            prepared(
                2,
                "go",
                Some("br have 0 \nbr block could not resolve the latest version\n")
            ),
            Err("pkg[binary_release] go: could not resolve the latest version (window 2)".into())
        );
        assert_eq!(
            prepared(2, "go", Some("br have 0 \nbr url http://x/a\nbr sum \nfetch 0 notool\n")),
            Err(Blocked {
                why: "pkg[binary_release] go: fetch http://x/a: neither curl nor wget is on the host (window 2)".into(),
                no_downloader: true
            })
        );
        assert_eq!(
            prepared(
                2,
                "go",
                Some("br have 0 \nbr block neither curl nor wget is on the host\n")
            ),
            Err(Blocked {
                why: "pkg[binary_release] go: neither curl nor wget is on the host (window 2)"
                    .into(),
                no_downloader: true
            })
        );
        assert_eq!(
            prepared(2, "go", None),
            Err("pkg[binary_release] go: the host gave no answer (window 2)".into())
        );
        assert_eq!(
            prepared(
                2,
                "go",
                Some("br have 0 \nbr url u\nbr sum s\nfetch 0 fetched\n")
            ),
            Err("pkg[binary_release] go: the host gave no answer (window 2)".into())
        );
    }
}
