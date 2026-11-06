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
//! `<cache>` is `/var/cache/telepatch/sha256` and `<stage>` is
//! `/var/tmp/telepatch`; `TP_FETCH_CACHE` and `TP_FETCH_STAGE` override them
//! (tests). `<run>` is [`run_id`]: one per process. Nothing is written
//! under `/tmp`: every temporary lives in the staging directory or beside
//! its target.
//!
//! The op scripts ([`render`]) read only the cache entry (or the staged
//! file): `.from` copies it to a candidate beside the target, with the
//! final mode and owner, and renames that over the target when `cmp`
//! differs. `.unpack` unpacks into a sibling `<target>.tpXXXXXX`, sets mode
//! and owner, writes a marker `.telepatch-fetch` (`<sum> strip=<n>`) inside,
//! then swaps it in: the target is absent (one rename), or the old tree is
//! renamed aside first, the new one renamed in, the old removed (two
//! renames, and the old tree comes back if the second fails). A target that
//! already holds the marker for this sum and strip is settled: nothing
//! changes.

use std::borrow::Borrow;
use std::sync::OnceLock;

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
}

impl Fetched {
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

    /// The trace error this fetch gives the op that takes it, if any.
    pub(crate) fn problem(&self) -> Option<String> {
        if self.url.trim().is_empty() {
            return Some("fetch has no url".to_string());
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

fn valid_sum(s: &str) -> bool {
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
        }
    }
}

/// The staging directory name of this process: one per run, so two runs on
/// one host never share one. `<unix nanos in hex>-<pid>`.
pub fn run_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{n:x}-{}", std::process::id())
    })
}

/// The `rm` that ends a host's run: the staging directory of [`run_id`].
pub fn cleanup_script() -> String {
    format!(
        "rm -rf -- \"${{TP_FETCH_STAGE:-/var/tmp/telepatch}}\"/{}\n",
        quote(run_id())
    )
}

const LOCATE: &str = "tp_cd=\"${TP_FETCH_CACHE:-/var/cache/telepatch/sha256}\"\ntp_sd=\"${TP_FETCH_STAGE:-/var/tmp/telepatch}\"\n";

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
}

/// The distinct `(url, sum)` the window's file ops fetch, first-seen order.
pub fn items(window: &Window) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::new();
    for op in &window.ops {
        let OpSpec::File(f) = &op.node.spec else {
            continue;
        };
        let Some(src) = &f.src else { continue };
        let (url, sum) = (&src.fetched.url, &src.fetched.sha256);
        match out.iter_mut().find(|i| i.url == *url && i.sum == *sum) {
            Some(i) => i.unpack |= src.unpack.is_some(),
            None => out.push(Item {
                url: url.clone(),
                sum: sum.clone(),
                unpack: src.unpack.is_some(),
            }),
        }
    }
    out
}

/// The prepare script for `items`, one `fetch <i> <word> ..` line each:
/// `cached`, `fetched`, `staged`, `mismatch <got>`, `fail <tool> <rc>
/// <message>`, `notool`, `badarchive`, `needtool <name>`.
pub fn prepare_script(items: &[Item]) -> String {
    let mut s = String::from(LOCATE);
    s.push_str(&format!(
        "tp_run=\"$tp_sd\"/{}\ntp_dl=\nif command -v curl >/dev/null 2>&1; then tp_dl=curl; elif command -v wget >/dev/null 2>&1; then tp_dl=wget; fi\ntp_keep=0\n",
        quote(run_id())
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
tp_one() {
  local i=$1 u=$2 s=$3 up=$4 f p rc err got
  if [ -n "$s" ]; then
    f="$tp_cd/$s"
    if [ -f "$f" ] && [ "$(sha256sum "$f" 2>/dev/null | cut -d' ' -f1)" = "$s" ]; then
      tp_check "$i" "$f" "$up" || return 0
      echo "fetch $i cached"; return 0
    fi
  fi
  if [ -z "$tp_dl" ]; then echo "fetch $i notool"; return 0; fi
  (umask 022; mkdir -p -- "$tp_run") || { echo "fetch $i fail $tp_dl 1 cannot create $tp_run"; return 0; }
  p="$tp_run/$i.part"
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
    if (umask 022; mkdir -p -- "$tp_cd") && mv -f -- "$p" "$tp_cd/.$s.part" && chmod 644 "$tp_cd/.$s.part" && mv -f -- "$tp_cd/.$s.part" "$tp_cd/$s"; then
      echo "fetch $i fetched"
    else
      rm -f -- "$p" "$tp_cd/.$s.part"
      echo "fetch $i fail $tp_dl 1 cannot write the cache $tp_cd"
    fi
  else
    tp_check "$i" "$p" "$up" || { rm -f -- "$p"; return 0; }
    chmod 644 "$p"
    mv -f -- "$p" "$tp_run/u-$(printf %s "$u" | sha256sum | cut -c1-32)" && tp_keep=1
    echo "fetch $i staged"
  fi
}
"#,
    );
    for (i, it) in items.iter().enumerate() {
        s.push_str(&format!(
            "tp_one {i} {} {} {}\n",
            quote(&it.url),
            quote(it.sum.as_deref().unwrap_or("")),
            if it.unpack { 1 } else { 0 }
        ));
    }
    s.push_str("if [ \"$tp_keep\" = 0 ]; then rm -rf -- \"$tp_run\"; fi\n");
    s
}

/// What prepare found for one item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    Cached,
    Fetched,
    /// Downloaded without a checksum, staged for the op.
    Staged,
    /// The host is blocked, with this reason (no window suffix yet).
    Blocked(String),
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
                "notool" => Found::Blocked(format!(
                    "fetch {}: neither curl nor wget is on the host",
                    it.url
                )),
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

/// The file op's body for a fetched source: appended after `tp_p=<path>`
/// ([`FileSpec::render`]); the mode and owner the spec names are on the
/// candidate before it replaces the target, and the usual mode and owner
/// checks follow.
pub fn render(f: &FileSpec, src: &FileSrc, o: &mut String) {
    o.push_str(LOCATE);
    match &src.fetched.sha256 {
        Some(s) => o.push_str(&format!("tp_s={}\ntp_f=\"$tp_cd/$tp_s\"\n", quote(s))),
        None => o.push_str(&format!(
            "tp_f=\"$tp_sd\"/{}/\"u-$(printf %s {} | sha256sum | cut -c1-32)\"\n",
            quote(run_id()),
            quote(&src.fetched.url)
        )),
    }
    o.push_str(
        "if [ ! -f \"$tp_f\" ]; then echo \"telepatch: fetched file $tp_f is missing (prepare did not fetch it)\" >&2; rc=1; fi\n",
    );
    if src.fetched.sha256.is_none() {
        o.push_str("if [ \"$rc\" -eq 0 ]; then tp_s=$(sha256sum \"$tp_f\" | cut -d' ' -f1); fi\n");
    }
    match src.unpack {
        None => render_place(f, o),
        Some(strip) => render_unpack(f, strip, o),
    }
}

fn render_place(f: &FileSpec, o: &mut String) {
    o.push_str(
        "if [ \"$rc\" -eq 0 ] && [ -d \"$tp_p\" ]; then echo \"telepatch: $tp_p is a directory\" >&2; rc=1; fi\n",
    );
    o.push_str(
        "if [ \"$rc\" -eq 0 ] && ! { [ -f \"$tp_p\" ] && cmp -s \"$tp_f\" \"$tp_p\"; }; then\n  tp_t=$(mktemp \"$tp_p.tpXXXXXX\") || rc=$?\n  if [ \"$rc\" -eq 0 ]; then\n    cp -- \"$tp_f\" \"$tp_t\" || rc=$?\n",
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
    o.push_str(
        "    if [ \"$rc\" -eq 0 ]; then mv -f \"$tp_t\" \"$tp_p\"; rc=$?; fi\n    if [ \"$rc\" -eq 0 ]; then changed=1; else rm -f \"$tp_t\"; fi\n  fi\nfi\n",
    );
}

fn render_unpack(f: &FileSpec, strip: u32, o: &mut String) {
    let strip_flag = if strip > 0 {
        format!(" --strip-components={strip}")
    } else {
        String::new()
    };
    o.push_str(MAGIC);
    o.push_str(&format!(
        "tp_n=\nif [ \"$rc\" -eq 0 ]; then\n  tp_k=$(tp_magic \"$tp_f\")\n  tp_mk=\"$tp_s strip={strip}\"\n  if [ -e \"$tp_p\" ] && [ ! -d \"$tp_p\" ]; then\n    echo \"telepatch: $tp_p exists and is not a directory\" >&2; rc=1\n  elif [ -d \"$tp_p\" ] && [ \"$(cat \"$tp_p/.telepatch-fetch\" 2>/dev/null)\" = \"$tp_mk\" ]; then\n    :\n  else\n    tp_n=$(mktemp -d \"$tp_p.tpXXXXXX\") || rc=$?\n    if [ \"$rc\" -eq 0 ]; then\n      case \"$tp_k\" in\n        zip)\n          tp_z=\"$tp_n.raw\"\n          mkdir \"$tp_z\" || rc=$?\n          if [ \"$rc\" -eq 0 ]; then\n            if command -v unzip >/dev/null 2>&1; then unzip -q -o \"$tp_f\" -d \"$tp_z\" || rc=$?; else bsdtar -xf \"$tp_f\" -C \"$tp_z\" || rc=$?; fi\n          fi\n"
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
    o.push_str(
        "    if [ \"$rc\" -eq 0 ]; then\n      if [ -d \"$tp_p\" ]; then\n        tp_o=\"$tp_n.old\"\n        if mv -- \"$tp_p\" \"$tp_o\" && mv -- \"$tp_n\" \"$tp_p\"; then\n          rm -rf -- \"$tp_o\"; changed=1\n        else\n          rc=$?\n          if [ ! -e \"$tp_p\" ]; then mv -- \"$tp_o\" \"$tp_p\"; fi\n        fi\n      elif mv -- \"$tp_n\" \"$tp_p\"; then\n        changed=1\n      else\n        rc=$?\n      fi\n    fi\n    if [ -n \"$tp_n\" ]; then rm -rf -- \"$tp_n\" \"$tp_n.raw\"; fi\n  fi\nfi\n",
    );
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
                sha256: None
            }
            .sha256(&up)
            .sha256,
            Some("a".repeat(64))
        );
        let f = Fetched {
            url: "u".into(),
            sha256: None,
        }
        .sha256("zz");
        assert_eq!(f.sha256.as_deref(), Some("zz"));
        assert!(f.problem().unwrap().contains("64 hex digits"));
    }

    #[test]
    fn parse_reads_every_word() {
        let it = |sum: Option<&str>| Item {
            url: "http://x/a".into(),
            sum: sum.map(str::to_string),
            unpack: false,
        };
        let items = [it(Some("w")), it(None), it(Some("w2")), it(Some("w3"))];
        let out = "fetch 0 cached\nfetch 1 staged\nfetch 2 mismatch g\nfetch 3 fail curl 22 curl: (22) 404\n";
        assert_eq!(
            parse(&items, out),
            [
                Found::Cached,
                Found::Staged,
                Found::Blocked("fetch http://x/a: sha256 mismatch: want w2 got g".into()),
                Found::Blocked(
                    "fetch http://x/a: download failed (curl rc 22): curl: (22) 404".into()
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
