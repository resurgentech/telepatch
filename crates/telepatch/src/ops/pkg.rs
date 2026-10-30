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

use std::collections::BTreeSet;
use std::ops::Deref;

use crate::facts::os::Family;
use crate::host::{Host, OpHandle, TraceError};
use crate::key::Key;
use crate::sh::quote;
use crate::trace::{Node, OpNode, OpSpec};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Manager {
    Apt,
    Dnf,
    Yum,
    Pacman,
    Pip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PkgMode {
    /// Install what is missing.
    Install,
    /// `apt-get build-dep` (apt only).
    BuildDep,
    /// pip `install -U` (pip only).
    Latest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkgSpec {
    pub manager: Manager,
    pub names: Vec<String>,
    pub mode: PkgMode,
    /// Run `apt-get update` first (set by [`apply_apt_update`]). Ignored by
    /// other managers.
    pub update_cache: bool,
}

impl PkgSpec {
    pub fn new(manager: Manager, names: Vec<String>) -> PkgSpec {
        let mut seen = BTreeSet::new();
        let names = names
            .into_iter()
            .filter(|n| seen.insert(n.clone()))
            .collect();
        PkgSpec {
            manager,
            names,
            mode: PkgMode::Install,
            update_cache: false,
        }
    }

    /// The effect key of one name. pip names live in their own namespace
    /// (`pkg:pip:<name>`) so they never overlap or merge with system names.
    pub fn key_of(&self, name: &str) -> Key {
        match self.manager {
            Manager::Pip => Key::pkg(format!("pip:{name}")),
            _ => Key::pkg(name),
        }
    }

    pub fn keys(&self) -> BTreeSet<Key> {
        self.names.iter().map(|n| self.key_of(n)).collect()
    }

    /// Can `other` be folded into this op (same manager and mode)?
    pub fn can_merge(&self, other: &PkgSpec) -> bool {
        self.manager == other.manager && self.mode == other.mode
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
        let names: Vec<String> = self.names.iter().map(|n| quote(n)).collect();
        let mut o = format!(
            "rc=0; changed=0; installed=()\ntp_want=({})\ntp_missing=()\n",
            names.join(" ")
        );
        match (self.manager, self.mode) {
            (Manager::Apt, PkgMode::BuildDep) => self.apt_build_dep(&mut o),
            (Manager::Apt, _) => self.apt(&mut o),
            (Manager::Dnf, _) => rpm_like(&mut o, "dnf", "dnf group list --installed"),
            (Manager::Yum, _) => rpm_like(&mut o, "yum", "yum grouplist installed"),
            (Manager::Pacman, _) => pacman(&mut o),
            (Manager::Pip, PkgMode::Latest) => pip_latest(&mut o),
            (Manager::Pip, _) => pip(&mut o),
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
            OpSpec::Pkg(p) if p.manager == Manager::Apt => std::mem::take(&mut stale),
            _ => {
                stale |= op.writes.iter().any(writes_apt_sources);
                false
            }
        })
        .collect();
    (flags, stale)
}

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
            if s.manager == Manager::Apt {
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
}

/// A `pkg::pip` handle: `.latest()` upgrades to the newest release.
#[derive(Clone)]
pub struct PipHandle(PkgHandle);

impl Deref for PipHandle {
    type Target = OpHandle;
    fn deref(&self) -> &OpHandle {
        &self.0.0
    }
}

impl PipHandle {
    pub fn latest(&self) -> &Self {
        self.0.spec(|s| s.mode = PkgMode::Latest);
        self
    }
}

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
        if let Err(e) = check_names(&v) {
            self.record_error(e);
        }
        PkgHandle(self.op(OpSpec::Pkg(PkgSpec::new(manager, v))))
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

    /// Python packages via pip (`pkg:pip:<name>` keys).
    pub fn pip<I, S>(&self, pkgs: I) -> PipHandle
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        PipHandle(self.pkg_op(Manager::Pip, names(pkgs)))
    }
}
