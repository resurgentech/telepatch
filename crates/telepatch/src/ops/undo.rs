//! Inverse capture (T106): the shell helpers every shell defines when it
//! opens (T126) and the script that removes what they made.
//!
//! A backup lives on the target's own filesystem. On the stage's filesystem
//! that is `<stage>/<run>/undo/<user>/<id>/`; elsewhere it is
//! `<root>/.telepatch/tmp/<run>/undo/<user>/<id>/` with the stage path a
//! symlink to it. `TP_FS_ROOTS` (a `:`-separated list) is the test seam: a
//! listed directory is the root of a filesystem of its own.
//!
//! Without `__tp_run` (a script run by hand) nothing is saved. A shell that
//! is not uid 0 never creates `.telepatch` at a root the device walk found:
//! the root shell prepares it ([`prepare_script`], before a user's run), or
//! the op fails with `not prepared`. [`sweep_script`] removes what a dead
//! controller left, and [`cleanup_script`] what this run made.

use crate::ops::file::FileSpec;
use crate::sh::quote;
use crate::trace::OpSpec;

/// The helpers, defined once per shell by `init_shell` ([`init_text`], T126),
/// not in each op script: a capturing op starts with [`guard_line`]. `tp_was P` records what
/// P is; `tp_save P [L]` backs it up (once) as `$tp_bd/old` and sets `tp_bk`
/// (the stage path), `L` following a symlink; `tp_gate P` refuses a mount
/// point and a tree holding one, then opens the backup dir for a move;
/// `tp_mk P` is `mkdir -p` that sets `tp_cx` to the topmost directory it
/// made; `tp_open`, `tp_cp`, `tp_note` are the `.from(local_dir)` copy and
/// its `created` manifest. A failure prints `cannot save undo data` (or the
/// mount message) and returns 1. `tp_id` is the op's id.
///
/// Mount detection: a path is a mount point when its device differs from
/// its parent's, or `TP_FS_ROOTS` lists it; a tree holds a mount when a
/// directory below it has another device (`find -printf %D`, stopping at
/// the first, so a mounted tree is not walked), or `TP_FS_ROOTS` lists a
/// directory strictly below it. Only directories are checked below.
pub const HELPERS: &str = concat!(
    crate::sandbox::sbx_def!(),
    r#"tp_fsroot() {
  local d=$1 r s= q v a; case $d in /*) ;; *) d=$PWD/$d;; esac
  IFS=: read -ra a <<<"${TP_FS_ROOTS-}"
  for r in "${a[@]}"; do r=${r%/}; [ -n "$r" ] || continue; case "$d/" in "$r"/*) [ ${#r} -gt ${#s} ] && s=$r;; esac; done
  if [ -n "$s" ]; then printf %s "$s"; return; fi
  while [ ! -e "$d" ] && [ ! -L "$d" ]; do d=$(dirname -- "$d"); done
  v=$(stat -c %d -- "$d")
  while q=$(dirname -- "$d"); [ "$q" != "$d" ] && [ "$(stat -c %d -- "$q")" = "$v" ]; do d=$q; done
  printf %s "$d"
}
tp_was() {
  if [ -L "$1" ]; then tp_wt=symlink; elif [ -d "$1" ]; then tp_wt=dir; elif [ -e "$1" ]; then tp_wt=file; else return 0; fi
  tp_ex=1; tp_wm=$(stat -c %a -- "$1"); tp_wo=$(stat -c %U:%G -- "$1")
}
tp_try() { local p=$1 w=$2 e; shift 2; e=$("$@" 2>&1) || { echo "telepatch: file $p: cannot save undo data: $w: ${e##*: }" >&2; return 1; }; }
tp_mkd() { [ -d "$2" ] || tp_try "$tp_up" "$2" mkdir -m "$1" -- "$2"; }
tp_idx() { grep -qxF -- "$1" "$2" 2>/dev/null || (umask 077; printf '%s\n' "$1" >> "$2"); }
tp_undo() {
  tp_bd=; tp_bk=; [ -n "${__tp_run-}" ] || return 0
  local sd=${TP_FETCH_STAGE:-/var/tmp/telepatch} u r s t i n=
  tp_sbx "$sd" || return 1
  tp_up=$1; u=$(id -un); r=$(tp_fsroot "$1"); s=$sd/$__tp_run; t=$r/.telepatch/tmp/$__tp_run
  [ -d "$r" ] || { echo "telepatch: file $1: is a mount point" >&2; return 1; }
  if [ "$r" != "$(tp_fsroot "$sd")" ] && [ ! -d "$t/undo/$u" ]; then
    case ":${TP_FS_ROOTS-}:" in *":$r:"*) ;; *) [ "$(id -u)" = 0 ] || { echo "telepatch: file $1: cannot save undo data: $t/undo/$u: not prepared" >&2; return 1; };; esac
  fi
  tp_try "$1" "$s/undo" mkdir -p -m 711 -- "$s/undo" && tp_mkd 700 "$s/undo/$u" || return 1
  tp_bk=$s/undo/$u/$tp_id
  if [ "$r" = "$(tp_fsroot "$sd")" ]; then tp_bd=$tp_bk
  else
    if [ ! -d "$t/undo/$u" ]; then
      tp_sbx "$r/.telepatch" || return 1
      [ -d "$t" ] || n=1
      for i in "$r/.telepatch" "$r/.telepatch/tmp" "$t" "$t/undo"; do
        tp_mkd 711 "$i" || return 1
        if [ "$i" = "$t" ] && [ -n "$n" ]; then tp_try "$1" "$s/fsroots" tp_idx "$r" "$s/fsroots" || return 1; fi
      done
      tp_mkd 700 "$t/undo/$u" || return 1
    fi
    tp_bd=$t/undo/$u/$tp_id
  fi
  tp_try "$1" "$tp_bd" mkdir -p -m 700 -- "$tp_bd" || return 1
  [ "$tp_bd" = "$tp_bk" ] || tp_try "$1" "$tp_bk" ln -sfn -- "$tp_bd" "$tp_bk"
}
tp_save() {
  [ "${tp_ex-}" = 1 ] && [ -z "${tp_sv-}" ] || return 0; tp_sv=1
  tp_undo "$1" && { [ -z "$tp_bd" ] || tp_try "$1" "$tp_bd/old" cp -a${2:+L} -- "$1" "$tp_bd/old"; } || { tp_bk=; return 1; }
}
tp_mnt() {
  local p=${1%/} r a; IFS=: read -ra a <<<"${TP_FS_ROOTS-}"
  for r in "${a[@]}"; do [ "${r%/}" = "$p" ] && return 0; done
  [ ! -L "$p" ] && [ -e "$p" ] && [ "$(stat -c %d -- "$p")" != "$(stat -c %d -- "$(dirname -- "$p")")" ]
}
tp_nest() {
  local p=${1%/} r a; IFS=: read -ra a <<<"${TP_FS_ROOTS-}"
  for r in "${a[@]}"; do r=${r%/}; case "$r/" in "$p"/?*) printf %s "$r"; return 0;; esac; done
  [ -d "$p" ] && [ ! -L "$p" ] || return 1
  r=$(find "$p" -mindepth 1 -type d -printf '%D %p\n' 2>/dev/null | awk -v d="$(stat -c %d -- "$p")" '$1 != d { sub(/^[^ ]* /, ""); print; exit }')
  [ -n "$r" ] && printf %s "$r"
}
tp_gate() {
  local m
  if tp_mnt "$1"; then echo "telepatch: file $1: is a mount point" >&2; return 1; fi
  if m=$(tp_nest "$1"); then echo "telepatch: file $1: a filesystem is mounted under it ($m)" >&2; return 1; fi
  [ -n "${2-}" ] && return 0
  tp_undo "$1" || { tp_bk=; return 1; }
}
tp_mk() {
  local x=$1; tp_cx=
  while [ ! -e "$x" ] && [ ! -L "$x" ]; do tp_cx=$x; x=$(dirname -- "$x"); done
  (umask 022; mkdir -p -- "$1")
}
tp_open() { [ -n "${tp_og-}" ] || { tp_undo "$tp_p" && tp_og=1 || { tp_bk=; return 1; }; }; }
tp_note() { [ -z "$tp_bd" ] || tp_try "$tp_p" "$tp_bd/created" tp_idx "$1 $2" "$tp_bd/created"; }
tp_cp() {
  tp_open || return 1; [ -z "$tp_bd" ] && return 0
  tp_try "$tp_p" "$tp_bd/old/$2" mkdir -p -- "$(dirname -- "$tp_bd/old/$2")" && tp_try "$tp_p" "$tp_bd/old/$2" cp -a -- "$1" "$tp_bd/old/$2"
}
"#
);

/// FNV-1a, 64 bit: a hash that is the same on every build and toolchain
/// (it appears in plan snapshots), unlike `DefaultHasher`.
pub const fn fnv1a64(b: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut i = 0;
    while i < b.len() {
        h ^= b[i] as u64;
        h = h.wrapping_mul(0x100000001b3);
        i += 1;
    }
    h
}

/// The helpers' version: [`fnv1a64`] of [`HELPERS`], 16 hex digits. A shell
/// carries it as `__tp_hv`; the guard line of an op names it.
pub fn helpers_hash() -> String {
    format!("{:016x}", fnv1a64(HELPERS.as_bytes()))
}

/// What a shell is sent once when it opens: the helpers and the version
/// they carry. A subshell (an op's `( ... )`) inherits both.
pub fn init_text() -> String {
    format!("{HELPERS}__tp_hv={}\n", helpers_hash())
}

/// The first line of a capturing op: the shell must carry this build's
/// helpers (`__tp_hv` and the functions), else the op ends with rc 125 and
/// a message, before it captures or changes anything.
pub fn guard_line() -> String {
    let h = helpers_hash();
    format!(
        "[ \"${{__tp_hv-}}\" = {h} ] && declare -F tp_save >/dev/null || {{ echo \"telepatch: shell helpers missing or stale (want {h})\" >&2; exit 125; }}\n"
    )
}

/// The shell function that removes one run's data: the per-filesystem
/// directory under each root the run's `fsroots` lists (a real directory
/// owned by root, or by the shell's own uid when that is not 0; anything
/// else is refused with a `telepatch: cleanup:` line), the two parents if
/// empty (also when the run's directory is gone: a tmpfs a reboot emptied),
/// then the stage directory and the links in it. `rm` never leaves its
/// filesystem and never follows a link.
const RMRUN: &str = concat!(
    crate::sandbox::sbx_def!(),
    r#"tp_rmrun() {
  local sd=${TP_FETCH_STAGE:-/var/tmp/telepatch}/$1 r t o
  tp_sbx "$sd" || return 1
  if [ -f "$sd/fsroots" ]; then
    while IFS= read -r r; do
      t=$r/.telepatch/tmp/$1
      tp_sbx "$r/.telepatch" || continue
      if [ -e "$t" ] || [ -L "$t" ]; then
        o=$(stat -c %u -- "$t" 2>/dev/null)
        if [ -d "$t" ] && [ ! -L "$t" ] && { [ "$o" = 0 ] || [ "$o" = "$(id -u)" ]; }; then
          rm -rf --one-file-system -- "$t"
        else
          echo "telepatch: cleanup: $t is not a directory owned by root: left"
        fi
      fi
      rmdir -- "$r/.telepatch/tmp" "$r/.telepatch" 2>/dev/null
    done < "$sd/fsroots"
  fi
  rm -rf --one-file-system -- "$sd"
}
"#
);

/// The script that ends a host's run: [`RMRUN`] for `$__tp_run` (`:?` so an
/// unset variable removes nothing).
pub fn cleanup_script() -> String {
    format!("{RMRUN}tp_rmrun \"${{__tp_run:?}}\"\n")
}

/// The stale sweep, once at a host's start: every `<stage>/*/` that is not
/// this run's and looks like a telepatch run (an `undo` directory or an
/// `fsroots` file; anything else is another's data and stays) is removed by
/// [`RMRUN`] when its mtime is more than 24 hours old, and named in a
/// warning when younger (another controller may be running it).
pub fn sweep_script() -> String {
    format!(
        r#"{RMRUN}tp_sbx "${{TP_FETCH_STAGE:-/var/tmp/telepatch}}" && for tp_d in "${{TP_FETCH_STAGE:-/var/tmp/telepatch}}"/*/; do
  tp_d=${{tp_d%/}}; tp_n=${{tp_d##*/}}
  [ "$tp_n" != "${{__tp_run:?}}" ] && [ -d "$tp_d" ] && [ ! -L "$tp_d" ] || continue
  [ -d "$tp_d/undo" ] || [ -f "$tp_d/fsroots" ] || continue
  if [ -n "$(find "$tp_d" -maxdepth 0 -mmin +1440 2>/dev/null)" ]; then
    tp_rmrun "$tp_n" && echo "telepatch: undo data from run $tp_n removed (older than 24 h)"
  else
    echo "telepatch: undo data from run $tp_n, less than 24 h old, left: another controller may be running"
  fi
done
true
"#
    )
}

/// The shell word for the path a capturing op writes, as seen by `user`
/// (`as_user(user)`): a leading `~` or `~/` is `user`'s home, a `~other`
/// stays, an absolute path is quoted. `None` for an op that does not
/// capture, a relative path (it depends on the shell's directory) or a
/// user name that is not safe in a `~name` word.
pub fn capture_word(spec: &OpSpec, user: &str) -> Option<String> {
    match spec {
        OpSpec::File(f) => {
            let p = &f.path;
            if let Some(rest) = p.strip_prefix('~') {
                let own = rest.is_empty() || rest.starts_with('/');
                if !own {
                    return Some(FileSpec::new(p.clone()).path_word());
                }
                if !user
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
                {
                    return None;
                }
                Some(FileSpec::new(format!("~{user}{rest}")).path_word())
            } else if p.starts_with('/') {
                Some(quote(p))
            } else {
                None
            }
        }
        OpSpec::Unit(u) => Some(format!(
            "\"${{TP_UNIT_DIR:-/etc/systemd/system}}\"/{}",
            quote(&u.rel())
        )),
        _ => None,
    }
}

/// The root shell's preparation op for one run of `user`'s login shell
/// (T106, "Who creates the directories"): the run's undo directories in the
/// stage (`undo` 0711, `undo/<user>` 0700) and, for each path (shell words,
/// [`capture_word`]) on a filesystem other than the stage's, the chain
/// `<root>/.telepatch/tmp/<run>/undo/<user>`, appended to `fsroots`. Owner
/// is `user` when the root shell is uid 0. Idempotent. A directory it could
/// not make prints a `telepatch: undo preparation` line; the op's own
/// capture then fails with `not prepared`.
pub fn prepare_script(user: &str, words: &[String]) -> String {
    let mut s = guard_line();
    s.push_str(&format!(
        r#"tp_u={}
tp_sd=${{TP_FETCH_STAGE:-/var/tmp/telepatch}}
tp_s=$tp_sd/${{__tp_run:?}}
tp_z=; [ "$(id -u)" = 0 ] && tp_z=1
tp_pm() {{
  [ -d "$2" ] && return 0
  local e; e=$(mkdir -m "$1" -- "$2" 2>&1) || {{ echo "telepatch: undo preparation for $tp_u: $2: ${{e##*: }}"; return 1; }}
  [ -z "$3" ] || [ -z "$tp_z" ] || chown -- "$3" "$2"
  return 0
}}
tp_pf() {{
  local r t i n=
  r=$(tp_fsroot "$1") && [ -d "$r" ] && [ "$r" != "$(tp_fsroot "$tp_sd")" ] || return 0
  case ":${{TP_FS_ROOTS-}}:" in *":$r:"*) ;; *) [ -n "$tp_z" ] || return 0;; esac
  t=$r/.telepatch/tmp/$__tp_run
  tp_sbx "$r/.telepatch" || return 0
  [ -d "$t" ] || n=1
  for i in "$r/.telepatch" "$r/.telepatch/tmp" "$t" "$t/undo"; do tp_pm 711 "$i" || return 0; done
  [ -z "$n" ] || tp_idx "$r" "$tp_s/fsroots"
  tp_pm 700 "$t/undo/$tp_u" "$tp_u"
}}
tp_sbx "$tp_s" && mkdir -p -m 711 -- "$tp_s/undo" && tp_pm 700 "$tp_s/undo/$tp_u" "$tp_u" || echo "telepatch: undo preparation for $tp_u: $tp_s/undo: cannot create"
"#,
        quote(user)
    ));
    for w in words {
        s.push_str(&format!("tp_pf {w}\n"));
    }
    s.push_str("true\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::unit::UnitSpec;

    fn file(p: &str) -> OpSpec {
        OpSpec::File(FileSpec::new(p))
    }

    #[test]
    fn a_bare_tilde_is_the_users_home_not_roots() {
        assert_eq!(capture_word(&file("~/a b"), "bob").unwrap(), "~bob/'a b'");
        assert_eq!(capture_word(&file("~"), "bob").unwrap(), "~bob");
        assert_eq!(capture_word(&file("~eve/x"), "bob").unwrap(), "~eve/'x'");
        assert_eq!(
            capture_word(&file("/etc/x y"), "bob").unwrap(),
            "'/etc/x y'"
        );
        assert_eq!(capture_word(&file("rel/x"), "bob"), None);
        assert_eq!(capture_word(&file("~/x"), "b ob"), None);
    }

    #[test]
    fn a_unit_is_the_unit_dir_word() {
        let w = capture_word(&OpSpec::Unit(UnitSpec::new("a.service")), "root").unwrap();
        assert_eq!(w, "\"${TP_UNIT_DIR:-/etc/systemd/system}\"/'a.service'");
    }

    #[test]
    fn the_preparation_script_names_each_path_word() {
        let s = prepare_script("bob", &["~bob/x".to_string(), "'/srv/a b'".to_string()]);
        assert!(s.contains("tp_u='bob'\n"), "{s}");
        assert!(s.contains("\ntp_pf ~bob/x\n"), "{s}");
        assert!(s.contains("\ntp_pf '/srv/a b'\n"), "{s}");
    }
}
