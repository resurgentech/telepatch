//! The `sh` op's body. `sh!` builds a [`ShellSpec`]; `h.raw(..)` is the toy
//! predecessor. Both leave `rc`/`changed`/`skipped` set for the trailer.

use crate::sh::ShellSpec;

/// Body of a `sh!` op (see [`ShellSpec::render`]).
pub fn body(spec: &ShellSpec) -> String {
    spec.render()
}

/// Body of a raw script: exit status becomes `rc`; never `changed`.
pub fn raw_body(script: &str) -> String {
    format!("{script}\nrc=$?\n")
}
