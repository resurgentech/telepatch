//! `unit(name)`: a systemd unit file, or a drop-in for one, owned by the
//! run. `.content(..)` (with `.mode(..)`) writes
//! `/etc/systemd/system/<unit>`; `.drop_in(name, text)` writes
//! `/etc/systemd/system/<unit>.d/<name>.conf`; `.absent()` removes the file
//! (the unit's, or the drop-in's). The unit is named as `svc` names it:
//! `.service` is appended when the name has no unit suffix. Root only.
//!
//! The file is written by the `file` op's candidate-and-validate path
//! ([`FileSpec::validate`]): the candidate is built beside the target,
//! given its final mode, checked, and moved over the target only when the
//! check passes; an unchanged file is not checked. The check is
//! `systemd-analyze verify --man=no` (and `--recursive-errors=no` where
//! the host's `systemd-analyze` has it, so only this unit's own errors
//! count, not those of the units it names): `verify` loads a unit by its
//! file name, so the candidate (`<unit>.tpXXXXXX`) is copied under the
//! unit's own name into a fresh `mktemp -d` and verified there. A drop-in
//! is verified with its unit: the unit's file (the first found in the unit
//! dir, `/run/systemd/system`, `/usr/local/lib/systemd/system`,
//! `/usr/lib/systemd/system`, `/lib/systemd/system`; the template for an
//! instance) is copied into the temp dir with the candidate as
//! `<unit>.d/<name>.conf` beside it (`verify` reads drop-ins next to the
//! file it is given). Any non-zero exit of `verify` fails the op. With no
//! `systemd-analyze` on the host, or a drop-in whose unit is not found, the
//! file is written unverified with a line on stderr saying so; prepare
//! says it too.
//!
//! Keys: writes `file:<path>` and `svc:<unit>`, so `svc` ops on the unit
//! order after it; posts `file:<path>` → `Exists(true)` (`false` for
//! `.absent()`). It is not `h.file`, so `UnitViaFile` does not apply.
//!
//! A changed unit sets the root shell's `__tp_dr` (the window flag); the
//! next `svc` op in the window, on a booted host, runs one `systemctl
//! daemon-reload` first and clears it; with no svc op after it, the
//! window ends with that reload (the flush). The flag never outlives the
//! window ([`crate::executor`], "Units and daemon-reload").
//!
//! `TP_UNIT_DIR` overrides `/etc/systemd/system` in the script (tests); the
//! keys and the plan always say `/etc/systemd/system`.

use std::ops::Deref;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::TraceError;
use crate::host::{Host, OpHandle};
use crate::key::Key;
use crate::ops::file::{FileKind, FileSpec};
use crate::sh::quote;
use crate::trace::{Node, NodeId, OpSpec, Principal, Trace};

/// Where units the run owns are written.
pub const UNIT_DIR: &str = "/etc/systemd/system";

/// What `.content(..)` takes: bytes now (`&str`, `String`, `&[u8]`,
/// `Vec<u8>`, `&[u8; N]`). A template value converts into it later (T89),
/// so `impl Into<Content>` callers keep compiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Content {
    bytes: Vec<u8>,
}

impl Content {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl From<Vec<u8>> for Content {
    fn from(bytes: Vec<u8>) -> Content {
        Content { bytes }
    }
}
impl From<&[u8]> for Content {
    fn from(b: &[u8]) -> Content {
        b.to_vec().into()
    }
}
impl<const N: usize> From<&[u8; N]> for Content {
    fn from(b: &[u8; N]) -> Content {
        b.to_vec().into()
    }
}
impl From<&Vec<u8>> for Content {
    fn from(b: &Vec<u8>) -> Content {
        b.clone().into()
    }
}
impl From<&str> for Content {
    fn from(s: &str) -> Content {
        s.as_bytes().into()
    }
}
impl From<String> for Content {
    fn from(s: String) -> Content {
        s.into_bytes().into()
    }
}
impl From<&String> for Content {
    fn from(s: &String) -> Content {
        s.as_bytes().into()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitSpec {
    /// The unit as the role wrote it.
    pub unit: String,
    /// `.drop_in(name, ..)`: the drop-in's file name, `.conf` included.
    pub drop_in: Option<String>,
    pub content: Option<Content>,
    pub mode: Option<u32>,
    pub absent: bool,
}

impl UnitSpec {
    pub fn new(unit: impl Into<String>) -> UnitSpec {
        UnitSpec {
            unit: unit.into(),
            drop_in: None,
            content: None,
            mode: None,
            absent: false,
        }
    }

    /// The unit's file name: the name with `.service` appended when it has
    /// no unit suffix (as `svc` keys it).
    pub fn file_name(&self) -> String {
        crate::ops::svc::key_name(&self.unit)
    }

    /// The path below the unit dir: `minikube.service`,
    /// `foo.service.d/override.conf`.
    pub fn rel(&self) -> String {
        match &self.drop_in {
            Some(d) => format!("{}.d/{d}", self.file_name()),
            None => self.file_name(),
        }
    }

    /// `/etc/systemd/system/<rel>`.
    pub fn path(&self) -> String {
        format!("{UNIT_DIR}/{}", self.rel())
    }

    pub fn default_writes(&self) -> std::collections::BTreeSet<Key> {
        [Key::file(self.path()), Key::svc(self.file_name())].into()
    }

    pub fn default_post(&self) -> std::collections::BTreeMap<Key, crate::key::Value> {
        [(
            Key::file(self.path()),
            crate::key::Value::Exists(!self.absent),
        )]
        .into()
    }

    /// The `file` spec the op is written with (path as the plan shows it).
    fn file_spec(&self) -> FileSpec {
        let mut f = FileSpec::new(self.path());
        if self.absent {
            f.kind = FileKind::Absent;
            return f;
        }
        f.content = Some(
            self.content
                .as_ref()
                .map(|c| c.bytes.clone())
                .unwrap_or_default(),
        );
        f.mode = self.mode;
        f.validate = Some(self.verify());
        f
    }

    /// The check of the candidate (`%s`), run by the file op in a
    /// subshell: see the module docs.
    fn verify(&self) -> String {
        let file = quote(&self.file_name());
        let rel = self.rel();
        let mut s = format!(
            "if ! command -v systemd-analyze >/dev/null 2>&1; then\n  echo {} >&2\n  exit 0\nfi\n",
            quote(&format!(
                "telepatch: no systemd-analyze: {rel} not verified"
            ))
        );
        s.push_str(concat!(
            "tp_vf=--man=no\n",
            "if systemd-analyze --help 2>/dev/null | grep -q -- --recursive-errors; then tp_vf=\"$tp_vf --recursive-errors=no\"; fi\n",
        ));
        match &self.drop_in {
            None => {
                s.push_str("tp_vd=$(mktemp -d) || exit 1\n");
                s.push_str(&format!(
                    "cp -- %s \"$tp_vd\"/{file} || {{ rm -rf -- \"$tp_vd\"; exit 1; }}\n"
                ));
            }
            Some(conf) => {
                let fname = self.file_name();
                let mut names = vec![quote(&fname)];
                // an instance's unit is its template
                if let Some((pre, _)) = fname.split_once('@')
                    && let Some(i) = fname.rfind('.')
                {
                    let t = format!("{pre}@{}", &fname[i..]);
                    if t != fname {
                        names.push(quote(&t));
                    }
                }
                s.push_str(&format!(
                    "tp_vb=\nfor tp_vx in \"$tp_ud\" /run/systemd/system /usr/local/lib/systemd/system /usr/lib/systemd/system /lib/systemd/system; do\n  for tp_vn in {}; do\n    if [ -f \"$tp_vx/$tp_vn\" ]; then tp_vb=\"$tp_vx/$tp_vn\"; break 2; fi\n  done\ndone\n",
                    names.join(" ")
                ));
                s.push_str(&format!(
                    "if [ -z \"$tp_vb\" ]; then\n  echo {} >&2\n  exit 0\nfi\n",
                    quote(&format!(
                        "telepatch: unit {fname} not found: drop-in {rel} not verified"
                    ))
                ));
                s.push_str("tp_vd=$(mktemp -d) || exit 1\n");
                s.push_str(&format!(
                    "{{ mkdir -- \"$tp_vd\"/{d} && cp -- \"$tp_vb\" \"$tp_vd\"/{file} && cp -- %s \"$tp_vd\"/{d}/{c}; }} || {{ rm -rf -- \"$tp_vd\"; exit 1; }}\n",
                    d = quote(&format!("{fname}.d")),
                    c = quote(conf),
                ));
            }
        }
        s.push_str(&format!(
            "systemd-analyze verify $tp_vf \"$tp_vd\"/{file}\ntp_vr=$?\nrm -rf -- \"$tp_vd\"\nexit \"$tp_vr\"\n"
        ));
        s
    }

    /// The op's script: the unit dir (`TP_UNIT_DIR` or
    /// `/etc/systemd/system`), the target's directory made, then the file
    /// op's body aimed at the target there.
    pub fn render(&self) -> String {
        let mut o = String::from("tp_ud=\"${TP_UNIT_DIR:-/etc/systemd/system}\"\n");
        if !self.absent {
            let dir = match &self.drop_in {
                Some(_) => format!("\"$tp_ud\"/{}", quote(&format!("{}.d", self.file_name()))),
                None => "\"$tp_ud\"".to_string(),
            };
            o.push_str(&format!("[ -d {dir} ] || (umask 022; mkdir -p -- {dir})\n"));
        }
        let body = self.file_spec().render();
        let from = format!("tp_p={}\n", quote(&self.path()));
        let to = format!("tp_p=\"$tp_ud\"/{}\n", quote(&self.rel()));
        debug_assert!(body.contains(&from), "file body: {body}");
        o.push_str(&body.replacen(&from, &to, 1));
        o
    }
}

/// Is `s` usable as one path segment (a unit or drop-in file name)?
fn segment_ok(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && !s.contains(['/', '\0', '\n'])
}

/// Handle for a `unit` op; builder methods edit the node in place.
#[derive(Clone)]
pub struct UnitHandle(OpHandle);

impl Deref for UnitHandle {
    type Target = OpHandle;
    fn deref(&self) -> &OpHandle {
        &self.0
    }
}

crate::host::op_builders!(UnitHandle);

impl UnitHandle {
    fn spec(&self, f: impl FnOnce(&mut UnitSpec)) -> Self {
        self.0.edit(|op| {
            if let OpSpec::Unit(s) = &mut op.spec {
                f(s);
                op.writes = s.default_writes();
                op.post = s.default_post();
            }
        });
        self.clone()
    }

    /// Ensure the unit file holds exactly this: bytes now, a template
    /// value after T89.
    #[track_caller]
    pub fn content(&self, content: impl Into<Content>) -> Self {
        let c = content.into();
        self.spec(|s| s.content = Some(c))
    }

    /// Ensure a drop-in `<unit>.d/<name>.conf` (`.conf` is added unless
    /// `name` has it) holding exactly `text`. The op is the drop-in, not
    /// the unit file.
    #[track_caller]
    pub fn drop_in(&self, name: &str, text: impl Into<Content>) -> Self {
        if !segment_ok(name) {
            self.0.record_error(TraceError::new(format!(
                "unit drop_in({name:?}): not a drop-in name"
            )));
            return self.clone();
        }
        let conf = if name.ends_with(".conf") {
            name.to_string()
        } else {
            format!("{name}.conf")
        };
        let c = text.into();
        self.spec(|s| {
            s.drop_in = Some(conf);
            s.content = Some(c);
        })
    }

    /// Ensure the file (the unit's, or the drop-in's) does not exist.
    #[track_caller]
    pub fn absent(&self) -> Self {
        self.spec(|s| s.absent = true)
    }

    /// The file's mode (a created file is 0644 otherwise).
    #[track_caller]
    pub fn mode(&self, mode: u32) -> Self {
        self.spec(|s| s.mode = Some(mode))
    }
}

impl<V> Host<V> {
    /// A systemd unit file the run owns: chain `.content(..)` (and
    /// `.mode(..)`), `.drop_in(name, text)` or `.absent()`. Root only.
    #[track_caller]
    pub fn unit(&self, unit: &str) -> UnitHandle {
        let h = self.op(OpSpec::Unit(UnitSpec::new(unit)));
        if !segment_ok(unit) {
            h.record_error(TraceError::new(format!("unit({unit:?}): not a unit name")));
        }
        if let Principal::User(u) = &self.run_as().principal {
            h.record_error(TraceError::new(format!(
                "unit({unit}) is root only: as_user({u}) cannot write units"
            )));
        }
        UnitHandle(h)
    }
}

/// Trace errors found once an op is complete: a `unit` that wants nothing,
/// or `.content` / `.drop_in` with `.absent()`.
pub fn errors(trace: &Trace) -> Vec<TraceError> {
    let mut out = Vec::new();
    for (i, n) in trace.nodes.iter().enumerate() {
        let Node::Op(op) = n else { continue };
        let OpSpec::Unit(s) = &op.spec else { continue };
        let msg = match (s.content.is_some(), s.absent) {
            (false, false) => format!("unit({}): nothing wanted", s.unit),
            (true, true) => format!(
                "unit({}): .{}() and .absent() are exclusive",
                s.unit,
                if s.drop_in.is_some() {
                    "drop_in"
                } else {
                    "content"
                }
            ),
            _ => continue,
        };
        let mut e = TraceError::new(msg);
        e.site = None;
        out.push(trace.locate(NodeId(i), e));
    }
    out
}

/// A prepare probe for the window's `unit` ops (in that order), one round
/// trip: `analyze yes|no`, then per op `<i> absent|same|differs|present`.
pub fn probe(specs: &[&UnitSpec]) -> String {
    let mut s = String::from(
        "if command -v systemd-analyze >/dev/null 2>&1; then echo \"analyze yes\"; else echo \"analyze no\"; fi\ntp_ud=\"${TP_UNIT_DIR:-/etc/systemd/system}\"\n",
    );
    for (i, u) in specs.iter().enumerate() {
        s.push_str(&format!("tp_p=\"$tp_ud\"/{}\n", quote(&u.rel())));
        s.push_str("if [ ! -e \"$tp_p\" ] && [ ! -L \"$tp_p\" ]; then echo \"");
        s.push_str(&format!("{i} absent\"\n"));
        if u.absent {
            s.push_str(&format!("else echo \"{i} present\"\nfi\n"));
            continue;
        }
        let bytes = u
            .content
            .as_ref()
            .map(|c| c.bytes.clone())
            .unwrap_or_default();
        let mut same = format!(
            "printf %s {} | base64 -d | cmp -s - \"$tp_p\"",
            quote(&STANDARD.encode(bytes))
        );
        if let Some(m) = u.mode {
            same.push_str(&format!(" && [ \"$(stat -L -c %a \"$tp_p\")\" = '{m:o}' ]"));
        }
        s.push_str(&format!(
            "elif {same}; then echo \"{i} same\"\nelse echo \"{i} differs\"\nfi\n"
        ));
    }
    s
}

/// `(rel, detail)` per op with something to say, from [`probe`]'s output:
/// `will create`, `will change`, `will remove`; a write with no
/// `systemd-analyze` adds `; no systemd-analyze: not verified` (or, when an
/// op that can install software comes earlier in the window, `; no
/// systemd-analyze yet: decided at run time, after this window's earlier
/// ops`). A probe that answered nothing reports nothing.
pub fn findings(specs: &[&UnitSpec], output: &str, earlier: bool) -> Vec<(String, String)> {
    let mut analyze = None;
    let mut state: Vec<Option<String>> = vec![None; specs.len()];
    for l in output.lines() {
        if let Some(v) = l.strip_prefix("analyze ") {
            analyze = Some(v == "yes");
        } else if let Some((i, st)) = l.split_once(' ')
            && let Ok(i) = i.parse::<usize>()
            && i < specs.len()
        {
            state[i] = Some(st.to_string());
        }
    }
    let Some(analyze) = analyze else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (u, st) in specs.iter().zip(state) {
        let detail = match st.as_deref() {
            Some("absent") if !u.absent => "will create",
            Some("differs") => "will change",
            Some("present") => "will remove",
            _ => continue,
        };
        let mut detail = detail.to_string();
        if !u.absent && !analyze {
            detail.push_str(if earlier {
                "; no systemd-analyze yet: decided at run time, after this window's earlier ops"
            } else {
                "; no systemd-analyze: not verified"
            });
        }
        out.push((u.rel(), detail));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn findings_parse_probe_output() {
        let a = UnitSpec {
            content: Some("x".into()),
            ..UnitSpec::new("a")
        };
        let b = UnitSpec {
            absent: true,
            ..UnitSpec::new("b.service")
        };
        let c = UnitSpec {
            drop_in: Some("o.conf".into()),
            content: Some("x".into()),
            ..UnitSpec::new("c.service")
        };
        let specs = [&a, &b, &c];
        assert_eq!(
            findings(&specs, "analyze yes\n0 absent\n1 present\n2 same\n", false),
            [
                ("a.service".to_string(), "will create".to_string()),
                ("b.service".to_string(), "will remove".to_string()),
            ]
        );
        assert_eq!(
            findings(&specs, "analyze no\n0 differs\n1 absent\n2 absent\n", true),
            [
                (
                    "a.service".to_string(),
                    "will change; no systemd-analyze yet: decided at run time, after this window's earlier ops".to_string()
                ),
                (
                    "c.service.d/o.conf".to_string(),
                    "will create; no systemd-analyze yet: decided at run time, after this window's earlier ops".to_string()
                ),
            ]
        );
        assert!(findings(&specs, "", false).is_empty());
    }

    #[test]
    fn an_instance_drop_in_is_verified_against_its_template() {
        let s = UnitSpec {
            drop_in: Some("o.conf".into()),
            content: Some("x".into()),
            ..UnitSpec::new("vnc@:1.service")
        };
        let v = s.verify();
        assert!(
            v.contains("for tp_vn in 'vnc@:1.service' 'vnc@.service'"),
            "{v}"
        );
    }
}
