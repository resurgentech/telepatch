//! The trailer protocol: wrap an op's body so its result comes back as one
//! line the engine can find, and parse that line into a typed result.
//!
//! # Wire format (what T9's parser splits on)
//!
//! [`render_op`] produces a subshell:
//!
//! ```text
//! (
//! __tp_esc() { ... }                  # JSON string escaping helper
//! __tp_emit() { ... }                 # prints the trailer; runs on EXIT
//! trap '__tp_emit $?' EXIT
//! rc=0; changed=0; skipped=0; installed=()
//! <op body: sets rc / changed / skipped / installed>
//! exit "$rc"
//! )
//! ```
//!
//! Because the trailer is printed by an `EXIT` trap inside the subshell, it
//! is emitted on every exit path, including a body that calls `exit N`
//! (then `rc` is `N`). `cd`, `export` and variables die with the subshell.
//!
//! The trailer is the byte sequence `"\n__tp_<nonce> <id> <json>\n"`. The
//! leading `\n` is a *separator*, always printed: an op whose output lacks a
//! trailing newline still gets its trailer at a line start, and the parser
//! drops exactly that one `\n` from the op's output. An op's output is the
//! bytes after the previous trailer's final `\n` and before the next
//! `"\n__tp_<nonce> "`. On a PTY the separator shows as one blank line when
//! the output already ended in a newline; that is the price of never having
//! to guess.
//!
//! `rc` is always a bare integer (non-numeric is coerced to 1), booleans are
//! `true`/`false`, strings are JSON-escaped for `\` and `"`.

use std::fmt::Write;

use serde::Deserialize;

use crate::facts::os::Os;
use crate::trace::OpNode;

/// Which result type an op's trailer carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Pkg,
    File,
    Sh,
}

/// A trailer's JSON payload, typed. Fields are all required.
pub trait OpResult: serde::de::DeserializeOwned {
    const KIND: OpKind;
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PkgResult {
    pub rc: i32,
    pub changed: bool,
    /// The names this op installed (empty for `build-dep`, whose set of
    /// installed packages is not known).
    pub installed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FileResult {
    pub rc: i32,
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ShellResult {
    pub rc: i32,
    pub changed: bool,
    pub skipped: bool,
}

impl OpResult for PkgResult {
    const KIND: OpKind = OpKind::Pkg;
}
impl OpResult for FileResult {
    const KIND: OpKind = OpKind::File;
}
impl OpResult for ShellResult {
    const KIND: OpKind = OpKind::Sh;
}

/// One parsed trailer line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trailer {
    pub nonce: String,
    pub id: usize,
    pub json: String,
}

impl Trailer {
    /// Parse `__tp_<nonce> <id> <json>` (no trailing newline).
    pub fn parse_line(line: &str) -> Option<Trailer> {
        let rest = line.strip_prefix("__tp_")?;
        let (nonce, rest) = rest.split_once(' ')?;
        let (id, json) = rest.split_once(' ')?;
        Some(Trailer {
            nonce: nonce.to_string(),
            id: id.parse().ok()?,
            json: json.to_string(),
        })
    }

    /// The typed result; an error if the JSON lacks a field of `R`.
    pub fn typed<R: OpResult>(&self) -> Result<R, serde_json::Error> {
        serde_json::from_str(&self.json)
    }
}

/// Split captured stdout into `(output, trailer)` pairs for `nonce`. Text
/// after the last trailer (an op still running) is dropped.
pub fn split_stream(stream: &str, nonce: &str) -> Vec<(String, Trailer)> {
    let marker = format!("\n__tp_{nonce} ");
    let mut out = Vec::new();
    let mut cursor = 0;
    while let Some(i) = stream[cursor..].find(&marker) {
        let start = cursor + i;
        let line_start = start + 1;
        let Some(len) = stream[line_start..].find('\n') else {
            break;
        };
        if let Some(t) = Trailer::parse_line(&stream[line_start..line_start + len]) {
            out.push((stream[cursor..start].to_string(), t));
        }
        cursor = line_start + len + 1;
    }
    out
}

const PRELUDE: &str = r#"__tp_esc() { local s=${1//\\/\\\\}; s=${s//\"/\\\"}; printf %s "$s"; }
__tp_emit() {
  if [ "$1" -ne 0 ]; then rc=$1; fi
  case "$rc" in ''|*[!0-9]*) rc=1;; esac
  case "$changed" in 1) __tp_c=true;; *) __tp_c=false;; esac
  case "$skipped" in 1) __tp_s=true;; *) __tp_s=false;; esac
  __tp_i=
  for __tp_x in "${installed[@]}"; do __tp_i="$__tp_i${__tp_i:+,}\"$(__tp_esc "$__tp_x")\""; done
"#;

/// The full script for one op: body wrapped in the trailer subshell.
/// `id` is the op's trace index; `nonce` is random per run.
pub fn render_op(op: &OpNode, id: usize, nonce: &str, os: &Os) -> String {
    let body = match &op.spec {
        crate::trace::OpSpec::Raw { script } => crate::ops::shell::raw_body(script),
        spec => spec.script(os),
    };
    wrap(op.spec.op_kind(), &body, id, nonce)
}

/// Wrap `body` (which sets `rc` / `changed` / ...) in the trailer subshell
/// for a result of `kind`.
pub fn wrap(kind: OpKind, body: &str, id: usize, nonce: &str) -> String {
    assert!(
        nonce.chars().all(|c| c.is_ascii_alphanumeric()),
        "nonce must be alphanumeric"
    );
    let json = match kind {
        OpKind::Pkg => r#"{"rc":%s,"changed":%s,"installed":[%s]}"#,
        OpKind::File => r#"{"rc":%s,"changed":%s}"#,
        OpKind::Sh => r#"{"rc":%s,"changed":%s,"skipped":%s}"#,
    };
    let args = match kind {
        OpKind::Pkg => r#""$rc" "$__tp_c" "$__tp_i""#,
        OpKind::File => r#""$rc" "$__tp_c""#,
        OpKind::Sh => r#""$rc" "$__tp_c" "$__tp_s""#,
    };
    let mut o = String::from("(\n");
    o.push_str(PRELUDE);
    let _ = writeln!(o, "  printf '\\n__tp_{nonce} {id} {}\\n' {args}", json);
    o.push_str("}\ntrap '__tp_emit $?' EXIT\nrc=0; changed=0; skipped=0; installed=()\n");
    o.push_str(body);
    if !body.ends_with('\n') {
        o.push('\n');
    }
    // stdin is the shell's op queue: an op must never read it.
    o.push_str("exit \"$rc\"\n) </dev/null\n");
    o
}

/// Why an op could not be rendered for the shell it was routed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderError {
    /// A login environment for `user` was requested in a shell that is
    /// `shell_user`'s. Switching to another user (`sudo -iu`) is not
    /// implemented yet.
    UserSwitchUnsupported { user: String, shell_user: String },
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderError::UserSwitchUnsupported { user, shell_user } => write!(
                f,
                "as_user({user}) from a {shell_user} shell: switching users is not supported yet"
            ),
        }
    }
}

impl std::error::Error for RenderError {}

const LOGIN_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// Render `op` for a shell whose user is `shell_user` ("root" for a root
/// shell). Ops with `run_as.login == false` are [`render_op`] unchanged. An
/// op issued through `as_user(u)` runs its whole wrapped script (trailer
/// included, stdout inherited) in a fresh `bash -l` for `u` started with
/// `env -i`, with HOME and SHELL taken from `getent passwd u` evaluated in
/// the shell at op time, so a profile edited earlier in the run is seen. If
/// `u` has no passwd entry the op fails with rc 1 and a message on stderr
/// (still with a trailer). Only `u == shell_user` is supported.
pub fn render_for(
    op: &OpNode,
    id: usize,
    nonce: &str,
    os: &Os,
    shell_user: &str,
) -> Result<String, RenderError> {
    let plain = render_op(op, id, nonce, os);
    if !op.run_as.login {
        return Ok(plain);
    }
    let user = match &op.run_as.principal {
        crate::trace::Principal::Root => "root",
        crate::trace::Principal::User(u) => u.as_str(),
    };
    if user != shell_user {
        return Err(RenderError::UserSwitchUnsupported {
            user: user.to_string(),
            shell_user: shell_user.to_string(),
        });
    }
    let q = crate::sh::quote(user);
    let failed = wrap(
        op.spec.op_kind(),
        &format!("echo \"telepatch: no passwd entry for \"{q} >&2\nrc=1\n"),
        id,
        nonce,
    );
    Ok(format!(
        "(\n__tp_pw=$(getent passwd {q}) || __tp_pw=\n\
         if [ -z \"$__tp_pw\" ]; then\n{failed}else\n\
         IFS=: read -r _ _ _ _ _ __tp_home __tp_shell <<<\"$__tp_pw\"\n\
         env -i HOME=\"$__tp_home\" USER={q} LOGNAME={q} SHELL=\"$__tp_shell\" PATH={LOGIN_PATH} bash -l -c {}\n\
         fi\n) </dev/null\n",
        crate::sh::quote(&plain)
    ))
}
