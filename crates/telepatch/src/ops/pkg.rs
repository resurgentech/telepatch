//! `pkg`: install packages with the host's manager.
//!
//! Managers follow Ansible's modules (apt.py, dnf.py, pip.py in
//! ansible/ansible `lib/ansible/modules/`): apt runs
//! `DEBIAN_FRONTEND=noninteractive apt-get -y -o Dpkg::Options::=--force-confdef
//! -o Dpkg::Options::=--force-confold install ...` and leaves recommends at
//! apt's default; dnf/yum use `install -y` with weak deps at default; pacman
//! uses `-S --needed --noconfirm`; pip runs `python3 -m pip` (falling back
//! to `pip3`) and `.latest()` is `install -U`.
//!
//! Every template installs only what is missing and sets `changed` iff it
//! installed something. The set is data ([`PkgSpec::names`]) so T8 can
//! merge ops of the same host and manager into a union.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Deref;

use crate::facts::os::Family;
use crate::host::{Host, OpHandle, TraceError};
use crate::key::{Key, Value};
use crate::sh::quote;
use crate::trace::{Node, OpNode, OpSpec, Principal};

mod cargo;
mod mise;
mod rustup;
mod script;
pub use cargo::{
    CargoProbe, parse_probe as parse_cargo_probe, probe as cargo_probe,
    will_changes as cargo_will_changes,
};
pub use mise::{
    MiseProbe, parse_probe as parse_mise_probe, probe as mise_probe,
    will_changes as mise_will_changes,
};
pub use rustup::{
    DEFAULT_INSTALLER, RustupProbe, RustupSpec, parse_probe as parse_rustup_probe,
    probe as rustup_probe, will_change as rustup_will_change,
};
pub use script::{Latest, Probed, ScriptSpec, VERSION_RE, parse_probe, version_token, will_change};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Manager {
    Apt,
    Dnf,
    Yum,
    Pacman,
}

/// The system config the mise source maintains.
pub const MISE_CONFIG: &str = "/etc/mise/config.toml";

/// Where a package comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The host's system manager.
    Distro(Manager),
    Pip,
    /// `cargo install`, per user ([`cargo`]).
    Cargo,
    /// Tools in mise's system layer, root only ([`mise`]).
    Mise,
    /// A rust toolchain with targets and components, per user
    /// ([`rustup`]; boxed to keep `OpNode` small).
    Rustup(Box<RustupSpec>),
    /// A vendor installer script ([`ScriptSpec`]; boxed to keep `OpNode`
    /// small).
    Script(Box<ScriptSpec>),
}

/// Which version of a package is wanted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wanted {
    /// Any installed version.
    Present,
    Pinned(String),
    Latest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PkgMode {
    /// Install what is missing.
    Install,
    /// `apt-get build-dep` (apt only).
    BuildDep,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkgSpec {
    pub source: Source,
    pub names: Vec<String>,
    pub wanted: Wanted,
    pub mode: PkgMode,
    /// Run `apt-get update` first (set by [`apply_apt_update`]). Ignored by
    /// other managers.
    pub update_cache: bool,
    /// The op's principal (`root` for `h` and `as_user("root")`), set when
    /// the op is pushed. Names the per-user sources' keys
    /// (`pkg:script:<user>:<name>`, `pkg:cargo:<user>:<name>`,
    /// `pkg:rustup:<user>:<toolchain>`); unused by distro and pip.
    pub user: String,
}

impl PkgSpec {
    pub fn new(manager: Manager, names: Vec<String>) -> PkgSpec {
        PkgSpec::from_source(Source::Distro(manager), names)
    }

    pub fn from_source(source: Source, names: Vec<String>) -> PkgSpec {
        let mut seen = BTreeSet::new();
        let names = names
            .into_iter()
            .filter(|n| seen.insert(n.clone()))
            .collect();
        PkgSpec {
            source,
            names,
            wanted: Wanted::Present,
            mode: PkgMode::Install,
            update_cache: false,
            user: "root".to_string(),
        }
    }

    /// The effect key of one name. Non-distro names live in their own
    /// namespace (`pkg:pip:<name>`) so they never overlap or merge with
    /// system names. Script, cargo and rustup keys name the
    /// principal (`pkg:script:<user>:<name>`); a rustup toolchain's targets
    /// and components are `<tc>/target/<t>` and `<tc>/component/<c>`.
    pub fn key_of(&self, name: &str) -> Key {
        match &self.source {
            Source::Distro(_) => Key::pkg(name),
            Source::Pip => Key::pkg(format!("pip:{name}")),
            Source::Cargo => Key::pkg(format!("cargo:{}:{name}", self.user)),
            Source::Mise => Key::pkg(format!("mise:root:{name}")),
            Source::Rustup(_) => Key::pkg(format!("rustup:{}:{name}", self.user)),
            Source::Script(_) => Key::pkg(format!("script:{}:{name}", self.user)),
        }
    }

    /// Default write set: the names' keys for distro and pip, `*` for
    /// script, cargo and rustup (they write files nobody listed; A14).
    pub fn default_writes(&self) -> BTreeSet<Key> {
        match &self.source {
            Source::Script(_) | Source::Cargo | Source::Mise | Source::Rustup(_) => {
                BTreeSet::from([Key::Any])
            }
            _ => self.keys(),
        }
    }

    /// Default postconditions: every name `Installed`; a script op also
    /// posts `file:<bin> = Exists(true)`, a rustup op each target and
    /// component.
    pub fn default_post(&self) -> BTreeMap<Key, Value> {
        let mut post: BTreeMap<Key, Value> = self
            .names
            .iter()
            .map(|n| (self.key_of(n), Value::Installed))
            .collect();
        if let Source::Script(s) = &self.source {
            post.insert(Key::file(&s.bin), Value::Exists(true));
        }
        if self.source == Source::Mise {
            post.insert(Key::file(MISE_CONFIG), Value::Exists(true));
        }
        if let (Source::Rustup(r), Some(tc)) = (&self.source, self.names.first()) {
            for t in &r.targets {
                post.insert(self.key_of(&format!("{tc}/target/{t}")), Value::Installed);
            }
            for c in &r.components {
                post.insert(
                    self.key_of(&format!("{tc}/component/{c}")),
                    Value::Installed,
                );
            }
        }
        post
    }

    /// The plan kind: `pkg` for distro and pip, `pkg[script]` for script,
    /// `pkg[cargo]` for cargo, `pkg[rustup]` for rustup.
    pub fn kind(&self) -> &'static str {
        match &self.source {
            Source::Script(_) => "pkg[script]",
            Source::Cargo => "pkg[cargo]",
            Source::Mise => "pkg[mise]",
            Source::Rustup(_) => "pkg[rustup]",
            _ => "pkg",
        }
    }

    pub fn keys(&self) -> BTreeSet<Key> {
        self.names.iter().map(|n| self.key_of(n)).collect()
    }

    /// May this op ever be folded into another? `Script` and `Rustup` ops
    /// never merge (one toolchain per op, with its own options); a
    /// pinned `Cargo` op has exactly one name (the pin is for that name).
    pub fn mergeable(&self) -> bool {
        match &self.source {
            Source::Script(_) | Source::Rustup(_) => false,
            Source::Cargo | Source::Mise => !matches!(self.wanted, Wanted::Pinned(_)),
            _ => true,
        }
    }

    /// Can `other` be folded into this op (same source, wanted and mode)?
    pub fn can_merge(&self, other: &PkgSpec) -> bool {
        self.mergeable()
            && other.mergeable()
            && self.source == other.source
            && self.wanted == other.wanted
            && self.mode == other.mode
    }

    /// Union of names, keeping first-seen order. `update_cache` is or-ed.
    pub fn merge(&mut self, other: &PkgSpec) {
        for n in &other.names {
            if !self.names.contains(n) {
                self.names.push(n.clone());
            }
        }
        self.update_cache |= other.update_cache;
    }

    pub fn render(&self) -> String {
        if let Source::Script(s) = &self.source {
            let name = self.names.first().map(String::as_str).unwrap_or("");
            return s.render(name, &self.wanted);
        }
        if self.source == Source::Cargo {
            return cargo::render(&self.names, &self.wanted);
        }
        if self.source == Source::Mise {
            return mise::render(&self.names, &self.wanted);
        }
        if let Source::Rustup(r) = &self.source {
            let name = self.names.first().map(String::as_str).unwrap_or("");
            return rustup::render(name, &self.wanted, r);
        }
        let names: Vec<String> = self
            .names
            .iter()
            .map(|n| match &self.wanted {
                Wanted::Pinned(v) if self.source == Source::Pip => quote(&format!("{n}=={v}")),
                _ => quote(n),
            })
            .collect();
        let mut o = format!(
            "rc=0; changed=0; installed=()\ntp_want=({})\ntp_missing=()\n",
            names.join(" ")
        );
        match (&self.source, self.mode) {
            (Source::Distro(Manager::Apt), PkgMode::BuildDep) => self.apt_build_dep(&mut o),
            (Source::Distro(Manager::Apt), _) => self.apt(&mut o),
            (Source::Distro(Manager::Dnf), _) => {
                rpm_like(&mut o, "dnf", "dnf group list --installed")
            }
            (Source::Distro(Manager::Yum), _) => rpm_like(&mut o, "yum", "yum grouplist installed"),
            (Source::Distro(Manager::Pacman), _) => pacman(&mut o),
            (Source::Pip, _) => match &self.wanted {
                Wanted::Latest => pip_latest(&mut o),
                Wanted::Pinned(_) => pip_pinned(&mut o),
                Wanted::Present => pip(&mut o),
            },
            // Rendered above; every source is built.
            (Source::Cargo | Source::Mise | Source::Rustup(_) | Source::Script(_), _) => {
                unreachable!()
            }
        }
        o
    }

    fn apt_update_block(&self, o: &mut String) {
        if self.update_cache {
            o.push_str("apt-get -q update || rc=$?\n");
        }
    }

    fn apt(&self, o: &mut String) {
        o.push_str("export DEBIAN_FRONTEND=noninteractive\n");
        self.apt_update_block(o);
        o.push_str(
            "for tp_n in \"${tp_want[@]}\"; do\n  [ \"$(dpkg-query -W -f='${db:Status-Status}' \"$tp_n\" 2>/dev/null)\" = installed ] || tp_missing+=(\"$tp_n\")\ndone\n",
        );
        o.push_str("if [ \"$rc\" -eq 0 ] && [ \"${#tp_missing[@]}\" -gt 0 ]; then\n");
        if !self.update_cache {
            o.push_str(APT_LISTS_GUARD);
        }
        o.push_str(
            "  if [ \"$rc\" -eq 0 ]; then\n    apt-get -y -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold install \"${tp_missing[@]}\"\n    rc=$?\n    if [ \"$rc\" -eq 0 ]; then changed=1; installed=(\"${tp_missing[@]}\"); fi\n  fi\nfi\n",
        );
    }

    /// Whether build-dep installed anything cannot be checked up front, so
    /// it always runs and `changed` follows apt's "N newly installed" line.
    fn apt_build_dep(&self, o: &mut String) {
        o.push_str("export DEBIAN_FRONTEND=noninteractive\n");
        self.apt_update_block(o);
        o.push_str("if [ \"$rc\" -eq 0 ]; then\n  :\n");
        if !self.update_cache {
            o.push_str(APT_LISTS_GUARD);
        }
        o.push_str(
            "fi\nif [ \"$rc\" -eq 0 ]; then\n  tp_out=$(mktemp)\n  LC_ALL=C apt-get -y -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold build-dep \"${tp_want[@]}\" 2>&1 | tee \"$tp_out\"\n  rc=${PIPESTATUS[0]}\n  if [ \"$rc\" -eq 0 ] && grep -Eq '[1-9][0-9]* newly installed' \"$tp_out\"; then changed=1; fi\n  rm -f \"$tp_out\"\nfi\n",
        );
    }
}

/// Runtime guard: update when apt has no package lists at all.
const APT_LISTS_GUARD: &str = "  if ! find /var/lib/apt/lists -maxdepth 1 -type f ! -name lock 2>/dev/null | grep -q .; then apt-get -q update || rc=$?; fi\n";

/// dnf and yum. Plain names and provides (`perl(ExtUtils::Embed)`) are
/// checked with `rpm -q --whatprovides`; `@group` names against the
/// manager's installed-groups list.
fn rpm_like(o: &mut String, mgr: &str, group_list: &str) {
    o.push_str(&format!(
        "for tp_n in \"${{tp_want[@]}}\"; do\n  case \"$tp_n\" in\n    @*) {group_list} 2>/dev/null | grep -qF -- \"${{tp_n#@}}\" || tp_missing+=(\"$tp_n\");;\n    *) rpm -q --whatprovides \"$tp_n\" >/dev/null 2>&1 || tp_missing+=(\"$tp_n\");;\n  esac\ndone\nif [ \"${{#tp_missing[@]}}\" -gt 0 ]; then\n  {mgr} install -y \"${{tp_missing[@]}}\"\n  rc=$?\n  if [ \"$rc\" -eq 0 ]; then changed=1; installed=(\"${{tp_missing[@]}}\"); fi\nfi\n"
    ));
}

/// `pacman -T` is satisfied by installed packages and their provides; the
/// group query covers group names.
fn pacman(o: &mut String) {
    o.push_str(
        "for tp_n in \"${tp_want[@]}\"; do\n  if [ -z \"$(pacman -T \"$tp_n\" 2>/dev/null)\" ] || [ -n \"$(pacman -Qqg \"$tp_n\" 2>/dev/null)\" ]; then :; else tp_missing+=(\"$tp_n\"); fi\ndone\nif [ \"${#tp_missing[@]}\" -gt 0 ]; then\n  pacman -S --needed --noconfirm \"${tp_missing[@]}\"\n  rc=$?\n  if [ \"$rc\" -eq 0 ]; then changed=1; installed=(\"${tp_missing[@]}\"); fi\nfi\n",
    );
}

const PIP_PICK: &str = "if python3 -m pip --version >/dev/null 2>&1; then tp_pip=(python3 -m pip)\nelif command -v pip3 >/dev/null 2>&1; then tp_pip=(pip3)\nelse echo 'telepatch: pip not found' >&2; rc=127; fi\n";

fn pip(o: &mut String) {
    o.push_str(PIP_PICK);
    o.push_str(
        "if [ \"$rc\" -eq 0 ]; then\n  for tp_n in \"${tp_want[@]}\"; do\n    \"${tp_pip[@]}\" show \"$tp_n\" >/dev/null 2>&1 || tp_missing+=(\"$tp_n\")\n  done\n  if [ \"${#tp_missing[@]}\" -gt 0 ]; then\n    \"${tp_pip[@]}\" install \"${tp_missing[@]}\"\n    rc=$?\n    if [ \"$rc\" -eq 0 ]; then changed=1; installed=(\"${tp_missing[@]}\"); fi\n  fi\nfi\n",
    );
}

/// Pinned: `tp_want` holds `name==version`; a name is missing when its
/// installed version is not the pinned one, and `install name==version`
/// then moves it (up or down).
fn pip_pinned(o: &mut String) {
    o.push_str(PIP_PICK);
    o.push_str(
        "if [ \"$rc\" -eq 0 ]; then\n  for tp_n in \"${tp_want[@]}\"; do\n    [ \"$(\"${tp_pip[@]}\" show \"${tp_n%%==*}\" 2>/dev/null | sed -n 's/^Version: //p')\" = \"${tp_n#*==}\" ] || tp_missing+=(\"$tp_n\")\n  done\n  if [ \"${#tp_missing[@]}\" -gt 0 ]; then\n    \"${tp_pip[@]}\" install \"${tp_missing[@]}\"\n    rc=$?\n    if [ \"$rc\" -eq 0 ]; then changed=1; installed=(\"${tp_missing[@]}\"); fi\n  fi\nfi\n",
    );
}

/// `install -U` always runs; a name counts as installed when its version
/// differs before and after.
fn pip_latest(o: &mut String) {
    o.push_str(PIP_PICK);
    o.push_str(
        "if [ \"$rc\" -eq 0 ]; then\n  tp_ver() { \"${tp_pip[@]}\" show \"$1\" 2>/dev/null | sed -n 's/^Version: //p'; }\n  declare -A tp_before\n  for tp_n in \"${tp_want[@]}\"; do tp_before[$tp_n]=$(tp_ver \"$tp_n\"); done\n  \"${tp_pip[@]}\" install -U \"${tp_want[@]}\"\n  rc=$?\n  if [ \"$rc\" -eq 0 ]; then\n    for tp_n in \"${tp_want[@]}\"; do\n      if [ \"$(tp_ver \"$tp_n\")\" != \"${tp_before[$tp_n]}\" ]; then installed+=(\"$tp_n\"); changed=1; fi\n    done\n  fi\nfi\n",
    );
}

/// For each op in order, does an apt op need `apt-get update` first?
///
/// A non-apt op that may have written apt sources (`writes *` or any
/// `file:/etc/apt/...` key) makes the cache stale; the next apt pkg op then
/// updates, and later apt ops do not until sources are written again.
/// Entries for non-pkg or non-apt ops are `false`. Call on the final
/// (merged) op list.
pub fn apt_update_needed(ops: &[OpNode]) -> Vec<bool> {
    apt_update_flags(false, ops).0
}

/// [`apt_update_needed`] starting from a known cache state (`stale`), also
/// returning the state after `ops`, so a caller handling ops in several
/// windows can carry it over.
pub fn apt_update_flags(mut stale: bool, ops: &[OpNode]) -> (Vec<bool>, bool) {
    let flags = ops
        .iter()
        .map(|op| match &op.spec {
            OpSpec::Pkg(p) if p.source == Source::Distro(Manager::Apt) => {
                std::mem::take(&mut stale)
            }
            _ => {
                stale |= op.writes.iter().any(writes_apt_sources);
                false
            }
        })
        .collect();
    (flags, stale)
}

/// `repo` ops need no case of their own: the key and source files they
/// write are under `/etc/apt/`.
fn writes_apt_sources(k: &Key) -> bool {
    match k {
        Key::Any => true,
        Key::File(p) => p.starts_with("/etc/apt/"),
        _ => false,
    }
}

/// Apply [`apt_update_needed`] to a node list (e.g. a trace's nodes),
/// setting `update_cache` on the apt ops that need it. Non-op nodes are
/// skipped.
pub fn apply_apt_update(nodes: &mut [Node]) {
    let ops: Vec<OpNode> = nodes
        .iter()
        .filter_map(|n| {
            if let Node::Op(o) = n {
                Some(o.clone())
            } else {
                None
            }
        })
        .collect();
    let mut flags = apt_update_needed(&ops).into_iter();
    for n in nodes.iter_mut() {
        if let Node::Op(o) = n {
            let f = flags.next().unwrap_or(false);
            if let OpSpec::Pkg(p) = &mut o.spec {
                p.update_cache = f;
            }
        }
    }
}

/// Handle for a `pkg` op.
#[derive(Clone)]
pub struct PkgHandle(OpHandle);

impl Deref for PkgHandle {
    type Target = OpHandle;
    fn deref(&self) -> &OpHandle {
        &self.0
    }
}

impl PkgHandle {
    fn spec(&self, f: impl FnOnce(&mut PkgSpec)) {
        self.0.edit(|op| {
            if let OpSpec::Pkg(s) = &mut op.spec {
                f(s);
            }
        });
    }

    /// Install the build dependencies of the named source packages instead
    /// (`apt-get build-dep`). Which packages that installs is unknown, so
    /// the op writes `*` and makes no `Installed` postcondition.
    pub fn build_dep(&self) -> &Self {
        let mut apt = true;
        self.spec(|s| {
            if s.source == Source::Distro(Manager::Apt) {
                s.mode = PkgMode::BuildDep;
            } else {
                apt = false;
            }
        });
        if !apt {
            self.0.record_error(TraceError::new(
                "build_dep() is only available on apt hosts",
            ));
            return self;
        }
        self.0.edit(|op| {
            op.writes = BTreeSet::from([Key::Any]);
            op.post.clear();
        });
        self
    }

    /// Run `apt-get update` before this op (normally set by the apt cache
    /// pass, [`apply_apt_update`]).
    pub fn update_cache(&self, on: bool) -> &Self {
        self.spec(|s| s.update_cache = on);
        self
    }

    /// Want the newest release (pip: `install -U`). Not supported on
    /// distro packages.
    pub fn latest(&self) -> &Self {
        self.want(Wanted::Latest)
    }

    /// Want exactly version `v` (pip: `name==v`). Not supported on distro
    /// packages.
    pub fn pin(&self, v: impl Into<String>) -> &Self {
        self.want(Wanted::Pinned(v.into()))
    }

    /// Edit a rustup op's options, then add the postconditions of what was
    /// added. On any other source: a trace error naming `method`.
    fn rustup_opt(&self, method: &str, f: impl FnOnce(&mut RustupSpec)) -> &Self {
        let mut rustup = true;
        self.spec(|s| match &mut s.source {
            Source::Rustup(r) => f(r),
            _ => rustup = false,
        });
        if !rustup {
            self.0.record_error(TraceError::new(format!(
                "{method}() is only available on rustup ops"
            )));
            return self;
        }
        self.0.edit(|op| {
            let OpSpec::Pkg(p) = &op.spec else { return };
            for (k, v) in p.default_post() {
                op.post.entry(k).or_insert(v);
            }
        });
        self
    }

    /// Rustup: also have these targets installed (`rustup target add`).
    pub fn targets<I, S>(&self, targets: I) -> &Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let v = names(targets);
        if let Err(e) = check_names(&v) {
            self.0.record_error(e);
        }
        self.rustup_opt("targets", |r| RustupSpec::add(&mut r.targets, v))
    }

    /// Rustup: also have these components installed (`rustup component
    /// add`).
    pub fn components<I, S>(&self, components: I) -> &Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let v = names(components);
        if let Err(e) = check_names(&v) {
            self.0.record_error(e);
        }
        self.rustup_opt("components", |r| RustupSpec::add(&mut r.components, v))
    }

    /// Rustup: the install profile (default `default`).
    pub fn profile(&self, profile: impl Into<String>) -> &Self {
        let p = profile.into();
        if let Err(e) = check_names(std::slice::from_ref(&p)) {
            self.0.record_error(e);
        }
        self.rustup_opt("profile", |r| r.profile = Some(p))
    }

    /// Rustup: make this toolchain the default (`rustup default`), when it
    /// is not already.
    pub fn set_default(&self) -> &Self {
        self.rustup_opt("set_default", |r| r.set_default = true)
    }

    /// Rustup: the shell command that installs rustup when it is absent;
    /// `--default-toolchain <tc> --profile <p>` is appended. Default:
    /// [`DEFAULT_INSTALLER`].
    pub fn installer(&self, cmd: impl Into<String>) -> &Self {
        let c = cmd.into();
        self.rustup_opt("installer", |r| r.installer = Some(c))
    }

    fn want(&self, w: Wanted) -> &Self {
        let mut distro = false;
        let mut script_err = None;
        let mut cargo_pin_many = false;
        let mut mise_pin_many = false;
        let mut rustup_pin = false;
        self.spec(|s| {
            if matches!(s.source, Source::Distro(_)) {
                distro = true;
                return;
            }
            if let (Source::Script(sc), Wanted::Latest) = (&s.source, &w) {
                let name = s.names.first().map(String::as_str).unwrap_or("");
                script_err = sc.latest_problem(name);
            }
            if matches!(s.source, Source::Rustup(_)) && matches!(w, Wanted::Pinned(_)) {
                rustup_pin = true;
                return;
            }
            cargo_pin_many =
                s.source == Source::Cargo && matches!(w, Wanted::Pinned(_)) && s.names.len() != 1;
            mise_pin_many =
                s.source == Source::Mise && matches!(w, Wanted::Pinned(_)) && s.names.len() != 1;
            s.wanted = w;
        });
        if mise_pin_many {
            self.0.record_error(TraceError::new(
                "mise: a pinned op takes exactly one package name",
            ));
        }
        if rustup_pin {
            self.0.record_error(TraceError::new(
                "rustup: Pinned is not supported; name a version toolchain instead (e.g. \"1.94.0\")",
            ));
        }
        if cargo_pin_many {
            self.0.record_error(TraceError::new(
                "cargo: a pinned op takes exactly one package name",
            ));
        }
        if distro {
            self.0.record_error(TraceError::new(
                "pin or latest on distro packages is not supported",
            ));
        }
        if let Some(e) = script_err {
            self.0.record_error(TraceError::new(e));
        }
        self
    }
}

/// Every source returns the same handle; kept as an alias for `h.pip`.
pub type PipHandle = PkgHandle;

fn names<I, S>(names: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    names.into_iter().map(|s| s.as_ref().to_string()).collect()
}

fn check_names(v: &[String]) -> Result<(), TraceError> {
    match v
        .iter()
        .find(|n| n.is_empty() || n.chars().any(|c| c.is_control()))
    {
        Some(n) => Err(TraceError::new(format!("invalid package name {n:?}"))),
        None => Ok(()),
    }
}

impl<V> Host<V> {
    /// Invalid input is recorded as a trace error; the node is still pushed
    /// (so the handle is valid) but the run fails before anything applies.
    fn pkg_op(&self, manager: Manager, v: Vec<String>) -> PkgHandle {
        self.pkg_spec_op(PkgSpec::new(manager, v))
    }

    /// Push a pkg op for any source.
    pub(crate) fn pkg_spec_op(&self, mut spec: PkgSpec) -> PkgHandle {
        if let Err(e) = check_names(&spec.names) {
            self.record_error(e);
        }
        if matches!(spec.source, Source::Rustup(_)) && spec.names.len() != 1 {
            self.record_error(TraceError::new(
                "rustup: exactly one toolchain name per rustup op",
            ));
        }
        if let Source::Script(s) = &spec.source {
            let name = spec.names.first().map(String::as_str).unwrap_or("");
            if spec.names.len() != 1 {
                self.record_error(TraceError::new(format!(
                    "script {name}: exactly one package name per script op"
                )));
            }
            for e in s.problems(name, &spec.wanted) {
                self.record_error(TraceError::new(e));
            }
        }
        spec.user = match &self.run_as().principal {
            Principal::Root => "root".to_string(),
            Principal::User(u) => u.clone(),
        };
        if spec.source == Source::Mise && spec.user != "root" {
            self.record_error(TraceError::new("mise source is system-wide (root only)"));
        }
        PkgHandle(self.op(OpSpec::Pkg(spec)))
    }

    /// Ensure packages are installed with the host's manager: apt on
    /// Debian-family, dnf on RedHat-family (Fedora, EL, Amazon Linux 2023),
    /// pacman on Arch. An unknown family is recorded as a trace error.
    pub fn pkg<I, S>(&self, pkgs: I) -> PkgHandle
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let m = match &self.os.family {
            Family::Debian => Manager::Apt,
            Family::RedHat => Manager::Dnf,
            Family::Arch => Manager::Pacman,
            Family::Other(f) => {
                self.record_error(TraceError::new(format!(
                    "pkg(): no package manager known for os family {f:?}"
                )));
                Manager::Apt
            }
        };
        self.pkg_op(m, names(pkgs))
    }

    /// Like [`Host::pkg`] but with `yum`, for RedHat-family hosts without dnf.
    pub fn yum<I, S>(&self, pkgs: I) -> PkgHandle
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.pkg_op(Manager::Yum, names(pkgs))
    }

    /// A package installed by a vendor script: `name` is the package name
    /// (one per op), `spec` says how to probe, install and update it. Keys
    /// `pkg:script:<user>:<name>`; writes `*` unless narrowed with
    /// `.writes([..])`; posts `Installed` and `file:<bin> = Exists(true)`.
    /// Missing required parts are trace errors.
    pub fn script(&self, name: impl AsRef<str>, spec: ScriptSpec) -> PkgHandle {
        self.pkg_spec_op(PkgSpec::from_source(
            Source::Script(Box::new(spec)),
            vec![name.as_ref().to_string()],
        ))
    }

    /// Crates installed with `cargo install`, for the op's principal
    /// (`pkg:cargo:<user>:<name>` keys); writes `*` unless narrowed with
    /// `.writes([..])`; posts `Installed` per name. `.pin(v)` takes one
    /// name; `.latest()` asks `cargo search` on the host.
    pub fn cargo<I, S>(&self, crates: I) -> PkgHandle
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.pkg_spec_op(PkgSpec::from_source(Source::Cargo, names(crates)))
    }

    /// Tools installed by `mise` into its system layer (config
    /// `/etc/mise/config.toml`, installs and shims under
    /// `/usr/local/share/mise`), root only: on a non-root principal this is
    /// a trace error ("mise source is system-wide (root only)").
    /// `Present` and `.latest()` write `"latest"` into the config, `.pin(v)`
    /// writes `v`. Keys `pkg:mise:root:<name>`; writes `*` unless narrowed
    /// with `.writes([..])`; posts `Installed` per name and
    /// `file:/etc/mise/config.toml = Exists(true)`. `.pin(v)` takes one
    /// name; `.latest()` asks `mise latest` on the host. Users reach the
    /// tools through `/usr/local/share/mise/shims` on their `PATH`.
    pub fn mise<I, S>(&self, tools: I) -> PkgHandle
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.pkg_spec_op(PkgSpec::from_source(Source::Mise, names(tools)))
    }

    /// A rust toolchain (`stable`, `1.94.0`, `nightly-2026-10-01`) for the
    /// op's principal, with options on the handle (`.targets`,
    /// `.components`, `.profile`, `.set_default`, `.installer`). Keys
    /// `pkg:rustup:<user>:<tc>` plus `...:<tc>/target/<t>` and
    /// `...:<tc>/component/<c>`; writes `*` unless narrowed. `.latest()`
    /// asks `rustup check`; `.pin(..)` is a trace error (name a version
    /// toolchain instead).
    pub fn rustup(&self, toolchain: impl AsRef<str>) -> PkgHandle {
        self.pkg_spec_op(PkgSpec::from_source(
            Source::Rustup(Box::default()),
            vec![toolchain.as_ref().to_string()],
        ))
    }

    /// Python packages via pip (`pkg:pip:<name>` keys).
    pub fn pip<I, S>(&self, pkgs: I) -> PipHandle
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.pkg_spec_op(PkgSpec::from_source(Source::Pip, names(pkgs)))
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::*;
    use crate::facts::os::Os;

    fn host() -> Host<()> {
        let os = Os::parse("ID=debian\nVERSION_ID=\"12\"\n").unwrap();
        Host::new("h", os, Rc::new(()))
    }

    fn spec_of(h: &Host<()>, i: usize) -> PkgSpec {
        match &h.snapshot().nodes[i] {
            Node::Op(OpNode {
                spec: OpSpec::Pkg(p),
                ..
            }) => p.clone(),
            n => panic!("not a pkg op: {n:?}"),
        }
    }

    fn errors(h: &Host<()>) -> Vec<String> {
        h.snapshot().errors.iter().map(|e| e.0.clone()).collect()
    }

    #[test]
    fn pip_pin_renders_name_eq_version() {
        let h = host();
        h.pip(["requests", "six"]).pin("2.31.0");
        let s = spec_of(&h, 0);
        assert_eq!(s.wanted, Wanted::Pinned("2.31.0".into()));
        let r = s.render();
        assert!(
            r.contains("tp_want=('requests==2.31.0' 'six==2.31.0')"),
            "{r}"
        );
        assert!(errors(&h).is_empty());
    }

    #[test]
    fn pip_latest_renders_install_u() {
        let h = host();
        h.pip(["a"]).latest();
        let s = spec_of(&h, 0);
        assert_eq!(s.wanted, Wanted::Latest);
        let mut want =
            String::from("rc=0; changed=0; installed=()\ntp_want=('a')\ntp_missing=()\n");
        pip_latest(&mut want);
        assert_eq!(s.render(), want);
        assert!(s.render().contains("install -U"));
    }

    #[test]
    fn distro_pin_or_latest_is_a_trace_error() {
        let h = host();
        h.pkg(["git"]).pin("1");
        h.pkg(["git"]).latest();
        let msg = "pin or latest on distro packages is not supported";
        assert_eq!(errors(&h), [msg, msg]);
        assert_eq!(spec_of(&h, 0).wanted, Wanted::Present);
    }

    #[test]
    fn script_never_merges() {
        let a = PkgSpec::from_source(Source::Script(Box::default()), names(["x"]));
        assert!(!a.can_merge(&a.clone()));
        let p = PkgSpec::from_source(Source::Pip, names(["x"]));
        assert!(p.can_merge(&p.clone()));
        let mut q = p.clone();
        q.wanted = Wanted::Latest;
        assert!(!p.can_merge(&q));
    }

    #[test]
    fn rustup_keys_writes_kind_and_post() {
        let h = host();
        h.rustup("stable")
            .targets(["x86_64-unknown-linux-musl", "x86_64-unknown-linux-musl"])
            .components(["rust-src"]);
        h.as_user("deploy").rustup("1.94.0");
        h.as_user("root")
            .rustup("nightly")
            .writes([Key::file("/root/.cargo/bin/rustc")]);
        let t = h.snapshot();
        assert!(t.errors.is_empty(), "{:?}", t.errors);
        let ops: Vec<&OpNode> = t
            .nodes
            .iter()
            .map(|n| match n {
                Node::Op(o) => o,
                _ => panic!(),
            })
            .collect();
        let post = |o: &OpNode| {
            o.post
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
        };
        assert_eq!(ops[0].writes, BTreeSet::from([Key::Any]));
        assert_eq!(
            post(ops[0]),
            [
                "pkg:rustup:root:stable=Installed",
                "pkg:rustup:root:stable/component/rust-src=Installed",
                "pkg:rustup:root:stable/target/x86_64-unknown-linux-musl=Installed",
            ]
        );
        assert_eq!(post(ops[1]), ["pkg:rustup:deploy:1.94.0=Installed"]);
        assert_eq!(post(ops[2]), ["pkg:rustup:root:nightly=Installed"]);
        assert_eq!(
            ops[2].writes,
            BTreeSet::from([Key::file("/root/.cargo/bin/rustc")])
        );
        assert_eq!(ops[0].spec.kind(), "pkg[rustup]");
        let Source::Rustup(r) = spec_of(&h, 0).source else {
            panic!()
        };
        assert_eq!(r.targets, ["x86_64-unknown-linux-musl"]);
        assert_eq!(r.profile(), "default");
        assert_eq!(r.installer(), DEFAULT_INSTALLER);
    }

    #[test]
    fn rustup_pinned_is_a_trace_error_and_never_merges() {
        let h = host();
        h.rustup("stable").pin("1.94.0");
        assert_eq!(errors(&h).len(), 1);
        assert!(
            errors(&h)[0].contains("name a version toolchain instead"),
            "{:?}",
            errors(&h)
        );
        assert_eq!(spec_of(&h, 0).wanted, Wanted::Present);
        h.rustup("stable").latest();
        assert_eq!(errors(&h).len(), 1);
        let a = PkgSpec::from_source(Source::Rustup(Box::default()), names(["stable"]));
        assert!(!a.mergeable());
        assert!(!a.can_merge(&a.clone()));
    }

    #[test]
    fn rustup_takes_exactly_one_name() {
        let h = host();
        h.pkg_spec_op(PkgSpec::from_source(
            Source::Rustup(Box::default()),
            names(["stable", "beta"]),
        ));
        assert_eq!(
            errors(&h),
            ["rustup: exactly one toolchain name per rustup op"]
        );
    }

    #[test]
    fn rustup_builders_on_other_sources_are_trace_errors() {
        let h = host();
        h.cargo(["x"]).targets(["t"]);
        h.pkg(["git"]).components(["c"]);
        h.pip(["x"]).profile("minimal");
        h.cargo(["x"]).set_default();
        h.pkg(["git"]).installer("true");
        assert_eq!(
            errors(&h),
            [
                "targets() is only available on rustup ops",
                "components() is only available on rustup ops",
                "profile() is only available on rustup ops",
                "set_default() is only available on rustup ops",
                "installer() is only available on rustup ops",
            ]
        );
    }

    fn uv() -> ScriptSpec {
        ScriptSpec::new()
            .bin("/usr/local/bin/uv")
            .version("uv --version")
            .install("sh install.sh")
    }

    #[test]
    fn script_missing_parts_are_trace_errors() {
        let h = host();
        h.script("uv", ScriptSpec::new());
        assert_eq!(
            errors(&h),
            [
                "script uv: bin is required",
                "script uv: version is required",
                "script uv: install is required",
            ]
        );
    }

    #[test]
    fn script_latest_needs_a_latest_source() {
        let h = host();
        h.script("uv", uv()).latest();
        assert_eq!(errors(&h), ["script uv: latest is required for .latest()"]);
        let h = host();
        h.script("uv", uv().latest(Latest::command("echo 1")))
            .latest();
        h.script("uv", uv()).pin("1.0");
        assert!(errors(&h).is_empty(), "{:?}", errors(&h));
    }

    #[test]
    fn script_update_defaults_to_install() {
        let h = host();
        h.script("uv", uv());
        let Source::Script(s) = spec_of(&h, 0).source else {
            panic!()
        };
        assert_eq!(s.update_cmd(), "sh install.sh");
        assert_eq!(s.clone().update("u").update_cmd(), "u");
    }

    #[test]
    fn script_keys_name_the_principal() {
        let h = host();
        h.script("uv", uv());
        h.as_user("deploy").script("uv", uv());
        h.as_user("root").script("uv", uv());
        let t = h.snapshot();
        let keys: Vec<(String, String)> = t
            .nodes
            .iter()
            .map(|n| match n {
                Node::Op(o) => (
                    o.writes
                        .iter()
                        .map(|k| k.to_string())
                        .collect::<Vec<_>>()
                        .join(","),
                    o.post
                        .iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>()
                        .join(","),
                ),
                _ => panic!(),
            })
            .collect();
        let post =
            |u: &str| format!("pkg:script:{u}:uv=Installed,file:/usr/local/bin/uv=Exists(true)");
        assert_eq!(
            keys,
            [
                ("*".to_string(), post("root")),
                ("*".to_string(), post("deploy")),
                ("*".to_string(), post("root")),
            ]
        );
    }

    #[test]
    fn script_writes_narrowing_keeps_post() {
        let h = host();
        h.script("uv", uv())
            .writes([Key::file("/usr/local/bin/uv")]);
        let Node::Op(o) = &h.snapshot().nodes[0] else {
            panic!()
        };
        assert_eq!(o.writes, BTreeSet::from([Key::file("/usr/local/bin/uv")]));
        assert_eq!(o.post.len(), 2);
        assert_eq!(o.spec.kind(), "pkg[script]");
    }

    #[test]
    fn cargo_keys_writes_kind_and_post() {
        let h = host();
        h.cargo(["cargo-deb", "cargo-generate-rpm"]);
        h.as_user("deploy").cargo(["cargo-deb"]);
        h.as_user("root")
            .cargo(["x"])
            .writes([Key::file("/root/.cargo/bin/x")]);
        let t = h.snapshot();
        assert!(t.errors.is_empty(), "{:?}", t.errors);
        let ops: Vec<&OpNode> = t
            .nodes
            .iter()
            .map(|n| match n {
                Node::Op(o) => o,
                _ => panic!(),
            })
            .collect();
        let post = |o: &OpNode| {
            o.post
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
        };
        assert_eq!(ops[0].writes, BTreeSet::from([Key::Any]));
        assert_eq!(
            post(ops[0]),
            [
                "pkg:cargo:root:cargo-deb=Installed",
                "pkg:cargo:root:cargo-generate-rpm=Installed"
            ]
        );
        assert_eq!(post(ops[1]), ["pkg:cargo:deploy:cargo-deb=Installed"]);
        assert_eq!(post(ops[2]), ["pkg:cargo:root:x=Installed"]);
        assert_eq!(
            ops[2].writes,
            BTreeSet::from([Key::file("/root/.cargo/bin/x")])
        );
        assert_eq!(ops[0].spec.kind(), "pkg[cargo]");
    }

    #[test]
    fn cargo_pin_takes_one_name_and_never_merges() {
        let h = host();
        h.cargo(["a"]).pin("1.0.0");
        assert!(errors(&h).is_empty());
        h.cargo(["a", "b"]).pin("1.0.0");
        assert_eq!(
            errors(&h),
            ["cargo: a pinned op takes exactly one package name"]
        );
        let mut a = PkgSpec::from_source(Source::Cargo, names(["a"]));
        let mut b = PkgSpec::from_source(Source::Cargo, names(["b"]));
        assert!(a.can_merge(&b));
        a.wanted = Wanted::Pinned("1.0.0".into());
        b.wanted = Wanted::Pinned("1.0.0".into());
        assert!(!a.can_merge(&b));
        assert!(!a.mergeable());
    }

    #[test]
    fn mise_keys_writes_kind_and_post() {
        let h = host();
        h.mise(["just", "fd"]);
        h.as_user("root")
            .mise(["bat"])
            .writes([Key::file("/usr/local/share/mise/installs/bat")]);
        let t = h.snapshot();
        assert!(t.errors.is_empty(), "{:?}", t.errors);
        let ops: Vec<&OpNode> = t
            .nodes
            .iter()
            .map(|n| match n {
                Node::Op(o) => o,
                _ => panic!(),
            })
            .collect();
        let post = |o: &OpNode| {
            o.post
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
        };
        assert_eq!(ops[0].writes, BTreeSet::from([Key::Any]));
        assert_eq!(
            post(ops[0]),
            [
                "pkg:mise:root:fd=Installed",
                "pkg:mise:root:just=Installed",
                "file:/etc/mise/config.toml=Exists(true)",
            ]
        );
        assert_eq!(
            post(ops[1]),
            [
                "pkg:mise:root:bat=Installed",
                "file:/etc/mise/config.toml=Exists(true)"
            ]
        );
        assert_eq!(
            ops[1].writes,
            BTreeSet::from([Key::file("/usr/local/share/mise/installs/bat")])
        );
        assert_eq!(ops[0].spec.kind(), "pkg[mise]");
    }

    #[test]
    fn mise_is_root_only() {
        let h = host();
        h.as_user("bot").mise(["just"]);
        assert_eq!(errors(&h), ["mise source is system-wide (root only)"]);
        let h = host();
        h.as_user("root").mise(["just"]);
        h.mise(["just"]);
        assert!(errors(&h).is_empty());
    }

    #[test]
    fn mise_pin_takes_one_name_and_never_merges() {
        let h = host();
        h.mise(["a"]).pin("1.0.0");
        assert!(errors(&h).is_empty());
        h.mise(["a", "b"]).pin("1.0.0");
        assert_eq!(
            errors(&h),
            ["mise: a pinned op takes exactly one package name"]
        );
        let mut a = PkgSpec::from_source(Source::Mise, names(["a"]));
        let mut b = PkgSpec::from_source(Source::Mise, names(["b"]));
        assert!(a.can_merge(&b));
        a.wanted = Wanted::Latest;
        assert!(!a.can_merge(&b));
        b.wanted = Wanted::Latest;
        assert!(a.can_merge(&b));
        a.wanted = Wanted::Pinned("1".into());
        b.wanted = Wanted::Pinned("1".into());
        assert!(!a.can_merge(&b));
    }
}
