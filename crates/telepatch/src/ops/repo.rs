//! `repo(n)`: a package repository the host should have, from the role's
//! data. Root only (the files live under `/etc`).
//!
//! * apt, `.apt(AptRepo)`: a deb822 `/etc/apt/sources.list.d/<n>.sources`
//!   (`Types: deb`, `URIs`, `Suites`, `Components`, `Architectures` from
//!   `dpkg --print-architecture` on the host, `Signed-By:
//!   /etc/apt/keyrings/<n>.asc`) and the key, ASCII-armored as the vendor
//!   serves it (no `gpg --dearmor`), fetched with `curl -fsSL` into that
//!   keyring file (0644).
//! * dnf, `.dnf_url(url)`: the vendor's `.repo` file fetched into
//!   `/etc/yum.repos.d/<n>.repo`; `.dnf(DnfRepo)`: a `.repo` file written
//!   from the role's data.
//!
//! Idempotence: a file whose content is known (the apt source after the
//! architecture is read on the host, the `.dnf` body) is compared byte for
//! byte and rewritten when it differs. A fetched file (the key, the
//! `.dnf_url` file) is fetched only when absent, so a settled host makes no
//! network call; refreshing a rotated key is not done.
//!
//! The template does not install anything: a host without `curl` fails the
//! op with `telepatch: repo <n>: curl not found` (the role installs `curl`
//! and `ca-certificates` first, as its own `pkg` op).
//!
//! The family is checked at trace time from `h.os`: `.apt` on a non-Debian
//! host and `.dnf*` on a non-Red-Hat host are trace errors.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Deref;

use crate::facts::os::Family;
use crate::host::{Host, OpHandle, TraceError};
use crate::key::{Key, Value};
use crate::sh::quote;
use crate::trace::{OpSpec, Principal};

/// A deb822 apt source. Each list is space-joined into the field; none of
/// the words may contain whitespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AptRepo {
    pub uris: Vec<String>,
    pub suites: Vec<String>,
    pub components: Vec<String>,
    /// Where the ASCII-armored signing key is fetched from (http or https).
    pub key_url: String,
}

/// A dnf `.repo` file written from data (`gpgcheck=1`, `enabled=1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnfRepo {
    /// The human name (`name=`).
    pub name: String,
    pub baseurl: String,
    pub gpgkey: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoKind {
    /// No source given yet; the op fails when it runs.
    Unset,
    Apt(AptRepo),
    DnfUrl(String),
    Dnf(DnfRepo),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSpec {
    pub name: String,
    pub kind: RepoKind,
}

const FETCH: &str = "tp_fetch() {
  if ! command -v curl >/dev/null 2>&1; then
    echo \"telepatch: repo $tp_n: curl not found\" >&2
    return 1
  fi
  (umask 022; mkdir -p -- \"$(dirname -- \"$2\")\") || return 1
  tp_t=$(mktemp \"$2.XXXXXX\") || return 1
  if curl -fsSL -o \"$tp_t\" --url \"$1\" && chmod 0644 \"$tp_t\" && mv -f -- \"$tp_t\" \"$2\"; then
    return 0
  fi
  rm -f -- \"$tp_t\"
  return 1
}
";

const PUT: &str = "tp_put() {
  if [ -f \"$1\" ] && [ \"$(tp_render \"$2\" | sha256sum | cut -d' ' -f1)\" = \"$(sha256sum < \"$1\" | cut -d' ' -f1)\" ]; then return 0; fi
  (umask 022; mkdir -p -- \"$(dirname -- \"$1\")\") || return 1
  tp_t=$(mktemp \"$1.XXXXXX\") || return 1
  if tp_render \"$2\" > \"$tp_t\" && chmod 0644 \"$tp_t\" && mv -f -- \"$tp_t\" \"$1\"; then
    changed=1
    return 0
  fi
  rm -f -- \"$tp_t\"
  return 1
}
";

/// A test, true when the file `file` (already quoted) holds what
/// `tp_render <arg>` prints. Compared by `sha256sum` (coreutils): no `cmp`
/// on the host. A missing file is "not the same", quietly.
fn same_as(arg: &str, file: &str) -> String {
    format!(
        "{{ [ -f {file} ] && [ \"$(tp_render {arg} | sha256sum | cut -d' ' -f1)\" = \"$(sha256sum < {file} | cut -d' ' -f1)\" ]; }}"
    )
}

impl RepoSpec {
    pub fn new(name: impl Into<String>) -> RepoSpec {
        RepoSpec {
            name: name.into(),
            kind: RepoKind::Unset,
        }
    }

    pub fn key_path(&self) -> String {
        format!("/etc/apt/keyrings/{}.asc", self.name)
    }

    pub fn source_path(&self) -> String {
        format!("/etc/apt/sources.list.d/{}.sources", self.name)
    }

    pub fn dnf_path(&self) -> String {
        format!("/etc/yum.repos.d/{}.repo", self.name)
    }

    /// The files this op writes.
    pub fn paths(&self) -> Vec<String> {
        match &self.kind {
            RepoKind::Unset => vec![],
            RepoKind::Apt(_) => vec![self.key_path(), self.source_path()],
            RepoKind::DnfUrl(_) | RepoKind::Dnf(_) => vec![self.dnf_path()],
        }
    }

    pub fn default_writes(&self) -> BTreeSet<Key> {
        let mut w: BTreeSet<Key> = self.paths().iter().map(Key::file).collect();
        w.insert(Key::repo(&self.name));
        w
    }

    pub fn default_post(&self) -> BTreeMap<Key, Value> {
        self.default_writes()
            .into_iter()
            .map(|k| (k, Value::Exists(true)))
            .collect()
    }

    /// `tp_render() { ... }`: prints the file's content. The apt source
    /// takes the architecture as `$1`; the `.dnf` body ignores it.
    fn render_fn(&self) -> Option<String> {
        match &self.kind {
            RepoKind::Apt(a) => Some(format!(
                "tp_render() {{ printf 'Types: deb\\nURIs: %s\\nSuites: %s\\nComponents: %s\\nArchitectures: %s\\nSigned-By: %s\\n' {} {} {} \"$1\" {}; }}\n",
                quote(&a.uris.join(" ")),
                quote(&a.suites.join(" ")),
                quote(&a.components.join(" ")),
                quote(&self.key_path()),
            )),
            RepoKind::Dnf(d) => {
                let body = format!(
                    "[{}]\nname={}\nbaseurl={}\nenabled=1\ngpgcheck=1\ngpgkey={}\n",
                    self.name, d.name, d.baseurl, d.gpgkey
                );
                Some(format!("tp_render() {{ printf %s {}; }}\n", quote(&body)))
            }
            _ => None,
        }
    }

    pub fn render(&self) -> String {
        let mut o = format!("tp_n={}\n", quote(&self.name));
        match &self.kind {
            RepoKind::Unset => {
                o.push_str(
                    "echo \"telepatch: repo $tp_n: no source given (.apt, .dnf_url or .dnf)\" >&2\nrc=1\n",
                );
            }
            RepoKind::Apt(a) => {
                o.push_str(FETCH);
                o.push_str(PUT);
                o.push_str(&self.render_fn().unwrap_or_default());
                o.push_str(&format!(
                    "tp_key={}\ntp_src={}\n",
                    quote(&self.key_path()),
                    quote(&self.source_path())
                ));
                o.push_str(&format!(
                    "if [ ! -e \"$tp_key\" ]; then\n  if tp_fetch {} \"$tp_key\"; then changed=1; else rc=1; fi\nfi\n",
                    quote(&a.key_url)
                ));
                o.push_str(
                    "if [ \"$rc\" -eq 0 ]; then\n  tp_arch=$(dpkg --print-architecture)\n  if [ -z \"$tp_arch\" ]; then\n    echo \"telepatch: repo $tp_n: cannot read the dpkg architecture\" >&2\n    rc=1\n  elif ! tp_put \"$tp_src\" \"$tp_arch\"; then\n    rc=1\n  fi\nfi\n",
                );
            }
            RepoKind::DnfUrl(url) => {
                o.push_str(FETCH);
                o.push_str(&format!(
                    "tp_f={}\nif [ ! -e \"$tp_f\" ]; then\n  if tp_fetch {} \"$tp_f\"; then changed=1; else rc=1; fi\nfi\n",
                    quote(&self.dnf_path()),
                    quote(url)
                ));
            }
            RepoKind::Dnf(_) => {
                o.push_str(PUT);
                o.push_str(&self.render_fn().unwrap_or_default());
                o.push_str(&format!(
                    "tp_f={}\nif ! tp_put \"$tp_f\" ''; then rc=1; fi\n",
                    quote(&self.dnf_path())
                ));
            }
        }
        o
    }

    /// The probe lines for this spec: `add <n>` when a file is absent, or
    /// (apt source, `.dnf` body) differs. A source that cannot be rendered
    /// (no architecture) is unknown, so it adds nothing.
    fn probe(&self) -> String {
        let n = quote(&self.name);
        let mut s = String::from("tp_add=0\n");
        match &self.kind {
            RepoKind::Unset => return String::new(),
            RepoKind::Apt(_) => {
                s.push_str(&self.render_fn().unwrap_or_default());
                s.push_str(&format!("[ -e {} ] || tp_add=1\n", quote(&self.key_path())));
                s.push_str(&format!(
                    "tp_arch=$(dpkg --print-architecture 2>/dev/null) && [ -n \"$tp_arch\" ] && {{ {} || tp_add=1; }}\n",
                    same_as("\"$tp_arch\"", &quote(&self.source_path()))
                ));
            }
            RepoKind::DnfUrl(_) => {
                s.push_str(&format!("[ -e {} ] || tp_add=1\n", quote(&self.dnf_path())));
            }
            RepoKind::Dnf(_) => {
                s.push_str(&self.render_fn().unwrap_or_default());
                s.push_str(&format!(
                    "{} || tp_add=1\n",
                    same_as("''", &quote(&self.dnf_path()))
                ));
            }
        }
        s.push_str(&format!("[ \"$tp_add\" -eq 0 ] || echo \"add \"{n}\n"));
        s
    }
}

/// Handle for a `repo` op. Carries the host's family so the wrong builder
/// is a trace error.
#[derive(Clone)]
pub struct RepoHandle(OpHandle, Family);

impl Deref for RepoHandle {
    type Target = OpHandle;
    fn deref(&self) -> &OpHandle {
        &self.0
    }
}

crate::host::op_builders!(RepoHandle);

fn name_ok(n: &str) -> bool {
    let mut c = n.chars();
    c.next().is_some_and(|f| f.is_ascii_alphanumeric())
        && c.all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

fn word_ok(w: &str) -> bool {
    !w.is_empty() && !w.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn url_ok(u: &str) -> bool {
    word_ok(u) && (u.starts_with("https://") || u.starts_with("http://"))
}

fn line_ok(l: &str) -> bool {
    !l.is_empty() && !l.chars().any(char::is_control)
}

impl RepoHandle {
    #[track_caller]
    fn fail(&self, msg: String) -> Self {
        let name = self.spec_name();
        self.0
            .record_error(TraceError::new(format!("repo({name}): {msg}")));
        self.clone()
    }

    fn spec_name(&self) -> String {
        let mut name = String::new();
        self.0.edit(|op| {
            if let OpSpec::Repo(r) = &op.spec {
                name = r.name.clone();
            }
        });
        name
    }

    fn set(&self, k: RepoKind) -> Self {
        self.0.edit(|op| {
            if let OpSpec::Repo(r) = &mut op.spec {
                r.kind = k;
            }
            op.writes = op.spec.default_writes();
            op.post = op.spec.default_post();
        });
        self.clone()
    }

    /// An apt repository: the deb822 source and its key. Debian family
    /// only.
    #[track_caller]
    pub fn apt(&self, r: AptRepo) -> Self {
        if self.1 != Family::Debian {
            return self.fail(format!(
                ".apt() needs a Debian-family host (this host is {:?}); pick per h.os",
                self.1
            ));
        }
        for (what, list) in [
            ("uris", &r.uris),
            ("suites", &r.suites),
            ("components", &r.components),
        ] {
            if list.is_empty() || !list.iter().all(|w| word_ok(w)) {
                return self.fail(format!("{what} must be non-empty words without whitespace"));
            }
        }
        if !url_ok(&r.key_url) {
            return self.fail("key_url must be an http(s) URL without whitespace".into());
        }
        self.set(RepoKind::Apt(r))
    }

    /// A vendor `.repo` file fetched to `/etc/yum.repos.d/<n>.repo` when
    /// absent. Red Hat family only.
    #[track_caller]
    pub fn dnf_url(&self, url: &str) -> Self {
        if self.1 != Family::RedHat {
            return self.fail(format!(
                ".dnf_url() needs a Red Hat-family host (this host is {:?}); pick per h.os",
                self.1
            ));
        }
        if !url_ok(url) {
            return self.fail("the .repo URL must be an http(s) URL without whitespace".into());
        }
        self.set(RepoKind::DnfUrl(url.to_string()))
    }

    /// A `.repo` file written from data. Red Hat family only.
    #[track_caller]
    pub fn dnf(&self, r: DnfRepo) -> Self {
        if self.1 != Family::RedHat {
            return self.fail(format!(
                ".dnf() needs a Red Hat-family host (this host is {:?}); pick per h.os",
                self.1
            ));
        }
        if ![&r.name, &r.baseurl, &r.gpgkey].iter().all(|v| line_ok(v)) {
            return self.fail("name, baseurl and gpgkey must be non-empty single lines".into());
        }
        self.set(RepoKind::Dnf(r))
    }
}

impl<V> Host<V> {
    /// Ensure the package repository `name` is configured. Root only.
    #[track_caller]
    pub fn repo(&self, name: &str) -> RepoHandle {
        let h = self.op(OpSpec::Repo(RepoSpec::new(name)));
        let handle = RepoHandle(h, self.os.family.clone());
        if !name_ok(name) {
            handle.fail("the name must be letters, digits, '.', '_' or '-' and start with a letter or digit".into());
        }
        if let Principal::User(u) = &self.run_as().principal {
            handle.fail(format!(
                "is root only: as_user({u}) cannot write package sources"
            ));
        }
        handle
    }
}

/// A prepare probe for the window's `repo` ops: prints `add <n>` for each
/// repo with a file absent or (apt source, `.dnf` body) different.
pub fn probe(specs: &[&RepoSpec]) -> String {
    specs.iter().map(|s| s.probe()).collect()
}

/// The repo names [`probe`]'s output lists, in order, without repeats.
pub fn findings(output: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in output.lines().filter_map(|l| l.strip_prefix("add ")) {
        if !out.iter().any(|o| o == n) {
            out.push(n.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn findings_parse_probe_output() {
        assert_eq!(findings("add a\nnoise\nadd b c\nadd a\n"), ["a", "b c"]);
    }

    #[test]
    fn names() {
        assert!(name_ok("docker") && name_ok("docker-ce.2_x"));
        assert!(!name_ok("") && !name_ok("../x") && !name_ok("-x") && !name_ok("a b"));
    }
}
