//! A tiny playbook binary: `hello plan hello --distro ubuntu2404`,
//! `hello run hello --local`. Built static by `just musl-example`.

use serde::Deserialize;
use std::process::ExitCode;
use telepatch::{Host, Playbook, PlaybookFuture, TraceError, role, sh};

#[derive(Deserialize, Default)]
struct Vars {
    #[serde(default)]
    greeting: String,
}

#[role]
async fn hello(h: &Host<Vars>) -> Result<(), TraceError> {
    sh!(h, "echo hello from telepatch");
    h.file("/tmp/telepatch-hello")
        .content(format!("{}\n", h.vars.greeting));
    Ok(())
}

fn entry(h: Host<Vars>) -> PlaybookFuture {
    Box::pin(async move { hello(&h).await })
}

fn main() -> ExitCode {
    telepatch::cli::main(&[Playbook::new("hello", entry)]).run()
}
