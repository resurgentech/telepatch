//! Controller-side files and commands.
//!
//! [`local_file`], [`local_dir`] and [`local`] name something on the
//! controller. Prepare reads it (or runs the argv, with no shell). The
//! executor streams a file to the host only when the host copy's sha256
//! differs, in chunks, each chunk its own shell script. An op body never
//! carries the bytes: the persistent shell's stdin is the op queue, and
//! every op is wrapped so it cannot read that stdin.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::sh::quote;
use crate::trace::Principal;

/// Raw bytes per copy-channel script. A 20 MiB file is many scripts.
pub const CHUNK: usize = 256 * 1024;

thread_local! {
    static ANCHOR: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Directory a relative [`local_file`] / [`local_dir`] path is taken
/// against: the vars file's directory when the run was given one, later
/// the inventory file's. Unset: the process current directory.
pub fn set_anchor(dir: Option<PathBuf>) {
    ANCHOR.with(|a| *a.borrow_mut() = dir);
}

pub fn anchor() -> PathBuf {
    ANCHOR.with(|a| {
        a.borrow()
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    })
}

/// How many chunk scripts a payload of `len` bytes takes. Empty is one
/// script that truncates the staging file.
pub fn chunk_count(len: u64) -> usize {
    if len == 0 {
        1
    } else {
        len.div_ceil(CHUNK as u64) as usize
    }
}

/// A controller file. The path is stored as written and read in prepare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFile {
    pub path: String,
}

/// A controller directory. Copied as a tree; nothing on the host is removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalDir {
    pub path: String,
}

/// A controller command: an argv, never a shell string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalCmd {
    pub argv: Vec<String>,
}

pub fn local_file(path: impl AsRef<str>) -> LocalFile {
    LocalFile {
        path: path.as_ref().to_string(),
    }
}

pub fn local_dir(path: impl AsRef<str>) -> LocalDir {
    LocalDir {
        path: path.as_ref().to_string(),
    }
}

pub fn local<I, S>(argv: I) -> LocalCmd
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    LocalCmd {
        argv: argv.into_iter().map(|s| s.as_ref().to_string()).collect(),
    }
}

/// What [`.from(..)`](crate::ops::file::FileHandle::from) accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromArg {
    Fetch(crate::ops::fetch::Fetched),
    File(LocalFile),
    Dir(LocalDir),
}

impl From<crate::ops::fetch::Fetched> for FromArg {
    fn from(f: crate::ops::fetch::Fetched) -> FromArg {
        FromArg::Fetch(f)
    }
}

impl From<&crate::ops::fetch::Fetched> for FromArg {
    fn from(f: &crate::ops::fetch::Fetched) -> FromArg {
        FromArg::Fetch(f.clone())
    }
}

impl From<LocalFile> for FromArg {
    fn from(f: LocalFile) -> FromArg {
        FromArg::File(f)
    }
}

impl From<&LocalFile> for FromArg {
    fn from(f: &LocalFile) -> FromArg {
        FromArg::File(f.clone())
    }
}

impl From<LocalDir> for FromArg {
    fn from(d: LocalDir) -> FromArg {
        FromArg::Dir(d)
    }
}

impl From<&LocalDir> for FromArg {
    fn from(d: &LocalDir) -> FromArg {
        FromArg::Dir(d.clone())
    }
}

/// A line's text, or a controller value prepare reads before render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineText {
    Literal(String),
    File(LocalFile),
    Cmd(LocalCmd),
}

impl From<&str> for LineText {
    fn from(s: &str) -> LineText {
        LineText::Literal(s.to_string())
    }
}

impl From<String> for LineText {
    fn from(s: String) -> LineText {
        LineText::Literal(s)
    }
}

impl From<&String> for LineText {
    fn from(s: &String) -> LineText {
        LineText::Literal(s.clone())
    }
}

impl From<&LocalFile> for LineText {
    fn from(f: &LocalFile) -> LineText {
        LineText::File(f.clone())
    }
}

impl From<LocalFile> for LineText {
    fn from(f: LocalFile) -> LineText {
        LineText::File(f)
    }
}

impl From<&LocalCmd> for LineText {
    fn from(c: &LocalCmd) -> LineText {
        LineText::Cmd(c.clone())
    }
}

impl From<LocalCmd> for LineText {
    fn from(c: LocalCmd) -> LineText {
        LineText::Cmd(c)
    }
}

impl LineText {
    /// The literal, once prepare has resolved a controller value.
    pub(crate) fn literal(&self) -> Option<&str> {
        match self {
            LineText::Literal(s) => Some(s),
            _ => None,
        }
    }
}

/// One controller file placed on the host by a `file` op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalKind {
    File,
    Dir,
    Fetch { url: String, sha256: Option<String> },
}

/// A file or directory inside a [`LocalPlace`], filled in prepare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    pub rel: String,
    pub abs: PathBuf,
    pub sha256: String,
    pub len: u64,
    pub is_dir: bool,
}

/// Controller bytes a file op places. The trace stores the path; prepare
/// fills [`LocalPlace::files`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalPlace {
    pub kind: LocalKind,
    pub written: String,
    pub prepared: bool,
    pub files: Vec<Staged>,
}

impl LocalPlace {
    pub fn file(path: impl Into<String>) -> LocalPlace {
        LocalPlace {
            kind: LocalKind::File,
            written: path.into(),
            prepared: false,
            files: Vec::new(),
        }
    }

    pub fn dir(path: impl Into<String>) -> LocalPlace {
        LocalPlace {
            kind: LocalKind::Dir,
            written: path.into(),
            prepared: false,
            files: Vec::new(),
        }
    }

    pub fn fetch(url: String, sha256: Option<String>) -> LocalPlace {
        LocalPlace {
            kind: LocalKind::Fetch {
                url: url.clone(),
                sha256,
            },
            written: url,
            prepared: false,
            files: Vec::new(),
        }
    }

    pub fn is_dir(&self) -> bool {
        matches!(self.kind, LocalKind::Dir)
    }

    /// The fetch url when this place has no checksum (policy `Unverified`).
    pub fn unverified_url(&self) -> Option<&str> {
        match &self.kind {
            LocalKind::Fetch { url, sha256: None } => Some(url),
            _ => None,
        }
    }
}

/// A file the copy channel may stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyFile {
    pub rel: String,
    pub abs: PathBuf,
    pub sha256: String,
    pub len: u64,
    pub is_dir: bool,
    pub same: bool,
}

/// One file op's controller copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyItem {
    pub who: Principal,
    pub dest_word: String,
    pub display: String,
    pub written: String,
    pub is_dir: bool,
    pub files: Vec<CopyFile>,
}

impl CopyItem {
    /// `file <path>: will change (from controller <src>, <size>, sha256 differs)`.
    /// `None` when every file already matches.
    pub fn note(&self) -> Option<String> {
        let mut n = 0u64;
        let mut differ = false;
        for f in &self.files {
            if f.is_dir || f.same {
                continue;
            }
            differ = true;
            n += f.len;
        }
        if !differ {
            return None;
        }
        Some(format!(
            "file {}: will change (from controller {}, {}, sha256 differs)",
            self.display,
            self.written,
            human_size(n)
        ))
    }
}

pub fn human_size(n: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    const KIB: f64 = 1024.0;
    let n = n as f64;
    if n >= MIB {
        format!("{:.1} MiB", n / MIB)
    } else if n >= KIB {
        format!("{:.1} KiB", n / KIB)
    } else {
        format!("{} B", n as u64)
    }
}

/// Shell assignments `tp_sd`, `tp_p` and `tp_base` for `dest_word`.
/// `tp_base` is the staging path of a single file, or the staging
/// directory of a tree. The same text is used by the copy channel and
/// the op, so both name one path.
pub fn stage_vars(dest_word: &str) -> String {
    let mut s = String::from("tp_sd=\"${TP_FETCH_STAGE:-/var/tmp/telepatch}\"\n");
    s.push_str(&format!("tp_p={dest_word}\n"));
    s.push_str("tp_base=\"$tp_sd/$__tp_run/c-$(printf %s \"$tp_p\" | sha256sum | cut -c1-16)\"\n");
    s
}

/// One chunk of a streamed file. `truncate` replaces the staging file;
/// later chunks append.
pub fn chunk_script(dest_word: &str, rel: &str, bytes: &[u8], truncate: bool) -> String {
    let redir = if truncate { ">" } else { ">>" };
    let stage = stage_path(rel);
    let mut s = stage_vars(dest_word);
    s.push_str(&format!("tp_stage={stage}\n"));
    s.push_str("mkdir -p -- \"$(dirname -- \"$tp_stage\")\"\n");
    s.push_str(&format!(
        "printf %s {} | base64 -d {redir} \"$tp_stage\"\n",
        quote(&STANDARD.encode(bytes))
    ));
    s
}

fn stage_path(rel: &str) -> String {
    if rel.is_empty() {
        "\"$tp_base\"".to_string()
    } else {
        format!("\"$tp_base\"/{}", quote(rel))
    }
}

/// Host path of one staged entry, for the prepare checksum probe.
pub fn host_path(rel: &str) -> String {
    if rel.is_empty() {
        "\"$tp_p\"".to_string()
    } else {
        format!("\"$tp_p\"/{}", quote(rel))
    }
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex(&Sha256::digest(bytes))
}

pub fn sha256_file(path: &Path) -> Result<(String, u64), String> {
    use sha2::{Digest, Sha256};
    let mut f =
        std::fs::File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut n = 0u64;
    loop {
        let k = f
            .read(&mut buf)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if k == 0 {
            break;
        }
        h.write_all(&buf[..k])
            .map_err(|e| format!("cannot hash {}: {e}", path.display()))?;
        n += k as u64;
    }
    Ok((hex(&h.finalize()), n))
}

fn hex(d: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(d.len() * 2);
    for b in d {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

pub fn resolve_path(written: &str) -> Result<PathBuf, String> {
    resolve_path_in(written, &anchor(), home().as_deref())
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

pub(crate) fn resolve_path_in(
    written: &str,
    anchor: &Path,
    home: Option<&Path>,
) -> Result<PathBuf, String> {
    let expanded = if written == "~" || written.starts_with("~/") {
        let home = home.ok_or_else(|| "local path starts with ~ and HOME is unset".to_string())?;
        if written == "~" {
            home.to_path_buf()
        } else {
            home.join(&written[2..])
        }
    } else {
        PathBuf::from(written)
    };
    if expanded.is_absolute() {
        Ok(expanded)
    } else {
        Ok(anchor.join(expanded))
    }
}

/// Read `place` on the controller and record each file's sha256.
pub fn prepare_place(place: &mut LocalPlace) -> Result<(), String> {
    match &place.kind {
        LocalKind::File => {
            let abs = resolve_path(&place.written)?;
            if !abs.is_file() {
                return Err(format!("local file {}: not a file", place.written));
            }
            let (sha, len) = sha256_file(&abs)?;
            place.files = vec![Staged {
                rel: String::new(),
                abs,
                sha256: sha,
                len,
                is_dir: false,
            }];
        }
        LocalKind::Dir => {
            let abs = resolve_path(&place.written)?;
            if !abs.is_dir() {
                return Err(format!("local dir {}: not a directory", place.written));
            }
            place.files = walk_dir(&abs)?;
        }
        LocalKind::Fetch { url, sha256 } => {
            let abs = download(url, sha256.as_deref())?;
            let (sha, len) = sha256_file(&abs)?;
            place.files = vec![Staged {
                rel: String::new(),
                abs,
                sha256: sha,
                len,
                is_dir: false,
            }];
        }
    }
    place.prepared = true;
    Ok(())
}

fn walk_dir(root: &Path) -> Result<Vec<Staged>, String> {
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<Staged>) -> Result<(), String> {
    let mut entries = std::fs::read_dir(dir)
        .map_err(|e| format!("local dir {}: {e}", dir.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("local dir {}: {e}", dir.display()))?;
    entries.sort_by_key(|e| e.file_name());
    for ent in entries {
        let path = ent.path();
        let ft = ent
            .file_type()
            .map_err(|e| format!("local dir {}: {e}", path.display()))?;
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if rel.split('/').any(|s| s.is_empty() || s == "..") {
            return Err(format!("local dir {}: refused path {rel}", root.display()));
        }
        if ft.is_symlink() {
            // Not followed and not copied: a link is not a file's bytes.
            continue;
        }
        if ft.is_dir() {
            let before = out.len();
            walk(root, &path, out)?;
            if out.len() == before {
                out.push(Staged {
                    rel,
                    abs: path,
                    sha256: String::new(),
                    len: 0,
                    is_dir: true,
                });
            }
        } else if ft.is_file() {
            let (sha, len) = sha256_file(&path)?;
            out.push(Staged {
                rel,
                abs: path,
                sha256: sha,
                len,
                is_dir: false,
            });
        }
    }
    Ok(())
}

/// The line's text. A trailing newline is dropped; more than one line is
/// an error. A command that exits non-zero is an error.
pub fn resolve_line(text: &LineText) -> Result<String, String> {
    match text {
        LineText::Literal(s) => Ok(s.clone()),
        LineText::File(f) => {
            let path = resolve_path(&f.path)?;
            let bytes = std::fs::read(&path).map_err(|e| format!("local file {}: {e}", f.path))?;
            one_line(&f.path, &bytes)
        }
        LineText::Cmd(c) => {
            let bytes = run_argv(&c.argv)?;
            let name = c.argv.first().map(String::as_str).unwrap_or("local");
            one_line(name, &bytes)
        }
    }
}

fn one_line(name: &str, bytes: &[u8]) -> Result<String, String> {
    let mut s =
        String::from_utf8(bytes.to_vec()).map_err(|_| format!("local {name}: not utf-8"))?;
    if s.ends_with('\n') {
        s.pop();
    }
    if s.contains('\n') {
        return Err(format!("local {name}: line value has more than one line"));
    }
    Ok(s)
}

pub fn run_argv(argv: &[String]) -> Result<Vec<u8>, String> {
    if argv.is_empty() {
        return Err("local command has no arguments".to_string());
    }
    let out = Command::new(&argv[0])
        .args(&argv[1..])
        .output()
        .map_err(|e| format!("local {}: {e}", argv[0]))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        return Err(format!(
            "local {} failed ({}){}",
            argv[0],
            out.status,
            if err.is_empty() {
                String::new()
            } else {
                format!(": {err}")
            }
        ));
    }
    Ok(out.stdout)
}

fn have(bin: &str) -> bool {
    Command::new(bin)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn cache_dir() -> PathBuf {
    if let Some(home) = home() {
        return home.join(".cache/telepatch/sha256");
    }
    anchor().join(".telepatch-cache")
}

/// Download `url` on the controller into the checksum cache. `expect` is
/// checked when it is `Some`. `file://` is accepted (`curl` / `wget`).
pub fn download(url: &str, expect: Option<&str>) -> Result<PathBuf, String> {
    let dir = cache_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("controller cache: {e}"))?;
    let name = match expect {
        Some(s) => s.to_string(),
        None => format!("u-{}", &sha256_bytes(url.as_bytes())[..32]),
    };
    let dest = dir.join(&name);
    if dest.is_file()
        && expect.is_some_and(|sum| sha256_file(&dest).is_ok_and(|(got, _)| got == sum))
    {
        return Ok(dest);
    }
    let part = dir.join(format!(".part-{name}"));
    let _ = std::fs::remove_file(&part);
    if let Err(e) = fetch_to(url, &part) {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    let (got, _) = sha256_file(&part)?;
    if let Some(sum) = expect
        && got != sum
    {
        let _ = std::fs::remove_file(&part);
        return Err(format!("fetch {url}: sha256 mismatch (got {got})"));
    }
    std::fs::rename(&part, &dest).map_err(|e| format!("controller cache: {e}"))?;
    Ok(dest)
}

fn fetch_to(url: &str, dest: &Path) -> Result<(), String> {
    let dest_s = dest
        .to_str()
        .ok_or_else(|| "controller cache path is not utf-8".to_string())?;
    if have("curl") {
        let out = Command::new("curl")
            .args(["-fsSL", "-o", dest_s, url])
            .output()
            .map_err(|e| format!("fetch {url}: curl: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        return Err(format!(
            "fetch {url}: download on the controller failed (curl {}){}",
            out.status,
            if err.is_empty() {
                String::new()
            } else {
                format!(": {err}")
            }
        ));
    }
    if have("wget") {
        let out = Command::new("wget")
            .args(["-q", "-O", dest_s, url])
            .output()
            .map_err(|e| format!("fetch {url}: wget: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        return Err(format!(
            "fetch {url}: download on the controller failed (wget {})",
            out.status
        ));
    }
    Err(format!(
        "fetch {url}: neither curl nor wget is on the controller"
    ))
}

/// The prepare probe: one `index hex` or `index absent` line per content
/// file, in `rows` order.
pub fn sum_script(dest_words: &[(String, String)]) -> String {
    let mut s = String::new();
    for (i, (dest_word, rel)) in dest_words.iter().enumerate() {
        s.push_str(&format!("tp_p={dest_word}\n"));
        s.push_str(&format!("tp_f={}\n", host_path(rel)));
        s.push_str(&format!(
            "if [ -f \"$tp_f\" ]; then echo \"{i} $(sha256sum \"$tp_f\" | cut -d' ' -f1)\"; else echo \"{i} absent\"; fi\n"
        ));
    }
    s
}

/// `None` is absent. Every index must be present.
pub fn parse_sums(n: usize, out: &str) -> Result<Vec<Option<String>>, String> {
    let mut got = vec![None; n];
    let mut seen = vec![false; n];
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(i), Some(word)) = (parts.next(), parts.next()) else {
            return Err(format!("local probe: {line}"));
        };
        let i: usize = i.parse().map_err(|_| format!("local probe: {line}"))?;
        if i >= n {
            return Err(format!("local probe: index {i}"));
        }
        seen[i] = true;
        got[i] = if word == "absent" {
            None
        } else if word.len() == 64 && word.bytes().all(|b| b.is_ascii_hexdigit()) {
            Some(word.to_ascii_lowercase())
        } else {
            return Err(format!("local probe: {line}"));
        };
    }
    if seen.iter().any(|s| !s) {
        return Err(format!("local probe: incomplete answer {out:?}"));
    }
    Ok(got)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_twenty_mib_file_is_more_than_one_chunk() {
        assert!(chunk_count(20 * 1024 * 1024) > 1);
        assert_eq!(chunk_count(0), 1);
        assert_eq!(chunk_count(CHUNK as u64), 1);
        assert_eq!(chunk_count(CHUNK as u64 + 1), 2);
    }

    #[test]
    fn relative_is_the_anchor_and_tilde_is_home() {
        let anchor = Path::new("/site");
        let home = Path::new("/home/op");
        assert_eq!(
            resolve_path_in("downloads/site-agent", anchor, Some(home)).unwrap(),
            PathBuf::from("/site/downloads/site-agent")
        );
        assert_eq!(
            resolve_path_in("~/.ssh/id_ed25519.pub", anchor, Some(home)).unwrap(),
            PathBuf::from("/home/op/.ssh/id_ed25519.pub")
        );
        assert_eq!(
            resolve_path_in("/abs/a", anchor, Some(home)).unwrap(),
            PathBuf::from("/abs/a")
        );
        assert!(resolve_path_in("~/x", anchor, None).is_err());
    }

    #[test]
    fn chunk_script_is_bounded_and_carries_only_its_chunk() {
        let nested = chunk_script("'/dest/certs'", "sub/b.txt", b"beta\0\n", true);
        assert!(
            nested.contains("sub/b.txt"),
            "nested stage path missing from script:\n{nested}"
        );
        let s = chunk_script("'/dest'", "", b"hi\0", true);
        assert!(s.contains("base64 -d >"));
        assert!(s.len() < 2000);
        assert!(!s.contains("hi"));
    }
}
