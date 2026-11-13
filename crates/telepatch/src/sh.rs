//! Runtime support for `sh!`: quoting and the shell op's script body.

use crate::host::{Host, OpHandle};
use crate::trace::{OpSpec, Site};

/// POSIX single-quote `s`: the result is one shell word whatever `s` holds.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The inputs of a `sh!` op. `unless`, `creates` and `changed_if` are
/// already interpolated (values quoted) by the macro.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShellSpec {
    pub cmd: String,
    /// Skip when this command exits 0 (run in the op's shell).
    pub unless: Option<String>,
    /// Skip when this path exists.
    pub creates: Option<String>,
    /// Regex (`grep -E`) over combined output; the op changed iff it matches.
    pub changed_if: Option<String>,
    /// The command that reverses this op, from `.undo("..")`. Recorded
    /// only: nothing runs it yet and nothing is captured on the host.
    pub undo: Option<String>,
    /// Where the `sh!` was written (`file!()`, `line!()` of the macro call);
    /// the plan and the run report warn about every such site. `None` for a
    /// spec built by hand.
    pub site: Option<Site>,
}

impl ShellSpec {
    /// The bash body. When run it leaves `rc`, `changed` and `skipped` set.
    /// With `changed_if` the command runs in a pipeline (a subshell) so its
    /// output can be captured while it streams.
    pub fn render(&self) -> String {
        let mut o = String::from("rc=0; changed=0; skipped=0\n");
        let mut conds = Vec::new();
        if let Some(u) = &self.unless {
            conds.push(format!("{{ {u}; }} >/dev/null 2>&1"));
        }
        if let Some(c) = &self.creates {
            conds.push(format!("[ -e {c} ]"));
        }
        let guarded = !conds.is_empty();
        if guarded {
            o.push_str(&format!(
                "if {}; then\n  skipped=1\nelse\n",
                conds.join(" || ")
            ));
        }
        match &self.changed_if {
            None => {
                o.push_str(&self.cmd);
                o.push_str("\nrc=$?\nif [ \"$rc\" -eq 0 ]; then changed=1; fi\n");
            }
            Some(re) => {
                o.push_str("tp_out=$(mktemp)\n{\n");
                o.push_str(&self.cmd);
                o.push_str("\n} 2>&1 | tee \"$tp_out\"\nrc=${PIPESTATUS[0]}\n");
                o.push_str(&format!(
                    "if [ \"$rc\" -eq 0 ] && grep -Eq {} \"$tp_out\"; then changed=1; fi\n",
                    quote(re)
                ));
                o.push_str("rm -f \"$tp_out\"\n");
            }
        }
        if guarded {
            o.push_str("fi\n");
        }
        o
    }
}

/// Used by the `sh!` expansion: append a shell op to `h`'s trace.
#[track_caller]
pub fn shell<V>(h: &Host<V>, spec: ShellSpec) -> OpHandle {
    h.op(OpSpec::Shell(spec))
}
