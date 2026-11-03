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
    Line(String),
    /// Replace the last line matching the ERE with the line, else append.
    LineRe { re: String, line: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSpec {
    pub path: String,
    pub kind: FileKind,
    pub content: Option<Vec<u8>>,
    pub lines: Vec<LineOp>,
    pub mode: Option<u32>,
    /// `user`, `user:group` or `:group`.
    pub owner: Option<String>,
    /// Check the candidate with this command (`%s` = its path) before it
    /// replaces the target; content and line edits only.
    pub validate: Option<String>,
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
        }
    }

    /// The path as a shell word: quoted, except a leading `~`/`~user` which
    /// must expand on the host.
    fn path_word(&self) -> String {
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

    pub fn render(&self) -> String {
        let mut o = format!("rc=0; changed=0\ntp_p={}\n", self.path_word());
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
            FileKind::File => match &self.validate {
                Some(v) if self.content.is_some() || !self.lines.is_empty() => {
                    self.render_validated(&mut o, v)
                }
                _ => self.render_file(&mut o),
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
    fn render_validated(&self, o: &mut String, validate: &str) {
        o.push_str(
            "if [ -d \"$tp_p\" ]; then echo \"telepatch: $tp_p is a directory\" >&2; rc=1; fi\n",
        );
        o.push_str(
            "if [ \"$rc\" -eq 0 ]; then\n  tp_c=$(mktemp \"$tp_p.tpXXXXXX\") || rc=$?\nfi\nif [ \"$rc\" -eq 0 ]; then\n",
        );
        match &self.content {
            Some(c) => o.push_str(&format!(
                "  tp_b={}\n  printf %s \"$tp_b\" | base64 -d > \"$tp_c\" || rc=$?\n",
                quote(&STANDARD.encode(c))
            )),
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
            "  if [ \"$rc\" -eq 0 ] && { [ ! -e \"$tp_p\" ] || ! cmp -s \"$tp_c\" \"$tp_p\"; }; then\n",
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

    fn render_file(&self, o: &mut String) {
        o.push_str(
            "if [ -d \"$tp_p\" ]; then echo \"telepatch: $tp_p is a directory\" >&2; rc=1; fi\n",
        );
        if self.content.is_none() {
            o.push_str(
                "if [ \"$rc\" -eq 0 ] && [ ! -e \"$tp_p\" ]; then\n  (umask 022; : > \"$tp_p\")\n  rc=$?\n  if [ \"$rc\" -eq 0 ]; then changed=1; fi\nfi\n",
            );
        }
        if let Some(c) = &self.content {
            o.push_str(&format!(
                "tp_b={}\nif [ \"$rc\" -eq 0 ] && ! {{ [ -f \"$tp_p\" ] && printf %s \"$tp_b\" | base64 -d | cmp -s - \"$tp_p\"; }}; then\n  tp_t=$(mktemp \"$tp_p.tpXXXXXX\") || rc=$?\n  if [ \"$rc\" -eq 0 ]; then\n    if printf %s \"$tp_b\" | base64 -d > \"$tp_t\"; then\n      if [ -e \"$tp_p\" ]; then\n        chmod --reference=\"$tp_p\" \"$tp_t\"; chown --reference=\"$tp_p\" \"$tp_t\" 2>/dev/null\n      else\n        chmod 644 \"$tp_t\"\n      fi\n      mv -f \"$tp_t\" \"$tp_p\"; rc=$?\n    else\n      rc=$?\n    fi\n    if [ \"$rc\" -eq 0 ]; then changed=1; else rm -f \"$tp_t\"; fi\n  fi\nfi\n",
                quote(&STANDARD.encode(c))
            ));
        }
        self.render_lines(o);
    }

    fn render_lines(&self, o: &mut String) {
        if !self.lines.is_empty() {
            o.push_str(
                "tp_append() {\n  if [ -s \"$1\" ] && [ -n \"$(tail -c1 \"$1\")\" ]; then printf '\\n' >> \"$1\"; fi\n  printf '%s\\n' \"$2\" >> \"$1\"\n}\n",
            );
        }
        for l in &self.lines {
            let (re, line) = match l {
                LineOp::Line(l) => (None, l),
                LineOp::LineRe { re, line } => (Some(re), line),
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

impl FileHandle {
    fn spec(&self, f: impl FnOnce(&mut FileSpec)) -> &Self {
        self.0.edit(|op| {
            if let OpSpec::File(s) = &mut op.spec {
                f(s);
            }
        });
        self.0.refresh_effects();
        self
    }

    /// Ensure this exact line is present, appending if missing.
    pub fn line(&self, line: &str) -> &Self {
        self.spec(|s| s.lines.push(LineOp::Line(line.to_string())))
    }
    /// Replace the last line matching `re` (an ERE, `grep -E`) with `line`,
    /// or append it when none matches.
    pub fn line_re(&self, re: &str, line: &str) -> &Self {
        self.spec(|s| {
            s.lines.push(LineOp::LineRe {
                re: re.to_string(),
                line: line.to_string(),
            })
        })
    }
    /// Ensure a directory.
    pub fn dir(&self) -> &Self {
        self.spec(|s| s.kind = FileKind::Dir)
    }
    /// Ensure the path does not exist (removes directories recursively).
    pub fn absent(&self) -> &Self {
        self.spec(|s| s.kind = FileKind::Absent)
    }
    /// Ensure exactly these bytes.
    pub fn content(&self, bytes: impl AsRef<[u8]>) -> &Self {
        let b = bytes.as_ref().to_vec();
        self.spec(|s| s.content = Some(b))
    }
    /// Check the candidate before it replaces the target. `cmd` is shell
    /// with `%s` standing for the candidate's path (`visudo -cf %s`); it
    /// applies to `.content(..)` and line edits. Without `%s` it is a trace
    /// error. A non-zero exit fails the op with that rc and leaves the
    /// target untouched.
    pub fn validate(&self, cmd: &str) -> &Self {
        if !cmd.contains("%s") {
            self.0.record_error(TraceError::new(format!(
                "file validate command has no %s: {cmd}"
            )));
            return self;
        }
        let c = cmd.to_string();
        self.spec(|s| s.validate = Some(c))
    }
    pub fn mode(&self, mode: u32) -> &Self {
        self.spec(|s| s.mode = Some(mode))
    }
    /// `"user"`, `"user:group"` or `":group"`.
    pub fn owner(&self, owner: &str) -> &Self {
        self.spec(|s| s.owner = Some(owner.to_string()))
    }
}

impl<V> Host<V> {
    /// Manage the file or directory at `path`. `~`/`~user` prefixes expand
    /// on the host, in the op's shell.
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
        let errs: Vec<String> = h.snapshot().errors.iter().map(|e| e.0.clone()).collect();
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("%s"), "{errs:?}");
        h.file("/etc/y").content("a").validate("visudo -cf %s");
        assert_eq!(h.snapshot().errors.len(), 1);
    }
}
