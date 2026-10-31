use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use tp::check_understanding;
use tp::classify::{Options, classify_playbook};
use tp::import;

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
        /// Print the `## Task intent` skeleton instead: one row per task
        /// with Task, Method and Signals filled and the judgment columns
        /// (Intent, Shape, Adequate, Primitive) left empty.
        #[arg(long, conflicts_with = "json")]
        intent_skeleton: bool,
    },
    /// Emit telepatch Rust from an Ansible role, a directory of roles or a
    /// playbook: one module per role, `// TODO(port)` where the mapping is
    /// not mechanical.
    Import {
        /// Role directory, directory of roles, or playbook.
        input: PathBuf,
        /// Directory holding the roles (default: derived from the input).
        #[arg(long)]
        roles_path: Option<PathBuf>,
        /// Write the files (`lib.rs`, `flags.rs`, `<role>.rs`) into this
        /// directory instead of printing them.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Print the per-role report (Markdown). Without `--out` the code is
        /// then not printed.
        #[arg(long)]
        report: bool,
    },
    /// Check an understanding doc: required levels in order, dated sources,
    /// no hedge words, a `Decision:` line. Prints one line per problem and
    /// exits 1, or prints `ok`.
    CheckUnderstanding {
        /// Understanding doc (Markdown).
        doc: PathBuf,
        /// Also require the trial-only `## skill-feedback` section.
        #[arg(long)]
        require_feedback: bool,
        /// Also check that the Task intent table has one row per in-scope
        /// task of this playbook (the mirrored copy under conversion/).
        #[arg(long)]
        playbook: Option<PathBuf>,
        /// Roles directory for `--playbook` (default: found from the playbook).
        #[arg(long, requires = "playbook")]
        roles_path: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        None => ExitCode::SUCCESS,
        Some(Command::CheckUnderstanding {
            doc,
            require_feedback,
            playbook,
            roles_path,
        }) => match check_understanding::check_file(
            &doc,
            check_understanding::Options { require_feedback },
        )
        .and_then(|mut problems| {
            if let Some(pb) = playbook {
                let report = classify_playbook(&Options {
                    playbook: pb,
                    roles_path,
                    base: None,
                })
                .map_err(|e| e.to_string())?;
                let text = std::fs::read_to_string(&doc).map_err(|e| e.to_string())?;
                let rows = check_understanding::intent_row_count(&text);
                let want = report.intent_rows.len();
                if rows != want {
                    problems.push(format!(
                        "{}: Task intent has {rows} rows; the playbook has {want} in-scope tasks",
                        doc.display()
                    ));
                }
            }
            Ok(problems)
        }) {
            Ok(problems) if problems.is_empty() => {
                println!("ok");
                ExitCode::SUCCESS
            }
            Ok(problems) => {
                for p in problems {
                    println!("{p}");
                }
                ExitCode::FAILURE
            }
            Err(e) => {
                eprintln!("tp check-understanding: {e}");
                ExitCode::FAILURE
            }
        },
        Some(Command::Import {
            input,
            roles_path,
            out,
            report,
        }) => match import::import(&import::Options {
            input,
            roles_path,
            base: None,
        }) {
            Ok(o) => {
                if let Some(dir) = &out {
                    if let Err(e) = std::fs::create_dir_all(dir).and_then(|()| {
                        o.files
                            .iter()
                            .try_for_each(|(n, t)| std::fs::write(dir.join(n), t))
                    }) {
                        eprintln!("tp import: {}: {e}", dir.display());
                        return ExitCode::FAILURE;
                    }
                } else if !report {
                    for (n, t) in &o.files {
                        println!("// ===== {n} =====\n{t}");
                    }
                }
                if report {
                    print!("{}", o.report());
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("tp import: {e}");
                ExitCode::FAILURE
            }
        },
        Some(Command::Classify {
            playbook,
            roles_path,
            base,
            json,
            intent_skeleton,
        }) => {
            match classify_playbook(&Options {
                playbook,
                roles_path,
                base,
            }) {
                Ok(report) => {
                    if json {
                        println!("{}", report.to_json());
                    } else if intent_skeleton {
                        print!("{}", report.to_intent_skeleton());
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
