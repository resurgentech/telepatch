//! `tp import`: emit telepatch Rust from an Ansible role or playbook.
//!
//! The emitter is mechanical. What it maps (module to primitive, `register`
//! check plus `rc != 0` to `unless =`, `creates:`, `admin_users` loops to
//! `for` + `as_user`, distro-flag includes to `if flag(&h.os)`) it maps; the
//! rest is a `// TODO(port): file:line reason` next to the original command,
//! and a line in the report. It never guesses: untranslatable conditions
//! make the statement inert (`if false /* TODO */`), `delegate_to` tasks are
//! comments only.
//!
//! Output is one module per role plus `flags.rs` (the `group_vars` distro
//! flags) and `lib.rs`. `vars.rs` is the porter's: the emitted code refers
//! to `crate::vars::Vars`.

pub mod cond;
mod emit;
mod flags;
mod jinja;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

pub use emit::{RoleReport, Todo};

use crate::ansible::{Loader, Skipped, Task, find_roles_dir};

#[derive(Debug, Clone)]
pub struct Options {
    /// A role directory (has `tasks/`), a directory of roles, or a playbook.
    pub input: PathBuf,
    /// Where roles live. Default: the input's parent (role dir), the input
    /// itself (roles dir), or the nearest `roles/` above a playbook.
    pub roles_path: Option<PathBuf>,
    /// Paths in comments are relative to this. Default: the parent of the
    /// roles dir.
    pub base: Option<PathBuf>,
}

#[derive(Debug)]
pub struct ImportError(pub String);

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ImportError {}

#[derive(Debug)]
pub struct Output {
    /// `(file name, text)`: `lib.rs`, `flags.rs`, one `<role>.rs` per role.
    /// Not `vars.rs`.
    pub files: Vec<(String, String)>,
    pub roles: Vec<RoleReport>,
    pub skipped: Vec<Skipped>,
}

impl Output {
    pub fn file(&self, name: &str) -> Option<&str> {
        self.files
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, t)| t.as_str())
    }

    /// The per-role report, Markdown.
    pub fn report(&self) -> String {
        let mut o = String::from("# tp import report\n");
        for r in &self.roles {
            o.push_str(&format!("\n## {}\n\n", r.name));
            o.push_str(&format!(
                "- tasks: {}\n- mapped: {}\n- TODO: {}\n",
                r.tasks,
                r.mapped,
                r.todos.len()
            ));
            if !r.todos.is_empty() {
                o.push_str("\nTODO by reason:\n\n");
                let mut by: Vec<(&str, usize)> = Vec::new();
                for t in &r.todos {
                    match by.iter_mut().find(|(k, _)| *k == t.reason) {
                        Some((_, n)) => *n += 1,
                        None => by.push((&t.reason, 1)),
                    }
                }
                by.sort();
                for (k, n) in by {
                    o.push_str(&format!("- {k}: {n}\n"));
                }
                o.push_str("\nListing:\n\n");
                for t in &r.todos {
                    if t.detail.is_empty() {
                        o.push_str(&format!("- {} {}\n", t.loc, t.reason));
                    } else {
                        o.push_str(&format!(
                            "- {} {}: {}\n",
                            t.loc,
                            t.reason,
                            t.detail.replace('\n', " ")
                        ));
                    }
                }
            }
        }
        if !self.skipped.is_empty() {
            o.push_str("\n## Not followed by the loader\n\n");
            for s in &self.skipped {
                o.push_str(&format!(
                    "- {}:{} {}: {}\n",
                    s.file, s.line, s.kind, s.detail
                ));
            }
        }
        o
    }
}

static TMP: AtomicUsize = AtomicUsize::new(0);

fn ident(s: &str) -> String {
    let mut o: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if o.is_empty() || o.starts_with(|c: char| c.is_ascii_digit()) {
        o.insert(0, '_');
    }
    o
}

fn has_tasks(dir: &Path) -> bool {
    dir.join("tasks").is_dir()
}

pub fn import(opts: &Options) -> Result<Output, ImportError> {
    let input = opts
        .input
        .canonicalize()
        .map_err(|e| ImportError(format!("{}: {e}", opts.input.display())))?;
    let mut temp: Option<PathBuf> = None;
    let (playbook, roles_path) = if input.is_file() {
        let rp = opts.roles_path.clone().or_else(|| find_roles_dir(&input));
        (input.clone(), rp)
    } else {
        let (rp, names) = if has_tasks(&input) {
            let name = input
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| ImportError("role directory has no name".into()))?
                .to_string();
            let parent = input.parent().unwrap_or(&input).to_path_buf();
            (parent, vec![name])
        } else {
            let mut names: Vec<String> = std::fs::read_dir(&input)
                .map_err(|e| ImportError(format!("{}: {e}", input.display())))?
                .filter_map(Result::ok)
                .filter(|e| has_tasks(&e.path()))
                .filter_map(|e| e.file_name().into_string().ok())
                .collect();
            names.sort();
            if names.is_empty() {
                return Err(ImportError(format!(
                    "{}: not a role directory, a directory of roles or a playbook",
                    input.display()
                )));
            }
            (input.clone(), names)
        };
        let mut text = String::from("---\n- hosts: all\n  roles:\n");
        for n in &names {
            text.push_str(&format!("    - {n}\n"));
        }
        let p = std::env::temp_dir().join(format!(
            "tp-import-{}-{}.yml",
            std::process::id(),
            TMP.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::write(&p, text).map_err(|e| ImportError(format!("{}: {e}", p.display())))?;
        temp = Some(p.clone());
        (p, Some(opts.roles_path.clone().unwrap_or(rp)))
    };

    let base = opts
        .base
        .clone()
        .or_else(|| {
            roles_path
                .as_ref()
                .and_then(|r| r.canonicalize().ok())
                .and_then(|r| r.parent().map(Path::to_path_buf))
        })
        .or_else(|| playbook.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    let group_vars = roles_path
        .as_ref()
        .and_then(|r| r.canonicalize().ok())
        .and_then(|r| r.parent().map(|p| p.join("inventories/group_vars/all")))
        .and_then(|p| std::fs::read_to_string(p).ok());

    let loaded = Loader::new(base, roles_path).load_playbook(&playbook);
    if let Some(p) = temp {
        let _ = std::fs::remove_file(p);
    }
    let loaded = loaded.map_err(|e| ImportError(e.to_string()))?;

    // Group tasks by role (tasks outside any role go to `play_<n>`); a role
    // used by a later play again is emitted once.
    let mut groups: Vec<(String, Vec<&Task>)> = Vec::new();
    let mut seen_in: Vec<(String, usize)> = Vec::new();
    for (pi, play) in loaded.plays.iter().enumerate() {
        for t in &play.tasks {
            let key = t.role.clone().unwrap_or_else(|| format!("play_{}", pi + 1));
            match seen_in.iter().find(|(k, _)| *k == key) {
                Some((_, p)) if *p != pi => continue,
                Some(_) => {}
                None => {
                    seen_in.push((key.clone(), pi));
                    groups.push((key.clone(), Vec::new()));
                }
            }
            groups
                .iter_mut()
                .find(|(k, _)| *k == key)
                .unwrap()
                .1
                .push(t);
        }
    }

    let mut files = Vec::new();
    let mut roles = Vec::new();
    let mut mods = Vec::new();
    for (name, tasks) in &groups {
        let f = ident(name);
        let (text, report) = emit::emit_role(name, &f, tasks);
        files.push((format!("{f}.rs"), text));
        roles.push(report);
        mods.push(f);
    }

    let mut lib = String::from(
        "//! Imported by `tp import`. Generated; do not edit (`vars.rs` is the porter's).\n\
         //! Regenerate with `just import-regen`.\n\n\
         #![allow(\n\
         \x20   unused_imports,\n\
         \x20   unused_variables,\n\
         \x20   unused_parens,\n\
         \x20   unreachable_code,\n\
         \x20   clippy::all\n\
         )]\n\n\
         pub mod flags;\n\
         pub mod vars;\n\n",
    );
    for m in &mods {
        lib.push_str(&format!("#[rustfmt::skip]\npub mod {m};\n"));
    }
    if !loaded.skipped.is_empty() {
        lib.push('\n');
        for s in &loaded.skipped {
            lib.push_str(&format!(
                "// TODO(port): {}:{} {}: {}\n",
                s.file,
                s.line,
                s.kind,
                s.detail.replace('\n', " ")
            ));
        }
    }
    let gv_line = |name: &str| -> Option<usize> {
        let gv = group_vars.as_ref()?;
        gv.lines()
            .position(|l| l.starts_with(&format!("{name}:")))
            .map(|i| i + 1)
    };
    let mut all = vec![
        ("lib.rs".to_string(), lib),
        ("flags.rs".to_string(), flags::render(&gv_line)),
    ];
    all.extend(files);
    Ok(Output {
        files: all,
        roles,
        skipped: loaded.skipped,
    })
}
