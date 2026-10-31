//! `tp classify`: sort every task of a playbook into the design's patterns,
//! a module category and a proposed telepatch primitive.
//!
//! Rules (also stated in the rendered report):
//! - A task is listed under every pattern it matches, so pattern counts
//!   overlap and do not sum to the task total.
//! - Tasks with no pattern but a mapped module are "plain": they only show in
//!   the category table.
//! - Tasks whose module has no mapping and no pattern are *unclassified*,
//!   listed in full.
//! - Counts are of distinct source tasks (file:line): a task file included
//!   twice is counted once.
//! - Tasks delegated to a non-controller host, `vars_prompt` plays, galaxy
//!   roles and unresolvable includes are *skipped*, never classified.

mod modules;
mod patterns;
mod render;
mod signals;

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

use crate::ansible::{LoadError, Loader, Skipped, Task, find_roles_dir};

#[derive(Debug, Clone)]
pub struct Options {
    pub playbook: PathBuf,
    /// Where roles live. Default: the nearest `roles/` directory found by
    /// searching upward from the playbook's directory.
    pub roles_path: Option<PathBuf>,
    /// Paths in the output are relative to this. Default: the directory that
    /// contains the roles dir, else the playbook's directory.
    pub base: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskRef {
    pub file: String,
    pub line: usize,
    pub name: Option<String>,
    pub module: String,
    pub category: Option<String>,
    pub primitive: Option<String>,
    /// `file:line` of the innermost include that pulled this task in.
    pub via: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PatternGroup {
    pub id: String,
    pub title: String,
    pub primitive: String,
    pub count: usize,
    pub tasks: Vec<TaskRef>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CategoryCount {
    pub category: String,
    pub count: usize,
    /// Task count per module as written.
    pub modules: BTreeMap<String, usize>,
    /// Distinct proposed primitives in this category.
    pub primitives: Vec<String>,
}

/// One row of the `## Task intent` skeleton: the factual columns the tool
/// fills (task, method, signals). The judgment columns (intent, shape,
/// adequate, primitive) are left to the agent; the tool never guesses them.
#[derive(Debug, Clone, Serialize)]
pub struct IntentRow {
    pub file: String,
    pub line: usize,
    pub name: Option<String>,
    /// The module and the arguments that matter, in a phrase.
    pub method: String,
    /// Observations about the task, not verdicts: `pipes network to sh`,
    /// `no creates`, `reports changed every run`, `register x, read by ...`.
    pub signals: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// The command, as shown in the provenance line.
    pub command: String,
    /// The Task intent skeleton, one row per in-scope task in load order.
    pub intent_rows: Vec<IntentRow>,
    /// In-scope tasks (excludes skipped ones).
    pub total_tasks: usize,
    /// In-scope tasks that matched no pattern and have a mapped module.
    pub plain_tasks: usize,
    pub patterns: Vec<PatternGroup>,
    pub categories: Vec<CategoryCount>,
    pub unclassified: Vec<TaskRef>,
    pub skipped: Vec<Skipped>,
}

#[derive(Debug)]
pub enum ClassifyError {
    Load(LoadError),
}

impl std::fmt::Display for ClassifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClassifyError::Load(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for ClassifyError {}

pub fn classify_playbook(opts: &Options) -> Result<Report, ClassifyError> {
    let roles_path = opts
        .roles_path
        .clone()
        .or_else(|| find_roles_dir(&opts.playbook));
    let base = opts
        .base
        .clone()
        .or_else(|| {
            roles_path
                .as_ref()
                .and_then(|r| r.parent().map(PathBuf::from))
        })
        .or_else(|| opts.playbook.parent().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    let base_canon = base.canonicalize().unwrap_or_else(|_| base.clone());
    let rel = |p: &PathBuf| {
        let c = p.canonicalize().unwrap_or_else(|_| p.clone());
        c.strip_prefix(&base_canon)
            .map_or_else(|_| c.display().to_string(), |r| r.display().to_string())
    };

    let mut command = format!("tp classify {}", rel(&opts.playbook));
    if let Some(r) = &opts.roles_path {
        command.push_str(&format!(" --roles-path {}", rel(r)));
    }

    let loaded = Loader::new(base, roles_path)
        .load_playbook(&opts.playbook)
        .map_err(ClassifyError::Load)?;
    let mut skipped = loaded.skipped;

    // Tasks delegated off the controller are out of scope.
    // A task reached through several includes (the same `per_user.yml` looped
    // over `admin_users` and over `[root]`) is one source task: count the
    // first occurrence only.
    let mut seen = std::collections::HashSet::new();
    let mut tasks: Vec<&Task> = Vec::new();
    for t in loaded.plays.iter().flat_map(|p| &p.tasks) {
        if !seen.insert((t.file.as_str(), t.line)) {
            continue;
        }
        if let Some(d) = patterns::delegates_elsewhere(t) {
            skipped.push(Skipped {
                kind: "delegate_to".into(),
                file: t.file.clone(),
                line: t.line,
                detail: format!(
                    "`{}` delegated to `{d}`",
                    t.name.as_deref().unwrap_or(&t.module)
                ),
            });
        } else {
            tasks.push(t);
        }
    }
    skipped.sort_by(|a, b| (&a.file, a.line, &a.kind).cmp(&(&b.file, b.line, &b.kind)));

    let hits = patterns::detect(&tasks);

    let mut groups: Vec<PatternGroup> = patterns::PATTERNS
        .iter()
        .map(|p| PatternGroup {
            id: p.id.into(),
            title: p.title.into(),
            primitive: p.primitive.into(),
            count: 0,
            tasks: Vec::new(),
        })
        .collect();
    let mut cats: BTreeMap<&'static str, CategoryCount> = BTreeMap::new();
    let mut unclassified = Vec::new();
    let mut plain = 0;

    for (t, h) in tasks.iter().zip(&hits) {
        let info = modules::info(&t.module);
        let mut primitive = info.map(|(_, p)| p.to_string());
        if let (Some(p), Some(prim)) = (&mut primitive, patterns::shell_primitive(t)) {
            *p = format!("{prim} (was `sh`)");
        }
        let r = TaskRef {
            file: t.file.clone(),
            line: t.line,
            name: t.name.clone(),
            module: t.module.clone(),
            category: info.map(|(c, _)| c.to_string()),
            primitive,
            via: t
                .include_chain
                .last()
                .map(|f| format!("{}:{}", f.file, f.line)),
        };
        for &i in h {
            groups[i].tasks.push(r.clone());
            groups[i].count += 1;
        }
        if info.is_none() && h.is_empty() {
            unclassified.push(r);
            continue;
        }
        // Mapped modules use their category; an unmapped module that still
        // matched a pattern is counted under "other" with no primitive.
        let (cat, prim) = info.map_or(("other", None), |(c, p)| (c, Some(p)));
        if h.is_empty() {
            plain += 1;
        }
        let c = cats.entry(cat).or_insert_with(|| CategoryCount {
            category: cat.into(),
            count: 0,
            modules: BTreeMap::new(),
            primitives: Vec::new(),
        });
        c.count += 1;
        *c.modules
            .entry(modules::short(&t.module).to_string())
            .or_default() += 1;
        if let Some(prim) = prim
            && !c.primitives.iter().any(|p| p == prim)
        {
            c.primitives.push(prim.to_string());
        }
    }

    let categories = modules::CATEGORY_ORDER
        .iter()
        .filter_map(|c| cats.remove(c))
        .collect();

    let intent_rows = tasks
        .iter()
        .map(|t| IntentRow {
            file: t.file.clone(),
            line: t.line,
            name: t.name.clone(),
            method: signals::method(t),
            signals: signals::signals(t, &tasks),
        })
        .collect();

    Ok(Report {
        command,
        intent_rows,
        total_tasks: tasks.len(),
        plain_tasks: plain,
        patterns: groups,
        categories,
        unclassified,
        skipped,
    })
}

impl Report {
    pub fn to_markdown(&self) -> String {
        render::markdown(self)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("report serializes")
    }

    /// The `## Task intent` skeleton for the understanding doc: factual
    /// columns filled, judgment columns empty.
    pub fn to_intent_skeleton(&self) -> String {
        render::intent_skeleton(self)
    }
}
