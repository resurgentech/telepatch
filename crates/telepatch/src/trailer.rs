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
    Group,
    User,
    Repo,
    Svc,
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
pub struct GroupResult {
    pub rc: i32,
    pub changed: bool,
    /// The group did not exist and this op made it.
    pub created: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct UserResult {
    pub rc: i32,
    pub changed: bool,
    /// The account did not exist and this op made it.
    pub created: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RepoResult {
    pub rc: i32,
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ShellResult {
    pub rc: i32,
    pub changed: bool,
    pub skipped: bool,
}

/// A `svc` op: `skipped` is true when the op did nothing because the host
/// has no `systemctl` or no running systemd (rc 0, never `changed`).
///
/// `was_*` are the unit's state before the op ran, the inverse data
/// (restore enabled, active and masked as captured); `None` when the host
/// could not say (no `systemctl`, `active` without a running systemd, a
/// guard that skipped the op).
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct SvcResult {
    pub rc: i32,
    pub changed: bool,
    pub skipped: bool,
    #[serde(default)]
    pub was_enabled: Option<bool>,
    #[serde(default)]
    pub was_active: Option<bool>,
    #[serde(default)]
    pub was_masked: Option<bool>,
    /// This op started the unit: it wants `.started()`, the unit was not
    /// active, and its `systemctl start` succeeded. False for an op that
    /// only enabled or unmasked a running unit (`changed` is true there),
    /// for an action, and for a skipped op. A restart or reload of the
    /// unit later in the run is dropped only when this is true
    /// ([`crate::executor`]).
    #[serde(default)]
    pub started: bool,
}

impl OpResult for PkgResult {
    const KIND: OpKind = OpKind::Pkg;
}
impl OpResult for FileResult {
    const KIND: OpKind = OpKind::File;
}
impl OpResult for SvcResult {
    const KIND: OpKind = OpKind::Svc;
}
impl OpResult for GroupResult {
    const KIND: OpKind = OpKind::Group;
}
impl OpResult for UserResult {
    const KIND: OpKind = OpKind::User;
}
impl OpResult for RepoResult {
    const KIND: OpKind = OpKind::Repo;
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
    render_op_with(op, id, nonce, os, RenderOpts::default())
}

/// How [`render_with`] renders an op beyond its spec.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderOpts {
    /// A guard later in the run reads this op: see [`render_source_for`].
    pub source: bool,
    /// A `unit` op came earlier in this window: a `svc` op first runs the
    /// one `daemon-reload` a changed unit asks for
    /// ([`crate::ops::svc::SvcSpec::render_exec_reload`]). Other ops
    /// ignore it.
    pub daemon_reload: bool,
}

fn render_op_with(op: &OpNode, id: usize, nonce: &str, os: &Os, opts: RenderOpts) -> String {
    let body = match &op.spec {
        crate::trace::OpSpec::Raw { script } => crate::ops::shell::raw_body(script),
        crate::trace::OpSpec::Svc(s) if opts.daemon_reload => s.render_exec_reload(),
        crate::trace::OpSpec::Svc(s) => s.render_exec(),
        spec => spec.exec_script(os),
    };
    wrap_with(op.spec.op_kind(), &body, id, nonce, opts.source)
}

/// Wrap `body` (which sets `rc` / `changed` / ...) in the trailer subshell
/// for a result of `kind`.
pub fn wrap(kind: OpKind, body: &str, id: usize, nonce: &str) -> String {
    wrap_with(kind, body, id, nonce, false)
}

/// The line a guard source's trailer adds: when the parent shell set
/// `__tp_f` (a `mktemp` file), the result's `changed` (`true`/`false`) and
/// then the `installed` names, one per line, go there too, so the parent
/// shell can set the variables a guard reads (the subshell cannot).
const RECORD: &str =
    "  [ -z \"${__tp_f-}\" ] || printf '%s\\n' \"$__tp_c\" \"${installed[@]}\" > \"$__tp_f\"\n";

/// [`RECORD`] for a `svc` op: `changed`, then `started` (`true`/`false`),
/// one per line (a svc op installs nothing).
const RECORD_SVC: &str =
    "  [ -z \"${__tp_f-}\" ] || printf '%s\\n' \"$__tp_c\" \"$__tp_st\" > \"$__tp_f\"\n";

/// The trailer an op prints instead of running when its guard is false:
/// rc 0, unchanged, and `"when":false`, which the executor reads as
/// skipped. Printed by the parent shell (no subshell, nothing runs).
pub fn guard_skip(kind: OpKind, id: usize, nonce: &str) -> String {
    assert!(
        nonce.chars().all(|c| c.is_ascii_alphanumeric()),
        "nonce must be alphanumeric"
    );
    let json = match kind {
        OpKind::Pkg => r#"{"rc":0,"changed":false,"installed":[],"when":false}"#,
        OpKind::File | OpKind::Repo => r#"{"rc":0,"changed":false,"when":false}"#,
        OpKind::Group | OpKind::User => r#"{"rc":0,"changed":false,"created":false,"when":false}"#,
        OpKind::Sh | OpKind::Svc => r#"{"rc":0,"changed":false,"skipped":true,"when":false}"#,
    };
    format!("printf '\\n__tp_{nonce} {id} %s\\n' '{json}'\n")
}

/// Was this trailer printed by [`guard_skip`]?
pub fn is_guard_skip(t: &Trailer) -> bool {
    serde_json::from_str::<serde_json::Value>(&t.json)
        .ok()
        .and_then(|v| v.get("when").and_then(|w| w.as_bool()))
        == Some(false)
}

fn wrap_with(kind: OpKind, body: &str, id: usize, nonce: &str, record: bool) -> String {
    assert!(
        nonce.chars().all(|c| c.is_ascii_alphanumeric()),
        "nonce must be alphanumeric"
    );
    let json = match kind {
        OpKind::Pkg => r#"{"rc":%s,"changed":%s,"installed":[%s]}"#,
        OpKind::File | OpKind::Repo => r#"{"rc":%s,"changed":%s}"#,
        OpKind::Group | OpKind::User => r#"{"rc":%s,"changed":%s,"created":%s}"#,
        OpKind::Sh => r#"{"rc":%s,"changed":%s,"skipped":%s}"#,
        OpKind::Svc => {
            r#"{"rc":%s,"changed":%s,"skipped":%s,"was_enabled":%s,"was_active":%s,"was_masked":%s,"started":%s}"#
        }
    };
    let args = match kind {
        OpKind::Pkg => r#""$rc" "$__tp_c" "$__tp_i""#,
        OpKind::File | OpKind::Repo => r#""$rc" "$__tp_c""#,
        OpKind::Group | OpKind::User => r#""$rc" "$__tp_c" "$__tp_k""#,
        OpKind::Sh => r#""$rc" "$__tp_c" "$__tp_s""#,
        OpKind::Svc => {
            r#""$rc" "$__tp_c" "$__tp_s" "${tp_was_e:-null}" "${tp_was_a:-null}" "${tp_was_m:-null}" "$__tp_st""#
        }
    };
    let mut o = String::from("(\n");
    o.push_str(PRELUDE);
    if matches!(kind, OpKind::Group | OpKind::User) {
        // Only a group or user op sets `created`; other kinds keep their bytes.
        o.push_str("  case \"$created\" in 1) __tp_k=true;; *) __tp_k=false;; esac\n");
    }
    if kind == OpKind::Svc {
        // Only a svc op sets `tp_started` (a start that ran).
        o.push_str("  case \"${tp_started-}\" in 1) __tp_st=true;; *) __tp_st=false;; esac\n");
    }
    if record {
        o.push_str(if kind == OpKind::Svc {
            RECORD_SVC
        } else {
            RECORD
        });
    }
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
    render_with(op, id, nonce, os, shell_user, RenderOpts::default())
}

/// [`render_for`] for an op a guard in the same run reads: its trailer
/// also writes its result to `$__tp_f` when the parent shell set it (see
/// [`crate::executor`]). A login op gets `__tp_f` through its `env -i`.
pub fn render_source_for(
    op: &OpNode,
    id: usize,
    nonce: &str,
    os: &Os,
    shell_user: &str,
) -> Result<String, RenderError> {
    render_with(
        op,
        id,
        nonce,
        os,
        shell_user,
        RenderOpts {
            source: true,
            daemon_reload: false,
        },
    )
}

/// [`render_for`] with [`RenderOpts`]. A login op gets `__tp_f` (a source)
/// and `__tp_dr` (the unit flag) through its `env -i`.
pub fn render_with(
    op: &OpNode,
    id: usize,
    nonce: &str,
    os: &Os,
    shell_user: &str,
    opts: RenderOpts,
) -> Result<String, RenderError> {
    let record = opts.source;
    let plain = render_op_with(op, id, nonce, os, opts);
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
    let failed = wrap_with(
        op.spec.op_kind(),
        &format!("echo \"telepatch: no passwd entry for \"{q} >&2\nrc=1\n"),
        id,
        nonce,
        record,
    );
    let mut pass = String::new();
    if record {
        pass.push_str("__tp_f=\"${__tp_f-}\" ");
    }
    if opts.daemon_reload && matches!(op.spec, crate::trace::OpSpec::Svc(_)) {
        pass.push_str("__tp_dr=\"${__tp_dr-}\" ");
    }
    if plain.contains("$__tp_run") {
        pass.push_str("__tp_run=\"${__tp_run-}\" ");
    }
    Ok(format!(
        "(\n__tp_pw=$(getent passwd {q}) || __tp_pw=\n\
         if [ -z \"$__tp_pw\" ]; then\n{failed}else\n\
         IFS=: read -r _ _ _ _ _ __tp_home __tp_shell <<<\"$__tp_pw\"\n\
         env -i {pass}HOME=\"$__tp_home\" USER={q} LOGNAME={q} SHELL=\"$__tp_shell\" PATH={LOGIN_PATH} bash -l -c {}\n\
         fi\n) </dev/null\n",
        crate::sh::quote(&plain)
    ))
}
