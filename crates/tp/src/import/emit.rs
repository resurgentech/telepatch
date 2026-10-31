//! One role's tasks to one Rust module.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::cond::{self, Analysis};
use super::jinja::{self, LIST_VARS};
use crate::ansible::{IncludeFrame, Task};

/// One thing the emitter left for the human.
#[derive(Debug, Clone)]
pub struct Todo {
    pub loc: String,
    /// Short, stable label; the report groups by it.
    pub reason: String,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct RoleReport {
    pub name: String,
    /// Tasks, not counting the include tasks that only pull files in.
    pub tasks: usize,
    /// Tasks emitted with nothing left to do.
    pub mapped: usize,
    pub todos: Vec<Todo>,
}

const INCLUDES: &[&str] = &[
    "include_tasks",
    "import_tasks",
    "include",
    "include_role",
    "import_role",
];

fn short(m: &str) -> &str {
    m.rsplit('.').next().unwrap_or(m)
}

enum Item<'a> {
    Task(&'a Task),
    Scope(&'a IncludeFrame, Vec<Item<'a>>),
}

/// Rebuild the include nesting from the flat list: each task carries its
/// chain of include frames, outermost first.
fn build_tree<'a>(tasks: &[&'a Task]) -> Vec<Item<'a>> {
    let mut frames: Vec<&'a IncludeFrame> = Vec::new();
    let mut stack: Vec<Vec<Item<'a>>> = vec![Vec::new()];
    for t in tasks {
        if INCLUDES.contains(&short(&t.module)) {
            continue;
        }
        let same = |a: &IncludeFrame, b: &IncludeFrame| a.file == b.file && a.line == b.line;
        let mut keep = 0;
        while keep < frames.len()
            && keep < t.include_chain.len()
            && same(frames[keep], &t.include_chain[keep])
        {
            keep += 1;
        }
        while frames.len() > keep {
            let kids = stack.pop().unwrap();
            let f = frames.pop().unwrap();
            stack.last_mut().unwrap().push(Item::Scope(f, kids));
        }
        for f in &t.include_chain[keep..] {
            frames.push(f);
            stack.push(Vec::new());
        }
        stack.last_mut().unwrap().push(Item::Task(t));
    }
    while let Some(f) = frames.pop() {
        let kids = stack.pop().unwrap();
        stack.last_mut().unwrap().push(Item::Scope(f, kids));
    }
    stack.pop().unwrap()
}

fn uses_item_user(items: &[Item]) -> bool {
    items.iter().any(|i| match i {
        Item::Task(t) => user_is_item(t),
        Item::Scope(_, k) => uses_item_user(k),
    })
}

fn user_is_item(t: &Task) -> bool {
    t.become_ == Some(true) && t.become_user.as_deref().map(str::trim) == Some("{{ item }}")
}

/// A register to fold into a later task's `unless =`.
struct Check {
    cmd: String,
    become_user: Option<String>,
}

fn truthy(s: &Option<String>) -> bool {
    matches!(
        s.as_deref().map(str::to_ascii_lowercase).as_deref(),
        Some("yes" | "true" | "on" | "1")
    )
}

fn is_sh(t: &Task) -> bool {
    matches!(short(&t.module), "shell" | "command")
}

/// The command text of a `shell`/`command` task.
fn command_text(t: &Task) -> Option<String> {
    match &t.args {
        Value::String(s) => Some(s.clone()),
        Value::Object(m) => m
            .get("cmd")
            .or_else(|| m.get("_raw_params"))
            .and_then(Value::as_str)
            .map(String::from),
        _ => None,
    }
}

fn plain_user(t: &Task) -> Option<String> {
    if t.become_ == Some(true) {
        t.become_user.clone()
    } else {
        None
    }
}

type Folds = (HashMap<String, Check>, HashSet<(String, usize)>);

/// Registers that can be folded: a check task (`register`, `changed_when:
/// false`, `ignore_errors`) whose every use is a lone `<reg>.rc != 0` on a
/// shell task that runs as the same user. A task file included twice shows
/// up twice in `tasks`; the decision is per source location, so every
/// instance must qualify.
fn find_folds(tasks: &[&Task]) -> Folds {
    let key = |t: &Task| (t.file.clone(), t.line);
    // Per candidate location: its check, and whether some instance disqualified it.
    let mut checks: HashMap<(String, usize), (String, Check)> = HashMap::new();
    let mut bad: HashSet<(String, usize)> = HashSet::new();
    let mut used: HashSet<(String, usize)> = HashSet::new();
    // reg -> location of the most recent check that registered it.
    let mut current: HashMap<String, ((String, usize), usize)> = HashMap::new();
    for (i, t) in tasks.iter().enumerate() {
        let mut whens: Vec<&String> = t.when.iter().chain(&t.inherited_when).collect();
        let frame_whens: Vec<&String> =
            t.include_chain.iter().flat_map(|f| f.when.iter()).collect();
        whens.extend(frame_whens.iter().copied());
        for (reg, (loc, ci)) in &current {
            let needle = format!("{reg}.");
            for w in &whens {
                if !w.contains(&needle) {
                    continue;
                }
                let a = cond::analyze(std::slice::from_ref(*w));
                let ok = a.rc_ne0.len() == 1
                    && a.rc_ne0[0] == *reg
                    && is_sh(t)
                    && !frame_whens.contains(w)
                    && *ci < i
                    && checks[loc].1.become_user == plain_user(t);
                if ok {
                    used.insert(loc.clone());
                } else {
                    bad.insert(loc.clone());
                }
            }
        }
        if let Some(reg) = &t.register {
            current.remove(reg);
            if is_sh(t)
                && t.changed_when.as_deref() == Some("false")
                && truthy(&t.ignore_errors)
                && t.when.is_empty()
                && t.inherited_when.is_empty()
                && t.loop_expr.is_none()
                && t.delegate_to.is_none()
                && t.failed_when.is_none()
                && t.notify.is_empty()
                && t.kw_args.is_null()
                && let Some(cmd) = command_text(t)
            {
                let cmd = cmd.trim_end().to_string();
                if jinja::render(&cmd, Some("item"), false).unmapped.is_empty() {
                    checks.entry(key(t)).or_insert_with(|| {
                        (
                            reg.clone(),
                            Check {
                                cmd,
                                become_user: plain_user(t),
                            },
                        )
                    });
                    current.insert(reg.clone(), (key(t), i));
                }
            }
        }
    }
    let mut folds = HashMap::new();
    let mut gone = HashSet::new();
    for (loc, (reg, c)) in checks {
        if used.contains(&loc) && !bad.contains(&loc) {
            gone.insert(loc);
            folds.insert(reg, c);
        }
    }
    (folds, gone)
}

#[derive(Clone)]
struct Cx {
    /// Inside a loop whose variable is `item`.
    in_loop: bool,
}

struct Em {
    out: String,
    ind: usize,
    todos: Vec<Todo>,
    /// Every task instance emitted: its location and whether it left a TODO.
    marks: Vec<(String, bool)>,
    folds: HashMap<String, Check>,
    gone: HashSet<(String, usize)>,
}

/// Emit the module for `role`; `tasks` are the loader's tasks for it.
pub fn emit_role(role: &str, fn_name: &str, tasks: &[&Task]) -> (String, RoleReport) {
    let (folds, gone) = find_folds(tasks);
    let tree = build_tree(tasks);
    let mut em = Em {
        out: String::new(),
        ind: 1,
        todos: Vec::new(),
        marks: Vec::new(),
        folds,
        gone,
    };
    em.items(&tree, &Cx { in_loop: false });
    let mut text = format!(
        "//! `{role}`: imported by `tp import`. Generated; do not edit.\n\
         //! Regenerate with `just import-regen`, then resolve the `TODO(port)` lines.\n\n\
         use telepatch::facts::os::Distro;\n\
         use telepatch::{{Host, TraceError, role, sh}};\n\n\
         use crate::flags::*;\n\
         use crate::vars::Vars;\n\n\
         #[role]\n\
         pub async fn {fn_name}(h: &Host<Vars>) -> Result<(), TraceError> {{\n"
    );
    text.push_str(&em.out);
    text.push_str("    Ok(())\n}\n");
    // A task file included twice is emitted twice; count and list it once.
    let mut locs: Vec<(String, bool)> = Vec::new();
    for (loc, had) in &em.marks {
        match locs.iter_mut().find(|(l, _)| l == loc) {
            Some((_, h)) => *h |= *had,
            None => locs.push((loc.clone(), *had)),
        }
    }
    let mut todos: Vec<Todo> = Vec::new();
    for t in em.todos {
        if !todos
            .iter()
            .any(|x| x.loc == t.loc && x.reason == t.reason && x.detail == t.detail)
        {
            todos.push(t);
        }
    }
    let report = RoleReport {
        name: role.to_string(),
        tasks: locs.len(),
        mapped: locs.iter().filter(|(_, had)| !had).count(),
        todos,
    };
    (text, report)
}

fn comment_safe(s: &str) -> String {
    s.replace("*/", "* /").replace('\n', " ")
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(n).collect::<String>())
    }
}

impl Em {
    fn line(&mut self, s: &str) {
        if s.is_empty() {
            self.out.push('\n');
            return;
        }
        for l in s.lines() {
            if l.is_empty() {
                self.out.push('\n');
            } else {
                self.out.push_str(&"    ".repeat(self.ind));
                self.out.push_str(l);
                self.out.push('\n');
            }
        }
    }

    fn todo(&mut self, loc: &str, reason: &str, detail: impl Into<String>) {
        let detail = detail.into();
        let tail = if detail.is_empty() {
            String::new()
        } else {
            format!(": {}", detail.replace('\n', " "))
        };
        self.line(&format!("// TODO(port): {loc} {reason}{tail}"));
        self.todos.push(Todo {
            loc: loc.to_string(),
            reason: reason.to_string(),
            detail,
        });
    }

    fn items(&mut self, items: &[Item], cx: &Cx) {
        for (n, it) in items.iter().enumerate() {
            if n > 0 {
                self.line("");
            }
            match it {
                Item::Task(t) => self.task(t, cx),
                Item::Scope(f, kids) => self.scope(f, kids, cx),
            }
        }
    }

    fn scope(&mut self, f: &IncludeFrame, kids: &[Item], cx: &Cx) {
        let loc = format!("{}:{}", f.file, f.line);
        self.line(&format!("// {loc} include {}", f.target));
        let a = cond::analyze(&f.when);
        self.unreachable_notes(&loc, &a);
        let mut cx2 = cx.clone();
        let mut closers = 0;
        if let Some(le) = &f.loop_expr {
            match loop_iter(le) {
                Ok(it) => self.line(&format!("for item in {it} {{")),
                Err(why) => {
                    self.todo(&loc, "loop not translated", format!("`{le}` ({why})"));
                    self.line("for item in std::iter::empty::<&str>() {");
                }
            }
            self.ind += 1;
            closers += 1;
            cx2.in_loop = true;
            if uses_item_user(kids) {
                self.line("let s = h.as_user(item);");
            }
        }
        let mut bad = a.bad.clone();
        bad.extend(
            a.rc_ne0
                .iter()
                .map(|r| format!("{r}.rc != 0 (on an include)")),
        );
        if !bad.is_empty() {
            self.todo(&loc, "when not translated", bad.join("; "));
            self.line(&format!(
                "if false /* TODO(port): when {} */ {{",
                comment_safe(&bad.join("; "))
            ));
            self.ind += 1;
            closers += 1;
        } else if !a.exprs.is_empty() {
            self.line(&format!("if {} {{", a.exprs.join(" && ")));
            self.ind += 1;
            closers += 1;
        }
        self.items(kids, &cx2);
        for _ in 0..closers {
            self.ind -= 1;
            self.line("}");
        }
    }

    fn unreachable_notes(&mut self, loc: &str, a: &Analysis) {
        for fl in &a.unreachable {
            self.todo(
                loc,
                "unreachable branch",
                format!("`{fl}`: no image reaches it; the design doc drops it"),
            );
        }
    }

    fn task(&mut self, t: &Task, cx: &Cx) {
        let loc = t.loc();
        let todo0 = self.todos.len();
        let name = t
            .name
            .as_ref()
            .map_or(String::new(), |n| format!(" {}", n.replace('\n', " ")));
        self.line(&format!("// {loc}{name}"));
        self.task_inner(t, cx, &loc);
        self.marks.push((loc, self.todos.len() != todo0));
    }

    fn task_inner(&mut self, t: &Task, cx: &Cx, loc: &str) {
        if t.is_handler {
            self.todo(
                loc,
                "handler",
                "handlers are not built; notify guards come later",
            );
            self.raw_comment(t);
            return;
        }
        if let Some(d) = &t.delegate_to {
            self.todo(
                loc,
                "delegate_to",
                format!("delegated to `{d}`, never guessed"),
            );
            self.raw_comment(t);
            return;
        }
        if self.gone.contains(&(t.file.clone(), t.line)) {
            self.line("// folded into the `unless =` of the task that tests it");
            return;
        }
        let m = short(&t.module);
        if m == "debug" || m == "meta" {
            self.line("// dropped: no effect on the host");
            return;
        }
        let conds: Vec<String> = t.inherited_when.iter().chain(&t.when).cloned().collect();
        let a = cond::analyze(&conds);
        self.unreachable_notes(loc, &a);
        let mut bad = a.bad.clone();
        // `<reg>.rc != 0` folds into `unless =`.
        let mut unless: Option<String> = None;
        if !a.rc_ne0.is_empty() {
            let ok = is_sh(t)
                && a.rc_ne0.len() == 1
                && self
                    .folds
                    .get(&a.rc_ne0[0])
                    .is_some_and(|c| c.become_user == plain_user(t));
            if ok {
                unless = Some(self.folds[&a.rc_ne0[0]].cmd.clone());
            } else {
                for r in &a.rc_ne0 {
                    bad.push(format!("{r}.rc != 0 (check not foldable)"));
                }
            }
        }
        let looped = t.loop_expr.is_some();
        if let Some(le) = &t.loop_expr {
            bad.push(format!("loop `{le}`"));
        }
        let stmt = self.statement(t, cx, loc, unless.as_deref());
        if !t.notify.is_empty() {
            self.todo(loc, "guard", format!("notify: {}", t.notify.join(", ")));
        }
        let body = match stmt {
            Ok(b) => b,
            Err((reason, detail)) => {
                match &t.loop_expr {
                    Some(le) => self.todo(
                        loc,
                        "task-level loop",
                        format!("`{le}` ({reason}: {detail})"),
                    ),
                    None => self.todo(loc, &reason, detail),
                }
                self.raw_comment(t);
                return;
            }
        };
        if !bad.is_empty() {
            let reason = if looped && a.bad.is_empty() && a.rc_ne0.is_empty() {
                "task-level loop"
            } else {
                "when not translated"
            };
            self.todo(loc, reason, bad.join("; "));
            self.line(&format!(
                "if false /* TODO(port): {} */ {{",
                comment_safe(&bad.join("; "))
            ));
            self.block(&body);
            self.line("}");
        } else if !a.exprs.is_empty() {
            self.line(&format!("if {} {{", a.exprs.join(" && ")));
            self.block(&body);
            self.line("}");
        } else {
            for l in &body {
                self.line(l);
            }
        }
    }

    fn block(&mut self, body: &[String]) {
        self.ind += 1;
        for l in body {
            self.line(l);
        }
        self.ind -= 1;
    }

    /// The module and its arguments as a comment, for tasks emitted as text.
    fn raw_comment(&mut self, t: &Task) {
        let args = serde_json::to_string(&t.args).unwrap_or_default();
        self.line(&format!("// {}: {}", t.module, trunc(&args, 200)));
    }

    /// The statement lines for a task, or `(reason, detail)` when it cannot
    /// be mapped (the caller then leaves a TODO and a comment).
    fn statement(
        &mut self,
        t: &Task,
        cx: &Cx,
        loc: &str,
        unless: Option<&str>,
    ) -> Result<Vec<String>, Fail> {
        let host = host_expr(t, cx)?;
        let m = short(&t.module);
        match m {
            "apt" | "dnf" | "yum" | "pacman" | "package" => pkg(t, &host, m),
            "pip" => pip(t, &host),
            "file" => file(t, &host),
            "copy" => copy(t, &host),
            "lineinfile" => lineinfile(t, &host),
            "shell" | "command" => self.sh(t, cx, loc, &host, unless),
            "set_fact" => Err((
                "set_fact".to_string(),
                "runtime variable; no mechanical mapping".to_string(),
            )),
            other => Err((
                format!("unmapped module `{other}`"),
                "no emitter mapping".to_string(),
            )),
        }
    }

    fn sh(
        &mut self,
        t: &Task,
        cx: &Cx,
        loc: &str,
        host: &str,
        unless: Option<&str>,
    ) -> Result<Vec<String>, Fail> {
        let lv = cx.in_loop.then_some("item");
        let Some(cmd) = command_text(t) else {
            return Err((
                "unsupported arguments".into(),
                "command given as argv or empty".into(),
            ));
        };
        let mut cmd = cmd.trim_end().to_string();
        if cmd.is_empty() {
            return Err(("unsupported arguments".into(), "empty command".into()));
        }
        let mut notes: Vec<String> = Vec::new();
        let mut creates = None;
        if let Some(kw) = t.kw_args.as_object() {
            for (k, v) in kw {
                match (k.as_str(), v.as_str()) {
                    ("creates", Some(c)) => creates = Some(c.to_string()),
                    ("executable", Some("/bin/bash" | "bash")) => {}
                    _ => {
                        return Err((
                            "unsupported arguments".into(),
                            format!("`args: {k}` on a command"),
                        ));
                    }
                }
            }
        }
        if let Value::Object(m) = &t.args {
            for (k, v) in m {
                match (k.as_str(), v.as_str()) {
                    ("cmd" | "_raw_params", _) => {}
                    ("creates", Some(c)) => creates = Some(c.to_string()),
                    _ => {
                        return Err((
                            "unsupported arguments".into(),
                            format!("command argument `{k}`"),
                        ));
                    }
                }
            }
        }
        if let Some(fw) = &t.failed_when {
            // Emitted live below; the YAML's failure rule is not carried over.
            self.todo(loc, "failed_when", fw.clone());
        }
        let mut changed_if = None;
        match t.changed_when.as_deref() {
            None => {}
            Some("false") => notes.push("changed_when: false (read-only in the YAML)".into()),
            Some(cw) => {
                let re = regex::Regex::new(r"^'([^']+)' in (\w+)\.(stdout|stderr)$").unwrap();
                match re.captures(cw.trim()) {
                    Some(c) if Some(c[2].to_string()) == t.register => {
                        changed_if = Some(regex::escape(&c[1]));
                        if &c[3] == "stderr" {
                            notes.push(
                                "the YAML tested stderr; changed_if sees combined output".into(),
                            );
                        }
                    }
                    _ => self.todo(loc, "changed_when", cw.to_string()),
                }
            }
        }
        if cmd.contains("set -o pipefail") {
            cmd = format!("({cmd})");
            notes.push("subshell keeps `pipefail` out of the persistent shell".into());
        }
        let user_is_loop = cx.in_loop && user_is_item(t);
        let c = jinja::render(&cmd, lv, false);
        let mut binds = c.binds.clone();
        let mut unmapped = c.unmapped.clone();
        let mut opts: Vec<String> = Vec::new();
        if let Some(u) = unless {
            let r = jinja::render(u.trim_end(), lv, false);
            binds.extend(r.binds);
            unmapped.extend(r.unmapped);
            opts.push(format!("unless = \"{}\"", r.lit));
        }
        if let Some(cr) = &creates {
            let r = jinja::render(cr, lv, user_is_loop);
            if user_is_loop && cr.contains("~{{") {
                notes.push("`~{{ item }}` is `~` in the user's own shell".into());
            }
            binds.extend(r.binds);
            unmapped.extend(r.unmapped);
            opts.push(format!("creates = \"{}\"", r.lit));
        }
        if let Some(ci) = &changed_if {
            opts.push(format!("changed_if = \"{}\"", jinja::escape(ci)));
        }
        if !unmapped.is_empty() {
            self.todo(loc, "Jinja not translated", unmapped.join(", "));
        }
        for n in notes {
            self.line(&format!("// {n}"));
        }
        let mut lines = Vec::new();
        for b in &binds {
            lines.push(format!("let {b} = &h.vars.{b};"));
        }
        let allow = if truthy(&t.ignore_errors) {
            ".allow_fail()"
        } else {
            ""
        };
        let call = if opts.is_empty() {
            format!("sh!({host}, \"{}\"){allow};", c.lit)
        } else {
            let mut s = format!("sh!(\n    {host},\n    \"{}\",\n", c.lit);
            for o in &opts {
                s.push_str(&format!("    {o},\n"));
            }
            s.push_str(&format!("){allow};"));
            s
        };
        if binds.is_empty() {
            return Ok(vec![call]);
        }
        lines.push(call);
        let mut v = vec!["{".to_string()];
        for l in &lines {
            for sub in l.lines() {
                v.push(format!("    {sub}"));
            }
        }
        v.push("}".into());
        Ok(v)
    }
}

fn loop_iter(expr: &str) -> Result<String, String> {
    let e = expr
        .strip_prefix("with_items:")
        .or_else(|| expr.strip_prefix("with_list:"))
        .unwrap_or(expr)
        .trim();
    let re = regex::Regex::new(r"^\{\{\s*(\w+)\s*\}\}$").unwrap();
    if let Some(c) = re.captures(e) {
        let v = &c[1];
        return if LIST_VARS.contains(&v) {
            Ok(format!("h.vars.{v}.iter()"))
        } else {
            Err(format!("variable `{v}` is not a known list in Vars"))
        };
    }
    if e.starts_with('[')
        && let Ok(items) = serde_json::from_str::<Vec<String>>(e)
        && items.iter().all(|i| !jinja::has_jinja(i))
    {
        let v: Vec<String> = items.iter().map(|i| lit(i)).collect();
        return Ok(format!("[{}]", v.join(", ")));
    }
    Err("not a list of literals or a known list variable".into())
}

type Fail = (String, String);

fn host_expr(t: &Task, cx: &Cx) -> Result<String, Fail> {
    if t.become_ != Some(true) {
        return Ok("h".into());
    }
    match t.become_user.as_deref().map(str::trim) {
        None => Ok("h".into()),
        Some("{{ item }}") if cx.in_loop => Ok("s".into()),
        Some(u) if !jinja::has_jinja(u) => Ok(format!("h.as_user({})", lit(u))),
        Some(u) => Err(("become_user not translated".into(), format!("`{u}`"))),
    }
}

fn args_map(t: &Task) -> Result<&serde_json::Map<String, Value>, Fail> {
    t.args.as_object().ok_or_else(|| {
        (
            "unsupported arguments".to_string(),
            "free-form arguments".to_string(),
        )
    })
}

fn only_keys(m: &serde_json::Map<String, Value>, ok: &[&str]) -> Result<(), Fail> {
    let extra: Vec<&str> = m
        .keys()
        .map(String::as_str)
        .filter(|k| !ok.contains(k))
        .collect();
    if extra.is_empty() {
        Ok(())
    } else {
        Err((
            "unsupported arguments".into(),
            format!("`{}`", extra.join("`, `")),
        ))
    }
}

fn strings(v: &Value) -> Option<Vec<String>> {
    match v {
        Value::String(s) => Some(vec![s.clone()]),
        Value::Array(a) => a.iter().map(|x| x.as_str().map(String::from)).collect(),
        _ => None,
    }
}

fn lit(s: &str) -> String {
    format!("\"{}\"", jinja::escape(s))
}

fn names_of(m: &serde_json::Map<String, Value>) -> Result<Vec<String>, Fail> {
    let names = m.get("name").and_then(strings).ok_or((
        "unsupported arguments".to_string(),
        "package names".to_string(),
    ))?;
    if names.iter().any(|n| jinja::has_jinja(n)) {
        return Err(("Jinja not translated".into(), "in a package name".into()));
    }
    Ok(names)
}

fn pkg(t: &Task, host: &str, module: &str) -> Result<Vec<String>, Fail> {
    let m = args_map(t)?;
    only_keys(m, &["name", "state"])?;
    let names = names_of(m)?;
    let tail = match m.get("state").and_then(Value::as_str) {
        None | Some("present" | "installed") => "",
        Some("build-dep") if module == "apt" => ".build_dep()",
        Some(s) => {
            return Err((
                "unsupported arguments".into(),
                format!("`state: {s}` on a package"),
            ));
        }
    };
    let method = if module == "yum" { "yum" } else { "pkg" };
    Ok(list_call(&format!("{host}.{method}"), &names, tail))
}

fn list_call(callee: &str, names: &[String], tail: &str) -> Vec<String> {
    let items: Vec<String> = names.iter().map(|n| lit(n)).collect();
    let one = format!("{callee}([{}]){tail};", items.join(", "));
    if one.len() <= 72 {
        return vec![one];
    }
    let mut v = vec![format!("{callee}([")];
    for i in items {
        v.push(format!("    {i},"));
    }
    v.push(format!("]){tail};"));
    v
}

fn pip(t: &Task, host: &str) -> Result<Vec<String>, Fail> {
    let m = args_map(t)?;
    only_keys(m, &["name", "state"])?;
    let names = names_of(m)?;
    let tail = match m.get("state").and_then(Value::as_str) {
        None | Some("present") => "",
        Some("latest") => ".latest()",
        Some(s) => {
            return Err((
                "unsupported arguments".into(),
                format!("`state: {s}` on pip"),
            ));
        }
    };
    Ok(list_call(&format!("{host}.pip"), &names, tail))
}

fn mode_of(v: &Value) -> Option<String> {
    let digits = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let d = digits.trim_start_matches('0');
    if !d.is_empty() && d.chars().all(|c| ('0'..='7').contains(&c)) {
        Some(format!("0o{d}"))
    } else {
        None
    }
}

fn path_arg<'a>(m: &'a serde_json::Map<String, Value>, keys: &[&str]) -> Result<&'a str, Fail> {
    let p = keys
        .iter()
        .find_map(|k| m.get(*k).and_then(Value::as_str))
        .ok_or(("unsupported arguments".to_string(), "no path".to_string()))?;
    if jinja::has_jinja(p) {
        return Err(("Jinja not translated".into(), format!("in path `{p}`")));
    }
    Ok(p)
}

fn mode_owner(m: &serde_json::Map<String, Value>, chain: &mut String) -> Result<(), Fail> {
    if let Some(v) = m.get("mode") {
        let mode = mode_of(v).ok_or(("unsupported arguments".to_string(), "mode".to_string()))?;
        chain.push_str(&format!(".mode({mode})"));
    }
    if let Some(o) = m.get("owner").and_then(Value::as_str) {
        if jinja::has_jinja(o) {
            return Err(("Jinja not translated".into(), "in owner".into()));
        }
        chain.push_str(&format!(".owner({})", lit(o)));
    }
    Ok(())
}

fn file(t: &Task, host: &str) -> Result<Vec<String>, Fail> {
    let m = args_map(t)?;
    only_keys(m, &["path", "dest", "name", "state", "mode", "owner"])?;
    let path = path_arg(m, &["path", "dest", "name"])?;
    let mut chain = format!("{host}.file({})", lit(path));
    match m.get("state").and_then(Value::as_str) {
        None | Some("file") => {
            if m.get("mode").is_none() && m.get("owner").is_none() {
                return Err((
                    "unsupported arguments".into(),
                    "file with nothing to ensure".into(),
                ));
            }
        }
        Some("absent") => chain.push_str(".absent()"),
        Some("directory") => chain.push_str(".dir()"),
        Some(s) => {
            return Err((
                "unsupported arguments".into(),
                format!("`state: {s}` on file"),
            ));
        }
    }
    mode_owner(m, &mut chain)?;
    Ok(vec![format!("{chain};")])
}

fn copy(t: &Task, host: &str) -> Result<Vec<String>, Fail> {
    let m = args_map(t)?;
    only_keys(m, &["dest", "content", "mode", "owner"])?;
    let path = path_arg(m, &["dest"])?;
    let content = m.get("content").and_then(Value::as_str).ok_or((
        "unsupported arguments".to_string(),
        "copy without content".to_string(),
    ))?;
    if jinja::has_jinja(content) {
        return Err(("Jinja not translated".into(), "in content".into()));
    }
    let mut chain = format!("{host}.file({}).content({})", lit(path), lit(content));
    mode_owner(m, &mut chain)?;
    Ok(vec![format!("{chain};")])
}

fn lineinfile(t: &Task, host: &str) -> Result<Vec<String>, Fail> {
    let m = args_map(t)?;
    only_keys(m, &["path", "dest", "line", "regexp", "state"])?;
    if m.get("state")
        .and_then(Value::as_str)
        .is_some_and(|s| s != "present")
    {
        return Err(("unsupported arguments".into(), "lineinfile state".into()));
    }
    let path = path_arg(m, &["path", "dest"])?;
    let line = m.get("line").and_then(Value::as_str).ok_or((
        "unsupported arguments".to_string(),
        "lineinfile without line".to_string(),
    ))?;
    if jinja::has_jinja(line) {
        return Err(("Jinja not translated".into(), "in line".into()));
    }
    let call = match m.get("regexp").and_then(Value::as_str) {
        Some(re) if !jinja::has_jinja(re) => format!(".line_re({}, {})", lit(re), lit(line)),
        Some(_) => return Err(("Jinja not translated".into(), "in regexp".into())),
        None => format!(".line({})", lit(line)),
    };
    Ok(vec![format!("{host}.file({}){call};", lit(path))])
}
