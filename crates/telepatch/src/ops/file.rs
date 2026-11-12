//! `file(p)`: content, lines, directories, removal, mode and owner.
//!
//! `changed` is decided by comparison on the host, so every template is
//! idempotent. A file the op has to create gets mode 0644 (umask 022) and
//! the owner of the running shell; an existing file keeps its mode and
//! owner unless `.mode()`/`.owner()` say otherwise.

use std::ops::Deref;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::host::{Host, OpHandle, TraceError};
use crate::local::{self, LineText, LocalPlace};
use crate::ops::content::{self, Content, ScriptBody};
use crate::ops::fetch::{Fetched, FileSrc};
use crate::sh::quote;
use crate::trace::OpSpec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    File,
    Dir,
    Absent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineOp {
    /// Ensure this exact line is present (append if missing).
    Line(LineText),
    /// Replace the last line matching the ERE with the line, else append.
    LineRe { re: String, line: LineText },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSpec {
    pub path: String,
    pub kind: FileKind,
    pub content: Option<Content>,
    pub lines: Vec<LineOp>,
    pub mode: Option<u32>,
    /// `user`, `user:group` or `:group`.
    pub owner: Option<String>,
    /// Check the candidate with this command (`%s` = its path) before it
    /// replaces the target; content and line edits only.
    pub validate: Option<String>,
    /// A download to place or unpack (`.from`, `.unpack`).
    pub src: Option<FileSrc>,
    /// A controller file, directory, or `.via_controller()` download.
    /// Streamed by the executor; the script checks the sha256 and does
    /// not carry the bytes.
    pub local: Option<LocalPlace>,
}

impl FileSpec {
    pub fn new(path: impl Into<String>) -> FileSpec {
        FileSpec {
            path: path.into(),
            kind: FileKind::File,
            content: None,
            lines: Vec::new(),
            mode: None,
            owner: None,
            validate: None,
            src: None,
            local: None,
        }
    }

    /// The path as a shell word: quoted, except a leading `~`/`~user` which
    /// must expand on the host.
    pub(crate) fn path_word(&self) -> String {
        let p = &self.path;
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

    /// True when the body is a template that formatted a [`crate::Secret`].
    pub(crate) fn is_secret(&self) -> bool {
        self.content.as_ref().is_some_and(|c| c.is_secret())
    }

    pub fn render(&self) -> String {
        self.render_mode(false)
    }

    /// `reveal` puts secret values into the body. Without it a secret file
    /// carries `# (secret)` and not the rendered text.
    pub(crate) fn render_mode(&self, reveal: bool) -> String {
        let body = self.content.as_ref().map(|c| c.script_body(reveal));
        let mut o = format!("rc=0; changed=0\ntp_p={}\n", self.path_word());
        if matches!(body, Some(ScriptBody::Hidden)) {
            o.push_str("# (secret)\n");
        }
        match self.kind {
            FileKind::Absent => {
                o.push_str(
                    "if [ -e \"$tp_p\" ] || [ -L \"$tp_p\" ]; then\n  rm -rf -- \"$tp_p\"\n  rc=$?\n  if [ \"$rc\" -eq 0 ]; then changed=1; fi\nfi\n",
                );
                return o;
            }
            FileKind::Dir => o.push_str(
                "if [ -d \"$tp_p\" ]; then :\nelif [ -e \"$tp_p\" ] || [ -L \"$tp_p\" ]; then\n  echo \"telepatch: $tp_p exists and is not a directory\" >&2; rc=1\nelse\n  (umask 022; mkdir -p -- \"$tp_p\")\n  rc=$?\n  if [ \"$rc\" -eq 0 ]; then changed=1; fi\nfi\n",
            ),
            FileKind::File if self.local.is_some() => self.render_controller(&mut o),
            FileKind::File if self.src.is_some() => {
                if let Some(src) = &self.src {
                    crate::ops::fetch::render(self, src, &mut o);
                }
            }
            FileKind::File => match &self.validate {
                Some(v) if self.content.is_some() || !self.lines.is_empty() => {
                    self.render_validated(&mut o, v, body.as_ref())
                }
                _ => self.render_file(&mut o, body.as_ref()),
            },
        }
        if let Some(m) = self.mode {
            o.push_str(&format!(
                "if [ \"$rc\" -eq 0 ] && [ \"$(stat -L -c %a \"$tp_p\")\" != '{m:o}' ]; then\n  if chmod {m:o} \"$tp_p\"; then changed=1; else rc=$?; fi\nfi\n"
            ));
        }
        if let Some(ow) = &self.owner {
            let (u, g) = match ow.split_once(':') {
                Some((u, g)) => (u, Some(g)),
                None => (ow.as_str(), None),
            };
            let id = |s: &str| s.chars().all(|c| c.is_ascii_digit()) && !s.is_empty();
            let mut fmt = String::new();
            if !u.is_empty() {
                fmt.push_str(if id(u) { "%u" } else { "%U" });
            }
            if let Some(g) = g {
                fmt.push(':');
                if !g.is_empty() {
                    fmt.push_str(if id(g) { "%g" } else { "%G" });
                }
            }
            o.push_str(&format!(
                "if [ \"$rc\" -eq 0 ] && [ \"$(stat -L -c '{fmt}' \"$tp_p\")\" != {w} ]; then\n  if chown {w} \"$tp_p\"; then changed=1; else rc=$?; fi\nfi\n",
                w = quote(ow)
            ));
        }
        o
    }

    /// Content and line edits built in a temp file beside the target, given
    /// the final mode and owner, checked, then moved over the target. The
    /// target is never written at any other mode, and is untouched when the
    /// check fails.
    fn render_validated(&self, o: &mut String, validate: &str, body: Option<&ScriptBody>) {
        o.push_str(
            "if [ -d \"$tp_p\" ]; then echo \"telepatch: $tp_p is a directory\" >&2; rc=1; fi\n",
        );
        o.push_str(
            "if [ \"$rc\" -eq 0 ]; then\n  tp_c=$(mktemp \"$tp_p.tpXXXXXX\") || rc=$?\nfi\nif [ \"$rc\" -eq 0 ]; then\n",
        );
        match body {
            Some(ScriptBody::Bytes(c)) => o.push_str(&format!(
                "  tp_b={}\n  printf %s \"$tp_b\" | base64 -d > \"$tp_c\" || rc=$?\n",
                quote(&STANDARD.encode(c))
            )),
            Some(ScriptBody::Hidden) => {}
            Some(ScriptBody::Broken) => {
                o.push_str("  echo 'telepatch: secret was not resolved' >&2; rc=1\n")
            }
            None => {
                o.push_str("  if [ -f \"$tp_p\" ]; then cat \"$tp_p\" > \"$tp_c\" || rc=$?; fi\n")
            }
        }
        // The line edits are the plain ones, aimed at the candidate.
        let mut edits = String::new();
        self.render_lines(&mut edits);
        o.push_str(&edits.replace("$tp_p", "$tp_c"));
        o.push_str("  changed=0\n");
        o.push_str(
            "  if [ \"$rc\" -eq 0 ] && { [ ! -e \"$tp_p\" ] || [ \"$(sha256sum < \"$tp_c\" | cut -d' ' -f1)\" != \"$(sha256sum < \"$tp_p\" | cut -d' ' -f1)\" ]; }; then\n",
        );
        match self.mode {
            Some(m) => o.push_str(&format!("    chmod {m:o} \"$tp_c\" || rc=$?\n")),
            None => o.push_str(
                "    if [ -e \"$tp_p\" ]; then chmod --reference=\"$tp_p\" \"$tp_c\" || rc=$?; else chmod 644 \"$tp_c\" || rc=$?; fi\n",
            ),
        }
        match &self.owner {
            Some(ow) => o.push_str(&format!(
                "    if [ \"$rc\" -eq 0 ]; then chown {} \"$tp_c\" || rc=$?; fi\n",
                quote(ow)
            )),
            None => o.push_str(
                "    if [ \"$rc\" -eq 0 ] && [ -e \"$tp_p\" ]; then chown --reference=\"$tp_p\" \"$tp_c\" 2>/dev/null; fi\n",
            ),
        }
        o.push_str(&format!(
            "    if [ \"$rc\" -eq 0 ]; then\n      tp_vc={}\n      ( eval \"$tp_vc\" ) || rc=$?\n    fi\n",
            quote(&validate.replace("%s", "\"$tp_c\""))
        ));
        o.push_str(
            "    if [ \"$rc\" -eq 0 ]; then\n      mv -f \"$tp_c\" \"$tp_p\"; rc=$?\n      if [ \"$rc\" -eq 0 ]; then changed=1; fi\n    fi\n  fi\n  rm -f \"$tp_c\"\nfi\n",
        );
    }

    fn render_file(&self, o: &mut String, body: Option<&ScriptBody>) {
        o.push_str(
            "if [ -d \"$tp_p\" ]; then echo \"telepatch: $tp_p is a directory\" >&2; rc=1; fi\n",
        );
        if self.content.is_none() {
            o.push_str(
                "if [ \"$rc\" -eq 0 ] && [ ! -e \"$tp_p\" ]; then\n  (umask 022; : > \"$tp_p\")\n  rc=$?\n  if [ \"$rc\" -eq 0 ]; then changed=1; fi\nfi\n",
            );
        }
        match body {
            Some(ScriptBody::Bytes(c)) => o.push_str(&format!(
                "tp_b={}\nif [ \"$rc\" -eq 0 ] && ! {{ [ -f \"$tp_p\" ] && [ \"$(printf %s \"$tp_b\" | base64 -d | sha256sum | cut -d' ' -f1)\" = \"$(sha256sum < \"$tp_p\" | cut -d' ' -f1)\" ]; }}; then\n  tp_t=$(mktemp \"$tp_p.tpXXXXXX\") || rc=$?\n  if [ \"$rc\" -eq 0 ]; then\n    if printf %s \"$tp_b\" | base64 -d > \"$tp_t\"; then\n      if [ -e \"$tp_p\" ]; then\n        chmod --reference=\"$tp_p\" \"$tp_t\"; chown --reference=\"$tp_p\" \"$tp_t\" 2>/dev/null\n      else\n        chmod 644 \"$tp_t\"\n      fi\n      mv -f \"$tp_t\" \"$tp_p\"; rc=$?\n    else\n      rc=$?\n    fi\n    if [ \"$rc\" -eq 0 ]; then changed=1; else rm -f \"$tp_t\"; fi\n  fi\nfi\n",
                quote(&STANDARD.encode(c))
            )),
            Some(ScriptBody::Hidden) => {}
            Some(ScriptBody::Broken) => {
                o.push_str("echo 'telepatch: secret was not resolved' >&2\nrc=1\n")
            }
            None => {}
        }
        self.render_lines(o);
    }

    pub(crate) fn render_lines(&self, o: &mut String) {
        if !self.lines.is_empty() {
            o.push_str(
                "tp_append() {\n  if [ -s \"$1\" ] && [ -n \"$(tail -c1 \"$1\")\" ]; then printf '\\n' >> \"$1\"; fi\n  printf '%s\\n' \"$2\" >> \"$1\"\n}\n",
            );
        }
        for l in &self.lines {
            let (re, line) = match l {
                LineOp::Line(l) => (None, l),
                LineOp::LineRe { re, line } => (Some(re.as_str()), line),
            };
            let Some(line) = line.literal() else {
                o.push_str("echo 'telepatch: local value was not prepared' >&2; rc=1\n");
                continue;
            };
            if line.contains('\n') || re.is_some_and(|r| r.contains('\n')) {
                o.push_str("echo 'telepatch: file line must be a single line' >&2; rc=2\n");
                continue;
            }
            o.push_str(&format!(
                "if [ \"$rc\" -eq 0 ]; then\n  if [ ! -e \"$tp_p\" ]; then (umask 022; : > \"$tp_p\") && changed=1 || rc=$?; fi\n  tp_l={}\n",
                quote(line)
            ));
            match re {
                None => o.push_str(
                    "  if [ \"$rc\" -eq 0 ] && ! grep -qxF -- \"$tp_l\" \"$tp_p\"; then\n    if tp_append \"$tp_p\" \"$tp_l\"; then changed=1; else rc=$?; fi\n  fi\nfi\n",
                ),
                Some(re) => o.push_str(&format!(
                    "  tp_n=$(grep -nE -- {} \"$tp_p\" | tail -n 1 | cut -d: -f1)\n  if [ \"$rc\" -ne 0 ]; then :\n  elif [ -z \"$tp_n\" ]; then\n    if tp_append \"$tp_p\" \"$tp_l\"; then changed=1; else rc=$?; fi\n  elif [ \"$(sed -n \"${{tp_n}}p\" \"$tp_p\")\" != \"$tp_l\" ]; then\n    tp_t=$(mktemp) && TP_L=\"$tp_l\" awk -v n=\"$tp_n\" 'NR==n {{ print ENVIRON[\"TP_L\"]; next }} {{ print }}' \"$tp_p\" > \"$tp_t\" && cat \"$tp_t\" > \"$tp_p\"\n    rc=$?\n    rm -f \"$tp_t\"\n    if [ \"$rc\" -eq 0 ]; then changed=1; fi\n  fi\nfi\n",
                    quote(re)
                )),
            }
        }
    }

    /// Place bytes the copy channel staged. The script never contains them.
    fn render_controller(&self, o: &mut String) {
        let Some(place) = &self.local else {
            return;
        };
        if !place.prepared {
            o.push_str("echo 'telepatch: controller file was not prepared' >&2; rc=1\n");
            return;
        }
        o.push_str(&local::stage_vars(&self.path_word()));
        if place.is_dir() {
            o.push_str(
                "if [ -e \"$tp_p\" ] && [ ! -d \"$tp_p\" ]; then echo \"telepatch: $tp_p exists and is not a directory\" >&2; rc=1; fi\n",
            );
            o.push_str(
                "if [ \"$rc\" -eq 0 ] && [ ! -d \"$tp_p\" ]; then mkdir -p -- \"$tp_p\" && changed=1 || rc=$?; fi\n",
            );
        } else {
            o.push_str(
                "if [ -d \"$tp_p\" ]; then echo \"telepatch: $tp_p is a directory\" >&2; rc=1; fi\n",
            );
        }
        for f in &place.files {
            if f.is_dir {
                let dest = local::host_path(&f.rel);
                o.push_str(&format!(
                    "if [ \"$rc\" -eq 0 ] && [ ! -d {dest} ]; then mkdir -p -- {dest} && changed=1 || rc=$?; fi\n"
                ));
                continue;
            }
            let dest = local::host_path(&f.rel);
            let stage = if f.rel.is_empty() && !place.is_dir() {
                "\"$tp_base\"".to_string()
            } else {
                format!("\"$tp_base\"/{}", quote(&f.rel))
            };
            let sum = quote(&f.sha256);
            o.push_str(&format!(
                "if [ \"$rc\" -eq 0 ]; then\n  tp_dest={dest}\n  tp_stage={stage}\n  tp_sum={sum}\n  if [ -f \"$tp_stage\" ]; then\n    tp_got=$(sha256sum \"$tp_stage\" | cut -d' ' -f1)\n    if [ \"$tp_got\" != \"$tp_sum\" ]; then echo \"telepatch: streamed $tp_dest sha256 is $tp_got\" >&2; rc=1\n    else\n      mkdir -p -- \"$(dirname -- \"$tp_dest\")\" || rc=$?\n      if [ \"$rc\" -eq 0 ]; then mv -f -- \"$tp_stage\" \"$tp_dest\" && changed=1 || rc=$?; fi\n    fi\n  elif [ -f \"$tp_dest\" ]; then\n    tp_got=$(sha256sum \"$tp_dest\" | cut -d' ' -f1)\n    if [ \"$tp_got\" != \"$tp_sum\" ]; then echo \"telepatch: $tp_dest was not streamed\" >&2; rc=1; fi\n  else\n    echo \"telepatch: $tp_dest was not streamed\" >&2; rc=1\n  fi\nfi\n"
            ));
        }
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use crate::local::{LocalPlace, Staged};

    #[test]
    fn controller_tree_script_names_each_relative_path() {
        let mut spec = FileSpec::new("/host/certs");
        spec.local = Some(LocalPlace {
            kind: crate::local::LocalKind::Dir,
            written: "certs".into(),
            prepared: true,
            files: vec![
                Staged {
                    rel: "a.txt".into(),
                    abs: "/src/a.txt".into(),
                    sha256: "ab".repeat(32),
                    len: 6,
                    is_dir: false,
                },
                Staged {
                    rel: "sub/b.txt".into(),
                    abs: "/src/sub/b.txt".into(),
                    sha256: "cd".repeat(32),
                    len: 7,
                    is_dir: false,
                },
            ],
        });
        let script = spec.render();
        assert!(
            !script.contains("base64"),
            "the op script must not carry bytes:\n{script}"
        );
        assert!(script.contains("sub/b.txt"), "{script}");
        assert!(script.contains("a.txt"), "{script}");
        assert!(script.len() < 4000, "op script grew to {}", script.len());
    }
}

/// Read controller files and commands this spec names. A miss is the
/// error string; nothing on the host has changed.
pub(crate) fn fill_local(spec: &mut FileSpec) -> Result<(), String> {
    for line in &mut spec.lines {
        match line {
            LineOp::Line(t) => {
                let s = local::resolve_line(t)?;
                *t = LineText::Literal(s);
            }
            LineOp::LineRe { line, .. } => {
                let s = local::resolve_line(line)?;
                *line = LineText::Literal(s);
            }
        }
    }
    if let Some(place) = &mut spec.local {
        local::prepare_place(place)?;
    }
    Ok(())
}

/// Handle for a `file` op; builder methods edit the node in place.
#[derive(Clone)]
pub struct FileHandle(OpHandle);

impl Deref for FileHandle {
    type Target = OpHandle;
    fn deref(&self) -> &OpHandle {
        &self.0
    }
}

crate::host::op_builders!(FileHandle);

impl FileHandle {
    fn spec(&self, f: impl FnOnce(&mut FileSpec)) -> Self {
        self.0.edit(|op| {
            if let OpSpec::File(s) = &mut op.spec {
                f(s);
            }
        });
        self.0.refresh_effects();
        self.clone()
    }

    /// The path, for trace errors.
    fn path(&self) -> String {
        self.0
            .with_node(|op| match &op.spec {
                OpSpec::File(s) => s.path.clone(),
                _ => String::new(),
            })
            .unwrap_or_default()
    }

    /// The builder already on this op that cannot go with `later`, as an
    /// author writes it. A fetched source (`.from`, `.unpack`) is exclusive
    /// with content, lines, `.dir()`, `.absent()` and `.validate()`, and
    /// the two sources with each other.
    fn clash(&self, later: &str) -> Option<&'static str> {
        let source = later == ".from()" || later == ".unpack()";
        self.0
            .with_node(|op| {
                let OpSpec::File(s) = &op.spec else {
                    return None;
                };
                if let Some(src) = &s.src {
                    return Some(src.builder());
                }
                if s.local.is_some() {
                    return Some(".from()");
                }
                if !source {
                    return None;
                }
                if s.content.is_some() {
                    Some(".content()")
                } else if let Some(l) = s.lines.first() {
                    Some(match l {
                        LineOp::Line(_) => ".line()",
                        LineOp::LineRe { .. } => ".line_re()",
                    })
                } else if s.kind == FileKind::Dir {
                    Some(".dir()")
                } else if s.kind == FileKind::Absent {
                    Some(".absent()")
                } else if s.validate.is_some() {
                    Some(".validate()")
                } else {
                    None
                }
            })
            .flatten()
    }

    /// Record the clash of `later` with an earlier builder; true when there
    /// is one (the later call then changes nothing).
    #[track_caller]
    fn exclusive(&self, later: &str) -> bool {
        match self.clash(later) {
            Some(earlier) => {
                self.0.record_error(TraceError::new(format!(
                    "file {}: {earlier} and {later} are exclusive",
                    self.path()
                )));
                true
            }
            None => false,
        }
    }

    /// Take a download for `.from` / `.unpack`.
    #[track_caller]
    fn take(&self, later: &str, f: &Fetched, unpack: Option<u32>) -> Self {
        if self.exclusive(later) {
            return self.clone();
        }
        if let Some(p) = f.problem() {
            self.0
                .record_error(TraceError::new(format!("file {}: {p}", self.path())));
            return self.clone();
        }
        let src = FileSrc {
            fetched: f.clone(),
            unpack,
        };
        self.spec(|s| s.src = Some(src))
    }

    /// Place the file a prepare-phase [`Fetched`] downloaded (`h.fetch(url)
    /// .sha256(sum)`), at this path. Mode and owner as for content: an
    /// existing file keeps them unless `.mode()` / `.owner()` say otherwise.
    /// Takes `&Fetched` or `Fetched`. Exclusive with content, lines,
    /// `.dir()`, `.absent()` and `.validate()`. Without a checksum on the
    /// fetch the op is the policy error `Unverified`.
    #[track_caller]
    pub fn from(&self, src: impl Into<crate::local::FromArg>) -> Self {
        match src.into() {
            crate::local::FromArg::Fetch(f) if f.via_controller => {
                if let Some(p) = f.problem() {
                    self.0
                        .record_error(TraceError::new(format!("file {}: {p}", self.path())));
                    return self.clone();
                }
                self.place_local(LocalPlace::fetch(f.url, f.sha256))
            }
            crate::local::FromArg::Fetch(f) => self.take(".from()", &f, None),
            crate::local::FromArg::File(f) => self.place_local(LocalPlace::file(f.path)),
            crate::local::FromArg::Dir(d) => self.place_local(LocalPlace::dir(d.path)),
        }
    }

    fn place_local(&self, place: LocalPlace) -> Self {
        if self.exclusive(".from()") {
            return self.clone();
        }
        self.spec(|s| s.local = Some(place))
    }

    /// Unpack a downloaded archive (tar gz, xz, zst, or zip) into this
    /// path, replacing the directory; `.strip(n)` drops leading path
    /// components. `.mode()` and `.owner()` apply to the directory (the
    /// owner recursively). A second run with the same checksum and strip
    /// changes nothing. Takes `&Fetched` or `Fetched`.
    #[track_caller]
    pub fn unpack(&self, fetched: impl std::borrow::Borrow<Fetched>) -> Self {
        let f = crate::ops::fetch::fetched_of(fetched);
        if f.via_controller {
            self.0.record_error(TraceError::new(format!(
                "file {}: .via_controller() is placed with .from(), not .unpack()",
                self.path()
            )));
            return self.clone();
        }
        self.take(".unpack()", &f, Some(0))
    }

    /// Drop `n` leading path components of an `.unpack(..)` (tar's
    /// `--strip-components`; for a zip, the files below the `n`th directory
    /// level). A trace error without `.unpack(..)`.
    #[track_caller]
    pub fn strip(&self, n: u32) -> Self {
        let unpacking = self
            .0
            .with_node(|op| matches!(&op.spec, OpSpec::File(s) if s.src.as_ref().is_some_and(|x| x.unpack.is_some())))
            .unwrap_or(false);
        if !unpacking {
            self.0.record_error(TraceError::new(format!(
                "file {}: .strip({n}) needs .unpack(..)",
                self.path()
            )));
            return self.clone();
        }
        self.spec(|s| {
            if let Some(src) = &mut s.src {
                src.unpack = Some(n);
            }
        })
    }

    /// Ensure this exact line is present, appending if missing.
    #[track_caller]
    pub fn line(&self, line: impl Into<LineText>) -> Self {
        if self.exclusive(".line()") {
            return self.clone();
        }
        let line = line.into();
        self.spec(|s| s.lines.push(LineOp::Line(line)))
    }
    /// Replace the last line matching `re` (an ERE, `grep -E`) with `line`,
    /// or append it when none matches.
    #[track_caller]
    pub fn line_re(&self, re: &str, line: &str) -> Self {
        if self.exclusive(".line_re()") {
            return self.clone();
        }
        self.spec(|s| {
            s.lines.push(LineOp::LineRe {
                re: re.to_string(),
                line: LineText::Literal(line.to_string()),
            })
        })
    }
    /// Ensure a directory.
    #[track_caller]
    pub fn dir(&self) -> Self {
        if self.exclusive(".dir()") {
            return self.clone();
        }
        self.spec(|s| s.kind = FileKind::Dir)
    }
    /// Ensure the path does not exist (removes directories recursively).
    #[track_caller]
    pub fn absent(&self) -> Self {
        if self.exclusive(".absent()") {
            return self.clone();
        }
        self.spec(|s| s.kind = FileKind::Absent)
    }
    /// Ensure exactly these bytes.
    #[track_caller]
    pub fn content<K>(&self, body: impl content::FileContent<K>) -> Self {
        if self.exclusive(".content()") {
            return self.clone();
        }
        let b = body.into_content();
        self.spec(|s| s.content = Some(b))
    }
    /// Check the candidate before it replaces the target. `cmd` is shell
    /// with `%s` standing for the candidate's path (`visudo -cf %s`); it
    /// applies to `.content(..)` and line edits. Without `%s` it is a trace
    /// error. A non-zero exit fails the op with that rc and leaves the
    /// target untouched.
    #[track_caller]
    pub fn validate(&self, cmd: &str) -> Self {
        if self.exclusive(".validate()") {
            return self.clone();
        }
        if !cmd.contains("%s") {
            self.0.record_error(TraceError::new(format!(
                "file validate command has no %s: {cmd}"
            )));
            return self.clone();
        }
        let c = cmd.to_string();
        self.spec(|s| s.validate = Some(c))
    }
    #[track_caller]
    pub fn mode(&self, mode: u32) -> Self {
        self.spec(|s| s.mode = Some(mode))
    }
    /// `"user"`, `"user:group"` or `":group"`.
    #[track_caller]
    pub fn owner(&self, owner: &str) -> Self {
        self.spec(|s| s.owner = Some(owner.to_string()))
    }
}

impl<V> Host<V> {
    /// Manage the file or directory at `path`. `~`/`~user` prefixes expand
    /// on the host, in the op's shell.
    #[track_caller]
    pub fn file(&self, path: &str) -> FileHandle {
        FileHandle(self.op(OpSpec::File(FileSpec::new(path))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::os::Os;
    use std::rc::Rc;

    fn host() -> Host<()> {
        Host::new(
            "t",
            Os::parse("ID=debian\nVERSION_ID=12\n").unwrap(),
            Rc::new(()),
        )
    }

    #[test]
    fn validate_without_percent_s_is_a_trace_error() {
        let h = host();
        h.file("/etc/x").content("a").validate("visudo -c");
        let errs: Vec<String> = h
            .snapshot()
            .errors
            .iter()
            .map(|e| e.message.clone())
            .collect();
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("%s"), "{errs:?}");
        h.file("/etc/y").content("a").validate("visudo -cf %s");
        assert_eq!(h.snapshot().errors.len(), 1);
    }
}
