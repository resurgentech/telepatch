//! Plan rendering: the readable form of a trace, used by snapshot tests and
//! later by `tp plan`.

use std::fmt::Write;

use crate::engine::{Split, Window, WindowKind};
use crate::facts::os::Os;
use crate::key::Key;
use crate::trace::{Node, OpNode, Site, Trace, sh_sites};

fn keys(set: &std::collections::BTreeSet<Key>) -> String {
    let v: Vec<String> = set.iter().map(|k| k.to_string()).collect();
    format!("[{}]", v.join(", "))
}

fn op_line(out: &mut String, id: usize, op: &OpNode, os: &Os) {
    let _ = write!(
        out,
        "  #{id} {} as {} reads {} writes {}",
        op.spec.kind(),
        op.run_as,
        keys(&op.reads),
        keys(&op.writes)
    );
    if let Some(g) = &op.guard {
        let _ = write!(out, " when {g}");
    }
    if op.at_end {
        out.push_str(" at_end");
    }
    if op.allow_fail {
        out.push_str(" allow_fail");
    }
    if let Some(d) = op.timeout {
        let _ = write!(out, " timeout {}", crate::trace::fmt_duration(d));
    }
    if !op.post.is_empty() {
        let p: Vec<String> = op.post.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let _ = write!(out, " post [{}]", p.join(", "));
    }
    out.push('\n');
    for a in &op.allowed {
        let _ = writeln!(out, "      {a}");
    }
    // `script` is the redacted form: a secret is `<secret>`, not its value.
    for line in op.spec.script(os).lines() {
        let _ = writeln!(out, "      | {line}");
    }
}

/// The warning line for one `sh!` site.
pub fn sh_warning(site: &Site) -> String {
    format!("warning: sh! in a role body at {site}")
}

/// The plan's closing block: a blank line, then one warning per `sh!` site.
/// Empty when there is none.
fn sh_warnings(out: &mut String, sites: &[Site]) {
    if sites.is_empty() {
        return;
    }
    out.push('\n');
    for s in sites {
        let _ = writeln!(out, "{}", sh_warning(s));
    }
}

/// Render one host's trace: header, then per role each node in order.
pub fn render_host(name: &str, os: &Os, trace: &Trace) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "host {name} (os: {:?}/{:?} {})",
        os.family, os.distro, os.version
    );
    let _ = writeln!(out, "blocking reads: {}", trace.blocking_reads());
    let mut stack: Vec<&str> = Vec::new();
    let mut headed = false;
    for (i, node) in trace.nodes.iter().enumerate() {
        if let Node::RoleBegin(n) = node {
            stack.push(n);
            let _ = writeln!(out, "\nrole {}", stack.join(" > "));
            headed = true;
            continue;
        }
        if let Node::RoleEnd(_) = node {
            stack.pop();
            headed = !stack.is_empty();
            continue;
        }
        if !headed {
            out.push_str("\nrole (none)\n");
            headed = true;
        }
        match node {
            Node::Op(op) => op_line(&mut out, i, op, os),
            Node::FactRead { key, value, source } => {
                let _ = writeln!(out, "  #{i} fact {key} = {value} ({source:?})");
            }
            Node::Barrier => {
                let _ = writeln!(out, "  #{i} barrier");
            }
            Node::RoleBegin(_) | Node::RoleEnd(_) => unreachable!(),
        }
    }
    sh_warnings(&mut out, &trace.sh_sites());
    out
}

/// The role path (nested names joined with ` > `) each node was traced in;
/// empty for nodes outside any role. Indexed by node.
pub fn node_roles(trace: &Trace) -> Vec<String> {
    let mut stack: Vec<&str> = Vec::new();
    trace
        .nodes
        .iter()
        .map(|n| match n {
            Node::RoleBegin(name) => {
                stack.push(name);
                stack.join(" > ")
            }
            Node::RoleEnd(_) => {
                let r = stack.join(" > ");
                stack.pop();
                r
            }
            _ => stack.join(" > "),
        })
        .collect()
}

fn role_name(r: &str) -> &str {
    if r.is_empty() { "(none)" } else { r }
}

fn window_kind(k: WindowKind) -> &'static str {
    match k {
        WindowKind::NonMutating => "non-mutating",
        WindowKind::ImplicitBarrier => "implicit barrier",
        WindowKind::Final => "final",
        WindowKind::AtEnd => "at end",
    }
}

/// The plan's line for a window's split candidate, printed after the
/// installing op: `  -- split after #3 (pkg curl) when the host has neither
/// curl nor wget: #5 needs a download --`.
fn split_line(s: &Split) -> String {
    format!(
        "  -- split after {} (pkg {}) when the host has neither curl nor wget: {} needs a download --",
        s.installer, s.tool, s.consumer
    )
}

/// Render the windows an executor received for one host: per window, its
/// ops in order under role headings; a merged op lists the member ops and
/// the roles they came from; a split candidate is a line after the op that
/// installs the downloader.
pub fn render_windows(name: &str, os: &Os, blocking_reads: usize, windows: &[Window]) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "host {name} (os: {:?}/{:?} {})",
        os.family, os.distro, os.version
    );
    let _ = writeln!(out, "blocking reads: {blocking_reads}");
    for w in windows {
        let _ = writeln!(out, "\nwindow {} ({})", w.number, window_kind(w.kind));
        let mut role: Option<&str> = None;
        for (i, op) in w.ops.iter().enumerate() {
            // The role of the member whose position the op takes.
            let r = op
                .members
                .iter()
                .find(|m| m.id == op.id)
                .unwrap_or(&op.members[0])
                .role
                .as_str();
            if role != Some(r) {
                let _ = writeln!(out, "role {}", role_name(r));
                role = Some(r);
            }
            op_line(&mut out, op.id.0, &op.node, os);
            if op.members.len() > 1 {
                let from: Vec<String> = op
                    .members
                    .iter()
                    .map(|m| format!("{} {}", role_name(&m.role), m.id))
                    .collect();
                let _ = writeln!(out, "      merged from {}", from.join(", "));
            }
            if let Some(s) = w.split.as_ref().filter(|s| s.at == i) {
                let _ = writeln!(out, "{}", split_line(s));
            }
        }
    }
    sh_warnings(
        &mut out,
        &sh_sites(windows.iter().flat_map(|w| w.ops.iter().map(|o| &o.node))),
    );
    out
}
