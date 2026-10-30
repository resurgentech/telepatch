//! The loader's output types. Plain data, `Serialize` so `--json` and the
//! future `tp import` can share them.

use serde::Serialize;

/// One step of the include chain that led to a task: the `include_tasks` (or
/// `include_role`) that pulled the file in, with its own `when` and `loop`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct IncludeFrame {
    /// File containing the include, relative to the base dir.
    pub file: String,
    pub line: usize,
    /// What the include pointed at (a file name or a role name).
    pub target: String,
    pub when: Vec<String>,
    pub loop_expr: Option<String>,
}

/// One Ansible task, flattened: blocks are expanded (their `when` lands in
/// `inherited_when`), includes are followed (they stay in the list as their
/// own tasks, and their children carry the [`IncludeFrame`]).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Task {
    /// Source file, relative to the base dir (never absolute).
    pub file: String,
    /// 1-based line of the task's first key.
    pub line: usize,
    pub name: Option<String>,
    /// Module name as written, FQCN included (`include_tasks`, `apt`, ...).
    pub module: String,
    /// The module's argument: a string for free-form (`shell: ls`), a map or
    /// list otherwise.
    pub args: serde_json::Value,
    /// The separate `args:` keyword (`creates`, `executable`, ...), if any.
    pub kw_args: serde_json::Value,
    pub when: Vec<String>,
    /// `when` conditions inherited from enclosing blocks and role entries.
    pub inherited_when: Vec<String>,
    /// `loop:` or `with_*:` as written (`with_items: x` keeps its prefix).
    pub loop_expr: Option<String>,
    pub register: Option<String>,
    #[serde(rename = "become")]
    pub become_: Option<bool>,
    pub become_user: Option<String>,
    pub changed_when: Option<String>,
    pub failed_when: Option<String>,
    pub ignore_errors: Option<String>,
    pub notify: Vec<String>,
    pub delegate_to: Option<String>,
    /// Role this task came from, if any.
    pub role: Option<String>,
    /// True for tasks from a `handlers:` list or a role's `handlers/`.
    pub is_handler: bool,
    /// Outermost include first.
    pub include_chain: Vec<IncludeFrame>,
}

impl Task {
    /// `file:line`, the reference used in every report.
    pub fn loc(&self) -> String {
        format!("{}:{}", self.file, self.line)
    }
}

/// Something the loader could not or would not follow.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Skipped {
    /// `galaxy_role`, `vars_prompt`, `unresolved_include`, `include_cycle`,
    /// `unparseable_file`, `no_module`; the classifier adds `delegate_to`.
    pub kind: String,
    pub file: String,
    pub line: usize,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Play {
    pub name: Option<String>,
    pub file: String,
    pub line: usize,
    pub hosts: Option<String>,
    pub has_vars_prompt: bool,
    /// Every task of the play in execution order (pre_tasks, roles, tasks,
    /// post_tasks, then handlers).
    pub tasks: Vec<Task>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Loaded {
    pub plays: Vec<Play>,
    pub skipped: Vec<Skipped>,
}
