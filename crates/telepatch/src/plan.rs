//! Plan rendering: the readable form of a trace, used by snapshot tests and
//! later by `tp plan`.

use std::fmt::Write;

use crate::facts::os::Os;
use crate::key::Key;
use crate::trace::{Guard, Node, OpNode, Trace};

fn keys(set: &std::collections::BTreeSet<Key>) -> String {
    let v: Vec<String> = set.iter().map(|k| k.to_string()).collect();
    format!("[{}]", v.join(", "))
}

fn guard(g: &Guard) -> String {
    match g {
        Guard::Changed(n) => format!("changed({n})"),
        Guard::ExitOk(n) => format!("exit_ok({n})"),
        Guard::Not(g) => format!("!{}", guard(g)),
    }
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
        let _ = write!(out, " when {}", guard(g));
    }
    if op.allow_fail {
        out.push_str(" allow_fail");
    }
    if !op.post.is_empty() {
        let p: Vec<String> = op.post.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let _ = write!(out, " post [{}]", p.join(", "));
    }
    out.push('\n');
    for line in op.spec.script(os).lines() {
        let _ = writeln!(out, "      | {line}");
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
    out
}
