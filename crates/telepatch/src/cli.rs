//! The playbook runner: a playbook repo's own binary calls [`main`] with its
//! playbooks. Subcommands: `list`, `plan <name> [--distro D] [--vars f]`,
//! `run <name> --local [--vars f] [--log f]`.
//!
//! Vars are TOML deserialized into the playbook repo's `V`; without
//! `--vars`, `V::default()`.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;

use clap::{Parser, Subcommand};
use serde::de::DeserializeOwned;

use crate::executor::{HostSpec, RunOpts, Stdout, Target, run};
use crate::facts::os::Os;
use crate::role::Playbook;
use crate::testing::{self, FixtureHost};
use crate::transport::local::LocalTransport;

#[derive(Parser)]
#[command(about = "A telepatch playbook runner")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print the playbook names.
    List,
    /// Render the optimized plan without applying anything.
    Plan {
        name: String,
        /// TOML file deserialized into the playbook's vars.
        #[arg(long)]
        vars: Option<PathBuf>,
        /// Plan against an embedded os-release fixture instead of this host.
        #[arg(long)]
        distro: Option<String>,
    },
    /// Apply the playbook to this machine (uid 0, else `sudo -n bash`).
    Run {
        name: String,
        /// Required: the only target there is.
        #[arg(long, required = true)]
        local: bool,
        #[arg(long)]
        vars: Option<PathBuf>,
        /// Write JSONL events here.
        #[arg(long)]
        log: Option<PathBuf>,
    },
}

/// Parse `std::env::args`, run, print, return the exit code.
pub fn main<V: DeserializeOwned + Default + 'static>(playbooks: &[Playbook<V>]) -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let (text, code) = dispatch(&args, playbooks, LocalTransport::new, Stdout::Inherit);
    print!("{text}");
    ExitCode::from(code as u8)
}

/// [`main`] without the process: returns everything it would print (op
/// output stream included) and the exit code. `local` builds the transport
/// for `run --local`; tests pass one with the uid injected as 0.
pub fn run_with_args<V: DeserializeOwned + Default + 'static>(
    args: &[String],
    playbooks: &[Playbook<V>],
    local: impl Fn() -> LocalTransport,
) -> (String, i32) {
    let buf = Rc::new(RefCell::new(String::new()));
    let (text, code) = dispatch(args, playbooks, local, Stdout::Capture(buf.clone()));
    let streamed = buf.borrow().clone();
    (streamed + &text, code)
}

fn dispatch<V: DeserializeOwned + Default + 'static>(
    args: &[String],
    playbooks: &[Playbook<V>],
    local: impl Fn() -> LocalTransport,
    stdout: Stdout,
) -> (String, i32) {
    let cli = match Cli::try_parse_from(args) {
        Ok(c) => c,
        Err(e) => return (e.render().to_string(), e.exit_code()),
    };
    let err = |m: String| (format!("{m}\n"), 2);
    let find = |name: &str| {
        playbooks.iter().find(|p| p.name == name).ok_or_else(|| {
            let names: Vec<&str> = playbooks.iter().map(|p| p.name).collect();
            format!("unknown playbook `{name}`; available: {}", names.join(", "))
        })
    };
    match cli.cmd {
        Cmd::List => (
            playbooks.iter().map(|p| format!("{}\n", p.name)).collect(),
            0,
        ),
        Cmd::Plan { name, vars, distro } => {
            let pb = match find(&name) {
                Ok(p) => p,
                Err(m) => return err(m),
            };
            let vars = match load_vars::<V>(vars.as_deref()) {
                Ok(v) => v,
                Err(m) => return err(m),
            };
            let (host, note) = match distro {
                Some(d) => match Os::fixture(&d) {
                    Some(os) => (FixtureHost::new(d, os, Rc::new(vars)), ""),
                    None => {
                        let names: Vec<&str> =
                            crate::facts::os::FIXTURES.iter().map(|(n, _)| *n).collect();
                        return err(format!(
                            "unknown distro `{d}`; available: {}",
                            names.join(", ")
                        ));
                    }
                },
                None => match local_os() {
                    Ok(os) => (
                        FixtureHost::new("local", os, Rc::new(vars)),
                        "note: local plan; facts assumed absent (no probes)\n",
                    ),
                    Err(m) => return err(m),
                },
            };
            let plan =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| testing::plan(pb, &host)));
            match plan {
                Ok(p) => (format!("{note}{p}"), 0),
                Err(_) => err(format!("planning `{name}` failed (see the panic above)")),
            }
        }
        Cmd::Run {
            name, vars, log, ..
        } => {
            let pb = match find(&name) {
                Ok(p) => p,
                Err(m) => return err(m),
            };
            let vars = match load_vars::<V>(vars.as_deref()) {
                Ok(v) => v,
                Err(m) => return err(m),
            };
            let host = HostSpec {
                name: "local".into(),
                target: Target::Local(local()),
                vars,
            };
            let report = run(pb, vec![host], RunOpts { log, stdout });
            (format!("\n{}", report.render()), report.exit_code())
        }
    }
}

fn load_vars<V: DeserializeOwned + Default>(path: Option<&Path>) -> Result<V, String> {
    let Some(path) = path else {
        return Ok(V::default());
    };
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read vars file {}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("cannot parse vars file {}: {e}", path.display()))
}

fn local_os() -> Result<Os, String> {
    let text = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .map_err(|e| format!("cannot read this host's os-release: {e}"))?;
    Os::parse(&text).map_err(|e| e.to_string())
}
