//! The trailer protocol: wrap an op's body so its result comes back as one
//! line the engine can find, and parse that line into a typed result.
//!
//! # Wire format (what T9's parser splits on)
//!
//! [`render_op`] produces a subshell:
//!
//! ```text
//! { (
//! exec 2>&3 3>&-                      # the op's own stderr: the stream
//! __tp_esc() { ... }                  # JSON string escaping helper
//! __tp_emit() { ... }                 # prints the trailer; runs on EXIT
//! trap '__tp_emit $?' EXIT
//! trap 'exit 143' TERM                # a TERMed op still prints its trailer
//! rc=0; changed=0; skipped=0; installed=()
//! printf '\n__tpS_<nonce> <id> %s %s\n' "$BASHPID" "$$"   # the start line
//! <op body: sets rc / changed / skipped / installed>
//! exit "$rc"
//! ) </dev/null; } 3>&2 2>/dev/null
//! ```
//!
//! Because the trailer is printed by an `EXIT` trap inside the subshell, it
//! is emitted on every exit path, including a body that calls `exit N`
//! (then `rc` is `N`). `cd`, `export` and variables die with the subshell.
//!
//! # Start line and process group
//!
//! The persistent shell runs under `set -m`, so the op subshell is its own
//! process group: its pgid is its `$BASHPID`, and its children share it.
//! The start line carries that pid and `$$` (the persistent shell's pid)
//! so a controller can `kill -- -PID` the op from a second shell after
//! checking `$$`. Its framing, `\n__tpS_<nonce> <id> <pid> <shell_pid>\n`,
//! is not `__tp_`, so it is never taken for a trailer, and the splitter
//! never shows it as output. A guard skip and a halt skip ([`skip`]) print
//! none. A login op (`as_user`) renders exactly like a root op: it is sent
//! to the user's persistent login shell ([`crate::executor`]), so `$$` in
//! its start line is that login bash and the op's subshell is its child.
//!
//! The persistent shell's stderr is merged into its stdout. Bash prints a
//! job notice for a job killed by a signal, and the notice holds the whole
//! script, so the group `{ ...; } 3>&2 2>/dev/null` swaps the parent's
//! stderr for `/dev/null` around the op only; the op's subshell puts its
//! own stderr back (`exec 2>&3`). A TERMed op exits 143 through the trap
//! and prints its trailer; a KILLed op prints none.
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
    /// A git checkout: `created` and `was_rev` are the inverse.
    Git,
    /// An author-defined resource: `rc` and `changed`. Irreversibility is
    /// known from the trace (no inverse script), not from this payload.
    Resource,
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

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct FileResult {
    pub rc: i32,
    pub changed: bool,
    /// The path existed before the op.
    #[serde(default)]
    pub existed: bool,
    /// What it was: `file`, `dir` or `symlink`; empty when it did not exist.
    #[serde(default)]
    pub was_type: String,
    /// Its mode before the op, octal (`640`); empty when it did not exist.
    #[serde(default)]
    pub was_mode: String,
    /// Its owner before the op, `user:group`; empty when it did not exist.
    #[serde(default)]
    pub was_owner: String,
    /// The topmost directory the op's `mkdir -p` created; empty when none.
    #[serde(default)]
    pub created_top: String,
    /// The stage path of the backup directory (`<stage>/<run>/undo/<user>/
    /// <id>`), whose `old` is the saved object; empty when nothing was saved.
    #[serde(default)]
    pub backup: String,
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
    /// Daemon-reloads this op actually ran, comma-joined: `unit` (a changed
    /// unit's reload, `__tp_dr`) and `need` (systemd `NeedDaemonReload`).
    /// Empty when none ran, including a guard skip and a host with no
    /// running systemd. The executor reports one entry per token.
    #[serde(default)]
    pub reloads: String,
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

/// A git checkout. `created` is set when this run made the directory.
/// `was_rev` is the HEAD from before the op moved it (empty when the
/// checkout was created, or when the op could not read one).
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct GitResult {
    pub rc: i32,
    pub changed: bool,
    pub created: bool,
    #[serde(default)]
    pub was_rev: String,
}

impl OpResult for GitResult {
    const KIND: OpKind = OpKind::Git;
}

/// A resource op. The same JSON shape as a file: whether it ran and
/// whether the apply changed the host. The inverse, when the resource
/// defined one, stays on the trace; this result does not carry it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ResourceResult {
    pub rc: i32,
    pub changed: bool,
}

impl OpResult for ResourceResult {
    const KIND: OpKind = OpKind::Resource;
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

/// Split captured stdout into `(output, trailer)` pairs for `nonce`; start
/// lines are dropped. Text
/// after the last trailer (an op still running) is dropped.
pub fn split_stream(stream: &str, nonce: &str) -> Vec<(String, Trailer)> {
    let stream = &strip_start_lines(stream, nonce);
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

/// `stream` without the start lines (`\n__tpS_<nonce> ...\n`) of `nonce`.
fn strip_start_lines(stream: &str, nonce: &str) -> String {
    let marker = format!("\n{START_TAG}{nonce} ");
    let mut out = String::with_capacity(stream.len());
    let mut rest = stream;
    while let Some(i) = rest.find(&marker) {
        out.push_str(&rest[..i]);
        match rest[i + 1..].find('\n') {
            Some(len) => rest = &rest[i + 1 + len + 1..],
            None => {
                rest = &rest[i..];
                break;
            }
        }
    }
    out.push_str(rest);
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
    render_op_with(op, id, nonce, os, RenderOpts::default(), true)
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

fn render_op_with(
    op: &OpNode,
    id: usize,
    nonce: &str,
    os: &Os,
    opts: RenderOpts,
    killable: bool,
) -> String {
    let body = op_body(op, os, opts);
    wrap_framed(op.spec.op_kind(), &body, id, nonce, opts.source, killable)
}

/// The op's body as the shell runs it (it sets `rc`, `changed`, ...). A
/// `wait_for` gets the op's own deadline, its `.timeout(d)`.
fn op_body(op: &OpNode, os: &Os, opts: RenderOpts) -> String {
    match &op.spec {
        crate::trace::OpSpec::Raw { script } => crate::ops::shell::raw_body(script),
        crate::trace::OpSpec::Svc(s) if opts.daemon_reload => s.render_exec_reload(),
        crate::trace::OpSpec::Svc(s) => s.render_exec(),
        crate::trace::OpSpec::Wait(w) => w.render(op.effective_timeout()),
        spec => spec.exec_script(os),
    }
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
        OpKind::File | OpKind::Repo | OpKind::Resource => {
            r#"{"rc":0,"changed":false,"when":false}"#
        }
        OpKind::Group | OpKind::User => r#"{"rc":0,"changed":false,"created":false,"when":false}"#,
        OpKind::Git => r#"{"rc":0,"changed":false,"created":false,"was_rev":"","when":false}"#,
        OpKind::Sh | OpKind::Svc => r#"{"rc":0,"changed":false,"skipped":true,"when":false}"#,
    };
    format!("printf '\\n__tp_{nonce} {id} %s\\n' '{json}'\n")
}

/// The trailer the halt gate prints for an op it does not run: rc 0,
/// `skipped`. No start line: nothing ran, so there is nothing to kill.
pub fn skip(kind: OpKind, id: usize, nonce: &str) -> String {
    wrap_plain(kind, "rc=0\nskipped=1\n", id, nonce, false)
}

/// Was this trailer printed by [`guard_skip`]?
pub fn is_guard_skip(t: &Trailer) -> bool {
    serde_json::from_str::<serde_json::Value>(&t.json)
        .ok()
        .and_then(|v| v.get("when").and_then(|w| w.as_bool()))
        == Some(false)
}

/// What follows `\n` and the nonce of the start line: not `__tp_`, so a
/// start line is never taken for a trailer.
pub(crate) const START_TAG: &str = "__tpS_";

/// The start line an op's subshell prints once its traps are set: its own
/// pid (the process group id, under `set -m`) and `$$`, the persistent
/// shell's pid.
fn start_line(nonce: &str, id: usize) -> String {
    format!("printf '\\n{START_TAG}{nonce} {id} %s %s\\n' \"$BASHPID\" \"$$\"\n")
}

/// `wrap_with` for a skip: the plain subshell, no start line and no stderr
/// swap.
fn wrap_plain(kind: OpKind, body: &str, id: usize, nonce: &str, record: bool) -> String {
    wrap_framed(kind, body, id, nonce, record, false)
}

fn wrap_with(kind: OpKind, body: &str, id: usize, nonce: &str, record: bool) -> String {
    wrap_framed(kind, body, id, nonce, record, true)
}

fn wrap_framed(
    kind: OpKind,
    body: &str,
    id: usize,
    nonce: &str,
    record: bool,
    killable: bool,
) -> String {
    assert!(
        nonce.chars().all(|c| c.is_ascii_alphanumeric()),
        "nonce must be alphanumeric"
    );
    let json = match kind {
        OpKind::Pkg => r#"{"rc":%s,"changed":%s,"installed":[%s]}"#,
        OpKind::File => {
            r#"{"rc":%s,"changed":%s,"existed":%s,"was_type":"%s","was_mode":"%s","was_owner":"%s","created_top":"%s","backup":"%s"}"#
        }
        OpKind::Repo | OpKind::Resource => r#"{"rc":%s,"changed":%s}"#,
        OpKind::Group | OpKind::User => r#"{"rc":%s,"changed":%s,"created":%s}"#,
        OpKind::Git => r#"{"rc":%s,"changed":%s,"created":%s,"was_rev":"%s"}"#,
        OpKind::Sh => r#"{"rc":%s,"changed":%s,"skipped":%s}"#,
        OpKind::Svc => {
            r#"{"rc":%s,"changed":%s,"skipped":%s,"was_enabled":%s,"was_active":%s,"was_masked":%s,"started":%s,"reloads":"%s"}"#
        }
    };
    let args = match kind {
        OpKind::Pkg => r#""$rc" "$__tp_c" "$__tp_i""#,
        OpKind::File => {
            r#""$rc" "$__tp_c" "$__tp_ex" "$(__tp_esc "${tp_wt-}")" "$(__tp_esc "${tp_wm-}")" "$(__tp_esc "${tp_wo-}")" "$(__tp_esc "${tp_ct-}")" "$(__tp_esc "${tp_bk-}")""#
        }
        OpKind::Repo | OpKind::Resource => r#""$rc" "$__tp_c""#,
        OpKind::Group | OpKind::User => r#""$rc" "$__tp_c" "$__tp_k""#,
        OpKind::Git => r#""$rc" "$__tp_c" "$__tp_k" "$(__tp_esc "${tp_was-}")""#,
        OpKind::Sh => r#""$rc" "$__tp_c" "$__tp_s""#,
        OpKind::Svc => {
            r#""$rc" "$__tp_c" "$__tp_s" "${tp_was_e:-null}" "${tp_was_a:-null}" "${tp_was_m:-null}" "$__tp_st" "$(__tp_esc "${__tp_drn-}")""#
        }
    };
    // The killable op: stderr swapped around it, its own group, a start line.
    let mut o = String::from(if killable {
        "{ (\nexec 2>&3 3>&-\n"
    } else {
        "(\n"
    });
    o.push_str(PRELUDE);
    if matches!(kind, OpKind::Group | OpKind::User | OpKind::Git) {
        // Only a group or user op sets `created`; other kinds keep their bytes.
        o.push_str("  case \"$created\" in 1) __tp_k=true;; *) __tp_k=false;; esac\n");
    }
    if kind == OpKind::File {
        // A file op records what its path was (`tp_ex`, set by `tp_was`).
        o.push_str("  case \"${tp_ex-}\" in 1) __tp_ex=true;; *) __tp_ex=false;; esac\n");
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
    o.push_str("}\ntrap '__tp_emit $?' EXIT\ntrap 'exit 143' TERM\n");
    o.push_str("rc=0; changed=0; skipped=0; installed=()\n");
    if kind == OpKind::File {
        // The id names the op's directory under the run's undo tree.
        let _ = writeln!(o, "tp_id={id}");
    }
    if killable {
        o.push_str(&start_line(nonce, id));
    }
    o.push_str(body);
    if !body.ends_with('\n') {
        o.push('\n');
    }
    // stdin is the shell's op queue: an op must never read it.
    o.push_str(if killable {
        "exit \"$rc\"\n) </dev/null; } 3>&2 2>/dev/null\n"
    } else {
        "exit \"$rc\"\n) </dev/null\n"
    });
    o
}

/// Why an op could not be rendered for the shell it was routed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderError {
    /// An `as_user(user)` op was sent to a shell that is `shell_user`'s: a
    /// sanity check, as each user has a login shell of their own.
    UserSwitchUnsupported { user: String, shell_user: String },
    /// A line of a script given to a here-doc equals the here-doc's
    /// delimiter, which would end the script early. Never cut silently.
    ScriptHasDelimiter { delimiter: String },
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderError::UserSwitchUnsupported { user, shell_user } => write!(
                f,
                "as_user({user}) from a {shell_user} shell: switching users is not supported yet"
            ),
            RenderError::ScriptHasDelimiter { delimiter } => write!(
                f,
                "a script line equals its here-doc delimiter {delimiter}: refusing to cut the script"
            ),
        }
    }
}

impl std::error::Error for RenderError {}

/// `script` as the body of a quoted here-doc ended by `delimiter`: the
/// text, a final newline if it lacks one, then the delimiter line. A line
/// equal to the delimiter is an error: the shell would end the script there.
pub(crate) fn heredoc_body(script: &str, delimiter: &str) -> Result<String, RenderError> {
    if script.lines().any(|l| l == delimiter) {
        return Err(RenderError::ScriptHasDelimiter {
            delimiter: delimiter.to_string(),
        });
    }
    let nl = if script.ends_with('\n') { "" } else { "\n" };
    Ok(format!("{script}{nl}{delimiter}\n"))
}

/// Render `op` for a shell whose user is `shell_user` ("root" for a root
/// shell). An op issued through `as_user(u)` is sent to `u`'s persistent
/// login shell ([`crate::executor`]), which already is `u` with `u`'s
/// login environment, so it renders exactly like a root op: the killable
/// subshell of [`render_op`], no per-op `bash`, `env -i`, here-doc or
/// `getent`. Only a sanity check stays: the op's user must be the shell's
/// user ([`RenderError::UserSwitchUnsupported`]).
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
/// [`crate::executor`]).
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

/// [`render_for`] with [`RenderOpts`]. `__tp_f` (a source), `__tp_dr` (the
/// unit flag) and `__tp_run` are variables of the shell the op is sent to:
/// the login shell's own, for a login op.
pub fn render_with(
    op: &OpNode,
    id: usize,
    nonce: &str,
    os: &Os,
    shell_user: &str,
    opts: RenderOpts,
) -> Result<String, RenderError> {
    if op.run_as.login {
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
    }
    Ok(render_op_with(op, id, nonce, os, opts, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_script_line_equal_to_the_delimiter_is_a_render_error() {
        let d = "__tp_S_n0nce_7";
        assert_eq!(
            heredoc_body(&format!("echo a\n{d}\necho b\n"), d),
            Err(RenderError::ScriptHasDelimiter {
                delimiter: d.to_string()
            })
        );
        // Only a whole line counts; a longer or indented line is body text.
        assert_eq!(
            heredoc_body(&format!("echo {d}\n {d}\n{d}x"), d).unwrap(),
            format!("echo {d}\n {d}\n{d}x\n{d}\n")
        );
        assert_eq!(
            heredoc_body("echo a\n", d).unwrap(),
            format!("echo a\n{d}\n")
        );
    }
}
