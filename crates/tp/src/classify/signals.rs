//! Factual observations about a task for the Task intent skeleton. Every
//! signal is something a reader could confirm by looking at the YAML line;
//! none of them is a shape or an adequacy verdict. Those columns belong to
//! the agent.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::modules::short;
use super::patterns::command_text;
use crate::ansible::Task;

pub(super) static NET_TO_SH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(curl|wget)\b[^|\n]*\|\s*(sudo\s+)?(env\s+\S+\s+)*(\w+=\S+\s+)*(sh|bash)\b")
        .unwrap()
});
static DOWNLOADS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(curl|wget)\b|https?://").unwrap());
static VERSION_LIKE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b\d+\.\d+(\.\d+)?\b|\bversion\b").unwrap());
pub(super) static TOOL_INSTALLERS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(rustup|cargo\s+install|pip3?\s+install|npm\s+install\s+-g|go\s+install|make\s+install)\b")
        .unwrap()
});

fn one_line(s: &str, max: usize) -> String {
    let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > max {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    } else {
        s
    }
}

fn kw(t: &Task, key: &str) -> Option<String> {
    t.kw_args.get(key).map(|v| match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    })
}

fn arg<'a>(t: &'a Task, key: &str) -> Option<&'a Value> {
    t.args.as_object().and_then(|m| m.get(key))
}

fn names_of(v: &Value) -> Option<usize> {
    match v {
        Value::Array(a) => Some(a.len()),
        Value::String(_) => Some(1),
        _ => None,
    }
}

/// The module and the arguments that matter, in one phrase.
pub fn method(t: &Task) -> String {
    let m = short(&t.module);
    let mut s = match m {
        "shell" | "command" | "raw" | "script" => {
            let cmd = command_text(t).unwrap_or_default();
            format!("`{m}: {}`", one_line(&cmd, 90).replace('|', "\\|"))
        }
        "include_tasks" | "import_tasks" | "include_role" | "import_role" => {
            let target = match &t.args {
                Value::String(s) => s.clone(),
                Value::Object(o) => o
                    .get("file")
                    .or_else(|| o.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("?")
                    .to_string(),
                _ => "?".to_string(),
            };
            format!("`{m}` {target}")
        }
        "apt" | "dnf" | "yum" | "pacman" | "pip" | "package" => {
            let n = arg(t, "name")
                .or_else(|| arg(t, "pkg"))
                .and_then(names_of)
                .map_or(String::new(), |n| format!(" {n} name(s)"));
            let state = arg(t, "state")
                .and_then(Value::as_str)
                .map_or(String::new(), |s| format!(", state {s}"));
            let extra = ["update_cache", "upgrade", "cache_valid_time", "executable"]
                .iter()
                .filter(|k| arg(t, k).is_some())
                .map(|k| format!(", {k}"))
                .collect::<String>();
            format!("`{m}`{n}{state}{extra}")
        }
        _ => {
            let keys = t
                .args
                .as_object()
                .map(|o| o.keys().cloned().collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            if keys.is_empty() {
                format!("`{m}`")
            } else {
                format!("`{m}` ({keys})")
            }
        }
    };
    if let Some(c) = kw(t, "creates") {
        s.push_str(&format!(", creates {c}"));
    }
    if let Some(r) = kw(t, "removes") {
        s.push_str(&format!(", removes {r}"));
    }
    s
}

/// True when `cond` mentions the registered variable `reg` as a word.
fn mentions(cond: &str, reg: &str) -> bool {
    cond.match_indices(reg).any(|(i, _)| {
        let before = cond[..i].chars().next_back();
        let after = cond[i + reg.len()..].chars().next();
        !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
            && !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

/// Observations, in a fixed order, as short phrases.
pub fn signals(t: &Task, all: &[&Task]) -> Vec<String> {
    let m = short(&t.module);
    let is_cmd = matches!(m, "shell" | "command" | "raw" | "script");
    let cmd = command_text(t).unwrap_or_default();
    let args_text = t.args.to_string();
    let mut out = Vec::new();

    if !t.when.is_empty() {
        out.push(format!("when `{}`", one_line(&t.when.join(" and "), 60)));
    }
    if let Some(l) = &t.loop_expr {
        out.push(format!("loop `{}`", one_line(l, 40)));
    }
    if let Some(f) = t.include_chain.last() {
        if let Some(l) = &f.loop_expr {
            out.push(format!("included under loop `{}`", one_line(l, 40)));
        }
        if !f.when.is_empty() {
            out.push(format!(
                "included when `{}`",
                one_line(&f.when.join(" and "), 60)
            ));
        }
    }
    if let Some(u) = &t.become_user {
        out.push(format!("become_user `{u}`"));
    }
    if NET_TO_SH.is_match(&cmd) {
        out.push("downloads from the network and executes it".into());
    } else if is_cmd && DOWNLOADS.is_match(&cmd) || matches!(m, "get_url" | "uri") {
        out.push("downloads from the network".into());
    }
    if TOOL_INSTALLERS.is_match(&cmd) {
        out.push("installs a tool outside the package manager".into());
    }
    if VERSION_LIKE.is_match(&cmd) || (!is_cmd && VERSION_LIKE.is_match(&args_text)) {
        out.push("mentions a version".into());
    }
    if is_cmd && kw(t, "creates").is_none() && kw(t, "removes").is_none() {
        out.push("no creates".into());
    }
    if is_cmd
        && t.changed_when.is_none()
        && kw(t, "creates").is_none()
        && kw(t, "removes").is_none()
    {
        out.push(if t.when.is_empty() {
            "reports changed every run".into()
        } else {
            "reports changed whenever its when holds".into()
        });
    }
    if let Some(c) = &t.changed_when {
        out.push(format!("changed_when `{}`", one_line(c, 50)));
    }
    if let Some(f) = &t.failed_when {
        out.push(format!("failed_when `{}`", one_line(f, 50)));
    }
    if t.ignore_errors.is_some() {
        out.push("ignore_errors".into());
    }
    if let Some(r) = &t.register {
        let readers: Vec<String> = all
            .iter()
            .filter(|o| (o.file.as_str(), o.line) != (t.file.as_str(), t.line))
            .filter(|o| {
                o.when
                    .iter()
                    .chain(&o.inherited_when)
                    .any(|w| mentions(w, r))
                    || o.args.to_string().contains(&format!("{r}."))
            })
            .map(|o| format!("`{}`", o.loc()))
            .collect();
        if readers.is_empty() {
            out.push(format!("register `{r}`, read by nothing in scope"));
        } else {
            out.push(format!("register `{r}`, read by {}", readers.join(", ")));
        }
    }
    if !t.notify.is_empty() {
        out.push(format!("notify {}", t.notify.join(", ")));
    }
    if t.is_handler {
        out.push("handler".into());
    }
    out
}
