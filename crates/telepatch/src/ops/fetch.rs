//! `fetch(url)`: a download, as a prepare-phase value (not an op).
//!
//! `h.fetch(url).sha256(sum)` returns a [`Fetched`]: a url and a checksum,
//! nothing more. It is not in the trace; the file op that uses it
//! (`h.file(p).from(&f)` places the file, `.unpack(&f).strip(n)` unpacks it)
//! carries it in its spec ([`FileSrc`]), so prepare finds it on the window's
//! file ops. A fetch no op uses is never downloaded and never an error.
//!
//! Prepare, on the host, once per window and per distinct `(url, sum)` (see
//! [`prepare_script`]):
//!
//! * with a checksum: a cache entry `<cache>/<sum>` whose `sha256sum` still
//!   matches is a hit, with no network call and no `curl`/`wget` needed.
//!   Otherwise the file is downloaded (`curl -fsSL`, else `wget -q`) into
//!   the staging directory `<stage>/<run>/`, its `sha256sum` compared, and
//!   only then moved into the cache (copied under a dot-name in the cache
//!   directory and renamed, so an entry under its final name is always
//!   complete). A mismatch or a failed download leaves no cache entry;
//! * without a checksum (an op that `.allow(Policy::Unverified, ..)`ed it):
//!   downloaded into the staging directory under `u-<hash of the url>` and
//!   never cached; the op reads it from there. The staging directory is
//!   removed when prepare ends if it holds no such file, else by the
//!   executor when the host is done;
//! * for an `.unpack(..)` op, the file's magic says what it is (gz, xz,
//!   zst, plain tar, zip; the url's extension is not consulted) and the
//!   tool that unpacks it must exist, else the host is blocked here.
//!
//! Where the bytes land (T119): an item's filesystem is the one its first
//! consumer's target is on, in window order (`tp_fsroot` from the nearest
//! existing ancestor; "same filesystem" is root equality; a consumer with no
//! target path, a package from a file, counts as the stage's, and keeps the
//! item there). On the stage's filesystem the above is unchanged. On another
//! filesystem `F` with root `R` prepare (the root shell) downloads to
//! `R/.telepatch/tmp/<run>/fetch/<i>.part`, verifies it there, and renames a
//! checksummed file into `F`'s own cache `R/.telepatch/cache/sha256/<sum>`
//! (directories root-owned 0755, files 0644); an unverified one stays at
//! `R/.telepatch/tmp/<run>/fetch/u-<hash>`. `<stage>/<run>/f-<sum>` (or
//! `u-<hash>`) is a symlink to it, so the stage is still the run's one
//! index, and `R` is appended to `<stage>/<run>/fsroots`, so cleanup and the
//! stale sweep remove `R/.telepatch/tmp/<run>` (never the cache). Lookup
//! order: `F`'s cache, then the main cache (read in place, nothing copied
//! onto `F`), else a download onto `F`. A root that cannot be written (or
//! that a non-root shell may not make a `.telepatch` in) blocks the host,
//! `fetch <url>: cannot stage on <R>: <error>`, before any download: there
//! is no fallback to the stage's filesystem. Every new path goes through
//! `tp_sbx` first. The prepare script starts with
//! [`crate::ops::undo::guard_line`] and calls `tp_fsroot` and `tp_idx` from
//! the shell's helpers.
//!
//! `<cache>` is `/var/cache/telepatch/sha256` and `<stage>` is
//! `/var/tmp/telepatch`; `TP_FETCH_CACHE` and `TP_FETCH_STAGE` override them
//! (tests). `<run>` is `"$__tp_run"`, set by the executor in each shell
//! from a per-process id. Nothing is written under `/tmp`: every temporary
//! lives in the staging directory or beside its target.
//!
//! The op scripts ([`render`]) read only the cache entry (or the staged
//! file; on another filesystem the `f-<sum>` link, [`lookup`]): `.from` copies it to a candidate beside the target, with the
//! final mode and owner, and renames that over the target when the
//! target's `sha256sum` differs from the fetch's (no `cmp`: Rocky's minimal
//! image has no diffutils). `.unpack` unpacks into a sibling `<target>.tpXXXXXX`, sets mode
//! and owner, writes a marker `.telepatch-fetch` (`<sum> strip=<n>`) inside,
//! then swaps it in: the target is absent (one rename), or the old tree is
//! renamed aside first, the new one renamed in, the old removed (two
//! renames, and the old tree comes back if the second fails). A target that
//! already holds the marker for this sum and strip is settled: nothing
//! changes.

use std::borrow::Borrow;
use std::collections::BTreeSet;
use std::time::Duration;

use crate::engine::Window;
use crate::host::Host;
use crate::ops::file::FileSpec;
use crate::sh::quote;
use crate::trace::OpSpec;

/// A download: a url and, if the author gave one, its sha256. Not an op;
/// see the module doc. Builders return it owned, and a clone names the
/// same download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    pub url: String,
    /// Lower-cased when it is a valid sum; kept as written when it is not
    /// (the file op that takes it reports the error).
    pub sha256: Option<String>,
    /// Download on the controller and stream the bytes, instead of fetching
    /// on the host. For an air-gapped host.
    pub via_controller: bool,
    /// Download again up to this many more times when a download attempt
    /// fails (not on a checksum mismatch).
    pub retry: u32,
    /// Stop one download attempt after this long; `None`: the default,
    /// [`DEFAULT_TIMEOUT`].
    pub timeout: Option<Duration>,
}

/// How long one download attempt may take when the fetch sets no
/// `.timeout(..)`: ten minutes, like an `sh` or `git` op.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10 * 60);

impl Fetched {
    /// Try the download up to `n` more times when it fails, waiting 1, 2,
    /// 4 .. 30 seconds between tries. A checksum mismatch is not retried.
    /// Of several ops fetching the same url and sum, the largest count
    /// counts.
    #[track_caller]
    pub fn retry(mut self, n: u32) -> Fetched {
        self.retry = n;
        self
    }

    /// Stop one download attempt after `d`. Two bounds apply: the
    /// downloader's own flag on each attempt (`curl --max-time d`, `wget
    /// --timeout=d`, which bounds a stall, not the total), and, behind
    /// it, the controller's watchdog on the whole prepare probe, in case
    /// the flag fails ([`controller_deadline`]). Without it an attempt
    /// has [`DEFAULT_TIMEOUT`]. Of several ops fetching the same url and
    /// sum, the longest counts.
    #[track_caller]
    pub fn timeout(mut self, d: Duration) -> Fetched {
        self.timeout = Some(d);
        self
    }

    /// The checksum the download must have: 64 hex digits, either case.
    #[track_caller]
    pub fn sha256(mut self, sum: impl AsRef<str>) -> Fetched {
        let s = sum.as_ref();
        self.sha256 = Some(if valid_sum(s) {
            s.to_ascii_lowercase()
        } else {
            s.to_string()
        });
        self
    }

    /// The controller downloads this and streams it to the host. The host
    /// does not need a route to `url`, nor `curl` or `wget`.
    #[track_caller]
    pub fn via_controller(mut self) -> Fetched {
        self.via_controller = true;
        self
    }

    /// The trace error this fetch gives the op that takes it, if any.
    pub(crate) fn problem(&self) -> Option<String> {
        if self.url.trim().is_empty() {
            return Some("fetch has no url".to_string());
        }
        if self.timeout.is_some_and(|d| d.is_zero()) {
            return Some(format!(
                "fetch {}: timeout(..): the duration must be more than zero",
                self.url
            ));
        }
        match &self.sha256 {
            Some(s) if !valid_sum(s) => Some(format!(
                "fetch {}: sha256 must be 64 hex digits, got {s:?}",
                self.url
            )),
            _ => None,
        }
    }
}

pub(crate) fn valid_sum(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// What a file op takes from a [`Fetched`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSrc {
    pub fetched: Fetched,
    /// `None`: place the file (`.from`). `Some(n)`: unpack it, dropping `n`
    /// leading path components (`.unpack`, `.strip(n)`).
    pub unpack: Option<u32>,
}

impl FileSrc {
    /// The builder that set it, as an author writes it.
    pub fn builder(&self) -> &'static str {
        if self.unpack.is_some() {
            ".unpack()"
        } else {
            ".from()"
        }
    }
}

/// Accepts `&Fetched` and `Fetched` alike.
pub(crate) fn fetched_of(f: impl Borrow<Fetched>) -> Fetched {
    f.borrow().clone()
}

impl<V> Host<V> {
    /// A download of `url`, to be verified with `.sha256(..)` and placed or
    /// unpacked by a `file` op. It is a value: nothing is traced until a
    /// file op takes it.
    #[track_caller]
    pub fn fetch(&self, url: impl AsRef<str>) -> Fetched {
        Fetched {
            url: url.as_ref().to_string(),
            sha256: None,
            via_controller: false,
            retry: 0,
            timeout: None,
        }
    }
}

/// The staging directory name of this run: one per thread, so two `run`s
/// in one process (the test binary) never share a directory on one
/// machine. `<unix nanos in hex>-<pid>`. It never appears in a rendered
/// script: the executor sets `$__tp_run` to it in each shell it opens
/// ([`shell_init`]) and the scripts name that variable. A `run` drives
/// every host on the thread that called it, so those hosts share one id;
/// they do not share a filesystem unless they are the same machine.
pub(crate) fn run_id() -> String {
    thread_local! {
        static ID: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    }
    ID.with(|id| {
        id.borrow_mut()
            .get_or_insert_with(|| {
                let n = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                format!("{n:x}-{}", std::process::id())
            })
            .clone()
    })
}

/// The script the executor sends once to each shell it opens (root and
/// login): `__tp_run`, this process's staging subdirectory name, and, for a
/// run's root shell outside `--check`, `__tp_pb`, the owner directory (already
/// encoded, [`owner_dir`]) prepare marks under (T135). Without `__tp_pb` prepare touches nothing
/// in a cache.
pub(crate) fn shell_init(owner: Option<&str>) -> String {
    let mut s = format!("__tp_run={}\n", quote(&run_id()));
    if let Some(pb) = owner {
        s.push_str(&format!("__tp_pb={}\n", quote(pb)));
    }
    s
}

/// The `rm` that ends a host's run: the staging directory `$__tp_run`
/// names (`:?` so an unset variable removes nothing).
pub fn cleanup_script() -> String {
    "rm -rf -- \"${TP_FETCH_STAGE:-/var/tmp/telepatch}\"/\"${__tp_run:?}\"\n".to_string()
}

/// The cache uses in a prepare probe's output: each `fuse <sum> <dir>` line
/// `tp_one` printed for an item it found in, or put into, a cache
/// directory. `<sum>` is 64 lower-case hex digits and `<dir>` an absolute
/// path (the rest of the line); anything else is ignored.
pub(crate) fn parse_uses(out: &str) -> Vec<(String, String)> {
    out.lines()
        .filter_map(|l| l.strip_prefix("fuse "))
        .filter_map(|r| r.split_once(' '))
        .filter(|(s, d)| {
            s.len() == 64
                && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                && d.starts_with('/')
        })
        .map(|(s, d)| (s.to_string(), d.to_string()))
        .collect()
}

/// A playbook's owner directory name, `owners/<owner_dir(name)>/` beside
/// each cache (T135): `A-Z a-z 0-9 _ - .` stay, every other byte (`/`, `%`,
/// space, any byte >= 0x80) becomes `%XX` with upper-case hex, and a leading
/// `.` becomes `%2E`, so no owner directory is hidden, `.` or `..`. `%` is
/// always encoded, so two names never share a directory. The one encoder:
/// no shell script encodes or decodes.
pub fn owner_dir(name: &str) -> String {
    let mut s = String::with_capacity(name.len());
    for (i, b) in name.bytes().enumerate() {
        if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || (b == b'.' && i > 0) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

/// The inverse of [`owner_dir`]: the playbook name an owner directory
/// stands for, or `None` for a name `owner_dir` never makes.
pub fn owner_name(dir: &str) -> Option<String> {
    let b = dir.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = b.get(i + 1..i + 3)?;
            if !h.iter().all(|c| matches!(c, b'0'..=b'9' | b'A'..=b'F')) {
                return None;
            }
            out.push(u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    let name = String::from_utf8(out).ok()?;
    (!name.is_empty() && owner_dir(&name) == dir).then_some(name)
}

/// A playbook name owns what it caches (T135): not empty, no NUL, tab,
/// comma or newline (T122), and its [`owner_dir`] at most 200 bytes.
pub fn valid_playbook_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['\0', '\t', ',', '\n']) && owner_dir(name).len() <= 200
}

/// The end-of-run cache script (T135), one probe on the root shell, after
/// the last window and before the stage is removed, never under `--check`.
/// For every directory the registry `<main>/../caches` lists, plus the main
/// cache and every directory in `uses` (`(sum, dir)`, from [`parse_uses`]),
/// it removes T121's legacy record and lock files (touching the items of a
/// directory that held a record), then each item whose mtime is more than 90
/// days before now, with its owner markers. When `owner` is the playbook of
/// an entitled run it also removes that playbook's markers for sums outside
/// F (`fetched` and the sums in `uses`) and each item that leaves with no
/// marker. See "Cache lifetime" and "Cache ownership" in the note (#d-fetch).
///
/// It removes only a regular file (not a link) whose name is 64 lower-case
/// hex digits directly in a registered directory, and markers under that
/// directory's `../owners`. Every path goes through `tp_sbx` first. Output:
/// `cache <dir>: ..` lines and `telepatch: ..` warnings. The clock is
/// `TP_CACHE_NOW` (epoch seconds).
pub fn end_script(
    uses: &[(String, String)],
    fetched: &BTreeSet<String>,
    owner: Option<&str>,
) -> String {
    let mut s = String::from("(\numask 022\n");
    s.push_str(crate::sandbox::SBX);
    s.push_str("tp_u=(");
    for (sum, dir) in uses {
        s.push(' ');
        s.push_str(&quote(&format!("{sum} {dir}")));
    }
    s.push_str(" )\ntp_f=(");
    for sum in fetched {
        s.push(' ');
        s.push_str(&quote(sum));
    }
    s.push_str(" )\ntp_pb=");
    s.push_str(&quote(&owner.map(owner_dir).unwrap_or_default()));
    s.push_str("\ntp_pn=");
    s.push_str(&quote(owner.unwrap_or("")));
    s.push('\n');
    s.push_str(TP_HEX);
    s.push_str(TP_SHAPE);
    s.push_str(END);
    s.push_str("tp_end\n)\n");
    s
}

/// `tp_hex S`: is `S` a cache item's name, 64 lower-case hex digits. The one
/// rule of the end-of-run script and of `cache usage` and `cache purge`.
const TP_HEX: &str = r#"tp_hex() { [ ${#1} -eq 64 ] && case $1 in *[!0123456789abcdef]*) return 1;; esac; }
"#;

/// `tp_shape LINE MAIN`: may a registry line name a cache directory? It sets
/// `tp_l` to the line without a trailing slash and returns 0 when it is
/// absolute, has no `.` or `..` component and is `MAIN` or ends in
/// `/.telepatch/cache/sha256`; 1 for a line that is not a path at all
/// (skipped silently); 2 for any other path (skipped with a warning). The one
/// rule of the end-of-run script and of `cache usage` and `cache purge`.
const TP_SHAPE: &str = r#"tp_shape() {
  case $1 in /?*) ;; *) return 1;; esac
  case $1 in */..|*/../*|*/.|*/./*) return 1;; esac
  tp_l=${1%/}
  [ "$tp_l" = "$2" ] || case $tp_l in */.telepatch/cache/sha256) ;; *) return 2;; esac
  return 0
}
"#;

/// [`end_script`]'s body: `tp_cd1 DIR NOW` for one cache directory, `tp_end`
/// for all. `tp_rmi FILE` removes a regular non-link file and prints its
/// size; `tp_unmark OWNERS SUM` removes every `OWNERS/*/SUM`.
const END: &str = r#"declare -A fset
tp_rmi() {
  local sz
  [ -f "$1" ] && [ ! -L "$1" ] || return 1
  sz=$(stat -c %s -- "$1" 2>/dev/null || echo 0)
  rm -f -- "$1" 2>/dev/null && [ ! -e "$1" ] && echo "$sz"
}
tp_unmark() {
  local x
  [ -L "$1" ] && return 0
  for x in "$1"/*/"$2"; do
    [ -L "${x%/*}" ] && continue
    if [ -f "$x" ] || [ -L "$x" ]; then rm -f -- "$x" 2>/dev/null; fi
  done
}
tp_mb() { echo "$(($1/1048576)).$(( ($1%1048576)*10/1048576 ))"; }
tp_cd1() {
  local d=$1 now=$2 od=${1%/*}/owners f s x sz cnt=0 bytes=0 left
  tp_sbx "$d" "$od" || return 0
  [ -d "$d" ] && [ ! -L "$d" ] || return 0
  # Legacy removal: T121's usage record and its files never shipped. A
  # directory that held a record counts its items as accessed now, once.
  if [ -e "$d/.telepatch-usage" ] || [ -L "$d/.telepatch-usage" ]; then
    for f in "$d"/*; do tp_hex "${f##*/}" && [ -f "$f" ] && [ ! -L "$f" ] && touch -c -m -d "@$now" -- "$f" 2>/dev/null; done
  fi
  rm -f -- "$d/.telepatch-usage" "$d"/.telepatch-usage.* 2>/dev/null
  for f in "$d"/*; do
    s=${f##*/}
    tp_hex "$s" && [ -f "$f" ] && [ ! -L "$f" ] || continue
    x=$(stat -c %Y -- "$f" 2>/dev/null) || continue
    [ $((now - x)) -gt 7776000 ] || continue
    if sz=$(tp_rmi "$f"); then cnt=$((cnt+1)); bytes=$((bytes+sz)); tp_unmark "$od" "$s"; fi
  done
  if [ "$cnt" -gt 0 ]; then echo "cache $d: removed $cnt items ($(tp_mb "$bytes") MB) unused for 90 days"; fi
  [ -n "$tp_pb" ] && [ ! -L "$od" ] && [ ! -L "$od/$tp_pb" ] && [ -d "$od/$tp_pb" ] || return 0
  for x in "$od/$tp_pb"/*; do
    s=${x##*/}
    tp_hex "$s" && [ -z "${fset[$s]-}" ] || continue
    { [ -f "$x" ] || [ -L "$x" ]; } && rm -f -- "$x" 2>/dev/null || continue
    left=0
    for f in "$od"/*/"$s"; do if [ -e "$f" ] || [ -L "$f" ]; then left=1; break; fi; done
    if [ "$left" = 0 ] && sz=$(tp_rmi "$d/$s"); then
      echo "cache $d: removed ${s:0:12} ($(tp_mb "$sz") MB): no longer fetched by $tp_pn"
    fi
  done
  rmdir -- "$od/$tp_pb" 2>/dev/null
  return 0
}
tp_end() {
  local now cm cp rg u line seen nl=$'\n'
  local -a cand=()
  now=${TP_CACHE_NOW-}; case $now in ''|*[!0123456789]*) now=$(date +%s);; esac
  cm=${TP_FETCH_CACHE:-/var/cache/telepatch/sha256}; cm=${cm%/}; cp=${cm%/*}; rg=$cp/caches
  # Legacy removal: T121's registry lock and run lock, never shipped.
  if tp_sbx "$cp/.caches.lock" "$cp/.lock"; then rm -f -- "$cp/.caches.lock" "$cp/.lock" 2>/dev/null; fi
  for u in "${tp_f[@]}"; do fset[$u]=1; done
  for u in "${tp_u[@]}"; do fset[${u%% *}]=1; cand+=("${u#* }"); done
  if tp_sbx "$rg" && [ -f "$rg" ]; then
    while IFS= read -r line || [ -n "$line" ]; do cand+=("$line"); done < "$rg"
  fi
  cand+=("$cm")
  seen=$nl
  for line in "${cand[@]}"; do
    # Only the main cache or a per-filesystem cache is ever a cache
    # directory; any other line is skipped, never read or expired.
    tp_shape "$line" "$cm"
    case $? in
      0) line=$tp_l;;
      1) continue;;
      *) echo "telepatch: cache registry $rg: ignoring $tp_l: not a telepatch cache directory"; continue;;
    esac
    case $seen in *"$nl$line$nl"*) continue;; esac
    seen+=$line$nl
    tp_cd1 "$line" "$now"
  done
  return 0
}
"#;

/// What `cache usage` and `cache purge` share, after [`TP_HEX`] and
/// [`TP_SHAPE`]: `tp_regdirs MAIN REGISTRY` fills `dirs` with the registry's
/// lines and the main cache, each through `tp_shape` and without repeats (a
/// `W` line for a line that is no cache directory); `tp_du PATH...` prints
/// the `du -sb` total of the paths. Output lines are `TAG TAB fields`, read by
/// `report::CacheHost::parse`.
const CACHE_CMD: &str = r#"declare -a dirs
tp_regdirs() {
  local cm=$1 rg=$2 line seen nl=$'\n'
  local -a cand=()
  dirs=()
  if [ -f "$rg" ]; then
    while IFS= read -r line || [ -n "$line" ]; do cand+=("$line"); done < "$rg"
  fi
  if [ -d "$cm" ]; then cand+=("$cm"); fi
  seen=$nl
  for line in "${cand[@]}"; do
    tp_shape "$line" "$cm"
    case $? in
      0) line=$tp_l;;
      1) continue;;
      *) printf 'W\tignoring registry line %s: not a telepatch cache directory\n' "$tp_l"; continue;;
    esac
    case $seen in *"$nl$line$nl"*) continue;; esac
    seen+=$line$nl
    dirs+=("$line")
  done
}
tp_du() { local n; n=$(du -sbc -- "$@" 2>/dev/null | tail -n 1); n=${n%%[!0-9]*}; echo "${n:-0}"; }
cm=${TP_FETCH_CACHE:-/var/cache/telepatch/sha256}; cm=${cm%/}; cp=${cm%/*}; rg=$cp/caches
if m=$(tp_sbx "$rg" 2>&1); then tp_regdirs "$cm" "$rg"; else printf 'W\t%s\n' "$m"; tp_regdirs "$cm" /dev/null/none; fi
"#;

/// The `cache usage` script: one probe on the root shell, read-only. It
/// prints `D<TAB>dir<TAB>bytes` for each cache directory (the registry's and
/// the main cache, existing ones), the bytes being `du -sb` of the directory
/// and of the `owners` directory beside it; `K<TAB>bytes` for the working
/// directories telepatch owns (the stage, and `.telepatch/tmp` of every root a
/// registered per-filesystem cache or a `<stage>/*/fsroots` names, each
/// counted once); and `W<TAB>text` for a warning. Every path goes through
/// `tp_sbx` first.
pub fn cache_usage_script() -> String {
    let mut s = String::from("(\n");
    s.push_str(crate::sandbox::SBX);
    s.push_str(TP_HEX);
    s.push_str(TP_SHAPE);
    s.push_str(CACHE_CMD);
    s.push_str(
        r#"declare -A tp_wk
tp_wl=()
tp_addroot() {
  local w
  case $1 in /*) ;; *) return 0;; esac
  case $1 in */..|*/../*|*/.|*/./*) return 0;; esac
  w=${1%/}/.telepatch/tmp
  [ -z "${tp_wk[$w]-}" ] || return 0
  tp_wk[$w]=1; tp_wl+=("$w")
}
tp_rd() {
  local d=$1 od=${1%/*}/owners m
  m=$(tp_sbx "$d" "$od" 2>&1) || { printf 'W\t%s\n' "$m"; return 0; }
  case $d in */.telepatch/cache/sha256) tp_addroot "${d%/.telepatch/cache/sha256}";; esac
  [ -d "$d" ] || return 0
  if [ -d "$od" ]; then printf 'D\t%s\t%s\n' "$d" "$(tp_du "$d" "$od")"; else printf 'D\t%s\t%s\n' "$d" "$(tp_du "$d")"; fi
}
for d in "${dirs[@]}"; do tp_rd "$d"; done
st=${TP_FETCH_STAGE:-/var/tmp/telepatch}
if m=$(tp_sbx "$st" 2>&1); then
  tp_wl+=("$st")
  for f in "$st"/*/fsroots; do
    [ -f "$f" ] && [ ! -L "$f" ] || continue
    while IFS= read -r r || [ -n "$r" ]; do tp_addroot "$r"; done < "$f"
  done
else printf 'W\t%s\n' "$m"; fi
wb=0
for w in "${tp_wl[@]}"; do
  m=$(tp_sbx "$w" 2>&1) || { printf 'W\t%s\n' "$m"; continue; }
  [ -d "$w" ] || continue
  wb=$((wb + $(tp_du "$w")))
done
printf 'K\t%s\n' "$wb"
true
)
"#,
    );
    s
}

/// The `cache purge` script, one probe on the root shell, with no lock and
/// no record. `owner` is `--owner` (a playbook name; the owner directory is
/// [`owner_dir`] of it, passed as one quoted word); `yes` false only counts.
/// For each cache directory it prints `X<TAB>dir<TAB>items<TAB>bytes` (the
/// items removed, or with `yes` false the items it would remove), `R<TAB>path`
/// for a directory removed and `W<TAB>text` for a warning. Every path goes
/// through `tp_sbx` first.
///
/// Without an owner, in each directory it removes every regular (not a link)
/// 64-hex file, the whole `owners` directory beside it, and any legacy
/// record file (T121); then, for a per-filesystem cache only,
/// `rmdir`s `.telepatch/cache/sha256`, `.telepatch/cache` and `.telepatch`,
/// each only if empty, so `.telepatch/tmp` keeps `.telepatch`. It never
/// touches the working directories or the registry, and removes T121's two
/// lock files beside the main cache. With an owner it removes `owners/<dir>`
/// (the markers, links as links) and deletes each regular item that had that
/// marker and has no other `owners/*/<sum>` left.
pub fn cache_purge_script(owner: Option<&str>, yes: bool) -> String {
    let mut s = String::from("(\n");
    s.push_str(crate::sandbox::SBX);
    s.push_str(TP_HEX);
    s.push_str(TP_SHAPE);
    s.push_str(CACHE_CMD);
    s.push_str("tp_o=");
    s.push_str(&quote(&owner.map(owner_dir).unwrap_or_default()));
    s.push_str(if yes { "\ntp_y=1\n" } else { "\ntp_y=0\n" });
    s.push_str(
        r#"tp_rm() {
  local sz
  [ -f "$1" ] && [ ! -L "$1" ] || return 0
  sz=$(stat -c %s -- "$1" 2>/dev/null || echo 0)
  if [ "$tp_y" = 1 ]; then rm -f -- "$1" 2>/dev/null && [ ! -e "$1" ] || return 0; fi
  cnt=$((cnt+1)); bytes=$((bytes+sz))
}
tp_pd() {
  local d=$1 od=${1%/*}/owners f s x m c t cnt=0 bytes=0 left
  m=$(tp_sbx "$d" "$od" 2>&1) || { printf 'W\t%s\n' "$m"; return 0; }
  [ -d "$d" ] && [ ! -L "$d" ] || return 0
  if [ -n "$tp_o" ]; then
    if [ -d "$od/$tp_o" ] && [ ! -L "$od" ] && [ ! -L "$od/$tp_o" ]; then
      for x in "$od/$tp_o"/*; do
        s=${x##*/}
        tp_hex "$s" && { [ -f "$x" ] || [ -L "$x" ]; } || continue
        left=0
        for f in "$od"/*/"$s"; do
          [ "$f" != "$x" ] && [ ! -L "${f%/*}" ] && { [ -e "$f" ] || [ -L "$f" ]; } && { left=1; break; }
        done
        if [ "$tp_y" = 1 ]; then rm -f -- "$x" 2>/dev/null; fi
        if [ "$left" = 0 ]; then tp_rm "$d/$s"; fi
      done
      if [ "$tp_y" = 1 ]; then rmdir -- "$od/$tp_o" 2>/dev/null; fi
    fi
    printf 'X\t%s\t%s\t%s\n' "$d" "$cnt" "$bytes"
    return 0
  fi
  for f in "$d"/*; do
    tp_hex "${f##*/}" && tp_rm "$f"
  done
  if [ "$tp_y" = 1 ]; then
    rm -f -- "$d/.telepatch-usage" "$d"/.telepatch-usage.* 2>/dev/null
    rm -rf --one-file-system -- "$od" 2>/dev/null
  fi
  printf 'X\t%s\t%s\t%s\n' "$d" "$cnt" "$bytes"
  if [ "$tp_y" = 1 ] && [ "$d" != "$cm" ]; then
    c=${d%/sha256}; t=${c%/cache}
    m=$(tp_sbx "$c" "$t" 2>&1) || { printf 'W\t%s\n' "$m"; return 0; }
    rmdir -- "$d" 2>/dev/null && printf 'R\t%s\n' "$d"
    rmdir -- "$c" 2>/dev/null && printf 'R\t%s\n' "$c"
    rmdir -- "$t" 2>/dev/null && printf 'R\t%s\n' "$t"
  fi
  return 0
}
for d in "${dirs[@]}"; do tp_pd "$d"; done
if [ "$tp_y" = 1 ] && [ -z "$tp_o" ] && tp_sbx "$cp/.caches.lock" "$cp/.lock"; then rm -f -- "$cp/.caches.lock" "$cp/.lock" 2>/dev/null; fi
true
)
"#,
    );
    s
}

/// `tp_run`, this run's staging directory, from the shell's `$__tp_run`.
pub(crate) const TP_RUN: &str = "tp_run=\"$tp_sd\"/\"$__tp_run\"\n";

pub(crate) const LOCATE: &str = concat!(
    crate::sandbox::sbx_def!(),
    "tp_cd=\"${TP_FETCH_CACHE:-/var/cache/telepatch/sha256}\"\ntp_sd=\"${TP_FETCH_STAGE:-/var/tmp/telepatch}\"\n",
    "tp_sbx \"$tp_cd\" \"$tp_sd\" || { tp_cd=/dev/null/refused; tp_sd=/dev/null/refused; }\n"
);

/// `tp_magic <file>` prints gz, xz, zst, zip, tar or unknown.
const MAGIC: &str = r#"tp_magic() {
  local h
  h=$(head -c 6 "$1" | od -An -tx1 | tr -d ' \n')
  case "$h" in
    1f8b*) echo gz;;
    fd377a585a00) echo xz;;
    28b52ffd*) echo zst;;
    504b0304*|504b0506*) echo zip;;
    *) if [ "$(dd if="$1" bs=1 skip=257 count=5 2>/dev/null)" = ustar ]; then echo tar; else echo unknown; fi;;
  esac
}
"#;

/// One download a window needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub url: String,
    pub sum: Option<String>,
    /// Some op unpacks it: the archive format and its tool are checked.
    pub unpack: bool,
    /// The most any op's fetch of it asks for ([`Fetched::retry`]).
    pub retry: u32,
    /// The longest any op's fetch of it asks for ([`Fetched::timeout`]);
    /// `None` when none set one.
    pub timeout: Option<Duration>,
    /// The shell word of each consumer's target path, in window order
    /// ([`crate::ops::undo::capture_word`]); `None` for a consumer with no
    /// path the prepare shell can name (a relative one).
    pub targets: Vec<Option<String>>,
}

impl Item {
    /// The word of the path whose filesystem the download lands on: the
    /// first consumer's target, in window order. `None`: the stage's own
    /// filesystem (the first consumer has no path: a package op, which
    /// installs across `/`, or a relative target).
    pub fn first_target(&self) -> Option<&str> {
        self.targets.first().and_then(|t| t.as_deref())
    }

    /// The limit of one attempt: the fetch's own, else the default.
    pub fn attempt_timeout(&self) -> Duration {
        self.timeout.unwrap_or(DEFAULT_TIMEOUT)
    }
}

/// A download this op needs: `(url, sum, unpack)` and its fetch's
/// `(retry, timeout)`. File ops and a package installed from a fetched
/// file.
type Want<'a> = (&'a str, &'a Option<String>, bool, u32, Option<Duration>);

fn op_fetch(spec: &OpSpec) -> Option<Want<'_>> {
    fn of(f: &Fetched, unpack: bool) -> Want<'_> {
        (f.url.as_str(), &f.sha256, unpack, f.retry, f.timeout)
    }
    match spec {
        OpSpec::File(f) => f
            .src
            .as_ref()
            .filter(|s| !s.fetched.via_controller)
            .map(|s| of(&s.fetched, s.unpack.is_some())),
        OpSpec::Pkg(p) => p.file.as_ref().map(|f| of(f, false)),
        _ => None,
    }
}

/// The distinct `(url, sum)` the window's file ops and package-file ops
/// fetch, first-seen order, each with its consumers' targets.
pub fn items(window: &Window) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::new();
    for op in &window.ops {
        let Some((url, sum, unpack, retry, timeout)) = op_fetch(&op.node.spec) else {
            continue;
        };
        let user = match &op.node.run_as.principal {
            crate::trace::Principal::User(u) => u.as_str(),
            crate::trace::Principal::Root => "root",
        };
        let target = crate::ops::undo::capture_word(&op.node.spec, user);
        match out
            .iter_mut()
            .find(|i| i.url == url && i.sum.as_ref() == sum.as_ref())
        {
            Some(i) => {
                i.unpack |= unpack;
                i.retry = i.retry.max(retry);
                i.timeout = i.timeout.max(timeout);
                i.targets.push(target);
            }
            None => out.push(Item {
                url: url.to_string(),
                sum: sum.clone(),
                unpack,
                retry,
                timeout,
                targets: vec![target],
            }),
        }
    }
    out
}

/// The most extra time the controller allows an attempt beyond its own
/// limit, before the downloader's flag is judged to have failed.
const ATTEMPT_GRACE: Duration = Duration::from_secs(5);

/// The controller's slack on top of a fetch probe's attempts.
const PROBE_SLACK: Duration = Duration::from_secs(30);

/// How long the controller lets the whole fetch probe of `items` run
/// before it stops it (the outer bound; the downloader's own flag is the
/// inner one). The probe runs its items one after another, so the bound
/// adds them up: for each item, `(retry + 1)` attempts of `timeout + 5 s`
/// and the waits between them (1, 2, 4 .. 30 s,
/// [`crate::executor::retry_wait`]), then 30 s of slack.
pub fn controller_deadline(items: &[Item]) -> Duration {
    let mut total = PROBE_SLACK;
    for it in items {
        total += (it.attempt_timeout() + ATTEMPT_GRACE) * (it.retry + 1);
        total += (1..=it.retry)
            .map(crate::executor::retry_wait)
            .sum::<Duration>();
    }
    total
}

/// The reason the host is blocked when the controller stopped the fetch
/// probe of `items` (`why`), which printed `out` first: every item with no
/// answer line in `out`, named with its limit.
pub fn stopped(
    items: &[Item],
    out: &str,
    why: &crate::executor::Stopped,
    limit: Duration,
) -> String {
    use crate::executor::Stopped;
    let missing: Vec<&Item> = items
        .iter()
        .enumerate()
        .filter(|(i, _)| {
            let pre = format!("fetch {i} ");
            !out.lines().any(|l| l.starts_with(pre.as_str()))
        })
        .map(|(_, it)| it)
        .collect();
    let what = |it: &Item| -> String {
        let d = crate::trace::fmt_duration(it.attempt_timeout());
        match why {
            Stopped::TimedOut { .. } | Stopped::EndedWithoutResult => {
                format!(
                    "fetch {}: download timed out after {d} (stopped by the controller)",
                    it.url
                )
            }
            Stopped::NoSecondShell(_) => format!(
                "fetch {}: download timed out after {d}; no second shell to stop it",
                it.url
            ),
            Stopped::NeverStarted => format!(
                "fetch {}: the download never started within {}; shell dropped",
                it.url,
                crate::trace::fmt_duration(limit)
            ),
        }
    };
    missing
        .iter()
        .map(|it| what(it))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Added to the prepare script: `curl` and `wget` become functions that
/// run the real tool with its own time limit (`curl --max-time`, `wget
/// --timeout=`, `tp_lt` seconds, per attempt) and again after a failure
/// (`tp_lr` more times, waiting 1, 2, 4 .. 30 seconds). No coreutils
/// `timeout`. A curl rc 28 or a wget that says it timed out ends as rc
/// 124 (`tp_lim`'s own word for it, which neither tool uses), so the
/// answer line can say the download timed out. `command` skips the
/// function; `tp_one` is the same text as without limits and calls them
/// by name.
const LIMITS: &str = r#"tp_lim() {
  local n=0 d=1 rc t=$1 o; shift
  while :; do
    if [ "$t" = curl ]; then
      command curl --max-time "$tp_lt" "$@"; rc=$?
      if [ "$rc" -eq 28 ]; then rc=124; fi
    else
      o=$(command wget --timeout="$tp_lt" "$@" 2>&1); rc=$?
      printf '%s
' "$o"
      if [ "$rc" -ne 0 ] && printf %s "$o" | grep -qi 'timed out'; then rc=124; fi
    fi
    if [ "$rc" -eq 0 ] || [ "$n" -ge "$tp_lr" ]; then return "$rc"; fi
    n=$((n+1)); sleep "$d"; d=$((d*2)); [ "$d" -le 30 ] || d=30
  done
}
curl() { tp_lim curl "$@"; }
wget() { tp_lim wget "$@"; }
"#;

/// `timeout`'s duration argument for the downloader flags: seconds, with
/// milliseconds when it has some, at least a millisecond.
fn secs_arg(d: Duration) -> String {
    let d = d.max(Duration::from_millis(1));
    let (s, ms) = (d.as_secs(), d.subsec_millis());
    if ms == 0 {
        s.to_string()
    } else {
        format!("{s}.{ms:03}").trim_end_matches('0').to_string()
    }
}

/// The prepare script for `items`, one `fetch <i> <word> ..` line each:
/// `cached`, `fetched`, `staged`, `mismatch <got>`, `fail <tool> <rc>
/// <message>`, `notool`, `badarchive`, `needtool <name>`, `nostage <root>:
/// <error>`. It starts with [`crate::ops::undo::guard_line`]: it calls
/// `tp_fsroot` and `tp_idx` from the shell's helpers.
pub fn prepare_script(items: &[Item]) -> String {
    let mut s = prelude();
    s.push_str(LIMITS);
    for (i, it) in items.iter().enumerate() {
        s.push_str(&format!(
            "tp_lt={} tp_lr={}\ntp_one {i} {} {} {}{}\n",
            secs_arg(it.attempt_timeout()),
            it.retry,
            quote(&it.url),
            quote(it.sum.as_deref().unwrap_or("")),
            if it.unpack { 1 } else { 0 },
            it.first_target()
                .map(|t| format!(" {t}"))
                .unwrap_or_default()
        ));
    }
    s.push_str("if [ \"$tp_keep\" = 0 ]; then rm -rf -- \"$tp_run\"; fi\n");
    s
}

/// The shell [`prepare_script`] runs its items with, also used by the
/// `binary_release` pkg source: `tp_cd`, `tp_sd`, `tp_run` (this run's
/// staging directory), `tp_dl` (`curl`, `wget` or empty), `tp_keep`, and
/// `tp_one <i> <url> <sum> <unpack 0|1>`, which prints one `fetch <i> ..`
/// line: from the cache, or downloaded into the staging directory, verified
/// and moved into the cache (with no sum: staged under `u-<hash of the
/// url>`, and `tp_keep=1`). A cache hit or a download into a cache is an
/// access (T135, `tp_acc`): a `fuse <sum> <dir>` line and, when the shell has
/// `__tp_pb` (not under `--check`), the item's mtime set to now, the marker
/// `<dir>/../owners/<owner_dir(playbook)>/<sum>` created and `<dir>` appended to the
/// registry, all without a lock.
pub(crate) fn prelude() -> String {
    let mut s = crate::ops::undo::guard_line();
    s.push_str(LOCATE);
    s.push_str(&format!(
        "{TP_RUN}tp_dl=\nif command -v curl >/dev/null 2>&1; then tp_dl=curl; elif command -v wget >/dev/null 2>&1; then tp_dl=wget; fi\ntp_keep=0\n",
    ));
    s.push_str(MAGIC);
    s.push_str(
        r#"tp_check() {
  local k t
  [ "$3" = 1 ] || return 0
  k=$(tp_magic "$2")
  case "$k" in
    unknown) echo "fetch $1 badarchive"; return 1;;
    zip)
      if ! command -v unzip >/dev/null 2>&1 && ! command -v bsdtar >/dev/null 2>&1; then
        echo "fetch $1 needtool unzip or bsdtar"; return 1
      fi;;
    *)
      command -v tar >/dev/null 2>&1 || { echo "fetch $1 needtool tar"; return 1; }
      case "$k" in gz) t=gzip;; xz) t=xz;; zst) t=zstd;; *) t=;; esac
      if [ -n "$t" ] && ! command -v "$t" >/dev/null 2>&1; then echo "fetch $1 needtool $t"; return 1; fi;;
  esac
  return 0
}
tp_link() { (umask 022; mkdir -p -- "$tp_run") && ln -sfn -- "$1" "$tp_run/$2" && tp_keep=1; }
tp_stg() {
  local i=$1 r=$2 b e x
  b=$r/.telepatch; tp_t=$b/tmp/$__tp_run; tp_c=$b/cache/sha256
  tp_sbx "$b" || { b=/dev/null/refused; tp_t=$b; tp_c=$b; }
  tp_sbx "$tp_c" || tp_c=/dev/null/refused
  if [ ! -d "$tp_t/fetch" ]; then
    case ":${TP_FS_ROOTS-}:" in *":$r:"*) ;; *) [ "$(id -u)" = 0 ] || { echo "fetch $i nostage $r: not root, so no .telepatch is made there"; return 1; };; esac
  fi
  e=$(umask 022; mkdir -p -- "$tp_run" 2>&1) || { echo "fetch $i nostage $r: cannot create $tp_run: ${e##*: }"; return 1; }
  for x in "$b" "$b/tmp" "$tp_t" "$tp_t/fetch"; do
    [ -d "$x" ] || e=$(umask 022; mkdir -m 711 -- "$x" 2>&1) || { echo "fetch $i nostage $r: ${e##*: }"; return 1; }
  done
  e=$(tp_idx "$r" "$tp_run/fsroots" 2>&1) || { echo "fetch $i nostage $r: ${e##*: }"; return 1; }
  tp_keep=1
}
tp_reg() {
  local d=${1%/} rg=${tp_cd%/*}/caches
  tp_sbx "$rg" "$d" || return 0
  grep -qxF -- "$d" "$rg" 2>/dev/null || (umask 022; mkdir -p -- "${rg%/*}" && printf '%s\n' "$d" >> "$rg") 2>/dev/null
  return 0
}
tp_acc() {
  local s=$1 d=${2%/} o
  printf 'fuse %s %s\n' "$s" "$d"
  [ -n "${__tp_pb-}" ] || return 0
  o=${d%/*}/owners/$__tp_pb
  tp_sbx "$d/$s" "$o/$s" || return 0
  case ${TP_CACHE_NOW-} in
    ''|*[!0123456789]*) touch -c -m -- "$d/$s";;
    *) touch -c -m -d "@$TP_CACHE_NOW" -- "$d/$s";;
  esac 2>/dev/null
  [ -e "$o/$s" ] || (umask 022; mkdir -p -- "$o" && : > "$o/$s") 2>/dev/null
  tp_reg "$d"
}
tp_one() {
  local i=$1 u=$2 s=$3 up=$4 t=${5-} f p rc err got r= h
  if [ -n "$t" ]; then
    r=$(tp_fsroot "$t")
    if [ -z "$r" ] || [ "$r" = "$(tp_fsroot "$tp_sd")" ]; then r=; fi
  fi
  if [ -n "$s" ]; then
    if [ -n "$r" ]; then
      f="$r/.telepatch/cache/sha256/$s"
      if [ -f "$f" ] && [ "$(sha256sum "$f" 2>/dev/null | cut -d' ' -f1)" = "$s" ]; then
        tp_check "$i" "$f" "$up" || return 0
        tp_link "$f" "f-$s" || { echo "fetch $i fail ${tp_dl:-sh} 1 cannot create $tp_run"; return 0; }
        echo "fetch $i cached"; tp_acc "$s" "$r/.telepatch/cache/sha256"; return 0
      fi
    fi
    f="$tp_cd/$s"
    if [ -f "$f" ] && [ "$(sha256sum "$f" 2>/dev/null | cut -d' ' -f1)" = "$s" ]; then
      tp_check "$i" "$f" "$up" || return 0
      echo "fetch $i cached"; tp_acc "$s" "$tp_cd"; return 0
    fi
  fi
  if [ -z "$tp_dl" ]; then echo "fetch $i notool"; return 0; fi
  if [ -n "$r" ]; then
    tp_stg "$i" "$r" || return 0
    p="$tp_t/fetch/$i.part"
  else
    (umask 022; mkdir -p -- "$tp_run") || { echo "fetch $i fail $tp_dl 1 cannot create $tp_run"; return 0; }
    p="$tp_run/$i.part"
  fi
  if [ "$tp_dl" = curl ]; then
    err=$(curl -fsSL -o "$p" -- "$u" 2>&1); rc=$?
  else
    err=$(wget -q -O "$p" -- "$u" 2>&1); rc=$?
  fi
  if [ "$rc" -ne 0 ]; then
    rm -f -- "$p"
    echo "fetch $i fail $tp_dl $rc $(printf %s "$err" | head -n 1 | tr -d '\r')"
    return 0
  fi
  got=$(sha256sum "$p" | cut -d' ' -f1)
  if [ -n "$s" ]; then
    if [ "$got" != "$s" ]; then rm -f -- "$p"; echo "fetch $i mismatch $got"; return 0; fi
    tp_check "$i" "$p" "$up" || { rm -f -- "$p"; return 0; }
    if [ -n "$r" ]; then
      if (umask 022; mkdir -p -- "$tp_c") && chmod 644 "$p" && mv -f -- "$p" "$tp_c/$s" && tp_link "$tp_c/$s" "f-$s"; then
        echo "fetch $i fetched"; tp_acc "$s" "$tp_c"
      else
        rm -f -- "$p"
        echo "fetch $i fail $tp_dl 1 cannot write the cache $tp_c"
      fi
    elif (umask 022; mkdir -p -- "$tp_cd") && mv -f -- "$p" "$tp_cd/.$s.part" && chmod 644 "$tp_cd/.$s.part" && mv -f -- "$tp_cd/.$s.part" "$tp_cd/$s"; then
      echo "fetch $i fetched"; tp_acc "$s" "$tp_cd"
    else
      rm -f -- "$p" "$tp_cd/.$s.part"
      echo "fetch $i fail $tp_dl 1 cannot write the cache $tp_cd"
    fi
  else
    tp_check "$i" "$p" "$up" || { rm -f -- "$p"; return 0; }
    chmod 644 "$p"
    h=u-$(printf %s "$u" | sha256sum | cut -c1-32)
    if [ -n "$r" ]; then
      mv -f -- "$p" "$tp_t/fetch/$h" && tp_link "$tp_t/fetch/$h" "$h" || { rm -f -- "$p"; echo "fetch $i fail $tp_dl 1 cannot stage on $r"; return 0; }
    else
      mv -f -- "$p" "$tp_run/$h" && tp_keep=1
    fi
    echo "fetch $i staged"
  fi
}
"#,
    );
    s
}

/// What prepare found for one item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    Cached,
    Fetched,
    /// Downloaded without a checksum, staged for the op.
    Staged,
    /// Not in the cache, and neither `curl` nor `wget` is on the host: the
    /// host is blocked, with this reason (no window suffix yet), unless an
    /// earlier op of the window installs one ([`crate::engine::Split`]).
    NoDownloader(String),
    /// The host is blocked, with this reason (no window suffix yet).
    Blocked(String),
}

/// The words a missing downloader blocks with: `fetch <url>: neither curl
/// nor wget is on the host`, `pkg[binary_release] go: neither ..`.
pub const NO_DOWNLOADER: &str = "neither curl nor wget is on the host";

/// Does `op` (a window op) fetch `it`?
pub fn uses(op: &crate::trace::OpNode, it: &Item) -> bool {
    op_fetch(&op.spec).is_some_and(|(url, sum, ..)| url == it.url && sum == &it.sum)
}

/// Parse [`prepare_script`]'s output, one [`Found`] per item. An item with
/// no line (the script died) is blocked.
pub fn parse(items: &[Item], out: &str) -> Vec<Found> {
    items
        .iter()
        .enumerate()
        .map(|(i, it)| {
            let pre = format!("fetch {i} ");
            let Some(rest) = out.lines().rev().find_map(|l| l.strip_prefix(pre.as_str())) else {
                return Found::Blocked(format!("fetch {}: the host gave no answer", it.url));
            };
            let (word, tail) = rest.split_once(' ').unwrap_or((rest, ""));
            match word {
                "cached" => Found::Cached,
                "fetched" => Found::Fetched,
                "staged" => Found::Staged,
                "mismatch" => Found::Blocked(format!(
                    "fetch {}: sha256 mismatch: want {} got {tail}",
                    it.url,
                    it.sum.as_deref().unwrap_or("")
                )),
                "nostage" => Found::Blocked(format!("fetch {}: cannot stage on {tail}", it.url)),
                "notool" => Found::NoDownloader(format!("fetch {}: {NO_DOWNLOADER}", it.url)),
                "badarchive" => Found::Blocked(format!(
                    "unpack {}: not a tar (gz, xz, zst) or zip archive",
                    it.url
                )),
                "needtool" => {
                    Found::Blocked(format!("unpack {}: {tail} not found on the host", it.url))
                }
                "fail" => {
                    let mut p = tail.splitn(3, ' ');
                    let (tool, rc, msg) = (
                        p.next().unwrap_or(""),
                        p.next().unwrap_or(""),
                        p.next().unwrap_or(""),
                    );
                    if rc == "124" {
                        return Found::Blocked(format!(
                            "fetch {}: download timed out after {}",
                            it.url,
                            crate::trace::fmt_duration(it.attempt_timeout())
                        ));
                    }
                    let msg = if msg.is_empty() {
                        String::new()
                    } else {
                        format!(": {msg}")
                    };
                    Found::Blocked(format!(
                        "fetch {}: download failed ({tool} rc {rc}){msg}",
                        it.url
                    ))
                }
                _ => Found::Blocked(format!("fetch {}: unreadable answer {rest:?}", it.url)),
            }
        })
        .collect()
}

/// An op's `tp_f` for a checksummed fetch: the main cache entry, or, when
/// prepare staged it on another filesystem (a cache hit or a download
/// there), the stage's link `f-<sum>` to it. One line.
pub(crate) fn lookup(url: &str) -> String {
    let say = quote(&format!("telepatch: fetch {url}: staged on "));
    format!(
        "tp_f=\"$tp_cd/$tp_s\"; if [ -L \"$tp_sd/$__tp_run/f-$tp_s\" ]; then tp_f=\"$tp_sd/$__tp_run/f-$tp_s\"; tp_r=$(readlink \"$tp_f\"); tp_r=${{tp_r%/.telepatch/*}}; tp_q=$(tp_fsroot \"$tp_p\"); [ \"$tp_q\" = \"$tp_r\" ] || echo {say}\"$tp_r, target now on $tp_q\" >&2; fi\n"
    )
}

/// A package op's `tp_f` for a checksummed fetch: [`lookup`] without the
/// target comparison (a package file has no one target path; the manager
/// installs it across `/`). The stage's link `f-<sum>` when prepare staged
/// it on another filesystem, else the main cache entry. One line.
pub(crate) const LOOKUP_PKG: &str = "tp_f=\"$tp_cd/$tp_s\"; if [ -L \"$tp_sd/$__tp_run/f-$tp_s\" ]; then tp_f=\"$tp_sd/$__tp_run/f-$tp_s\"; fi\n";

/// The line an op that reads `tp_f`, the file of fetch `f`, runs before it
/// creates anything (T135). With no locks, a `cache purge` may have removed
/// a cached file since prepare: the op fails, rc 1, saying so. An
/// unverified file is the run's own staged file, never purged. `after_rc`:
/// only when `rc` is still 0 (a package op, which may have failed before).
pub(crate) fn missing(f: &Fetched, after_rc: bool) -> String {
    let say = match &f.sha256 {
        Some(s) => quote(&format!(
            "telepatch: the cached download {} for {} is gone (a cache purge during the run?); run again to download it",
            s.get(..12).unwrap_or(s),
            f.url
        )),
        None => {
            "\"telepatch: fetched file $tp_f is missing (prepare did not fetch it)\"".to_string()
        }
    };
    let rc = if after_rc {
        "[ \"$rc\" -eq 0 ] && "
    } else {
        ""
    };
    format!("if {rc}[ ! -f \"$tp_f\" ]; then echo {say} >&2; rc=1; fi\n")
}

/// The file op's body for a fetched source: appended after `tp_p=<path>`
/// ([`FileSpec::render`]); the mode and owner the spec names are on the
/// candidate before it replaces the target, and the usual mode and owner
/// checks follow.
pub fn render(f: &FileSpec, src: &FileSrc, o: &mut String) {
    o.push_str(LOCATE);
    match &src.fetched.sha256 {
        Some(s) => o.push_str(&format!("tp_s={}\n{}", quote(s), lookup(&src.fetched.url))),
        None => o.push_str(&format!(
            "tp_f=\"$tp_sd\"/\"$__tp_run\"/\"u-$(printf %s {} | sha256sum | cut -c1-32)\"\n",
            quote(&src.fetched.url)
        )),
    }
    o.push_str(&missing(&src.fetched, false));
    if src.fetched.sha256.is_none() {
        o.push_str("if [ \"$rc\" -eq 0 ]; then tp_s=$(sha256sum \"$tp_f\" | cut -d' ' -f1); fi\n");
    }
    match src.unpack {
        None => place(f, o, true),
        Some(strip) => unpack(f, strip, o, true),
    }
}

/// `.from`: the candidate beside `tp_p`, renamed over it when its sum is
/// not `tp_s` (one rename). Needs `rc`, `changed`, `tp_p`, `tp_f`, `tp_s`.
pub(crate) fn render_place(f: &FileSpec, o: &mut String) {
    place(f, o, false)
}

/// `capture`: the op is a file op, whose script has the undo helpers; the
/// old file is copied just before the rename over it. (`pkg[binary_release]`
/// is not, and renders without.)
fn place(f: &FileSpec, o: &mut String, capture: bool) {
    o.push_str(
        "if [ \"$rc\" -eq 0 ] && [ -d \"$tp_p\" ]; then echo \"telepatch: $tp_p is a directory\" >&2; rc=1; fi\n",
    );
    o.push_str(
        "if [ \"$rc\" -eq 0 ] && ! { [ -f \"$tp_p\" ] && [ \"$(sha256sum < \"$tp_p\" | cut -d' ' -f1)\" = \"$tp_s\" ]; }; then\n  tp_t=$(mktemp \"$tp_p.tpXXXXXX\") || rc=$?\n  if [ \"$rc\" -eq 0 ]; then\n    cp -- \"$tp_f\" \"$tp_t\" || rc=$?\n",
    );
    match f.mode {
        Some(m) => o.push_str(&format!(
            "    if [ \"$rc\" -eq 0 ]; then chmod {m:o} \"$tp_t\" || rc=$?; fi\n"
        )),
        None => o.push_str(
            "    if [ \"$rc\" -eq 0 ]; then\n      if [ -e \"$tp_p\" ]; then chmod --reference=\"$tp_p\" \"$tp_t\" || rc=$?; else chmod 644 \"$tp_t\" || rc=$?; fi\n    fi\n",
        ),
    }
    match &f.owner {
        Some(ow) => o.push_str(&format!(
            "    if [ \"$rc\" -eq 0 ]; then chown {} \"$tp_t\" || rc=$?; fi\n",
            quote(ow)
        )),
        None => o.push_str(
            "    if [ \"$rc\" -eq 0 ] && [ -e \"$tp_p\" ]; then chown --reference=\"$tp_p\" \"$tp_t\" 2>/dev/null; fi\n",
        ),
    }
    let save = if capture { "tp_save \"$tp_p\" && " } else { "" };
    o.push_str(&format!(
        "    if [ \"$rc\" -eq 0 ]; then {save}mv -f \"$tp_t\" \"$tp_p\"; rc=$?; fi\n    if [ \"$rc\" -eq 0 ]; then changed=1; else rm -f \"$tp_t\"; fi\n  fi\nfi\n"
    ));
}

/// `.unpack`: a sibling tree swapped in (see the module doc for the
/// renames). Needs `rc`, `changed`, `tp_p`, `tp_f`, `tp_s`.
pub(crate) fn render_unpack(f: &FileSpec, strip: u32, o: &mut String) {
    unpack(f, strip, o, false)
}

/// `capture`: as for [`place`]. The old tree is then moved into the backup
/// dir (mount points refused first), not removed.
fn unpack(f: &FileSpec, strip: u32, o: &mut String, capture: bool) {
    let strip_flag = if strip > 0 {
        format!(" --strip-components={strip}")
    } else {
        String::new()
    };
    o.push_str(MAGIC);
    // A mount point (or a tree holding one) is refused before the scratch
    // tree is built.
    let gate = if capture {
        "    if [ -d \"$tp_p\" ]; then tp_gate \"$tp_p\" check || rc=1; fi\n    if [ \"$rc\" -eq 0 ]; then tp_n=$(mktemp -d \"$tp_p.tpXXXXXX\") || rc=$?; fi\n"
    } else {
        "    tp_n=$(mktemp -d \"$tp_p.tpXXXXXX\") || rc=$?\n"
    };
    o.push_str(&format!(
        "tp_n=\nif [ \"$rc\" -eq 0 ]; then\n  tp_k=$(tp_magic \"$tp_f\")\n  tp_mk=\"$tp_s strip={strip}\"\n  if [ -e \"$tp_p\" ] && [ ! -d \"$tp_p\" ]; then\n    echo \"telepatch: $tp_p exists and is not a directory\" >&2; rc=1\n  elif [ -d \"$tp_p\" ] && [ \"$(cat \"$tp_p/.telepatch-fetch\" 2>/dev/null)\" = \"$tp_mk\" ]; then\n    :\n  else\n{gate}    if [ \"$rc\" -eq 0 ]; then\n      case \"$tp_k\" in\n        zip)\n          tp_z=\"$tp_n.raw\"\n          mkdir \"$tp_z\" || rc=$?\n          if [ \"$rc\" -eq 0 ]; then\n            if command -v unzip >/dev/null 2>&1; then unzip -q -o \"$tp_f\" -d \"$tp_z\" || rc=$?; else bsdtar -xf \"$tp_f\" -C \"$tp_z\" || rc=$?; fi\n          fi\n"
    ));
    if strip == 0 {
        o.push_str(
            "          if [ \"$rc\" -eq 0 ]; then cp -a \"$tp_z\"/. \"$tp_n\"/ || rc=$?; fi\n",
        );
    } else {
        o.push_str(&format!(
            "          if [ \"$rc\" -eq 0 ]; then\n            find \"$tp_z\" -mindepth {strip} -maxdepth {strip} -type d -print0 | while IFS= read -r -d '' tp_d; do cp -a \"$tp_d\"/. \"$tp_n\"/ || exit 1; done\n            rc=${{PIPESTATUS[1]}}\n          fi\n"
        ));
    }
    o.push_str(&format!(
        "          rm -rf \"$tp_z\";;\n        gz|xz|zst|tar) tar -xf \"$tp_f\" -C \"$tp_n\"{strip_flag} --no-same-owner || rc=$?;;\n        *) echo \"telepatch: $tp_f is not a tar or zip archive\" >&2; rc=1;;\n      esac\n    fi\n    if [ \"$rc\" -eq 0 ]; then printf '%s\\n' \"$tp_mk\" > \"$tp_n/.telepatch-fetch\" || rc=$?; fi\n"
    ));
    match f.mode {
        Some(m) => o.push_str(&format!(
            "    if [ \"$rc\" -eq 0 ]; then chmod {m:o} \"$tp_n\" || rc=$?; fi\n"
        )),
        None => o.push_str(
            "    if [ \"$rc\" -eq 0 ]; then\n      if [ -d \"$tp_p\" ]; then chmod --reference=\"$tp_p\" \"$tp_n\" || rc=$?; else chmod 755 \"$tp_n\" || rc=$?; fi\n    fi\n",
        ),
    }
    match &f.owner {
        Some(ow) => o.push_str(&format!(
            "    if [ \"$rc\" -eq 0 ]; then chown -R {} \"$tp_n\" || rc=$?; fi\n",
            quote(ow)
        )),
        None => o.push_str(
            "    if [ \"$rc\" -eq 0 ] && [ -d \"$tp_p\" ]; then chown --reference=\"$tp_p\" \"$tp_n\" 2>/dev/null; fi\n",
        ),
    }
    // The old tree goes to the backup dir (or, with no run to save under,
    // beside the scratch tree and is removed once the swap is done).
    let swap = if capture {
        "        tp_undo \"$tp_p\" || { tp_bk=; rc=1; }\n        if [ \"$rc\" -ne 0 ]; then :\n        elif { if [ -n \"$tp_bd\" ]; then tp_o=$tp_bd/old; tp_try \"$tp_p\" \"$tp_o\" mv -- \"$tp_p\" \"$tp_o\"; else tp_o=\"$tp_n.old\"; mv -- \"$tp_p\" \"$tp_o\"; fi; } && mv -- \"$tp_n\" \"$tp_p\"; then\n          if [ -z \"$tp_bd\" ]; then rm -rf -- \"$tp_o\"; fi; changed=1\n        else\n          rc=$?\n          if [ ! -e \"$tp_p\" ]; then mv -- \"$tp_o\" \"$tp_p\"; tp_bk=; fi\n        fi\n"
    } else {
        "        tp_o=\"$tp_n.old\"\n        if mv -- \"$tp_p\" \"$tp_o\" && mv -- \"$tp_n\" \"$tp_p\"; then\n          rm -rf -- \"$tp_o\"; changed=1\n        else\n          rc=$?\n          if [ ! -e \"$tp_p\" ]; then mv -- \"$tp_o\" \"$tp_p\"; fi\n        fi\n"
    };
    o.push_str(&format!(
        "    if [ \"$rc\" -eq 0 ]; then\n      if [ -d \"$tp_p\" ]; then\n{swap}      elif mv -- \"$tp_n\" \"$tp_p\"; then\n        changed=1\n      else\n        rc=$?\n      fi\n    fi\n    if [ -n \"$tp_n\" ]; then rm -rf -- \"$tp_n\" \"$tp_n.raw\"; fi\n  fi\nfi\n"
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sum_is_lower_cased_when_valid_and_kept_when_not() {
        let up = "A".repeat(64);
        assert_eq!(
            Fetched {
                url: "u".into(),
                sha256: None,
                via_controller: false,
                retry: 0,
                timeout: None,
            }
            .sha256(&up)
            .sha256,
            Some("a".repeat(64))
        );
        let f = Fetched {
            url: "u".into(),
            sha256: None,
            via_controller: false,
            retry: 0,
            timeout: None,
        }
        .sha256("zz");
        assert_eq!(f.sha256.as_deref(), Some("zz"));
        assert!(f.problem().unwrap().contains("64 hex digits"));
    }

    fn item(url: &str, retry: u32, timeout: Option<Duration>) -> Item {
        Item {
            url: url.into(),
            sum: None,
            unpack: false,
            retry,
            timeout,
            targets: Vec::new(),
        }
    }

    #[test]
    fn the_controller_s_deadline_adds_every_item_s_attempts_waits_and_slack() {
        let s = Duration::from_secs;
        // One default attempt: 10 min + 5 s, and 30 s of slack.
        assert_eq!(controller_deadline(&[item("a", 0, None)]), s(600 + 5 + 30));
        // Three attempts of 20 s + 5 s, waits of 1 and 2 s, slack.
        assert_eq!(
            controller_deadline(&[item("a", 2, Some(s(20)))]),
            s(3 * 25 + 1 + 2 + 30)
        );
        // Items run one after another: the bound is their sum, one slack.
        assert_eq!(
            controller_deadline(&[item("a", 0, Some(s(10))), item("b", 1, Some(s(20)))]),
            s(15 + 2 * 25 + 1 + 30)
        );
        assert_eq!(controller_deadline(&[]), s(30));
    }

    #[test]
    fn the_longest_timeout_and_the_most_retries_win_per_url_and_a_missing_one_is_the_default() {
        let m = |a: Option<u64>, b: Option<u64>| {
            let s = Duration::from_secs;
            item("u", 0, a.map(s))
                .timeout
                .max(item("u", 0, b.map(s)).timeout)
        };
        assert_eq!(m(None, Some(5)), Some(Duration::from_secs(5)));
        assert_eq!(m(Some(9), Some(5)), Some(Duration::from_secs(9)));
        assert_eq!(item("u", 0, None).attempt_timeout(), DEFAULT_TIMEOUT);
        assert_eq!(DEFAULT_TIMEOUT, Duration::from_secs(600));
    }

    #[test]
    fn the_script_gives_every_item_its_limit_and_uses_no_coreutils_timeout() {
        let script = prepare_script(&[
            item("http://x/a", 2, Some(Duration::from_millis(1500))),
            item("http://x/b", 0, None),
        ]);
        assert!(script.contains("tp_lt=1.5 tp_lr=2\ntp_one 0 "), "{script}");
        assert!(script.contains("tp_lt=600 tp_lr=0\ntp_one 1 "), "{script}");
        assert!(script.contains("--max-time") && script.contains("--timeout="));
        assert!(!script.contains("timeout -k") && !script.contains("timeout \"$"));
    }

    #[test]
    fn a_stopped_probe_names_every_item_with_no_answer_and_the_controller() {
        let items = [
            item("http://x/a", 0, Some(Duration::from_secs(1))),
            item("http://x/b", 0, None),
            item("http://x/c", 0, Some(Duration::from_secs(2))),
        ];
        let why = crate::executor::Stopped::TimedOut { trailer: None };
        let got = stopped(&items, "fetch 0 cached\n", &why, Duration::from_secs(99));
        assert_eq!(
            got,
            "fetch http://x/b: download timed out after 10m (stopped by the controller); \
             fetch http://x/c: download timed out after 2s (stopped by the controller)"
        );
        // Everyone answered: nothing to name.
        let all = "fetch 0 cached\nfetch 1 cached\nfetch 2 cached\n";
        assert_eq!(stopped(&items, all, &why, Duration::from_secs(99)), "");
    }

    #[test]
    fn rc_124_is_a_timed_out_download() {
        let items = [item("http://x/a", 0, Some(Duration::from_secs(7)))];
        assert_eq!(
            parse(&items, "fetch 0 fail curl 124 curl: (28) x\n"),
            [Found::Blocked(
                "fetch http://x/a: download timed out after 7s".into()
            )]
        );
    }

    #[test]
    fn parse_uses_takes_a_lower_case_sum_and_an_absolute_dir_and_nothing_else() {
        let a = "a".repeat(64);
        let out = format!(
            "fetch 0 cached\nfuse {a} /c/with space\nfuse {} /c\nfuse {a} rel\nfuse short /c\nfuse {a}\nxfuse {a} /c\n",
            "A".repeat(64)
        );
        assert_eq!(parse_uses(&out), [(a, "/c/with space".to_string())]);
    }

    #[test]
    fn parse_reads_every_word() {
        let it = |sum: Option<&str>| Item {
            url: "http://x/a".into(),
            sum: sum.map(str::to_string),
            unpack: false,
            retry: 0,
            timeout: None,
            targets: Vec::new(),
        };
        let items = [
            it(Some("w")),
            it(None),
            it(Some("w2")),
            it(Some("w3")),
            it(Some("w4")),
        ];
        let out = "fetch 0 cached\nfetch 1 staged\nfetch 2 mismatch g\nfetch 3 fail curl 22 curl: (22) 404\nfetch 4 notool\n";
        assert_eq!(
            parse(&items, out),
            [
                Found::Cached,
                Found::Staged,
                Found::Blocked("fetch http://x/a: sha256 mismatch: want w2 got g".into()),
                Found::Blocked(
                    "fetch http://x/a: download failed (curl rc 22): curl: (22) 404".into()
                ),
                Found::NoDownloader(
                    "fetch http://x/a: neither curl nor wget is on the host".into()
                ),
            ]
        );
        assert_eq!(
            parse(&items[..1], ""),
            [Found::Blocked(
                "fetch http://x/a: the host gave no answer".into()
            )]
        );
    }
}
