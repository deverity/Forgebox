//! Development tasks for Forgebox, run as `cargo xtask <command>`.
//!
//! The commands are placeholders until M0-E1-T2, which implements them for real.

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "xtask", about = "Forgebox development tasks")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Generate JSON Schema for manifests (registry/schema).
    Schema,
    /// Generate TypeScript types from the IPC contracts.
    GenTs,
    /// Lint the manifests in registry/.
    LintRegistry,
}

/// Returns the message printed for a command. Placeholder until M0-E1-T2.
fn run(command: &Command) -> &'static str {
    match command {
        Command::Schema | Command::GenTs | Command::LintRegistry => {
            "not implemented yet (M0-E1-T2)"
        }
    }
}

fn main() {
    let cli = Cli::parse();
    println!("{}", run(&cli.command));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_subcommands() {
        assert!(matches!(
            Cli::try_parse_from(["xtask", "schema"]).unwrap().command,
            Command::Schema
        ));
        assert!(matches!(
            Cli::try_parse_from(["xtask", "gen-ts"]).unwrap().command,
            Command::GenTs
        ));
        assert!(matches!(
            Cli::try_parse_from(["xtask", "lint-registry"])
                .unwrap()
                .command,
            Command::LintRegistry
        ));
    }

    #[test]
    fn rejects_unknown_subcommand() {
        assert!(Cli::try_parse_from(["xtask", "nope"]).is_err());
    }

    #[test]
    fn placeholder_message_points_to_task() {
        assert_eq!(run(&Command::GenTs), "not implemented yet (M0-E1-T2)");
    }
}
