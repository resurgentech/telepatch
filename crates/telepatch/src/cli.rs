//! The playbook runner: a playbook repo's own binary calls [`main`] with its
//! playbooks. Subcommands: `list`, `plan <name> [--distro D] [--vars f]`,
//! `run <name> (--local | --host <dest>... | --inventory <file> [--limit n,..])
//! [--ssh-key <path>] [--ssh-option K=V]... [--vars f] [--log f]
//! [--serial n] [--max-fail n] [--only a,b] [--check [--diff]]`.
//!
//! `run` takes exactly one of `--local` (this machine), `--host`
//! (repeatable; each `<dest>` goes to `ssh` verbatim, `user@host` or
//! `ssh://user@host:port`, and is the host's name in the report) and
//! `--inventory` (the hosts of an `inventory.toml`, see [`crate::inventory`];
//! a host's name there is its `<dest>`; `--limit` narrows them to the named
//! groups and hosts, an unknown name a usage error). Hosts run in parallel;
//! with `--local` and `--host` each has the same vars, with `--inventory`
//! its own, and `--vars` is the lowest place. `--ssh-key` and `--ssh-option`
//! apply to every ssh host and need `--host` or `--inventory`.
//!
//! `--serial n` applies to `n` hosts at a time (n >= 1; hosts start in the
//! order given). `--max-fail n` stops starting hosts once more than `n` have
//! failed (failed, blocked or error): hosts already started finish, each one
//! never started is reported `skipped` with `max-fail n reached`, and the run
//! exits 1. It holds back only hosts `--serial` has queued. `--only a,b` runs
//! the named roles' bodies and skips the rest (see
//! [`crate::host::Host::with_only`]): a role called inside a named role runs
//! with it, and a name the playbook did not trace is an error on that host
//! listing the roles it traced. All three also apply under `--check`.
//!
//! `--check` traces and prepares every host and applies nothing
//! ([`crate::executor::run_check`]); `--diff` (only with `--check`) adds a
//! unified diff per file that would change, none for a secret file. The
//! exit code is then 0 nothing to change, 2 changes pending, 1 a host
//! blocked. A usage error also exits 2: it prints usage, not a check.
//!
//! Vars are TOML layered into the playbook repo's `V` per role section
//! (see [`crate::vars`]); without `--vars` and the playbook's defaults, `V::default()`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;

use clap::{ArgGroup, Parser, Subcommand};
use serde::de::DeserializeOwned;

use crate::executor::{CheckOpts, HostSpec, RunControls, RunOpts, Stdout, Target, run_with};
use crate::facts::os::Os;
use crate::inventory::{InvHost, Inventory};
use crate::role::Playbook;
use crate::secret::SecretBackend;
use crate::testing::{self, FixtureHost};
use crate::transport::local::LocalTransport;
use crate::transport::ssh::SshTransport;
use crate::vars::VarLayer;

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
    /// Apply the playbook to this machine or to hosts over SSH.
    #[command(group(
        ArgGroup::new("target").required(true).args(["local", "host", "inventory"])
    ))]
    Run {
        name: String,
        /// Apply to this machine (uid 0, else `sudo -n -H bash`).
        #[arg(long)]
        local: bool,
        /// Apply over SSH to this destination (`user@host`, or
        /// `ssh://user@host:port`), passed to ssh verbatim and used as the
        /// host's name in the report. Repeatable; hosts run in parallel.
        #[arg(long)]
        host: Vec<String>,
        /// Run on the hosts of this `inventory.toml` (see
        /// `telepatch::inventory`), each with its own vars and groups.
        /// Not with `--local` or `--host`.
        #[arg(long)]
        inventory: Option<PathBuf>,
        /// With `--inventory`: only these groups and hosts (comma-separated
        /// or repeated). An unknown name is an error.
        #[arg(long, value_delimiter = ',')]
        limit: Vec<String>,
        /// Private key for every `--host`: `-i <path> -o IdentitiesOnly=yes`.
        /// Without it your ssh config and agent pick the key.
        #[arg(long, conflicts_with = "local")]
        ssh_key: Option<PathBuf>,
        /// Extra `-o K=V` for ssh on every `--host`. Repeatable.
        #[arg(long = "ssh-option", value_name = "K=V", conflicts_with = "local",
              value_parser = parse_ssh_option)]
        ssh_option: Vec<String>,
        /// TOML file deserialized into the playbook's vars (the same for
        /// every host; with `--inventory` the lowest place, under the
        /// inventory's).
        #[arg(long)]
        vars: Option<PathBuf>,
        /// Write JSONL events here.
        #[arg(long)]
        log: Option<PathBuf>,
        /// Trace and prepare, apply nothing.
        #[arg(long)]
        check: bool,
        /// With `--check`: a unified diff for every file that will change.
        #[arg(long, requires = "check")]
        diff: bool,
        /// Run on at most this many hosts at a time (at least 1).
        #[arg(long, value_name = "N", value_parser = parse_serial)]
        serial: Option<usize>,
        /// Stop starting hosts once more than this many have failed (failed,
        /// blocked or error); hosts already started finish. Holds back only
        /// hosts `--serial` has queued.
        #[arg(long, value_name = "N")]
        max_fail: Option<usize>,
        /// Run only these roles (comma-separated or repeated); the rest are
        /// skipped. A role called inside a named role runs with it. A name
        /// the playbook did not trace is an error listing those it did.
        #[arg(long, value_delimiter = ',')]
        only: Vec<String>,
    },
}

fn parse_serial(s: &str) -> Result<usize, String> {
    match s.parse::<usize>() {
        Ok(0) => Err("must be at least 1".into()),
        Ok(n) => Ok(n),
        Err(_) => Err(format!("`{s}` is not a number")),
    }
}

fn parse_ssh_option(s: &str) -> Result<String, String> {
    match s.split_once('=') {
        Some((k, _)) if !k.is_empty() => Ok(s.to_string()),
        _ => Err(format!("`{s}` is not K=V")),
    }
}

/// A playbook binary: [`main`] returns this, [`.secrets`](Main::secrets)
/// registers stores, [`.run`](Main::run) parses args and runs.
pub struct Main<'a, V> {
    playbooks: &'a [Playbook<V>],
    backends: Vec<Box<dyn SecretBackend>>,
}

/// Parse `std::env::args` once [`.run`](Main::run) is called. Each
/// playbook's own defaults ([`Playbook::defaults`]) are the third of the
/// four layers (see [`crate::vars`]).
pub fn main<V: DeserializeOwned + Default + 'static>(playbooks: &[Playbook<V>]) -> Main<'_, V> {
    Main {
        playbooks,
        backends: Vec::new(),
    }
}

impl<'a, V: DeserializeOwned + Default + 'static> Main<'a, V> {
    /// Register a secret store. Built-in `env`, `file` and `prompt` stay
    /// available. The first extra backend with a scheme wins over later ones.
    pub fn secrets(mut self, backend: impl SecretBackend + 'static) -> Self {
        self.backends.push(Box::new(backend));
        self
    }

    /// Parse args, run, print, return the exit code.
    pub fn run(self) -> ExitCode {
        let args: Vec<String> = std::env::args().collect();
        let playbooks = self.playbooks;
        let code = crate::secret::with_backends(self.backends, || {
            let (text, code) = dispatch(
                &args,
                playbooks,
                LocalTransport::new,
                SshTransport::new,
                Stdout::Inherit,
            );
            print!("{text}");
            code
        });
        ExitCode::from(code as u8)
    }
}

/// [`main`] without the process: returns everything it would print (op
/// output stream included) and the exit code. `local` builds the transport
/// for `run --local`; tests pass one with the uid injected as 0. `ssh`
/// builds the (unconfigured) transport for each `run --host`; tests pass
/// one that runs a stub `ssh`. `--ssh-key`/`--ssh-option` are applied on
/// top of what it returns.
pub fn run_with_args<V: DeserializeOwned + Default + 'static>(
    args: &[String],
    playbooks: &[Playbook<V>],
    local: impl Fn() -> LocalTransport,
    ssh: impl Fn() -> SshTransport,
) -> (String, i32) {
    let buf = Rc::new(RefCell::new(String::new()));
    let (text, code) = dispatch(args, playbooks, local, ssh, Stdout::Capture(buf.clone()));
    let streamed = buf.borrow().clone();
    (streamed + &text, code)
}

fn dispatch<V: DeserializeOwned + Default + 'static>(
    args: &[String],
    playbooks: &[Playbook<V>],
    local: impl Fn() -> LocalTransport,
    ssh: impl Fn() -> SshTransport,
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
            let vars = match crate::vars::load_file_for::<V>(pb, vars.as_deref()) {
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
            name,
            vars,
            log,
            host: dests,
            inventory,
            limit,
            ssh_key,
            ssh_option,
            check,
            diff,
            serial,
            max_fail,
            only,
            ..
        } => {
            let pb = match find(&name) {
                Ok(p) => p,
                Err(m) => return err(m),
            };
            if inventory.is_none() && !limit.is_empty() {
                return err("--limit needs --inventory".into());
            }
            // Vars are loaded once per host so `V` needs no `Clone`.
            let mut hosts = Vec::new();
            if let Some(path) = &inventory {
                match inventory_hosts(pb, path, vars.as_deref(), &limit) {
                    Ok(list) => {
                        for (name, groups, vars) in list {
                            let mut t = ssh();
                            if let Some(k) = &ssh_key {
                                t = t.identity(k);
                            }
                            for o in &ssh_option {
                                t = t.option(o);
                            }
                            hosts.push(HostSpec {
                                name,
                                target: Target::Ssh(t),
                                groups,
                                vars,
                            });
                        }
                    }
                    Err(m) => return err(m),
                }
            } else if dests.is_empty() {
                match crate::vars::load_file_for::<V>(pb, vars.as_deref()) {
                    Ok(vars) => hosts.push(HostSpec {
                        groups: Vec::new(),
                        name: "local".into(),
                        target: Target::Local(local()),
                        vars,
                    }),
                    Err(m) => return err(m),
                }
            }
            for dest in dests {
                let vars = match crate::vars::load_file_for::<V>(pb, vars.as_deref()) {
                    Ok(v) => v,
                    Err(m) => return err(m),
                };
                let mut t = ssh();
                if let Some(k) = &ssh_key {
                    t = t.identity(k);
                }
                for o in &ssh_option {
                    t = t.option(o);
                }
                hosts.push(HostSpec {
                    groups: Vec::new(),
                    name: dest,
                    target: Target::Ssh(t),
                    vars,
                });
            }
            crate::local::set_anchor(vars.as_deref().and_then(|p| {
                p.parent().map(|d| {
                    if d.as_os_str().is_empty() {
                        std::path::PathBuf::from(".")
                    } else {
                        d.to_path_buf()
                    }
                })
            }));
            let opts = RunOpts { log, stdout };
            let controls = RunControls {
                serial,
                max_fail,
                only,
            };
            let report = run_with(
                pb,
                hosts,
                opts,
                check.then_some(CheckOpts { diff }),
                controls,
            );
            (format!("\n{}", report.render()), report.exit_code())
        }
    }
}

/// The hosts of the inventory at `path` that `limit` leaves in (all with no
/// limit), each as (name, groups, vars). Every `vars` of the file is checked
/// first, so a bad key in a group the limit leaves out still stops the run.
fn inventory_hosts<V: DeserializeOwned>(
    pb: &Playbook<V>,
    path: &std::path::Path,
    vars: Option<&std::path::Path>,
    limit: &[String],
) -> Result<Vec<(String, Vec<String>, V)>, String> {
    let inv = Inventory::load(path)?;
    inv.check_vars::<V>()?;
    let file = match vars {
        None => None,
        Some(p) => {
            let text = std::fs::read_to_string(p)
                .map_err(|e| format!("cannot read vars file {}: {e}", p.display()))?;
            Some(VarLayer::parse(&p.display().to_string(), &text)?)
        }
    };
    let defaults = pb.defaults_toml()?;
    let picked: Vec<&InvHost> = if limit.is_empty() {
        inv.hosts().iter().collect()
    } else {
        inv.limit(limit)?
    };
    if picked.is_empty() {
        return Err(format!("inventory {} names no hosts", path.display()));
    }
    picked
        .into_iter()
        .map(|h| {
            let v = inv.vars_for::<V>(h, file.as_ref(), &defaults)?;
            Ok((h.name.clone(), h.groups.clone(), v))
        })
        .collect()
}

fn local_os() -> Result<Os, String> {
    let text = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .map_err(|e| format!("cannot read this host's os-release: {e}"))?;
    Os::parse(&text).map_err(|e| e.to_string())
}
