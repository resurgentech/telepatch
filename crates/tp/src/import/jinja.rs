//! Turning Ansible command text (with `{{ var }}`) into the contents of an
//! `sh!` string literal.
//!
//! `sh!` quotes every `{ident}` it interpolates and treats `{`/`}` as
//! special, so literal braces are doubled. Jinja that cannot be mapped keeps
//! its raw text (braces doubled, so the output still has `{{ x }}` in it):
//! the command compiles and fails loudly on the host rather than guessing.

use std::collections::BTreeSet;

/// Scalar `Vars` fields a command may interpolate.
pub const SCALAR_VARS: &[&str] = &["rustup_version", "cargo_version", "rustc_version"];
/// `Vars` fields that are lists of strings.
pub const LIST_VARS: &[&str] = &["admin_users"];

#[derive(Debug, Default)]
pub struct Rendered {
    /// Contents of the Rust string literal (escaped, no quotes).
    pub lit: String,
    /// Jinja that was left as raw text.
    pub unmapped: Vec<String>,
    /// `Vars` fields to bind to a local of the same name first.
    pub binds: BTreeSet<String>,
}

/// `loop_var`: the name `{{ x }}` maps to the Rust loop variable `item`.
/// `drop_tilde_user`: `~{{ loop_var }}` becomes `~` (for a task that runs as
/// that user, where the tilde is the user's own home).
pub fn render(src: &str, loop_var: Option<&str>, drop_tilde_user: bool) -> Rendered {
    let mut r = Rendered::default();
    let mut out = String::new();
    let mut rest = src;
    while let Some(start) = rest.find("{{") {
        push_text(&mut out, &rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            // Unterminated: treat the rest as text.
            push_text(&mut out, &rest[start..]);
            rest = "";
            break;
        };
        let expr = after[..end].trim();
        if Some(expr) == loop_var {
            if drop_tilde_user && out.ends_with('~') {
                // `~{{ item }}/x` in item's own shell is `~/x`.
            } else {
                out.push_str("{item}");
            }
        } else if SCALAR_VARS.contains(&expr) {
            out.push_str(&format!("{{{expr}}}"));
            r.binds.insert(expr.to_string());
        } else {
            r.unmapped.push(format!("{{{{ {expr} }}}}"));
            out.push_str(&format!("{{{{{{{{ {expr} }}}}}}}}"));
        }
        rest = &after[end + 2..];
    }
    push_text(&mut out, rest);
    if src.contains("{%") || src.contains("{#") {
        r.unmapped.push("{% ... %} statement".into());
    }
    r.lit = escape(&out);
    r
}

fn push_text(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '{' => out.push_str("{{"),
            '}' => out.push_str("}}"),
            c => out.push(c),
        }
    }
}

/// Escape for a Rust `"..."` literal.
pub fn escape(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

/// Does `s` hold any Jinja?
pub fn has_jinja(s: &str) -> bool {
    s.contains("{{") || s.contains("{%") || s.contains("{#")
}
