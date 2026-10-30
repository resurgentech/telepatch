use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use tp::classify::{Options, classify_playbook};

/// telepatch tooling: understand and port Ansible roles.
#[derive(Parser)]
#[command(name = "tp", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Classify a playbook's tasks into the design's patterns and print the
    /// "Current method" section.
    Classify {
        /// Playbook to classify.
        playbook: PathBuf,
        /// Directory holding the roles. Default: the nearest `roles/`
        /// directory found by searching upward from the playbook.
        #[arg(long)]
        roles_path: Option<PathBuf>,
        /// Output paths are relative to this. Default: the parent of the
        /// roles directory.
        #[arg(long)]
        base: Option<PathBuf>,
        /// Print JSON instead of Markdown.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        None => ExitCode::SUCCESS,
        Some(Command::Classify {
            playbook,
            roles_path,
            base,
            json,
        }) => {
            match classify_playbook(&Options {
                playbook,
                roles_path,
                base,
            }) {
                Ok(report) => {
                    if json {
                        println!("{}", report.to_json());
                    } else {
                        print!("{}", report.to_markdown());
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("tp classify: {e}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}
