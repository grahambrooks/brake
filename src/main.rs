//! The `brake` CLI.
//!
//! This binary parses arguments, renders output, and exits. It does not decide
//! whether something is a breaking change — that lives in the library, so it
//! can be tested without spawning a process and so `forge` reaches the same
//! code. See `design/03-implementation-plan.md` §1.
//!
//! The subcommands below are the interface specified in
//! `design/02-contract-gates.md` §7. None of them are implemented; each exits
//! `2` (tool failure), which is the honest answer for a gate that cannot yet
//! answer — and specifically not `0`, which would be a gate that silently
//! stops gating.

use std::path::PathBuf;

use brake::Verdict;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "brake",
    version = brake::VERSION,
    about = "A brake on breaking API changes",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check the contracts among the given paths against their baseline.
    ///
    /// The primary surface: a pre-commit hook passes the changed files, and
    /// scoping the run to the change is what makes the gate adoptable on a
    /// repository that already has findings.
    Check {
        /// Contract files to check. Defaults to every configured contract.
        paths: Vec<PathBuf>,
        /// Scope to files changed since this git ref.
        #[arg(long)]
        since: Option<String>,
        /// Path to brake.toml.
        #[arg(long, default_value = "brake.toml")]
        config: PathBuf,
        /// Minimum severity to report.
        #[arg(long, default_value = "warning")]
        severity: String,
        /// Output format: auto, text, json, sarif.
        #[arg(long, short, default_value = "auto")]
        format: String,
        /// Also run declared generator commands and check for drift.
        #[arg(long)]
        drift: bool,
    },
    /// Check every configured contract in the repository.
    Analyze {
        /// Repository root.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Path to brake.toml.
        #[arg(long, default_value = "brake.toml")]
        config: PathBuf,
        /// Output format: auto, text, json, sarif.
        #[arg(long, short, default_value = "auto")]
        format: String,
    },
    /// Report every change with its classification, without failing.
    Diff {
        /// Path to brake.toml.
        #[arg(long, default_value = "brake.toml")]
        config: PathBuf,
        /// Output format: auto, text, json, sarif.
        #[arg(long, short, default_value = "auto")]
        format: String,
    },
    /// Explain why a rule exists and what to do about it.
    Explain {
        /// A rule ID, for example `response-field-removed`.
        rule: String,
    },
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();

    if let Command::Explain { rule } = &cli.command {
        if let Some(definition) = brake::rules::catalogue::lookup(rule) {
            println!(
                "{}\nseverity: {:?}\nsummary: {}\n\n{}",
                definition.id, definition.severity, definition.summary, definition.explanation
            );
            return std::process::ExitCode::from(Verdict::Clean.exit_code() as u8);
        }

        eprintln!(
            "brake {}: unknown rule id `{}`.\n\
             \n\
             Use a rule ID from the catalogue. Exiting {}.",
            brake::VERSION,
            rule,
            Verdict::ToolFailure.exit_code()
        );
        return std::process::ExitCode::from(Verdict::ToolFailure.exit_code() as u8);
    }

    let unimplemented = match &cli.command {
        Command::Check { .. } => "check",
        Command::Analyze { .. } => "analyze",
        Command::Diff { .. } => "diff",
        Command::Explain { .. } => unreachable!("handled above"),
    };

    eprintln!(
        "brake {}: `{unimplemented}` is not implemented yet.\n\
         \n\
         The design is complete and the build order is in\n\
         design/03-implementation-plan.md — M1 is the walking skeleton.\n\
         \n\
         Exiting {} (tool failure) rather than 0, deliberately: a gate that\n\
         reports clean when it cannot answer is worse than no gate.",
        brake::VERSION,
        Verdict::ToolFailure.exit_code(),
    );

    std::process::ExitCode::from(Verdict::ToolFailure.exit_code() as u8)
}
