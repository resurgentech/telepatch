//! Playbook loader: resolves roles through a roles path, follows
//! `include_tasks`/`import_tasks`/`include_role`, flattens blocks, and keeps
//! file:line on everything.
//!
//! Rules (recorded here because the output depends on them):
//! - A role is *galaxy* (not followed, reported as skipped) if its name
//!   contains a dot, or if no `<roles_path>/<name>/` directory exists.
//! - Include targets resolve relative to the including file's directory,
//!   then to the role's `tasks/` dir. A target containing `{{` cannot be
//!   resolved statically and is reported as skipped (the include task itself
//!   is kept).
//! - Role `meta/` dependencies are not followed.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::task::{IncludeFrame, Loaded, Play, Skipped, Task};
use super::yaml::{self, Node, Val};

const MAX_DEPTH: usize = 32;

/// Task keywords that are not module names.
const KEYWORDS: &[&str] = &[
    "name",
    "when",
    "loop",
    "register",
    "become",
    "become_user",
    "become_method",
    "changed_when",
    "failed_when",
    "ignore_errors",
    "ignore_unreachable",
    "notify",
    "listen",
    "delegate_to",
    "delegate_facts",
    "run_once",
    "tags",
    "vars",
    "environment",
    "no_log",
    "loop_control",
    "args",
    "until",
    "retries",
    "delay",
    "check_mode",
    "diff",
    "any_errors_fatal",
    "connection",
    "throttle",
    "timeout",
    "remote_user",
    "module_defaults",
    "collections",
    "port",
    "debugger",
    "async",
    "poll",
    "block",
    "rescue",
    "always",
];

#[derive(Debug)]
pub struct LoadError(pub String);

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for LoadError {}

/// Find the roles dir by searching upward from `playbook`'s directory for a
/// `roles/` directory. Returns its path.
pub fn find_roles_dir(playbook: &Path) -> Option<PathBuf> {
    let start = playbook.canonicalize().ok()?;
    let mut dir = start.parent();
    while let Some(d) = dir {
        let cand = d.join("roles");
        if cand.is_dir() {
            return Some(cand);
        }
        dir = d.parent();
    }
    None
}

pub struct Loader {
    /// Paths in output are relative to this.
    base: PathBuf,
    roles_path: Option<PathBuf>,
    skipped: Vec<Skipped>,
}

#[derive(Clone, Default)]
struct Ctx {
    role: Option<String>,
    chain: Vec<IncludeFrame>,
    inherited_when: Vec<String>,
    become_: Option<bool>,
    become_user: Option<String>,
    tasks_dir: Option<PathBuf>,
    handler: bool,
    /// Canonical paths of files currently being included (cycle guard).
    stack: Vec<PathBuf>,
}

impl Loader {
    pub fn new(base: PathBuf, roles_path: Option<PathBuf>) -> Self {
        let base = base.canonicalize().unwrap_or(base);
        let roles_path = roles_path.map(|p| p.canonicalize().unwrap_or(p));
        Loader {
            base,
            roles_path,
            skipped: Vec::new(),
        }
    }

    fn rel(&self, p: &Path) -> String {
        let p = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        match p.strip_prefix(&self.base) {
            Ok(r) => r.to_string_lossy().into_owned(),
            Err(_) => p.to_string_lossy().into_owned(),
        }
    }

    fn skip(&mut self, kind: &str, file: &Path, line: usize, detail: String) {
        let file = self.rel(file);
        self.skipped.push(Skipped {
            kind: kind.into(),
            file,
            line,
            detail,
        });
    }

    pub fn load_playbook(mut self, playbook: &Path) -> Result<Loaded, LoadError> {
        let docs = read_yaml(playbook)?;
        let mut plays = Vec::new();
        for doc in &docs {
            let Some(items) = doc.as_seq() else {
                return Err(LoadError(format!(
                    "{}: not a list of plays",
                    playbook.display()
                )));
            };
            for item in items {
                plays.push(self.load_play(playbook, item));
            }
        }
        Ok(Loaded {
            plays,
            skipped: self.skipped,
        })
    }

    fn load_play(&mut self, file: &Path, node: &Node) -> Play {
        let mut ctx = Ctx::default();
        if let Ok(c) = file.canonicalize() {
            ctx.stack.push(c);
        }
        let has_vars_prompt = node.get("vars_prompt").is_some();
        if let Some(vp) = node.get("vars_prompt") {
            let names: Vec<String> = vp
                .as_seq()
                .map(|s| s.iter().filter_map(|n| n.get("name")?.scalar()).collect())
                .unwrap_or_default();
            self.skip(
                "vars_prompt",
                file,
                vp.line,
                format!("interactive prompt for {}", names.join(", ")),
            );
        }
        ctx.become_ = node.get("become").and_then(Node::truthy);
        ctx.become_user = node.get("become_user").and_then(Node::scalar);
        let mut tasks = Vec::new();
        if let Some(n) = node.get("pre_tasks") {
            self.parse_tasks(n, file, &ctx, &mut tasks);
        }
        if let Some(roles) = node.get("roles").and_then(Node::as_seq) {
            for r in roles {
                self.load_role_entry(r, file, &ctx, &mut tasks);
            }
        }
        if let Some(n) = node.get("tasks") {
            self.parse_tasks(n, file, &ctx, &mut tasks);
        }
        if let Some(n) = node.get("post_tasks") {
            self.parse_tasks(n, file, &ctx, &mut tasks);
        }
        if let Some(n) = node.get("handlers") {
            let hctx = Ctx {
                handler: true,
                ..ctx.clone()
            };
            self.parse_tasks(n, file, &hctx, &mut tasks);
        }
        Play {
            name: node.get("name").and_then(Node::scalar),
            file: self.rel(file),
            line: node.line,
            hosts: node.get("hosts").and_then(Node::scalar),
            has_vars_prompt,
            tasks,
        }
    }

    fn load_role_entry(&mut self, entry: &Node, file: &Path, ctx: &Ctx, out: &mut Vec<Task>) {
        let (name, when) = match &entry.val {
            Val::Map(_) => (
                entry
                    .get("role")
                    .or_else(|| entry.get("name"))
                    .and_then(Node::scalar),
                entry.get("when").map(strings).unwrap_or_default(),
            ),
            _ => (entry.scalar(), Vec::new()),
        };
        let Some(name) = name else { return };
        let mut ctx = ctx.clone();
        ctx.inherited_when.extend(when);
        self.load_role(&name, file, entry.line, &ctx, None, out);
    }

    /// Load `tasks/<tasks_from or main>.yml` and `handlers/main.yml` of a role.
    fn load_role(
        &mut self,
        name: &str,
        from: &Path,
        line: usize,
        ctx: &Ctx,
        tasks_from: Option<&str>,
        out: &mut Vec<Task>,
    ) {
        let dir = self.roles_path.as_ref().map(|r| r.join(name));
        let dir = match dir {
            Some(d) if !name.contains('.') && d.is_dir() => d,
            _ => {
                let why = if name.contains('.') {
                    "name has a dot, treated as a galaxy role"
                } else {
                    "not found on the roles path, treated as a galaxy role"
                };
                self.skip("galaxy_role", from, line, format!("{name}: {why}"));
                return;
            }
        };
        let mut rctx = ctx.clone();
        rctx.role = Some(name.to_string());
        rctx.tasks_dir = Some(dir.join("tasks"));
        let entry = tasks_from.unwrap_or("main");
        if let Some(f) = find_yaml(&dir.join("tasks"), entry) {
            self.parse_file(&f, &rctx, out);
        }
        if let Some(f) = find_yaml(&dir.join("handlers"), "main") {
            let hctx = Ctx {
                handler: true,
                ..rctx
            };
            self.parse_file(&f, &hctx, out);
        }
    }

    fn parse_file(&mut self, path: &Path, ctx: &Ctx, out: &mut Vec<Task>) {
        let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if ctx.stack.contains(&canon) || ctx.stack.len() > MAX_DEPTH {
            let line = ctx.chain.last().map_or(1, |f| f.line);
            self.skip(
                "include_cycle",
                path,
                line,
                "file already being included".into(),
            );
            return;
        }
        let docs = match read_yaml(path) {
            Ok(d) => d,
            Err(e) => {
                self.skip("unparseable_file", path, 1, e.0);
                return;
            }
        };
        let mut ctx = ctx.clone();
        ctx.stack.push(canon);
        for d in &docs {
            self.parse_tasks(d, path, &ctx, out);
        }
    }

    fn parse_tasks(&mut self, list: &Node, file: &Path, ctx: &Ctx, out: &mut Vec<Task>) {
        let Some(items) = list.as_seq() else { return };
        for item in items {
            self.parse_task(item, file, ctx, out);
        }
    }

    fn parse_task(&mut self, node: &Node, file: &Path, ctx: &Ctx, out: &mut Vec<Task>) {
        if !matches!(node.val, Val::Map(_)) {
            return;
        }
        if node.get("block").is_some() {
            let mut bctx = ctx.clone();
            bctx.inherited_when
                .extend(node.get("when").map(strings).unwrap_or_default());
            if let Some(b) = node.get("become").and_then(Node::truthy) {
                bctx.become_ = Some(b);
            }
            if let Some(u) = node.get("become_user").and_then(Node::scalar) {
                bctx.become_user = Some(u);
            }
            for key in ["block", "rescue", "always"] {
                if let Some(n) = node.get(key) {
                    self.parse_tasks(n, file, &bctx, out);
                }
            }
            return;
        }

        let Val::Map(entries) = &node.val else { return };
        let mut delegate_to = node.get("delegate_to").and_then(Node::scalar);
        let (module, args) =
            if let Some(la) = node.get("local_action").or_else(|| node.get("action")) {
                if node.get("local_action").is_some() {
                    delegate_to = Some("localhost".into());
                }
                split_action(la)
            } else if let Some((k, v)) = entries
                .iter()
                .find(|(k, _)| !KEYWORDS.contains(&k.as_str()) && !k.starts_with("with_"))
            {
                (k.clone(), v.to_json())
            } else {
                self.skip(
                    "no_module",
                    file,
                    node.line,
                    "task has no module key".into(),
                );
                return;
            };

        let loop_expr = node.get("loop").map(show).or_else(|| {
            entries
                .iter()
                .find(|(k, _)| k.starts_with("with_"))
                .map(|(k, v)| format!("{k}: {}", show(v)))
        });

        let when = node.get("when").map(strings).unwrap_or_default();
        let task = Task {
            file: self.rel(file),
            line: node.line,
            name: node.get("name").and_then(Node::scalar),
            module: module.clone(),
            args,
            kw_args: node.get("args").map_or(Value::Null, Node::to_json),
            when: when.clone(),
            inherited_when: ctx.inherited_when.clone(),
            loop_expr: loop_expr.clone(),
            register: node.get("register").and_then(Node::scalar),
            become_: node.get("become").and_then(Node::truthy).or(ctx.become_),
            become_user: node
                .get("become_user")
                .and_then(Node::scalar)
                .or_else(|| ctx.become_user.clone()),
            changed_when: node.get("changed_when").and_then(Node::scalar),
            failed_when: node.get("failed_when").and_then(Node::scalar),
            ignore_errors: node.get("ignore_errors").and_then(Node::scalar),
            notify: node.get("notify").map(strings).unwrap_or_default(),
            delegate_to,
            role: ctx.role.clone(),
            is_handler: ctx.handler,
            include_chain: ctx.chain.clone(),
        };
        let short = module.rsplit('.').next().unwrap_or(&module).to_string();
        let args = task.args.clone();
        out.push(task);

        match short.as_str() {
            "include_tasks" | "import_tasks" | "include" => {
                let Some(target) = include_target(&args) else {
                    return;
                };
                self.follow_include(&target, node, file, ctx, when, loop_expr, out);
            }
            "include_role" | "import_role" => {
                let Some(role) = args.get("name").and_then(Value::as_str).map(String::from) else {
                    return;
                };
                let tasks_from = args
                    .get("tasks_from")
                    .and_then(Value::as_str)
                    .map(String::from);
                if role.contains("{{") {
                    self.skip(
                        "unresolved_include",
                        file,
                        node.line,
                        format!("role name `{role}` is templated"),
                    );
                    return;
                }
                let mut ictx = ctx.clone();
                ictx.chain.push(IncludeFrame {
                    file: self.rel(file),
                    line: node.line,
                    target: role.clone(),
                    when,
                    loop_expr,
                });
                self.load_role(&role, file, node.line, &ictx, tasks_from.as_deref(), out);
            }
            _ => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn follow_include(
        &mut self,
        target: &str,
        node: &Node,
        file: &Path,
        ctx: &Ctx,
        when: Vec<String>,
        loop_expr: Option<String>,
        out: &mut Vec<Task>,
    ) {
        if target.contains("{{") {
            self.skip(
                "unresolved_include",
                file,
                node.line,
                format!("`{target}` is templated"),
            );
            return;
        }
        let mut candidates = Vec::new();
        if let Some(d) = file.parent() {
            candidates.push(d.join(target));
        }
        if let Some(d) = &ctx.tasks_dir {
            candidates.push(d.join(target));
        }
        let Some(path) = candidates.into_iter().find(|p| p.is_file()) else {
            self.skip(
                "unresolved_include",
                file,
                node.line,
                format!("`{target}` not found"),
            );
            return;
        };
        let mut ictx = ctx.clone();
        ictx.chain.push(IncludeFrame {
            file: self.rel(file),
            line: node.line,
            target: target.to_string(),
            when,
            loop_expr,
        });
        self.parse_file(&path, &ictx, out);
    }
}

fn include_target(args: &Value) -> Option<String> {
    match args {
        Value::String(s) => Some(first_word(s)),
        Value::Object(m) => m
            .get("file")
            .or_else(|| m.get("_raw_params"))
            .and_then(Value::as_str)
            .map(first_word),
        _ => None,
    }
}

/// A free-form include may trail `key=value` args; a templated name keeps its
/// spaces (`{{ x }}.yml`).
fn first_word(s: &str) -> String {
    if s.contains("{{") {
        s.trim().to_string()
    } else {
        s.split_whitespace().next().unwrap_or("").to_string()
    }
}

/// `local_action:`/`action:` is either `module: x` + args, or a free-form
/// string whose first word is the module.
fn split_action(n: &Node) -> (String, Value) {
    match &n.val {
        Val::Map(m) => {
            let module = n.get("module").and_then(Node::scalar).unwrap_or_default();
            let rest: serde_json::Map<String, Value> = m
                .iter()
                .filter(|(k, _)| k != "module")
                .map(|(k, v)| (k.clone(), v.to_json()))
                .collect();
            (module, Value::Object(rest))
        }
        Val::Str(s) => {
            let mut it = s.splitn(2, char::is_whitespace);
            let module = it.next().unwrap_or("").to_string();
            (module, Value::String(it.next().unwrap_or("").to_string()))
        }
        _ => (String::new(), Value::Null),
    }
}

/// A scalar or list of scalars as a list of strings.
fn strings(n: &Node) -> Vec<String> {
    match &n.val {
        Val::Seq(s) => s.iter().filter_map(Node::scalar).collect(),
        _ => n.scalar().into_iter().collect(),
    }
}

fn show(n: &Node) -> String {
    match n.scalar() {
        Some(s) => s,
        None => n.to_json().to_string(),
    }
}

fn find_yaml(dir: &Path, stem: &str) -> Option<PathBuf> {
    ["yml", "yaml"]
        .iter()
        .map(|e| dir.join(format!("{stem}.{e}")))
        .find(|p| p.is_file())
}

fn read_yaml(path: &Path) -> Result<Vec<Node>, LoadError> {
    let text =
        std::fs::read_to_string(path).map_err(|e| LoadError(format!("{}: {e}", path.display())))?;
    yaml::parse(&text).map_err(|e| LoadError(format!("{}: {e}", path.display())))
}
