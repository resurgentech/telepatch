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

mod binary_release;
mod cargo;
mod mise;
mod rustup;
mod script;
pub use binary_release::{Arch, BinaryRelease, Sum, prepared as binary_release_prepared};
pub use cargo::{
    CargoProbe, parse_probe as parse_cargo_probe, probe as cargo_probe,
    will_changes as cargo_will_changes,
};
pub use mise::{
    MiseProbe, parse_probe as parse_mise_probe, probe as mise_probe,
    will_changes as mise_will_changes,
};
pub use rustup::{
    DEFAULT_INSTALLER, RustupProbe, RustupSeen, RustupSpec, conflicts as rustup_conflicts,
    parse_probe as parse_rustup_probe, probe as rustup_probe, will_change as rustup_will_change,
};
pub use script::{
    InstallerScript, Latest, Probed, VERSION_RE, parse_probe, version_token, will_change,
};

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
    Cargo {
        /// `.locked()`: `cargo install --locked`. Part of the source, so
        /// locked and unlocked ops never merge.
        locked: bool,
    },
    /// Tools in mise's system layer, root only ([`mise`]).
    Mise,
    /// A rust toolchain with targets and components, per user
    /// ([`rustup`]; boxed to keep `OpNode` small).
    Rustup(Box<RustupSpec>),
    /// A vendor installer script ([`InstallerScript`]; boxed to keep `OpNode`
    /// small).
    Script(Box<InstallerScript>),
    /// A vendor's tarball or single binary ([`BinaryRelease`]), root only;
    /// boxed to keep `OpNode` small.
    BinaryRelease(Box<BinaryRelease>),
}

/// Which version of a package is wanted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wanted {
    /// Any installed version.
    Present,
    Pinned(String),
    Latest,
    /// Not installed. Removes only names the host already has.
    Absent,
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
    /// Apt only: `apt-get purge` instead of `remove` (config files too).
    /// Meaningful with [`Wanted::Absent`].
    pub purge: bool,
    /// A local `.deb` or `.rpm` from a prepare-phase fetch. Apt and dnf
    /// only, and only with [`.pin`](PkgHandle::pin): the manager installs
    /// `./x.deb` or `./x.rpm` from the checksum cache when name and version
    /// are not already that pin. `None` is a normal name install.
    pub file: Option<crate::ops::fetch::Fetched>,
    /// Run `apt-get update` first (set by [`apply_apt_update`]). Ignored by
    /// other managers.
    pub update_cache: bool,
    /// The op's principal (`root` for `h` and `as_user("root")`), set when
    /// the op is pushed. Names the per-user sources' keys
    /// (`pkg:script:<user>:<name>`, `pkg:cargo:<user>:<name>`,
    /// `pkg:rustup:<user>:<toolchain>`); unused by distro and pip.
    pub user: String,
    /// A source call (`.pip()`, `.yum()`, ...) already ran on this op; a
    /// second one is a trace error.
    pub sourced: bool,
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
            purge: false,
            file: None,
            update_cache: false,
            user: "root".to_string(),
            sourced: false,
        }
    }

    /// The checks a source puts on the op's names and principal, as
    /// messages (empty when fine).
    pub fn source_problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if matches!(self.source, Source::Rustup(_)) && self.names.len() != 1 {
            out.push("rustup: exactly one toolchain name per rustup op".to_string());
        }
        if let Source::Script(s) = &self.source {
            let name = self.names.first().map(String::as_str).unwrap_or("");
            if self.names.len() != 1 {
                out.push(format!(
                    "script {name}: exactly one package name per script op"
                ));
            }
            out.extend(s.problems(name, &self.wanted));
        }
        if let Source::BinaryRelease(b) = &self.source {
            let name = self.names.first().map(String::as_str).unwrap_or("");
            if self.names.len() != 1 {
                out.push(format!(
                    "binary_release {name}: exactly one package name per binary_release op"
                ));
            }
            out.extend(b.problems(name));
        }
        if self.source == Source::Mise && self.user != "root" {
            out.push("mise source is system-wide (root only)".to_string());
        }
        if matches!(self.source, Source::BinaryRelease(_)) && self.user != "root" {
            out.push("binary_release source is system-wide (root only)".to_string());
        }
        out
    }

    /// The source's name for messages.
    fn source_name(&self) -> &'static str {
        match &self.source {
            Source::Distro(Manager::Yum) => "yum",
            Source::Distro(_) => "the distro manager",
            Source::Pip => "pip",
            Source::Cargo { .. } => "cargo",
            Source::Mise => "mise",
            Source::Rustup(_) => "rustup",
            Source::Script(_) => "installer_script",
            Source::BinaryRelease(_) => "binary_release",
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
            Source::Cargo { .. } => Key::pkg(format!("cargo:{}:{name}", self.user)),
            Source::Mise => Key::pkg(format!("mise:root:{name}")),
            Source::Rustup(_) => Key::pkg(format!("rustup:{}:{name}", self.user)),
            Source::Script(_) => Key::pkg(format!("script:{}:{name}", self.user)),
            Source::BinaryRelease(_) => Key::pkg(format!("binary_release:{}:{name}", self.user)),
        }
    }

    /// Default write set: the names' keys for distro and pip; for cargo the
    /// names' keys plus `home:<user>` (`cargo install` writes under the
    /// user's `~/.cargo`, which [`Key::Home`] stands for); `*` for script,
    /// binary release, mise and rustup (they write files nobody listed;
    /// A14).
    pub fn default_writes(&self) -> BTreeSet<Key> {
        match &self.source {
            Source::Script(_) | Source::BinaryRelease(_) | Source::Mise | Source::Rustup(_) => {
                BTreeSet::from([Key::Any])
            }
            Source::Cargo { .. } => {
                let mut w = self.keys();
                w.insert(Key::home(&self.user));
                w
            }
            _ => self.keys(),
        }
    }

    /// Default postconditions: every name `Installed`, or `Absent` when
    /// the op removes. A script op also posts `file:<bin> = Exists(true)`,
    /// a rustup op each target and component.
    pub fn default_post(&self) -> BTreeMap<Key, Value> {
        if self.wanted == Wanted::Absent {
            return self
                .names
                .iter()
                .map(|n| (self.key_of(n), Value::Absent))
                .collect();
        }
        let mut post: BTreeMap<Key, Value> = self
            .names
            .iter()
            .map(|n| (self.key_of(n), Value::Installed))
            .collect();
        if let Source::Script(s) = &self.source {
            post.insert(Key::file(&s.bin), Value::Exists(true));
        }
        if let Source::BinaryRelease(b) = &self.source {
            post.insert(Key::file(&b.bin), Value::Exists(true));
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
            Source::BinaryRelease(_) => "pkg[binary_release]",
            Source::Cargo { .. } => "pkg[cargo]",
            Source::Mise => "pkg[mise]",
            Source::Rustup(_) => "pkg[rustup]",
            _ => "pkg",
        }
    }

    /// The names a merged op's member answers for in `installed` reports:
    /// the names, and for a rustup op also `<tc>/target/<t>` and
    /// `<tc>/component/<c>`.
    pub fn member_names(&self) -> Vec<String> {
        let mut v = self.names.clone();
        if let (Source::Rustup(r), Some(tc)) = (&self.source, self.names.first()) {
            v.extend(r.targets.iter().map(|t| format!("{tc}/target/{t}")));
            v.extend(r.components.iter().map(|c| format!("{tc}/component/{c}")));
        }
        v
    }

    pub fn keys(&self) -> BTreeSet<Key> {
        self.names.iter().map(|n| self.key_of(n)).collect()
    }

    /// May this op ever be folded into another? `Script` and
    /// `BinaryRelease` ops never merge (one name each);
    /// a pinned `Cargo`, `Mise`, `Rustup` or distro op has exactly one name
    /// (the pin is for that name). Rustup ops merge per user and toolchain
    /// (see [`PkgSpec::merge_scope`]).
    pub fn mergeable(&self) -> bool {
        if self.file.is_some() {
            return false;
        }
        match &self.source {
            Source::Script(_) | Source::BinaryRelease(_) => false,
            Source::Cargo { .. } | Source::Mise | Source::Distro(_) | Source::Rustup(_) => {
                !matches!(self.wanted, Wanted::Pinned(_))
            }
            _ => true,
        }
    }

    /// The source as merge compares it: a rustup op's options (targets,
    /// components, `set_default`) are what merging unions, so they are not
    /// part of it. Everything else, `Cargo { locked }` included, is.
    pub fn merge_source(&self) -> Source {
        match &self.source {
            Source::Rustup(_) => Source::Rustup(Box::default()),
            s => s.clone(),
        }
    }

    /// What else must be equal for a rustup op to merge: user, toolchain,
    /// profile and installer. `None` for other sources.
    pub fn merge_scope(&self) -> Option<(String, String, String, String)> {
        let Source::Rustup(r) = &self.source else {
            return None;
        };
        Some((
            self.user.clone(),
            self.names.first().cloned().unwrap_or_default(),
            r.profile().to_string(),
            r.installer().to_string(),
        ))
    }

    /// `wanted` as merge compares it: `None` for rustup, whose Present and
    /// Latest ops merge into one that wants Latest ([`PkgSpec::merge`];
    /// Pinned rustup never gets this far, see [`PkgSpec::mergeable`]).
    /// Every other source merges only with an equal `wanted`.
    pub fn merge_wanted(&self) -> Option<Wanted> {
        match &self.source {
            Source::Rustup(_) => None,
            _ => Some(self.wanted.clone()),
        }
    }

    /// Can `other` be folded into this op (same source, mode and, except
    /// for rustup, wanted)? The spec half of the optimizer's merge key.
    pub fn can_merge(&self, other: &PkgSpec) -> bool {
        self.mergeable()
            && other.mergeable()
            && self.merge_source() == other.merge_source()
            && self.merge_scope() == other.merge_scope()
            && self.merge_wanted() == other.merge_wanted()
            && self.mode == other.mode
            && self.purge == other.purge
    }

    /// Union of names, keeping first-seen order. `update_cache` is or-ed.
    /// Rustup: targets and components are unioned, `set_default` is or-ed
    /// and the merged op wants the stronger of the two (Latest over
    /// Present).
    pub fn merge(&mut self, other: &PkgSpec) {
        if let (Source::Rustup(a), Source::Rustup(b)) = (&mut self.source, &other.source) {
            RustupSpec::add(&mut a.targets, b.targets.iter().cloned());
            RustupSpec::add(&mut a.components, b.components.iter().cloned());
            a.set_default |= b.set_default;
            if other.wanted == Wanted::Latest {
                self.wanted = Wanted::Latest;
            }
        }
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
        if let Source::BinaryRelease(b) = &self.source {
            let name = self.names.first().map(String::as_str).unwrap_or("");
            return b.render(name);
        }
        if let Source::Cargo { locked } = self.source {
            return cargo::render(&self.names, &self.wanted, locked);
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
        if let (Source::Distro(_), Wanted::Pinned(v)) = (&self.source, &self.wanted) {
            o.push_str(&format!("tp_pin={}\n", quote(v)));
        }
        match (&self.source, self.mode) {
            (Source::Distro(Manager::Apt), PkgMode::BuildDep) => self.apt_build_dep(&mut o),
            (Source::Distro(Manager::Apt), _) if self.file.is_some() => self.apt_file(&mut o),
            (Source::Distro(Manager::Apt), _) => match &self.wanted {
                Wanted::Present => self.apt(&mut o),
                Wanted::Absent => self.apt_absent(&mut o),
                w => self.apt_versioned(&mut o, w),
            },
            (Source::Distro(Manager::Dnf), _) if self.file.is_some() => self.rpm_file(&mut o),
            (Source::Distro(m @ (Manager::Dnf | Manager::Yum)), _) => {
                let (mgr, groups) = match m {
                    Manager::Dnf => ("dnf", "dnf group list --installed"),
                    _ => ("yum", "yum grouplist installed"),
                };
                match &self.wanted {
                    Wanted::Present => rpm_like(&mut o, mgr, groups),
                    Wanted::Absent => rpm_absent(&mut o, mgr),
                    w => rpm_versioned(&mut o, mgr, w),
                }
            }
            (Source::Distro(Manager::Pacman), _) => match &self.wanted {
                Wanted::Present => pacman(&mut o),
                Wanted::Absent => pacman_absent(&mut o),
                w => pacman_versioned(&mut o, w),
            },
            (Source::Pip, _) => match &self.wanted {
                Wanted::Latest => pip_latest(&mut o),
                Wanted::Pinned(_) => pip_pinned(&mut o),
                Wanted::Present => pip(&mut o),
                Wanted::Absent => o.push_str(
                    "echo 'telepatch: .absent() is only available on the distro manager' >&2\nrc=1\n",
                ),
            },
            // Rendered above; every source is built.
            (
                Source::Cargo { .. }
                | Source::Mise
                | Source::Rustup(_)
                | Source::Script(_)
                | Source::BinaryRelease(_),
                _,
            ) => unreachable!(),
        }
        o
    }

    fn apt_update_block(&self, o: &mut String) {
        if self.update_cache {
            o.push_str("apt-get -q update || rc=$?\n");
        }
    }

    /// A Present name is missing unless dpkg says `installed` or an
    /// installed package Provides it. Pin, latest, absent and a `.deb`
    /// file install keep their own status checks.
    fn apt(&self, o: &mut String) {
        o.push_str("export DEBIAN_FRONTEND=noninteractive\n");
        self.apt_update_block(o);
        o.push_str(APT_PRESENT);
        o.push_str("if [ \"$rc\" -eq 0 ] && [ \"${#tp_missing[@]}\" -gt 0 ]; then\n");
        if !self.update_cache {
            o.push_str(APT_LISTS_GUARD);
        }
        o.push_str(
            "  if [ \"$rc\" -eq 0 ]; then\n    apt-get -y -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold install \"${tp_missing[@]}\"\n    rc=$?\n    if [ \"$rc\" -eq 0 ]; then changed=1; installed=(\"${tp_missing[@]}\"); fi\n  fi\nfi\n",
        );
    }

    /// Remove only names dpkg reports as installed. `purge` also takes a
    /// name left in `config-files`, and if `apt-get purge` returns 0 while
    /// the name is still `installed` or `config-files`, finishes with
    /// `dpkg --purge`. `--force-confdef` / `--force-confold` are upgrade
    /// prompts and are not passed here: with them, purge can unlink the
    /// conffiles and still leave the package registered. `installed` in
    /// the trailer is the names removed (the inverse).
    fn apt_absent(&self, o: &mut String) {
        let verb = if self.purge { "purge" } else { "remove" };
        o.push_str("export DEBIAN_FRONTEND=noninteractive\ntp_have=()\n");
        if self.purge {
            o.push_str(
                "for tp_n in \"${tp_want[@]}\"; do\n  tp_st=$(dpkg-query -W -f='${db:Status-Status}' \"$tp_n\" 2>/dev/null || true)\n  case \"$tp_st\" in installed|config-files) tp_have+=(\"$tp_n\");; esac\ndone\n",
            );
        } else {
            o.push_str(&format!(
                "for tp_n in \"${{tp_want[@]}}\"; do\n  {APT_INSTALLED_TEST} && tp_have+=(\"$tp_n\")\ndone\n"
            ));
        }
        o.push_str(&format!(
            "if [ \"$rc\" -eq 0 ] && [ \"${{#tp_have[@]}}\" -gt 0 ]; then\n  apt-get -y {verb} \"${{tp_have[@]}}\"\n  rc=$?\n"
        ));
        if self.purge {
            o.push_str(
                "  if [ \"$rc\" -eq 0 ]; then\n    tp_left=()\n    for tp_n in \"${tp_have[@]}\"; do\n      tp_st=$(dpkg-query -W -f='${db:Status-Status}' \"$tp_n\" 2>/dev/null || true)\n      case \"$tp_st\" in installed|config-files) tp_left+=(\"$tp_n\");; esac\n    done\n    if [ \"${#tp_left[@]}\" -gt 0 ]; then dpkg --purge \"${tp_left[@]}\"; rc=$?; fi\n  fi\n",
            );
        }
        o.push_str(
            "  if [ \"$rc\" -eq 0 ]; then changed=1; installed=(\"${tp_have[@]}\"); fi\nfi\n",
        );
    }

    /// Install one pinned package from a fetched `.deb`. The version check
    /// is dpkg's status and version. The file is the checksum cache (or the
    /// unverified staging path); a symlink `./x.deb` in the staging
    /// directory is what `apt-get install` sees, and it is removed before
    /// the op returns. Nothing is written under `/tmp`.
    fn apt_file(&self, o: &mut String) {
        let Some(f) = &self.file else { return };
        let name = self.names.first().map(String::as_str).unwrap_or("");
        o.push_str("export DEBIAN_FRONTEND=noninteractive\n");
        self.apt_update_block(o);
        o.push_str(&format!("tp_name={}\n", quote(name)));
        locate_file(o, f);
        o.push_str(
            "if [ \"$rc\" -eq 0 ] && [ ! -f \"$tp_f\" ]; then echo \"telepatch: fetched file $tp_f is missing (prepare did not fetch it)\" >&2; rc=1; fi\n",
        );
        o.push_str(
            "tp_st=$(dpkg-query -W -f='${db:Status-Status}:${Version}' \"$tp_name\" 2>/dev/null || true)\n",
        );
        o.push_str("if [ \"$rc\" -eq 0 ] && [ \"$tp_st\" != \"installed:$tp_pin\" ]; then\n");
        o.push_str(
            "  tp_dir=\"$tp_sd/$__tp_run\"\n  mkdir -p -- \"$tp_dir\" || rc=$?\n  if [ \"$rc\" -eq 0 ]; then ln -sfn -- \"$tp_f\" \"$tp_dir/x.deb\" || rc=$?; fi\n",
        );
        o.push_str(
            "  if [ \"$rc\" -eq 0 ]; then ( cd -- \"$tp_dir\" && apt-get -y --allow-downgrades -o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold install ./x.deb ); rc=$?; fi\n",
        );
        o.push_str("  rm -f -- \"$tp_dir/x.deb\"\n");
        o.push_str(
            "  if [ \"$rc\" -eq 0 ]; then\n    tp_st=$(dpkg-query -W -f='${db:Status-Status}:${Version}' \"$tp_name\" 2>/dev/null || true)\n    if [ \"$tp_st\" = \"installed:$tp_pin\" ]; then changed=1; installed=(\"$tp_name\"); else echo \"telepatch: $tp_name is '$tp_st' after installing ./x.deb, wanted installed:$tp_pin\" >&2; rc=1; fi\n  fi\nfi\n",
        );
    }

    /// `.pin(v)` and `.latest()` on apt. Pinned: a name is missing when its
    /// dpkg version is not exactly the string the user wrote; `install
    /// name=v` (with `--allow-downgrades`) then moves it. Latest: a name
    /// is installed when missing, and `install --only-upgrade` runs for an
    /// installed name whose `apt-cache policy` candidate is newer
    /// (`dpkg --compare-versions`) than the installed version.
    fn apt_versioned(&self, o: &mut String, w: &Wanted) {
        const OPTS: &str = "-o Dpkg::Options::=--force-confdef -o Dpkg::Options::=--force-confold";
        o.push_str("export DEBIAN_FRONTEND=noninteractive\ntp_up=()\n");
        self.apt_update_block(o);
        match w {
            Wanted::Pinned(_) => o.push_str(
                "for tp_n in \"${tp_want[@]}\"; do\n  [ \"$(dpkg-query -W -f='${db:Status-Status}:${Version}' \"$tp_n\" 2>/dev/null)\" = \"installed:$tp_pin\" ] || tp_missing+=(\"$tp_n\")\ndone\n",
            ),
            _ => o.push_str(
                "for tp_n in \"${tp_want[@]}\"; do\n  tp_st=$(dpkg-query -W -f='${db:Status-Status}:${Version}' \"$tp_n\" 2>/dev/null)\n  case \"$tp_st\" in\n    installed:*)\n      tp_cand=$(LC_ALL=C apt-cache policy \"$tp_n\" 2>/dev/null | sed -n 's/^  Candidate: //p')\n      if [ -n \"$tp_cand\" ] && [ \"$tp_cand\" != '(none)' ] && dpkg --compare-versions \"$tp_cand\" gt \"${tp_st#installed:}\"; then tp_up+=(\"$tp_n\"); fi;;\n    *) tp_missing+=(\"$tp_n\");;\n  esac\ndone\n",
            ),
        }
        o.push_str(
            "if [ \"$rc\" -eq 0 ] && [ \"$(( ${#tp_missing[@]} + ${#tp_up[@]} ))\" -gt 0 ]; then\n",
        );
        if !self.update_cache {
            o.push_str(APT_LISTS_GUARD);
        }
        o.push_str("  if [ \"$rc\" -eq 0 ] && [ \"${#tp_missing[@]}\" -gt 0 ]; then\n");
        match w {
            Wanted::Pinned(_) => o.push_str(&format!(
                "    tp_args=(); for tp_n in \"${{tp_missing[@]}}\"; do tp_args+=(\"$tp_n=$tp_pin\"); done\n    apt-get -y --allow-downgrades {OPTS} install \"${{tp_args[@]}}\"\n"
            )),
            _ => o.push_str(&format!(
                "    apt-get -y {OPTS} install \"${{tp_missing[@]}}\"\n"
            )),
        }
        o.push_str(
            "    rc=$?\n    if [ \"$rc\" -eq 0 ]; then changed=1; installed+=(\"${tp_missing[@]}\"); fi\n  fi\n",
        );
        o.push_str(&format!(
            "  if [ \"$rc\" -eq 0 ] && [ \"${{#tp_up[@]}}\" -gt 0 ]; then\n    apt-get -y {OPTS} install --only-upgrade \"${{tp_up[@]}}\"\n    rc=$?\n    if [ \"$rc\" -eq 0 ]; then changed=1; installed+=(\"${{tp_up[@]}}\"); fi\n  fi\nfi\n"
        ));
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

/// Present install check. Status `installed` counts, and so does a name in
/// the Provides of an installed package (`www-browser` from `w3m`). A
/// versioned provide (`mail-transport-agent (= 1)`) counts as its name.
/// `config-files` and `not-installed` do not.
const APT_PRESENT: &str = r#"tp_apt_present() {
  [ "$(dpkg-query -W -f='${db:Status-Status}' "$1" 2>/dev/null)" = installed ] && return 0
  dpkg-query -W -f='${db:Status-Status} ${Provides}\n' 2>/dev/null | awk -v n="$1" '$1 != "installed" { next } { line=$0; sub(/^installed[[:space:]]+/, "", line); k=split(line, parts, /,/); for (i=1; i<=k; i++) { p=parts[i]; gsub(/^[[:space:]]+|[[:space:]]+$/, "", p); sub(/[[:space:]].*/, "", p); if (p == n) found=1 } } END { exit (found ? 0 : 1) }'
}
for tp_n in "${tp_want[@]}"; do
  tp_apt_present "$tp_n" || tp_missing+=("$tp_n")
done
"#;

/// The apt function `tp_apt_present`, as the Present install script
/// renders it, without the loop that calls it. Prepare's probe runs the
/// same text.
pub fn apt_present_fn() -> &'static str {
    APT_PRESENT
        .split_once("for tp_n in")
        .map_or(APT_PRESENT, |(f, _)| f)
}

/// The tests the install and absent scripts apply to `$tp_n`, one per
/// manager. Prepare's probe runs the same text, so it announces exactly
/// what the scripts would do.
pub const APT_INSTALLED_TEST: &str =
    r#"[ "$(dpkg-query -W -f='${db:Status-Status}' "$tp_n" 2>/dev/null)" = installed ]"#;
pub const RPM_PRESENT_TEST: &str = r#"rpm -q --whatprovides "$tp_n" >/dev/null 2>&1"#;
pub const RPM_ABSENT_TEST: &str = r#"rpm -q "$tp_n" >/dev/null 2>&1"#;
pub const PACMAN_PRESENT_TEST: &str =
    r#"[ -z "$(pacman -T "$tp_n" 2>/dev/null)" ] || [ -n "$(pacman -Qqg "$tp_n" 2>/dev/null)" ]"#;
pub const PACMAN_ABSENT_TEST: &str = r#"pacman -Q "$tp_n" >/dev/null 2>&1"#;

/// dnf and yum. Plain names and provides (`perl(ExtUtils::Embed)`) are
/// checked with `rpm -q --whatprovides`; `@group` names against the
/// manager's installed-groups list.
fn rpm_like(o: &mut String, mgr: &str, group_list: &str) {
    o.push_str(&format!(
        "for tp_n in \"${{tp_want[@]}}\"; do\n  case \"$tp_n\" in\n    @*) {group_list} 2>/dev/null | grep -qF -- \"${{tp_n#@}}\" || tp_missing+=(\"$tp_n\");;\n    *) {RPM_PRESENT_TEST} || tp_missing+=(\"$tp_n\");;\n  esac\ndone\nif [ \"${{#tp_missing[@]}}\" -gt 0 ]; then\n  {mgr} install -y \"${{tp_missing[@]}}\"\n  rc=$?\n  if [ \"$rc\" -eq 0 ]; then changed=1; installed=(\"${{tp_missing[@]}}\"); fi\nfi\n"
    ));
}

/// Where the fetched package file is, as `tp_f`: the stage's link to where
/// prepare staged it, else the checksum cache; or the unverified staging
/// path when the fetch has no sum. Reading across filesystems is fine: a
/// package file is staged on its first consumer's filesystem, and a
/// package op as first consumer counts as the stage's own.
fn locate_file(o: &mut String, f: &crate::ops::fetch::Fetched) {
    o.push_str(crate::ops::fetch::LOCATE);
    match &f.sha256 {
        Some(s) => o.push_str(&format!(
            "tp_s={}\n{}",
            quote(s),
            crate::ops::fetch::LOOKUP_PKG
        )),
        None => o.push_str(&format!(
            "tp_f=\"$tp_sd\"/\"$__tp_run\"/\"u-$(printf %s {} | sha256sum | cut -c1-32)\"\n",
            quote(&f.url)
        )),
    }
}

/// Install one pinned package from a fetched `.rpm` (`dnf install -y ./x.rpm`).
/// The version check is `rpm -q` in the forms a pin may be written.
impl PkgSpec {
    fn rpm_file(&self, o: &mut String) {
        let Some(f) = &self.file else { return };
        let name = self.names.first().map(String::as_str).unwrap_or("");
        o.push_str(&format!("tp_name={}\n", quote(name)));
        locate_file(o, f);
        o.push_str(
            "if [ \"$rc\" -eq 0 ] && [ ! -f \"$tp_f\" ]; then echo \"telepatch: fetched file $tp_f is missing (prepare did not fetch it)\" >&2; rc=1; fi\n",
        );
        o.push_str(&format!(
            "tp_have() {{ rpm -q --qf '{RPM_FORMS}' \"$1\" 2>/dev/null | grep -qxF -- \"$tp_pin\"; }}\n"
        ));
        o.push_str("if [ \"$rc\" -eq 0 ] && ! tp_have \"$tp_name\"; then\n");
        o.push_str(
            "  tp_dir=\"$tp_sd/$__tp_run\"\n  mkdir -p -- \"$tp_dir\" || rc=$?\n  if [ \"$rc\" -eq 0 ]; then ln -sfn -- \"$tp_f\" \"$tp_dir/x.rpm\" || rc=$?; fi\n",
        );
        o.push_str(
            "  if [ \"$rc\" -eq 0 ]; then ( cd -- \"$tp_dir\" && dnf install -y ./x.rpm ); rc=$?; fi\n",
        );
        o.push_str("  rm -f -- \"$tp_dir/x.rpm\"\n");
        o.push_str(
            "  if [ \"$rc\" -eq 0 ]; then\n    if tp_have \"$tp_name\"; then changed=1; installed=(\"$tp_name\"); else echo \"telepatch: $tp_name is not $tp_pin after installing ./x.rpm\" >&2; rc=1; fi\n  fi\nfi\n",
        );
    }
}

/// dnf and yum: remove only packages `rpm -q` says are installed, by name.
fn rpm_absent(o: &mut String, mgr: &str) {
    o.push_str(&format!(
        "tp_have=()\nfor tp_n in \"${{tp_want[@]}}\"; do\n  {RPM_ABSENT_TEST} && tp_have+=(\"$tp_n\")\ndone\nif [ \"${{#tp_have[@]}}\" -gt 0 ]; then\n  {mgr} remove -y \"${{tp_have[@]}}\"\n  rc=$?\n  if [ \"$rc\" -eq 0 ]; then changed=1; installed=(\"${{tp_have[@]}}\"); fi\nfi\n"
    ));
}

/// `pacman -T` is satisfied by installed packages and their provides; the
/// group query covers group names.
fn pacman(o: &mut String) {
    o.push_str(&format!(
        "for tp_n in \"${{tp_want[@]}}\"; do\n  if {PACMAN_PRESENT_TEST}; then :; else tp_missing+=(\"$tp_n\"); fi\ndone\nif [ \"${{#tp_missing[@]}}\" -gt 0 ]; then\n  pacman -S --needed --noconfirm \"${{tp_missing[@]}}\"\n  rc=$?\n  if [ \"$rc\" -eq 0 ]; then changed=1; installed=(\"${{tp_missing[@]}}\"); fi\nfi\n"
    ));
}

/// Remove only names `pacman -Q` lists. No dependency cascade.
fn pacman_absent(o: &mut String) {
    o.push_str(&format!(
        "tp_have=()\nfor tp_n in \"${{tp_want[@]}}\"; do\n  {PACMAN_ABSENT_TEST} && tp_have+=(\"$tp_n\")\ndone\nif [ \"${{#tp_have[@]}}\" -gt 0 ]; then\n  pacman -R --noconfirm \"${{tp_have[@]}}\"\n  rc=$?\n  if [ \"$rc\" -eq 0 ]; then changed=1; installed=(\"${{tp_have[@]}}\"); fi\nfi\n"
    ));
}

/// rpm's installed versions of one package, one per line, in the forms a
/// pin may be written: `V`, `V-R`, and `E:V-R` when there is an epoch.
const RPM_FORMS: &str =
    "%{VERSION}\\n%{VERSION}-%{RELEASE}\\n%|EPOCH?{%{EPOCH}:%{VERSION}-%{RELEASE}\\n}|";

/// `.pin(v)` and `.latest()` on dnf and yum. Pinned: a name is missing when
/// no installed copy matches the pin in any of [`RPM_FORMS`]; `install
/// name-v` runs, then `downgrade name-v` when that left the name at
/// another version. Latest: a missing name is installed, and an installed
/// one for which `check-update` exits 100 is upgraded alone.
fn rpm_versioned(o: &mut String, mgr: &str, w: &Wanted) {
    o.push_str("tp_up=()\n");
    match w {
        Wanted::Pinned(_) => o.push_str(&format!(
            "tp_have() {{ rpm -q --qf '{RPM_FORMS}' \"$1\" 2>/dev/null | grep -qxF -- \"$tp_pin\"; }}\nfor tp_n in \"${{tp_want[@]}}\"; do\n  tp_have \"$tp_n\" || tp_missing+=(\"$tp_n\")\ndone\nfor tp_n in \"${{tp_missing[@]}}\"; do\n  {mgr} install -y \"$tp_n-$tp_pin\"\n  rc=$?\n  if [ \"$rc\" -eq 0 ] && ! tp_have \"$tp_n\"; then {mgr} downgrade -y \"$tp_n-$tp_pin\"; rc=$?; fi\n  [ \"$rc\" -eq 0 ] || break\ndone\nif [ \"$rc\" -eq 0 ] && [ \"${{#tp_missing[@]}}\" -gt 0 ]; then changed=1; installed=(\"${{tp_missing[@]}}\"); fi\n"
        )),
        _ => o.push_str(&format!(
            "for tp_n in \"${{tp_want[@]}}\"; do\n  if rpm -q \"$tp_n\" >/dev/null 2>&1; then\n    {mgr} -q check-update \"$tp_n\" >/dev/null 2>&1\n    if [ $? -eq 100 ]; then tp_up+=(\"$tp_n\"); fi\n  else tp_missing+=(\"$tp_n\"); fi\ndone\nif [ \"${{#tp_missing[@]}}\" -gt 0 ]; then\n  {mgr} install -y \"${{tp_missing[@]}}\"\n  rc=$?\n  if [ \"$rc\" -eq 0 ]; then changed=1; installed+=(\"${{tp_missing[@]}}\"); fi\nfi\nif [ \"$rc\" -eq 0 ] && [ \"${{#tp_up[@]}}\" -gt 0 ]; then\n  {mgr} upgrade -y \"${{tp_up[@]}}\"\n  rc=$?\n  if [ \"$rc\" -eq 0 ]; then changed=1; installed+=(\"${{tp_up[@]}}\"); fi\nfi\n"
        )),
    }
}

/// The installed version of a pacman package, empty when not installed.
const PACMAN_INSTALLED: &str = "pacman -Q \"$tp_n\" 2>/dev/null | cut -d' ' -f2";
/// The repo's version of a package, empty when no sync db has it.
const PACMAN_REPO: &str =
    "pacman -Si \"$tp_n\" 2>/dev/null | sed -n 's/^Version *: //p' | head -n1";

/// `.pin(v)` and `.latest()` on pacman. Arch repos hold one version of a
/// package, so a pin installs only when it equals the repo's version
/// (prepare blocks any other; the script refuses too). Latest: `-S
/// --needed` for what is missing or whose repo version is newer
/// (`vercmp`).
fn pacman_versioned(o: &mut String, w: &Wanted) {
    o.push_str("tp_up=()\n");
    match w {
        Wanted::Pinned(_) => o.push_str(&format!(
            "for tp_n in \"${{tp_want[@]}}\"; do\n  [ \"$({PACMAN_INSTALLED})\" = \"$tp_pin\" ] || tp_missing+=(\"$tp_n\")\ndone\nfor tp_n in \"${{tp_missing[@]}}\"; do\n  tp_repo=$({PACMAN_REPO})\n  if [ \"$tp_repo\" != \"$tp_pin\" ]; then echo \"telepatch: pacman cannot install $tp_n at $tp_pin; the repo has ${{tp_repo:-no such package}}\" >&2; rc=1; fi\ndone\nif [ \"$rc\" -eq 0 ] && [ \"${{#tp_missing[@]}}\" -gt 0 ]; then\n  pacman -S --noconfirm \"${{tp_missing[@]}}\"\n  rc=$?\n  if [ \"$rc\" -eq 0 ]; then changed=1; installed=(\"${{tp_missing[@]}}\"); fi\nfi\n"
        )),
        _ => o.push_str(&format!(
            "for tp_n in \"${{tp_want[@]}}\"; do\n  tp_have=$({PACMAN_INSTALLED})\n  if [ -z \"$tp_have\" ]; then tp_missing+=(\"$tp_n\"); continue; fi\n  tp_repo=$({PACMAN_REPO})\n  if [ -n \"$tp_repo\" ] && [ \"$(vercmp \"$tp_repo\" \"$tp_have\")\" -gt 0 ]; then tp_up+=(\"$tp_n\"); fi\ndone\ntp_todo=(\"${{tp_missing[@]}}\" \"${{tp_up[@]}}\")\nif [ \"${{#tp_todo[@]}}\" -gt 0 ]; then\n  pacman -S --needed --noconfirm \"${{tp_todo[@]}}\"\n  rc=$?\n  if [ \"$rc\" -eq 0 ]; then changed=1; installed=(\"${{tp_todo[@]}}\"); fi\nfi\n"
        )),
    }
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
    // A `.latest()` op reads candidates from the lists, so it needs them
    // refreshed; one refresh serves every later op of the window, until
    // sources are written again.
    let mut refreshed = false;
    let flags = ops
        .iter()
        .map(|op| match &op.spec {
            OpSpec::Pkg(p) if p.source == Source::Distro(Manager::Apt) => {
                let flag = std::mem::take(&mut stale) || (p.wanted == Wanted::Latest && !refreshed);
                refreshed |= flag;
                flag
            }
            _ => {
                if op.writes.iter().any(writes_apt_sources) {
                    stale = true;
                    refreshed = false;
                }
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

crate::host::op_builders!(PkgHandle);

impl PkgHandle {
    fn spec(&self, f: impl FnOnce(&mut PkgSpec)) {
        self.0.edit(|op| {
            if let OpSpec::Pkg(s) = &mut op.spec {
                f(s);
            }
        });
    }

    /// Switch this op to another source: re-run the source's name and
    /// principal checks and re-derive the default writes and postconditions
    /// (a `.writes(..)` or `.post(..)` set before the switch is replaced;
    /// reads, guard, allowances and `.at_end()` stay). Distro-only state
    /// (`build_dep`, `update_cache`) is dropped. A second source call on
    /// one op is a trace error and changes nothing.
    #[track_caller]
    fn source(&self, method: &str, source: Source) -> Self {
        let mut errs: Vec<String> = Vec::new();
        self.0.edit(|op| {
            let OpSpec::Pkg(s) = &mut op.spec else { return };
            if s.sourced {
                errs.push(format!(
                    "{method}(): this pkg op's source is already {}; one source call per op",
                    s.source_name()
                ));
                return;
            }
            if s.file.is_some() {
                errs.push("from() is only available on apt and dnf hosts".to_string());
                s.file = None;
            }
            s.sourced = true;
            s.source = source;
            s.wanted = Wanted::Present;
            s.mode = PkgMode::Install;
            s.update_cache = false;
            errs.extend(s.source_problems());
            op.writes = op.spec.default_writes();
            op.post = op.spec.default_post();
        });
        for e in errs {
            self.0.record_error(TraceError::new(e));
        }
        self.clone()
    }

    /// Install with `yum` instead of the host's default manager.
    #[track_caller]
    pub fn yum(&self) -> Self {
        self.source("yum", Source::Distro(Manager::Yum))
    }

    /// Python packages via pip (`pkg:pip:<name>` keys).
    #[track_caller]
    pub fn pip(&self) -> Self {
        self.source("pip", Source::Pip)
    }

    /// Crates installed with `cargo install`, for the op's principal
    /// (`pkg:cargo:<user>:<name>` keys); writes those keys and
    /// `home:<user>` (which overlaps every `file:` and `bin:` key, so a
    /// fact read under the home waits for the op) unless narrowed with
    /// `.writes([..])`; posts `Installed` per name. The optimizer never
    /// moves a cargo op across a distro `pkg` op (a build dependency), and
    /// a rustup op before it writes `*`, so cargo runs after the toolchain
    /// in trace order. `.pin(v)` takes one name; `.latest()` asks `cargo
    /// search` on the host.
    #[track_caller]
    pub fn cargo(&self) -> Self {
        self.source("cargo", Source::Cargo { locked: false })
    }

    /// Cargo: install with `cargo install --locked` (the crates' own
    /// `Cargo.lock`). Part of the source: a locked op never merges with an
    /// unlocked one. On any other source: a trace error.
    #[track_caller]
    pub fn locked(&self) -> Self {
        let mut cargo = true;
        self.spec(|s| match &mut s.source {
            Source::Cargo { locked } => *locked = true,
            _ => cargo = false,
        });
        if !cargo {
            self.0
                .record_error(TraceError::new("locked() is only available on cargo ops"));
        }
        self.clone()
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
    #[track_caller]
    pub fn mise(&self) -> Self {
        self.source("mise", Source::Mise)
    }

    /// The one name is a rust toolchain (`stable`, `1.94.0`,
    /// `nightly-2026-10-01`) for the op's principal, with options on the
    /// handle (`.targets`, `.components`, `.profile`, `.set_default`,
    /// `.installer`). Keys `pkg:rustup:<user>:<tc>` plus
    /// `...:<tc>/target/<t>` and `...:<tc>/component/<c>`; writes `*`
    /// unless narrowed. `.latest()` asks `rustup check`; `.pin(..)` is a
    /// trace error (name a version toolchain instead). Ops for one user and
    /// toolchain merge across roles, Present and Latest into one that wants
    /// Latest, when nothing between them touches what the later op adds
    /// (the optimizer's rustup hoist, [`crate::optimize`]); a different
    /// `.profile(..)` or `.installer(..)` between two such ops is a trace
    /// error naming both sites. In a later window only what the earlier
    /// ones lacked is asked.
    #[track_caller]
    pub fn rustup(&self) -> Self {
        self.source("rustup", Source::Rustup(Box::default()))
    }

    /// The one name is a package installed by a vendor script: `script`
    /// says how to probe, install and update it. Keys
    /// `pkg:script:<user>:<name>`; writes `*` unless narrowed with
    /// `.writes([..])`; posts `Installed` and `file:<bin> = Exists(true)`.
    /// Missing required parts are trace errors.
    #[track_caller]
    pub fn installer_script(&self, script: InstallerScript) -> Self {
        self.source("installer_script", Source::Script(Box::new(script)))
    }

    /// The one name is a vendor's binary release: `release` says where to
    /// download it, how to verify it, and where it goes ([`BinaryRelease`];
    /// the source module's doc has the rules). Root only. Keys
    /// `pkg:binary_release:root:<name>`; writes `*` unless narrowed with
    /// `.writes([..])`; posts `Installed` and `file:<bin> = Exists(true)`.
    /// `.pin(v)` and `.latest()` chain on the handle. No checksum source is
    /// the policy error `Unverified`.
    #[track_caller]
    pub fn binary_release(&self, release: BinaryRelease) -> Self {
        self.source("binary_release", Source::BinaryRelease(Box::new(release)))
    }

    /// Install the build dependencies of the named source packages instead
    /// (`apt-get build-dep`). Which packages that installs is unknown, so
    /// the op writes `*` and makes no `Installed` postcondition.
    #[track_caller]
    pub fn build_dep(&self) -> Self {
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
            return self.clone();
        }
        self.0.edit(|op| {
            op.writes = BTreeSet::from([Key::Any]);
            op.post.clear();
        });
        self.clone()
    }

    /// Run `apt-get update` before this op (normally set by the apt cache
    /// pass, [`apply_apt_update`]).
    #[track_caller]
    pub fn update_cache(&self, on: bool) -> Self {
        self.spec(|s| s.update_cache = on);
        self.clone()
    }

    /// Remove the named packages. Only names the host has installed are
    /// removed. Posts `Absent`. Does not merge with an install op.
    #[track_caller]
    pub fn absent(&self) -> Self {
        self.want(Wanted::Absent);
        self.0.edit(|op| {
            let OpSpec::Pkg(p) = &op.spec else { return };
            if p.wanted != Wanted::Absent {
                return;
            }
            for (k, v) in p.default_post() {
                op.post.insert(k, v);
            }
        });
        self.clone()
    }

    /// Apt: remove config files too (`apt-get purge`). Implies
    /// [`Self::absent`]. Any other manager is a trace error.
    #[track_caller]
    pub fn purge(&self) -> Self {
        let mut apt = false;
        self.spec(|s| {
            apt = s.source == Source::Distro(Manager::Apt);
            if apt {
                s.purge = true;
            }
        });
        if !apt {
            self.0
                .record_error(TraceError::new("purge() is only available on apt hosts"));
            return self.clone();
        }
        self.absent()
    }

    /// Want the newest release (pip: `install -U`; distro: upgrade what is
    /// behind).
    #[track_caller]
    pub fn latest(&self) -> Self {
        self.want(Wanted::Latest)
    }

    /// Want exactly version `v` (pip: `name==v`; distro: one name, the
    /// manager's own version string).
    #[track_caller]
    pub fn pin(&self, v: impl Into<String>) -> Self {
        self.want(Wanted::Pinned(v.into()))
    }

    /// Install this one package from a fetched `.deb` (apt) or `.rpm`
    /// (dnf): `apt-get install ./x.deb` or `dnf install -y ./x.rpm`, from
    /// the prepare-phase cache, when the manager does not already report
    /// the name at [`.pin`](Self::pin)'s version. Any other manager is a
    /// trace error. Takes `&Fetched` or `Fetched`. Without `.pin(v)` the
    /// trace fails. A fetch with no checksum is the policy error
    /// `Unverified`.
    #[track_caller]
    pub fn from(&self, fetched: impl std::borrow::Borrow<crate::ops::fetch::Fetched>) -> Self {
        let f = crate::ops::fetch::fetched_of(fetched);
        let mut err: Option<String> = None;
        self.spec(|s| {
            let name = s.names.first().cloned().unwrap_or_default();
            if !matches!(s.source, Source::Distro(Manager::Apt | Manager::Dnf)) {
                err = Some("from() is only available on apt and dnf hosts".to_string());
                return;
            }
            if let Some(p) = f.problem() {
                err = Some(format!("pkg {name}: {p}"));
                return;
            }
            if s.mode == PkgMode::BuildDep {
                err = Some("pkg: .from() does not apply to build-dep".to_string());
                return;
            }
            if s.names.iter().any(|n| n.starts_with('@')) {
                err = Some("pkg: a @group cannot be installed from a file".to_string());
                return;
            }
            s.file = Some(f.clone());
        });
        if let Some(e) = err {
            self.0.record_error(TraceError::new(e));
        }
        self.clone()
    }

    /// Edit a rustup op's options, then add the postconditions of what was
    /// added. On any other source: a trace error naming `method`.
    #[track_caller]
    fn rustup_opt(&self, method: &str, f: impl FnOnce(&mut RustupSpec)) -> Self {
        let mut rustup = true;
        self.spec(|s| match &mut s.source {
            Source::Rustup(r) => f(r),
            _ => rustup = false,
        });
        if !rustup {
            self.0.record_error(TraceError::new(format!(
                "{method}() is only available on rustup ops"
            )));
            return self.clone();
        }
        self.0.edit(|op| {
            let OpSpec::Pkg(p) = &op.spec else { return };
            for (k, v) in p.default_post() {
                op.post.entry(k).or_insert(v);
            }
        });
        self.clone()
    }

    /// Rustup: also have these targets installed (`rustup target add`).
    #[track_caller]
    pub fn targets<I, S>(&self, targets: I) -> Self
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
    #[track_caller]
    pub fn components<I, S>(&self, components: I) -> Self
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
    #[track_caller]
    pub fn profile(&self, profile: impl Into<String>) -> Self {
        let p = profile.into();
        if let Err(e) = check_names(std::slice::from_ref(&p)) {
            self.0.record_error(e);
        }
        self.rustup_opt("profile", |r| r.profile = Some(p))
    }

    /// Rustup: make this toolchain the default (`rustup default`), when it
    /// is not already.
    #[track_caller]
    pub fn set_default(&self) -> Self {
        self.rustup_opt("set_default", |r| r.set_default = true)
    }

    /// Rustup: the shell command that installs rustup when it is absent;
    /// `--default-toolchain <tc> --profile <p>` is appended. Default:
    /// [`DEFAULT_INSTALLER`].
    #[track_caller]
    pub fn installer(&self, cmd: impl Into<String>) -> Self {
        let c = cmd.into();
        self.rustup_opt("installer", |r| r.installer = Some(c))
    }

    #[track_caller]
    fn want(&self, w: Wanted) -> Self {
        let mut distro_err: Option<&str> = None;
        let mut script_err = None;
        let mut cargo_pin_many = false;
        let mut mise_pin_many = false;
        let mut rustup_pin = false;
        self.spec(|s| {
            if s.purge && !matches!(w, Wanted::Absent) {
                distro_err = Some("pkg: .purge() applies only to an absent op");
                return;
            }
            if matches!(w, Wanted::Absent) {
                if !matches!(s.source, Source::Distro(_)) {
                    distro_err = Some("absent() is only available on the distro manager");
                } else if s.mode == PkgMode::BuildDep {
                    distro_err = Some("pkg: build-dep cannot be absent");
                } else if s.names.iter().any(|n| n.starts_with('@')) {
                    distro_err = Some("pkg: a @group cannot be absent");
                } else {
                    s.wanted = Wanted::Absent;
                }
                return;
            }
            if matches!(s.source, Source::Distro(_)) {
                if s.mode == PkgMode::BuildDep {
                    distro_err = Some("pkg: build-dep cannot be pinned or latest");
                } else if s.names.iter().any(|n| n.starts_with('@')) {
                    distro_err = Some("pkg: a @group cannot be pinned or latest");
                } else if matches!(w, Wanted::Pinned(_)) && s.names.len() != 1 {
                    distro_err = Some("pkg: a pinned op takes exactly one package name");
                } else {
                    s.wanted = w;
                }
                return;
            }
            if let (Source::Script(sc), Wanted::Latest) = (&s.source, &w) {
                let name = s.names.first().map(String::as_str).unwrap_or("");
                script_err = sc.latest_problem(name);
            }
            if let Source::BinaryRelease(b) = &s.source {
                let name = s.names.first().map(String::as_str).unwrap_or("");
                script_err = b.want_problem(name, &w);
            }
            if matches!(s.source, Source::Rustup(_)) && matches!(w, Wanted::Pinned(_)) {
                rustup_pin = true;
                return;
            }
            cargo_pin_many = matches!(s.source, Source::Cargo { .. })
                && matches!(w, Wanted::Pinned(_))
                && s.names.len() != 1;
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
        if let Some(e) = distro_err {
            self.0.record_error(TraceError::new(e));
        }
        if let Some(e) = script_err {
            self.0.record_error(TraceError::new(e));
        }
        self.clone()
    }
}

fn names<I, S>(names: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    names.into_iter().map(|s| s.as_ref().to_string()).collect()
}

/// Trace errors that are only visible once the builders have all run:
/// `.from()` without `.pin()`, or with more than one name.
pub(crate) fn file_errors(trace: &crate::trace::Trace) -> Vec<TraceError> {
    let mut out = Vec::new();
    for (i, n) in trace.nodes.iter().enumerate() {
        let crate::trace::Node::Op(op) = n else {
            continue;
        };
        let OpSpec::Pkg(p) = &op.spec else {
            continue;
        };
        if p.file.is_none() {
            continue;
        }
        let msg = if p.names.len() != 1 {
            "pkg: .from() takes exactly one package name"
        } else if !matches!(p.wanted, Wanted::Pinned(_)) {
            "pkg: .from() needs .pin(version)"
        } else {
            continue;
        };
        let mut e = TraceError::new(msg);
        e.site = None;
        out.push(trace.locate(crate::trace::NodeId(i), e));
    }
    out
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
    #[track_caller]
    fn pkg_op(&self, manager: Manager, v: Vec<String>) -> PkgHandle {
        self.pkg_spec_op(PkgSpec::new(manager, v))
    }

    /// Push a pkg op for any source.
    #[track_caller]
    pub(crate) fn pkg_spec_op(&self, mut spec: PkgSpec) -> PkgHandle {
        if let Err(e) = check_names(&spec.names) {
            self.record_error(e);
        }
        spec.user = match &self.run_as().principal {
            Principal::Root => "root".to_string(),
            Principal::User(u) => u.clone(),
        };
        for e in spec.source_problems() {
            self.record_error(TraceError::new(e));
        }
        PkgHandle(self.op(OpSpec::Pkg(spec)))
    }

    /// Ensure packages are installed with the host's manager: apt on
    /// Debian-family, dnf on RedHat-family (Fedora, EL, Amazon Linux 2023),
    /// pacman on Arch. An unknown family is recorded as a trace error.
    #[track_caller]
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
        h.snapshot()
            .errors
            .iter()
            .map(|e| e.message.clone())
            .collect()
    }

    #[test]
    fn pip_pin_renders_name_eq_version() {
        let h = host();
        h.pkg(["requests", "six"]).pip().pin("2.31.0");
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
        h.pkg(["a"]).pip().latest();
        let s = spec_of(&h, 0);
        assert_eq!(s.wanted, Wanted::Latest);
        let mut want =
            String::from("rc=0; changed=0; installed=()\ntp_want=('a')\ntp_missing=()\n");
        pip_latest(&mut want);
        assert_eq!(s.render(), want);
        assert!(s.render().contains("install -U"));
    }

    #[test]
    fn distro_pin_and_latest_set_wanted() {
        let h = host();
        h.pkg(["git"]).pin("1:2.0-1");
        h.pkg(["git"]).latest();
        assert!(errors(&h).is_empty(), "{:?}", errors(&h));
        assert_eq!(spec_of(&h, 0).wanted, Wanted::Pinned("1:2.0-1".into()));
        assert_eq!(spec_of(&h, 1).wanted, Wanted::Latest);
    }

    #[test]
    fn distro_pin_takes_one_plain_name() {
        let h = host();
        h.pkg(["a", "b"]).pin("1");
        h.pkg(["@Tools"]).pin("1");
        h.pkg(["@Tools"]).latest();
        h.pkg(["a"]).build_dep().latest();
        assert_eq!(
            errors(&h),
            [
                "pkg: a pinned op takes exactly one package name",
                "pkg: a @group cannot be pinned or latest",
                "pkg: a @group cannot be pinned or latest",
                "pkg: build-dep cannot be pinned or latest",
            ]
        );
    }

    #[test]
    fn absent_posts_absent_and_removes_only_installed_names() {
        let h = host();
        h.pkg(["cmake"]).absent();
        assert!(errors(&h).is_empty(), "{:?}", errors(&h));
        let t = h.snapshot();
        let Node::Op(op) = &t.nodes[0] else { panic!() };
        let OpSpec::Pkg(p) = &op.spec else { panic!() };
        assert_eq!(p.wanted, Wanted::Absent);
        assert!(!p.purge);
        assert_eq!(op.post.get(&Key::pkg("cmake")), Some(&Value::Absent));
        let r = p.render();
        assert!(r.contains("db:Status-Status"), "{r}");
        assert!(r.contains(" apt-get "), "{r}");
        assert!(r.contains(" remove "), "{r}");
        assert!(!r.contains(" install "), "{r}");
        assert!(!r.contains("force-conf"), "{r}");
        assert!(!r.contains("dpkg --purge"), "{r}");
        assert!(!r.contains("config-files"), "{r}");
        assert!(r.contains("installed=(\"${tp_have[@]}\")"), "{r}");

        let mut dnf = PkgSpec::new(Manager::Dnf, names(["cmake"]));
        dnf.wanted = Wanted::Absent;
        let r = dnf.render();
        assert!(r.contains("rpm -q"), "{r}");
        assert!(r.contains("dnf remove -y"), "{r}");
        assert!(!r.contains("dnf install"), "{r}");

        let mut pac = PkgSpec::new(Manager::Pacman, names(["cmake"]));
        pac.wanted = Wanted::Absent;
        let r = pac.render();
        assert!(r.contains("pacman -Q"), "{r}");
        assert!(r.contains("pacman -R --noconfirm"), "{r}");
        assert!(!r.contains("pacman -S"), "{r}");
    }

    #[test]
    fn purge_is_apt_only_and_removes_config_files() {
        let h = host();
        h.pkg(["nano"]).absent().purge();
        assert!(errors(&h).is_empty(), "{:?}", errors(&h));
        let p = spec_of(&h, 0);
        assert!(p.purge);
        assert_eq!(p.wanted, Wanted::Absent);
        let r = p.render();
        assert!(r.contains(" purge "), "{r}");
        assert!(!r.contains(" remove "), "{r}");
        // Upgrade-only dpkg options stay off this path, and a package left
        // in config-files is finished with dpkg --purge.
        assert!(!r.contains("force-conf"), "{r}");
        assert!(r.contains("config-files"), "{r}");
        assert!(r.contains("dpkg --purge"), "{r}");

        let os = Os::parse("ID=fedora\nVERSION_ID=41\n").unwrap();
        let f = Host::new("f", os, Rc::new(()));
        f.pkg(["nano"]).absent().purge();
        assert_eq!(errors(&f), ["purge() is only available on apt hosts"]);

        let h = host();
        h.pkg(["nano"]).pip().purge();
        assert!(
            errors(&h).iter().any(|e| e.contains("purge()")),
            "{:?}",
            errors(&h)
        );

        let h = host();
        h.pkg(["requests"]).pip().absent();
        assert_eq!(
            errors(&h),
            ["absent() is only available on the distro manager"]
        );
    }

    fn keyring(h: &Host<()>) -> crate::ops::fetch::Fetched {
        h.fetch("http://127.0.0.1:8765/keyring.deb")
            .sha256("ab".repeat(32))
    }

    /// `h.pkg([name]).from(deb).pin(v)` installs that file through apt or
    /// dnf when the manager's name and version are not already the pin.
    /// The file is the checksum cache, named `./x.deb` or `./x.rpm` for
    /// the install, and the script never writes under `/tmp`.
    #[test]
    fn from_a_fetched_file_checks_name_and_version() {
        let h = host();
        let deb = keyring(&h);
        h.pkg(["cuda-keyring"]).from(&deb).pin("1.2.3");
        assert!(errors(&h).is_empty(), "{:?}", errors(&h));
        let p = spec_of(&h, 0);
        assert_eq!(p.wanted, Wanted::Pinned("1.2.3".into()));
        assert_eq!(
            p.file.as_ref().map(|f| f.url.as_str()),
            Some(deb.url.as_str())
        );
        let r = p.render();
        assert!(r.contains("db:Status-Status"), "{r}");
        assert!(r.contains("${Version}"), "{r}");
        assert!(r.contains("installed:$tp_pin"), "{r}");
        assert!(r.contains("apt-get"), "{r}");
        assert!(r.contains("install ./x.deb"), "{r}");
        assert!(r.contains("/var/cache/telepatch/sha256"), "{r}");
        assert!(!r.replace("/var/tmp", "").contains("/tmp"), "{r}");
        assert!(!p.mergeable(), "a file install does not merge");
        let Node::Op(op) = &h.snapshot().nodes[0] else {
            panic!("not an op");
        };
        assert!(crate::engine::needs_download(op));
        let window = crate::engine::Window {
            number: 1,
            kind: crate::engine::WindowKind::Final,
            ops: vec![crate::engine::WindowOp {
                id: crate::trace::NodeId(0),
                node: op.clone(),
                members: Vec::new(),
            }],
            split: None,
            assertions: Vec::new(),
        };
        let got = crate::ops::fetch::items(&window);
        let sum = "ab".repeat(32);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].url, deb.url);
        assert_eq!(got[0].sum.as_deref(), Some(sum.as_str()));
        assert!(!got[0].unpack);
        assert!(crate::ops::fetch::uses(op, &got[0]));

        let os = Os::parse("ID=fedora\nVERSION_ID=41\n").unwrap();
        let f = Host::new("f", os, Rc::new(()));
        let rpm = f
            .fetch("http://127.0.0.1:8765/keyring.rpm")
            .sha256("cd".repeat(32));
        f.pkg(["cuda-keyring"]).from(&rpm).pin("1.2.3-1");
        assert!(errors(&f).is_empty(), "{:?}", errors(&f));
        let r = spec_of(&f, 0).render();
        assert!(r.contains("dnf install -y ./x.rpm"), "{r}");
        assert!(r.contains("rpm -q"), "{r}");
        assert!(r.contains("/var/cache/telepatch/sha256"), "{r}");
        assert!(!r.replace("/var/tmp", "").contains("/tmp"), "{r}");

        let h = host();
        h.pkg(["cuda-keyring"]).from(keyring(&h));
        assert!(
            errors(&h).iter().any(|e| e.contains(".pin(")),
            "{:?}",
            errors(&h)
        );

        let os = Os::parse("ID=arch\nBUILD_ID=rolling\n").unwrap();
        let a = Host::new("a", os, Rc::new(()));
        a.pkg(["cuda-keyring"]).from(keyring(&a)).pin("1.2.3");
        assert!(
            errors(&a).iter().any(|e| e.contains("apt and dnf")),
            "{:?}",
            errors(&a)
        );

        let h = host();
        h.pkg(["cuda-keyring"]).yum().from(keyring(&h)).pin("1.2.3");
        assert!(
            errors(&h).iter().any(|e| e.contains("apt and dnf")),
            "{:?}",
            errors(&h)
        );
    }

    #[test]
    fn absent_never_merges_with_an_install() {
        let inst = PkgSpec::new(Manager::Apt, names(["a"]));
        let mut ab = PkgSpec::new(Manager::Apt, names(["b"]));
        ab.wanted = Wanted::Absent;
        assert!(!inst.can_merge(&ab));
        assert!(!ab.can_merge(&inst));
        let mut ab2 = ab.clone();
        ab2.names = names(["c"]);
        assert!(ab.can_merge(&ab2));
        let mut pur = ab.clone();
        pur.purge = true;
        assert!(!ab.can_merge(&pur));
        assert!(pur.can_merge(&pur));
    }

    #[test]
    fn distro_pinned_never_merges_and_latest_not_with_present() {
        let m = Manager::Apt;
        let present = PkgSpec::new(m, names(["a"]));
        let mut latest = PkgSpec::new(m, names(["b"]));
        latest.wanted = Wanted::Latest;
        let mut pinned = PkgSpec::new(m, names(["c"]));
        pinned.wanted = Wanted::Pinned("1".into());
        assert!(!present.can_merge(&latest));
        assert!(!latest.can_merge(&present));
        assert!(latest.can_merge(&latest.clone()));
        assert!(!pinned.can_merge(&pinned.clone()));
        assert!(!present.can_merge(&pinned));
    }

    fn distro(m: Manager, w: Wanted) -> String {
        let mut p = PkgSpec::new(m, names(["curl"]));
        p.wanted = w;
        p.render()
    }

    #[test]
    fn distro_pin_renders_per_manager() {
        let pin = || Wanted::Pinned("1.2-3".into());
        let apt = distro(Manager::Apt, pin());
        assert!(apt.contains("tp_pin='1.2-3'"), "{apt}");
        assert!(apt.contains("${Version}"), "{apt}");
        assert!(apt.contains("$tp_n=$tp_pin"), "{apt}");
        assert!(apt.contains("--allow-downgrades"), "{apt}");
        let dnf = distro(Manager::Dnf, pin());
        assert!(dnf.contains("tp_pin='1.2-3'"), "{dnf}");
        assert!(dnf.contains("%{VERSION}-%{RELEASE}"), "{dnf}");
        assert!(dnf.contains("dnf install -y \"$tp_n-$tp_pin\""), "{dnf}");
        assert!(dnf.contains("dnf downgrade -y"), "{dnf}");
        let yum = distro(Manager::Yum, pin());
        assert!(yum.contains("yum install -y \"$tp_n-$tp_pin\""), "{yum}");
        let pac = distro(Manager::Pacman, pin());
        assert!(pac.contains("pacman -Q"), "{pac}");
        assert!(pac.contains("pacman -Si"), "{pac}");
        assert!(pac.contains("pacman -S --noconfirm"), "{pac}");
    }

    #[test]
    fn distro_latest_upgrades_only_what_is_behind() {
        let apt = distro(Manager::Apt, Wanted::Latest);
        assert!(apt.contains("apt-cache policy"), "{apt}");
        assert!(apt.contains("--only-upgrade"), "{apt}");
        let dnf = distro(Manager::Dnf, Wanted::Latest);
        assert!(dnf.contains("dnf -q check-update"), "{dnf}");
        assert!(dnf.contains("dnf upgrade -y \"${tp_up[@]}\""), "{dnf}");
        let pac = distro(Manager::Pacman, Wanted::Latest);
        assert!(pac.contains("vercmp"), "{pac}");
        assert!(pac.contains("pacman -S --needed --noconfirm"), "{pac}");
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
        h.pkg(["stable"])
            .rustup()
            .targets(["x86_64-unknown-linux-musl", "x86_64-unknown-linux-musl"])
            .components(["rust-src"]);
        h.as_user("deploy").pkg(["1.94.0"]).rustup();
        h.as_user("root")
            .pkg(["nightly"])
            .rustup()
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
        h.pkg(["stable"]).rustup().pin("1.94.0");
        assert_eq!(errors(&h).len(), 1);
        assert!(
            errors(&h)[0].contains("name a version toolchain instead"),
            "{:?}",
            errors(&h)
        );
        assert_eq!(spec_of(&h, 0).wanted, Wanted::Present);
        h.pkg(["stable"]).rustup().latest();
        assert_eq!(errors(&h).len(), 1);
        // The pin never reaches a spec through the builders; merge still
        // refuses one.
        let mut a = PkgSpec::from_source(Source::Rustup(Box::default()), names(["stable"]));
        assert!(a.mergeable());
        a.wanted = Wanted::Pinned("1.94.0".into());
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
        h.pkg(["x"]).cargo().targets(["t"]);
        h.pkg(["git"]).components(["c"]);
        h.pkg(["x"]).pip().profile("minimal");
        h.pkg(["x"]).cargo().set_default();
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

    fn uv() -> InstallerScript {
        InstallerScript::new()
            .bin("/usr/local/bin/uv")
            .version("uv --version")
            .install("sh install.sh")
    }

    #[test]
    fn script_missing_parts_are_trace_errors() {
        let h = host();
        h.pkg(["uv"]).installer_script(InstallerScript::new());
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
        h.pkg(["uv"]).installer_script(uv()).latest();
        assert_eq!(errors(&h), ["script uv: latest is required for .latest()"]);
        let h = host();
        h.pkg(["uv"])
            .installer_script(uv().latest(Latest::command("echo 1")))
            .latest();
        h.pkg(["uv"]).installer_script(uv()).pin("1.0");
        assert!(errors(&h).is_empty(), "{:?}", errors(&h));
    }

    #[test]
    fn script_update_defaults_to_install() {
        let h = host();
        h.pkg(["uv"]).installer_script(uv());
        let Source::Script(s) = spec_of(&h, 0).source else {
            panic!()
        };
        assert_eq!(s.update_cmd(), "sh install.sh");
        assert_eq!(s.clone().update("u").update_cmd(), "u");
    }

    #[test]
    fn script_keys_name_the_principal() {
        let h = host();
        h.pkg(["uv"]).installer_script(uv());
        h.as_user("deploy").pkg(["uv"]).installer_script(uv());
        h.as_user("root").pkg(["uv"]).installer_script(uv());
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
        h.pkg(["uv"])
            .installer_script(uv())
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
        h.pkg(["cargo-deb", "cargo-generate-rpm"]).cargo();
        h.as_user("deploy").pkg(["cargo-deb"]).cargo();
        h.as_user("root")
            .pkg(["x"])
            .cargo()
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
        assert_eq!(
            ops[0].writes,
            BTreeSet::from([
                Key::pkg("cargo:root:cargo-deb"),
                Key::pkg("cargo:root:cargo-generate-rpm"),
                Key::home("root"),
            ])
        );
        assert_eq!(
            ops[1].writes,
            BTreeSet::from([Key::pkg("cargo:deploy:cargo-deb"), Key::home("deploy")])
        );
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
        h.pkg(["a"]).cargo().pin("1.0.0");
        assert!(errors(&h).is_empty());
        h.pkg(["a", "b"]).cargo().pin("1.0.0");
        assert_eq!(
            errors(&h),
            ["cargo: a pinned op takes exactly one package name"]
        );
        let mut a = PkgSpec::from_source(Source::Cargo { locked: false }, names(["a"]));
        let mut b = PkgSpec::from_source(Source::Cargo { locked: false }, names(["b"]));
        assert!(a.can_merge(&b));
        a.wanted = Wanted::Pinned("1.0.0".into());
        b.wanted = Wanted::Pinned("1.0.0".into());
        assert!(!a.can_merge(&b));
        assert!(!a.mergeable());
    }

    #[test]
    fn locked_is_cargo_only_and_part_of_the_source() {
        let h = host();
        h.pkg(["x"]).cargo().locked();
        assert!(errors(&h).is_empty());
        h.pkg(["x"]).locked();
        h.pkg(["x"]).pip().locked();
        assert_eq!(
            errors(&h),
            [
                "locked() is only available on cargo ops",
                "locked() is only available on cargo ops"
            ]
        );
        let l = PkgSpec::from_source(Source::Cargo { locked: true }, names(["a"]));
        let u = PkgSpec::from_source(Source::Cargo { locked: false }, names(["b"]));
        assert!(l.can_merge(&l.clone()));
        assert!(!l.can_merge(&u));
    }

    #[test]
    fn rustup_merges_per_user_toolchain_and_unions_options() {
        let rs = |user: &str, tc: &str, t: &str| {
            let mut p = PkgSpec::from_source(Source::Rustup(Box::default()), names([tc]));
            p.user = user.to_string();
            if let Source::Rustup(r) = &mut p.source {
                r.targets.push(t.to_string());
            }
            p
        };
        let mut a = rs("root", "stable", "t1");
        let mut b = rs("root", "stable", "t2");
        if let Source::Rustup(r) = &mut b.source {
            r.set_default = true;
        }
        assert!(a.can_merge(&b));
        assert!(!a.can_merge(&rs("deploy", "stable", "t1")));
        assert!(!a.can_merge(&rs("root", "nightly", "t1")));
        a.merge(&b);
        let Source::Rustup(r) = &a.source else {
            panic!()
        };
        assert_eq!(r.targets, ["t1", "t2"]);
        assert!(r.set_default);
    }

    /// For rustup, `wanted` is not part of what must be equal: Present and
    /// Latest merge and the merged op wants Latest, whichever came first.
    /// Pinned never merges.
    #[test]
    fn rustup_present_and_latest_merge_to_latest() {
        let rs = |w: Wanted| {
            let mut p = PkgSpec::from_source(Source::Rustup(Box::default()), names(["stable"]));
            p.wanted = w;
            p
        };
        for (a, b) in [
            (Wanted::Present, Wanted::Latest),
            (Wanted::Latest, Wanted::Present),
            (Wanted::Latest, Wanted::Latest),
        ] {
            let mut x = rs(a.clone());
            let y = rs(b.clone());
            assert!(x.can_merge(&y), "{a:?} {b:?}");
            x.merge(&y);
            assert_eq!(x.wanted, Wanted::Latest, "{a:?} {b:?}");
        }
        let mut x = rs(Wanted::Present);
        x.merge(&rs(Wanted::Present));
        assert_eq!(x.wanted, Wanted::Present);
        assert!(!rs(Wanted::Present).can_merge(&rs(Wanted::Pinned("1".into()))));
        // Distro and cargo keep `wanted` in the key.
        let mut d = PkgSpec::new(Manager::Apt, names(["git"]));
        let mut l = d.clone();
        l.wanted = Wanted::Latest;
        assert!(!d.can_merge(&l));
        d.source = Source::Cargo { locked: false };
        l.source = Source::Cargo { locked: false };
        assert!(!d.can_merge(&l));
    }

    #[test]
    fn mise_keys_writes_kind_and_post() {
        let h = host();
        h.pkg(["just", "fd"]).mise();
        h.as_user("root")
            .pkg(["bat"])
            .mise()
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
        h.as_user("bot").pkg(["just"]).mise();
        assert_eq!(errors(&h), ["mise source is system-wide (root only)"]);
        let h = host();
        h.as_user("root").pkg(["just"]).mise();
        h.pkg(["just"]).mise();
        assert!(errors(&h).is_empty());
    }

    #[test]
    fn mise_pin_takes_one_name_and_never_merges() {
        let h = host();
        h.pkg(["a"]).mise().pin("1.0.0");
        assert!(errors(&h).is_empty());
        h.pkg(["a", "b"]).mise().pin("1.0.0");
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
    // T78: one `pkg`, every source chained.

    fn op_of(h: &Host<()>, i: usize) -> OpNode {
        match &h.snapshot().nodes[i] {
            Node::Op(o) => o.clone(),
            n => panic!("not an op: {n:?}"),
        }
    }

    fn chain_script() -> InstallerScript {
        InstallerScript::new()
            .bin("/usr/local/bin/uv")
            .version("uv --version")
            .install("sh install.sh")
    }

    #[test]
    fn chained_pip_switches_the_source_and_keeps_the_op() {
        let h = host();
        let handle = h.pkg(["requests", "six"]).pip();
        let o = op_of(&h, 0);
        let OpSpec::Pkg(p) = &o.spec else { panic!() };
        assert_eq!(p.source, Source::Pip);
        assert_eq!(p.kind(), "pkg");
        assert_eq!(handle.id().0, 0);
        assert_eq!(h.snapshot().nodes.len(), 1);
        let w: Vec<String> = o.writes.iter().map(|k| k.to_string()).collect();
        assert_eq!(w, ["pkg:pip:requests", "pkg:pip:six"]);
        assert!(errors(&h).is_empty(), "{:?}", errors(&h));
    }

    #[test]
    fn chained_yum_is_the_old_yum_op() {
        let h = host();
        h.pkg(["git"]).yum();
        let p = spec_of(&h, 0);
        assert_eq!(p.source, Source::Distro(Manager::Yum));
        assert_eq!(p.kind(), "pkg");
        assert_eq!(p.keys().iter().next().unwrap().to_string(), "pkg:git");
    }

    #[test]
    fn chained_cargo_mise_rustup_installer_script_switch_defaults() {
        let h = host();
        h.pkg(["cargo-deb"]).cargo();
        h.pkg(["just"]).mise();
        h.pkg(["stable"])
            .rustup()
            .targets(["wasm32-unknown-unknown"]);
        h.pkg(["uv"]).installer_script(chain_script());
        let kinds: Vec<&str> = (0..4).map(|i| spec_of(&h, i).kind()).collect();
        assert_eq!(
            kinds,
            ["pkg[cargo]", "pkg[mise]", "pkg[rustup]", "pkg[script]"]
        );
        assert_eq!(
            op_of(&h, 0).writes,
            BTreeSet::from([Key::pkg("cargo:root:cargo-deb"), Key::home("root")])
        );
        for i in 1..4 {
            assert_eq!(op_of(&h, i).writes, BTreeSet::from([Key::Any]), "{i}");
        }
        let post =
            |i: usize| -> Vec<String> { op_of(&h, i).post.keys().map(|k| k.to_string()).collect() };
        assert_eq!(post(0), ["pkg:cargo:root:cargo-deb"]);
        assert_eq!(
            post(1),
            ["pkg:mise:root:just", "file:/etc/mise/config.toml"]
        );
        assert_eq!(
            post(2),
            [
                "pkg:rustup:root:stable",
                "pkg:rustup:root:stable/target/wasm32-unknown-unknown"
            ]
        );
        assert_eq!(post(3), ["pkg:script:root:uv", "file:/usr/local/bin/uv"]);
        assert!(errors(&h).is_empty(), "{:?}", errors(&h));
    }

    #[test]
    fn a_second_source_call_is_a_trace_error_at_the_caller() {
        let h = host();
        let l = line!() + 1;
        h.pkg(["x"]).pip().cargo();
        let t = h.snapshot();
        assert_eq!(t.errors.len(), 1, "{:?}", t.errors);
        assert!(
            t.errors[0].message.contains("source") && t.errors[0].message.contains("cargo"),
            "{}",
            t.errors[0].message
        );
        let site = t.errors[0].site.expect("site");
        assert_eq!((site.file, site.line), (file!(), l));
        // The first source stands.
        assert_eq!(spec_of(&h, 0).source, Source::Pip);
        // `.yum().yum()` counts too.
        let h = host();
        h.pkg(["x"]).yum().yum();
        assert_eq!(errors(&h).len(), 1);
    }

    #[test]
    fn a_source_switch_reruns_the_source_checks() {
        let h = host();
        h.pkg(["stable", "beta"]).rustup();
        h.pkg(["a", "b"]).installer_script(chain_script());
        h.pkg(["uv"]).installer_script(InstallerScript::new());
        h.as_user("bob").pkg(["node"]).mise();
        assert_eq!(
            errors(&h),
            [
                "rustup: exactly one toolchain name per rustup op",
                "script a: exactly one package name per script op",
                "script uv: bin is required",
                "script uv: version is required",
                "script uv: install is required",
                "mise source is system-wide (root only)",
            ]
        );
    }

    #[test]
    fn a_source_switch_drops_distro_only_state_and_rederives_effects() {
        let h = host();
        h.pkg(["libc6"]).build_dep().update_cache(true).pip();
        let o = op_of(&h, 0);
        let OpSpec::Pkg(p) = &o.spec else { panic!() };
        assert_eq!(p.mode, PkgMode::Install);
        assert!(!p.update_cache);
        assert_eq!(p.wanted, Wanted::Present);
        assert_eq!(o.post.len(), 1);
        assert_eq!(o.writes.len(), 1);
        // Reads, guards and allowances set before the switch stay.
        let h = host();
        h.pkg(["a"]).reads([Key::file("/x")]).at_end().cargo();
        let o = op_of(&h, 0);
        assert!(o.at_end);
        assert_eq!(o.reads, BTreeSet::from([Key::file("/x")]));
    }

    /// Present accepts dpkg `installed` or an installed package's Provides.
    /// A name that is neither is still missing. Pin, latest, absent and a
    /// `.deb` file install keep the status check. dnf and pacman stay on
    /// `rpm -q --whatprovides` and `pacman -T`.
    #[test]
    fn apt_present_counts_a_provide_and_leaves_other_checks() {
        let mut present = PkgSpec::new(Manager::Apt, names(["w3m", "www-browser", "missing-name"]));
        present.wanted = Wanted::Present;
        let r = present.render();
        assert!(
            r.contains("${Provides}"),
            "present check does not look at Provides:\n{r}"
        );
        assert!(
            !r.contains(
                "[ \"$(dpkg-query -W -f='${db:Status-Status}' \"$tp_n\" 2>/dev/null)\" = installed ] || tp_missing"
            ),
            "present check is still status-only:\n{r}"
        );
        assert!(r.contains("db:Status-Status"), "{r}");

        let mut pin = PkgSpec::new(Manager::Apt, names(["git"]));
        pin.wanted = Wanted::Pinned("1:2".into());
        let pin_r = pin.render();
        assert!(!pin_r.contains("${Provides}"), "{pin_r}");
        assert!(pin_r.contains("installed:$tp_pin"), "{pin_r}");

        let mut latest = PkgSpec::new(Manager::Apt, names(["git"]));
        latest.wanted = Wanted::Latest;
        let latest_r = latest.render();
        assert!(!latest_r.contains("${Provides}"), "{latest_r}");
        assert!(latest_r.contains("Candidate:"), "{latest_r}");

        let mut absent = PkgSpec::new(Manager::Apt, names(["git"]));
        absent.wanted = Wanted::Absent;
        let absent_r = absent.render();
        assert!(!absent_r.contains("${Provides}"), "{absent_r}");
        assert!(absent_r.contains("db:Status-Status"), "{absent_r}");

        let h = host();
        let deb = keyring(&h);
        h.pkg(["cuda-keyring"]).from(&deb).pin("1.2.3");
        let file_r = spec_of(&h, 0).render();
        assert!(!file_r.contains("${Provides}"), "{file_r}");
        assert!(file_r.contains("installed:$tp_pin"), "{file_r}");

        let mut dnf = PkgSpec::new(Manager::Dnf, names(["mailx"]));
        dnf.wanted = Wanted::Present;
        let dnf_r = dnf.render();
        assert!(dnf_r.contains("rpm -q --whatprovides"), "{dnf_r}");
        assert!(!dnf_r.contains("${Provides}"), "{dnf_r}");

        let mut pac = PkgSpec::new(Manager::Pacman, names(["sh"]));
        pac.wanted = Wanted::Present;
        let pac_r = pac.render();
        assert!(pac_r.contains("pacman -T"), "{pac_r}");
        assert!(!pac_r.contains("${Provides}"), "{pac_r}");

        let script = format!(
            r#"
dpkg-query() {{
  local fmt="" name=""
  while [ $# -gt 0 ]; do
    case $1 in
      -f=*) fmt=${{1#-f=}} ;;
      -W) ;;
      *) name=$1 ;;
    esac
    shift
  done
  case $fmt in
    *Provides*)
      printf '%s\n' 'installed www-browser, mail-reader' 'installed mail-transport-agent (= 1)' 'config-files gone-browser' 'not-installed not-browser'
      ;;
    *)
      case $name in
        w3m) printf 'installed' ;;
        *) printf '' ;;
      esac
      ;;
  esac
}}
apt-get() {{ echo "APTGET $*"; return 0; }}
{r}
printf 'MISSING:%s\n' "${{tp_missing[*]}}"
printf 'CHANGED:%s\n' "$changed"
"#
        );
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(&script)
            .output()
            .expect("bash");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{text}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            text.contains("MISSING:missing-name\n"),
            "w3m is installed and www-browser is provided; only missing-name should be missing:\n{text}"
        );
        assert!(text.contains("APTGET"), "{text}");
        assert!(
            !text.contains("APTGET") || !text.contains("www-browser"),
            "{text}"
        );
        assert!(text.contains("CHANGED:1"), "{text}");
    }
}
