//! `git(url)`: a checkout at a path, optionally at one revision.
//!
//! The probe is `git -C <path> rev-parse HEAD` and `git remote get-url
//! origin`, plus a dirty-tree check. No `.rev` clones when the path is
//! missing and otherwise leaves a clean checkout of the same remote
//! alone. `.rev(r)` checks out that commit. `.latest()` fast-forwards
//! the current branch (or the remote's default branch, when detached).
//! A different remote or a dirty tree blocks the host in prepare.
//!
//! The trailer records the inverse: `created` when this run made the
//! checkout, otherwise `was_rev`, the HEAD from before it moved.

use std::ops::Deref;

use crate::TraceError;
use crate::host::{Host, OpHandle};
use crate::key::{Key, Value};
use crate::sh::quote;
use crate::trace::{Node, NodeId, OpSpec, RunAs, Trace};

/// What an existing checkout should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Follow {
    /// Clone if missing; leave a clean checkout of this remote alone.
    Leave,
    /// Check out this revision (detached).
    Rev(String),
    /// Follow the branch the checkout is on.
    Latest,
}

/// A checkout of `url` at `path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSpec {
    pub url: String,
    pub path: String,
    pub depth: Option<u32>,
    pub follow: Follow,
}

impl GitSpec {
    pub fn new(url: impl Into<String>) -> GitSpec {
        GitSpec {
            url: url.into(),
            path: String::new(),
            depth: None,
            follow: Follow::Leave,
        }
    }

    pub fn default_writes(&self) -> std::collections::BTreeSet<Key> {
        std::collections::BTreeSet::from([Key::git(&self.path)])
    }

    pub fn default_post(&self) -> std::collections::BTreeMap<Key, Value> {
        std::collections::BTreeMap::from([(Key::git(&self.path), Value::Exists(true))])
    }

    /// The op body. Sets `rc`, `changed`, `created` and `tp_was`.
    pub fn render(&self) -> String {
        let mut s = String::from("rc=0; changed=0; created=0; tp_was=\n");
        s.push_str(&format!("tp_p={}\n", shell_path(&self.path)));
        s.push_str(&format!("tp_want={}\n", quote(&self.url)));
        s.push_str("if ! command -v git >/dev/null 2>&1; then\n");
        s.push_str("  echo 'git: git is not installed' >&2\n");
        s.push_str("  rc=1\n");
        s.push_str("elif [ ! -e \"$tp_p\" ]; then\n");
        s.push_str("  mkdir -p -- \"$(dirname -- \"$tp_p\")\"\n");
        s.push_str(&format!(
            "  git clone --quiet{} -- \"$tp_want\" \"$tp_p\" || rc=$?\n",
            depth_arg(self.depth)
        ));
        s.push_str("  if [ \"$rc\" -eq 0 ]; then\n");
        s.push_str("    created=1\n    changed=1\n");
        if let Follow::Rev(rev) = &self.follow {
            s.push_str(&checkout_rev(rev, true));
        }
        s.push_str("  fi\n");
        s.push_str("elif [ ! -d \"$tp_p\" ] || ! git -C \"$tp_p\" rev-parse --is-inside-work-tree >/dev/null 2>&1; then\n");
        s.push_str("  echo \"git: $tp_p exists and is not a checkout\" >&2\n");
        s.push_str("  rc=1\n");
        s.push_str("else\n");
        s.push_str("  tp_url=$(git -C \"$tp_p\" remote get-url origin 2>/dev/null || true)\n");
        s.push_str("  tp_was=$(git -C \"$tp_p\" rev-parse HEAD 2>/dev/null || true)\n");
        s.push_str("  if [ \"$tp_url\" != \"$tp_want\" ]; then\n");
        s.push_str("    echo \"git: $tp_p remote is $tp_url, wanted $tp_want\" >&2\n    rc=1\n");
        s.push_str("  elif [ -n \"$(git -C \"$tp_p\" status --porcelain 2>/dev/null)\" ]; then\n");
        s.push_str("    echo \"git: $tp_p dirty tree\" >&2\n    rc=1\n");
        s.push_str("  else\n");
        match &self.follow {
            // An empty `else` is a bash syntax error.
            Follow::Leave => s.push_str("    :\n"),
            Follow::Rev(rev) => s.push_str(&checkout_rev(rev, false)),
            Follow::Latest => s.push_str(&update_latest()),
        }
        s.push_str("  fi\nfi\n");
        s
    }
}

fn depth_arg(depth: Option<u32>) -> String {
    match depth {
        Some(d) => format!(" --depth {d}"),
        None => String::new(),
    }
}

/// A shell word for `path`. `~` and `~/...` expand in the op's shell.
fn shell_path(path: &str) -> String {
    if path == "~" {
        "\"$HOME\"".to_string()
    } else if let Some(rest) = path.strip_prefix("~/") {
        format!("\"$HOME\"/{}", quote(rest))
    } else {
        quote(path)
    }
}

/// Resolve `rev` and check it out detached. `fresh` is a checkout this
/// run just cloned: always detach, and keep `changed` set.
///
/// Fetch does not pass `--depth`. A later `--depth 1` makes the new tip
/// a shallow root and `merge` then reports unrelated histories.
fn checkout_rev(rev: &str, fresh: bool) -> String {
    let rev = quote(rev);
    let mut s = format!("    tp_rev={rev}\n");
    s.push_str("    tp_obj=\n");
    s.push_str("    if ! git -C \"$tp_p\" rev-parse --verify --quiet \"$tp_rev^{commit}\" >/dev/null 2>&1; then\n");
    s.push_str("      git -C \"$tp_p\" fetch --quiet origin \"$tp_rev\" >/dev/null 2>&1 || true\n");
    s.push_str("      if ! git -C \"$tp_p\" rev-parse --verify --quiet \"$tp_rev^{commit}\" >/dev/null 2>&1; then\n");
    s.push_str(
        "        git -C \"$tp_p\" fetch --quiet --unshallow origin >/dev/null 2>&1 || git -C \"$tp_p\" fetch --quiet origin >/dev/null 2>&1 || rc=$?\n",
    );
    s.push_str("      fi\n");
    s.push_str("    fi\n");
    s.push_str("    if [ \"$rc\" -eq 0 ]; then\n");
    s.push_str("      tp_obj=$(git -C \"$tp_p\" rev-parse --verify --quiet \"$tp_rev^{commit}\") || rc=$?\n");
    s.push_str("    fi\n");
    if fresh {
        s.push_str("    if [ \"$rc\" -eq 0 ]; then\n");
        s.push_str("      git -C \"$tp_p\" -c advice.detachedHead=false checkout --quiet --detach \"$tp_obj\" || rc=$?\n");
        s.push_str("    fi\n");
    } else {
        s.push_str("    if [ \"$rc\" -eq 0 ]; then\n");
        s.push_str("      tp_now=$(git -C \"$tp_p\" rev-parse HEAD)\n");
        s.push_str("      tp_sym=$(git -C \"$tp_p\" rev-parse --abbrev-ref HEAD)\n");
        s.push_str("      if [ \"$tp_obj\" != \"$tp_now\" ] || [ \"$tp_sym\" != HEAD ]; then\n");
        s.push_str("        git -C \"$tp_p\" -c advice.detachedHead=false checkout --quiet --detach \"$tp_obj\" || rc=$?\n");
        s.push_str("        if [ \"$rc\" -eq 0 ]; then changed=1; fi\n");
        s.push_str("      fi\n");
        s.push_str("    fi\n");
    }
    s
}

/// Fetch and fast-forward. No `--depth`: see [`checkout_rev`].
fn update_latest() -> String {
    "    git -C \"$tp_p\" fetch --quiet origin || rc=$?\n\
         if [ \"$rc\" -eq 0 ]; then\n\
           tp_sym=$(git -C \"$tp_p\" rev-parse --abbrev-ref HEAD)\n\
           if [ \"$tp_sym\" != HEAD ] && git -C \"$tp_p\" rev-parse --abbrev-ref '@{u}' >/dev/null 2>&1; then\n\
             tp_target=$(git -C \"$tp_p\" rev-parse '@{u}') || rc=$?\n\
             if [ \"$rc\" -eq 0 ] && [ \"$tp_target\" != \"$tp_was\" ]; then\n\
               git -C \"$tp_p\" merge --ff-only '@{u}' || rc=$?\n\
               if [ \"$rc\" -eq 0 ]; then changed=1; fi\n\
             fi\n\
           else\n\
             tp_target=$(git -C \"$tp_p\" rev-parse origin/HEAD) || rc=$?\n\
             if [ \"$rc\" -eq 0 ] && [ \"$tp_target\" != \"$tp_was\" ]; then\n\
               git -C \"$tp_p\" -c advice.detachedHead=false checkout --quiet --detach \"$tp_target\" || rc=$?\n\
               if [ \"$rc\" -eq 0 ]; then changed=1; fi\n\
             fi\n\
           fi\n\
         fi\n"
        .to_string()
}

fn url_ok(s: &str) -> bool {
    !s.is_empty() && !s.chars().any(|c| c.is_whitespace())
}

fn path_ok(s: &str) -> bool {
    !s.is_empty() && !s.chars().any(|c| c == '\n' || c == '\r' || c == '\0')
}

/// What the prepare probe found at one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen {
    Missing,
    NotRepo,
    Dirty {
        url: String,
    },
    Clean {
        head: String,
        url: String,
        /// Remote tip of the branch `.latest()` would follow. `None` when
        /// the probe was not asked, or the remote did not answer.
        tip: Option<String>,
        /// `rev^{commit}` when it is already in the checkout.
        obj: Option<String>,
    },
}

/// One probe for every git op on one principal. Prints `nogit` when `git`
/// is absent, otherwise one block per spec.
pub fn probe_script(specs: &[&GitSpec]) -> String {
    let mut s =
        String::from("if ! command -v git >/dev/null 2>&1; then\n  echo nogit\n  exit 0\nfi\n");
    for (i, spec) in specs.iter().enumerate() {
        s.push_str(&format!("echo 'i {i}'\n"));
        s.push_str(&format!("tp_p={}\n", shell_path(&spec.path)));
        s.push_str(&format!("tp_want={}\n", quote(&spec.url)));
        s.push_str("if [ ! -e \"$tp_p\" ]; then\n  echo 'state missing'\n");
        s.push_str("elif [ ! -d \"$tp_p\" ] || ! git -C \"$tp_p\" rev-parse --is-inside-work-tree >/dev/null 2>&1; then\n");
        s.push_str("  echo 'state notrepo'\nelse\n");
        s.push_str("  tp_head=$(git -C \"$tp_p\" rev-parse HEAD 2>/dev/null || echo -)\n");
        s.push_str("  tp_url=$(git -C \"$tp_p\" remote get-url origin 2>/dev/null || echo -)\n");
        s.push_str("  if [ -n \"$(git -C \"$tp_p\" status --porcelain 2>/dev/null)\" ]; then\n");
        s.push_str("    echo 'state dirty'\n  else\n    echo 'state clean'\n  fi\n");
        s.push_str("  echo \"head $tp_head\"\n  echo \"url $tp_url\"\n");
        if matches!(spec.follow, Follow::Latest) {
            s.push_str("  tp_ref=$(git -C \"$tp_p\" rev-parse --abbrev-ref HEAD 2>/dev/null || echo HEAD)\n");
            s.push_str("  if [ \"$tp_ref\" = HEAD ]; then\n");
            s.push_str("    tp_tip=$(git ls-remote \"$tp_want\" HEAD 2>/dev/null | awk 'NR==1{print $1}')\n");
            s.push_str("  else\n");
            s.push_str("    tp_tip=$(git ls-remote \"$tp_want\" \"refs/heads/$tp_ref\" 2>/dev/null | awk 'NR==1{print $1}')\n");
            s.push_str("    if [ -z \"$tp_tip\" ]; then\n");
            s.push_str("      tp_tip=$(git ls-remote \"$tp_want\" HEAD 2>/dev/null | awk 'NR==1{print $1}')\n");
            s.push_str("    fi\n  fi\n");
            s.push_str("  echo \"tip ${tp_tip:--}\"\n");
        }
        if let Follow::Rev(rev) = &spec.follow {
            s.push_str(&format!("  tp_rev={}\n", quote(rev)));
            s.push_str("  tp_obj=$(git -C \"$tp_p\" rev-parse --verify --quiet \"$tp_rev^{commit}\" 2>/dev/null || echo -)\n");
            s.push_str("  echo \"obj $tp_obj\"\n");
        }
        s.push_str("fi\n");
    }
    s
}

/// Parse [`probe_script`] output. `Err` when `git` is not installed or
/// the block is unreadable.
pub fn parse(n: usize, output: &str) -> Result<Vec<Seen>, String> {
    let lines: Vec<&str> = output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.contains(&"nogit") {
        return Err("git: git is not installed".into());
    }
    let mut out = Vec::new();
    let mut i = 0;
    for idx in 0..n {
        let want = format!("i {idx}");
        if lines.get(i) != Some(&want.as_str()) {
            return Err(format!("git probe: expected {want}, saw {lines:?}"));
        }
        i += 1;
        let state = lines
            .get(i)
            .and_then(|l| l.strip_prefix("state "))
            .ok_or_else(|| format!("git probe: no state for {idx}"))?;
        i += 1;
        let mut head = String::new();
        let mut url = String::new();
        let mut tip: Option<String> = None;
        let mut obj: Option<String> = None;
        while i < lines.len() && !lines[i].starts_with("i ") {
            if let Some(v) = lines[i].strip_prefix("head ") {
                head = v.to_string();
            } else if let Some(v) = lines[i].strip_prefix("url ") {
                url = v.to_string();
            } else if let Some(v) = lines[i].strip_prefix("tip ") {
                tip = dash(v);
            } else if let Some(v) = lines[i].strip_prefix("obj ") {
                obj = dash(v);
            }
            i += 1;
        }
        out.push(match state {
            "missing" => Seen::Missing,
            "notrepo" => Seen::NotRepo,
            "dirty" => Seen::Dirty { url },
            "clean" => Seen::Clean {
                head,
                url,
                tip,
                obj,
            },
            other => return Err(format!("git probe: unknown state {other}")),
        });
    }
    Ok(out)
}

fn dash(v: &str) -> Option<String> {
    if v.is_empty() || v == "-" {
        None
    } else {
        Some(v.to_string())
    }
}

fn same_url(a: &str, b: &str) -> bool {
    a.trim().trim_end_matches('/') == b.trim().trim_end_matches('/')
}

/// Prepare lines for specs the probe says will change. `Err` blocks the
/// host: a different remote, a dirty tree, or a path that is not a checkout.
pub fn notes(specs: &[&GitSpec], output: &str) -> Result<Vec<String>, String> {
    let seen = parse(specs.len(), output)?;
    let mut lines = Vec::new();
    for (spec, seen) in specs.iter().zip(seen) {
        if let Some(line) = one(spec, &seen)? {
            lines.push(line);
        }
    }
    Ok(lines)
}

fn remote_msg(path: &str, url: &str, wanted: &str) -> String {
    format!("git {path}: remote is {url}, wanted {wanted}")
}

fn one(spec: &GitSpec, seen: &Seen) -> Result<Option<String>, String> {
    let path = &spec.path;
    match seen {
        Seen::Missing => Ok(Some(match &spec.follow {
            Follow::Rev(r) => format!("git {path}: will clone and checkout {r}"),
            Follow::Leave | Follow::Latest => format!("git {path}: will clone"),
        })),
        Seen::NotRepo => Err(format!("git {path}: exists and is not a checkout")),
        Seen::Dirty { url } => {
            if !url.is_empty() && url != "-" && !same_url(url, &spec.url) {
                Err(remote_msg(path, url, &spec.url))
            } else {
                Err(format!("git {path}: dirty tree"))
            }
        }
        Seen::Clean {
            head,
            url,
            tip,
            obj,
        } => {
            if !same_url(url, &spec.url) {
                return Err(remote_msg(path, url, &spec.url));
            }
            match &spec.follow {
                Follow::Leave => Ok(None),
                Follow::Rev(r) => {
                    if obj.as_deref() == Some(head.as_str()) && !head.is_empty() {
                        Ok(None)
                    } else {
                        Ok(Some(format!("git {path}: will checkout {r}")))
                    }
                }
                Follow::Latest => {
                    if tip.as_deref() == Some(head.as_str()) && !head.is_empty() {
                        Ok(None)
                    } else {
                        Ok(Some(format!("git {path}: will update")))
                    }
                }
            }
        }
    }
}

/// `.into(path)` was never called.
pub fn errors(trace: &Trace) -> Vec<TraceError> {
    let mut out = Vec::new();
    for (i, n) in trace.nodes.iter().enumerate() {
        let Node::Op(op) = n else { continue };
        let OpSpec::Git(g) = &op.spec else { continue };
        if g.path.is_empty() {
            out.push(trace.locate(
                NodeId(i),
                TraceError::new(format!("git {}: .into(path) is required", g.url)),
            ));
        }
    }
    out
}

/// Git ops of `window`, grouped by [`RunAs`] (shell), trace order.
pub fn by_run_as<'a>(
    ops: impl IntoIterator<Item = (&'a RunAs, &'a GitSpec)>,
) -> Vec<(RunAs, Vec<&'a GitSpec>)> {
    let mut out: Vec<(RunAs, Vec<&GitSpec>)> = Vec::new();
    for (who, spec) in ops {
        if spec.path.is_empty() {
            continue;
        }
        match out.iter_mut().find(|(w, _)| w == who) {
            Some((_, v)) => v.push(spec),
            None => out.push((who.clone(), vec![spec])),
        }
    }
    out
}

#[derive(Clone)]
pub struct GitHandle(OpHandle);

impl Deref for GitHandle {
    type Target = OpHandle;
    fn deref(&self) -> &OpHandle {
        &self.0
    }
}

crate::host::op_builders!(GitHandle);

impl GitHandle {
    #[track_caller]
    fn fail(&self, msg: impl Into<String>) -> Self {
        self.0.record_error(TraceError::new(msg));
        self.clone()
    }

    fn edit_spec(&self, f: impl FnOnce(&mut GitSpec)) {
        self.0.edit(|op| {
            if let OpSpec::Git(g) = &mut op.spec {
                f(g);
            }
        });
    }

    /// Clone and fetch with `--depth n` (`n` >= 1).
    #[track_caller]
    pub fn depth(&self, n: u32) -> Self {
        if n == 0 {
            return self.fail("git: .depth() must be at least 1");
        }
        self.edit_spec(|g| g.depth = Some(n));
        self.clone()
    }

    /// Check out revision `r` (a commit, tag or branch). Exclusive with
    /// [`.latest`](GitHandle::latest).
    #[track_caller]
    pub fn rev(&self, r: &str) -> Self {
        if !url_ok(r) {
            return self.fail("git: .rev() must be a non-empty single line without whitespace");
        }
        let mut clash = false;
        self.0.edit(|op| {
            if let OpSpec::Git(g) = &mut op.spec {
                clash = matches!(g.follow, Follow::Latest);
                g.follow = Follow::Rev(r.to_string());
            }
        });
        if clash {
            self.fail("git: .rev() and .latest() are exclusive")
        } else {
            self.clone()
        }
    }

    /// Fast-forward to the upstream of the current branch. Exclusive with
    /// [`.rev`](GitHandle::rev).
    #[track_caller]
    pub fn latest(&self) -> Self {
        let mut clash = false;
        self.0.edit(|op| {
            if let OpSpec::Git(g) = &mut op.spec {
                clash = matches!(g.follow, Follow::Rev(_));
                g.follow = Follow::Latest;
            }
        });
        if clash {
            self.fail("git: .rev() and .latest() are exclusive")
        } else {
            self.clone()
        }
    }

    /// The path of the checkout. `~` and `~/...` expand in the op's shell.
    ///
    /// Takes `self` by value so this is the inherent method, not
    /// [`Into::into`](Into::into) (every type implements that, and a
    /// by-value name match hides an `&self` method).
    #[track_caller]
    pub fn into(self, path: &str) -> Self {
        if !path_ok(path) {
            return self.fail("git: the path must be a non-empty single line");
        }
        self.0.edit(|op| {
            if let OpSpec::Git(g) = &mut op.spec {
                g.path = path.to_string();
                op.writes = op.spec.default_writes();
                op.post = op.spec.default_post();
            }
        });
        self
    }
}

impl<V> Host<V> {
    /// Check out `url` (finish the chain with [`.into`](GitHandle::into)).
    /// Runs in this handle's shell, including `as_user`.
    #[track_caller]
    pub fn git(&self, url: &str) -> GitHandle {
        let h = GitHandle(self.op(OpSpec::Git(GitSpec::new(url))));
        if !url_ok(url) {
            return h.fail("git: the url must be a non-empty single line without whitespace");
        }
        h
    }
}
