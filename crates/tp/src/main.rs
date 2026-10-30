use clap::Parser;

/// telepatch tooling: understand and port Ansible roles.
#[derive(Parser)]
#[command(name = "tp", version, about)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}
