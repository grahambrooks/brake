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
use std::{io::IsTerminal, process::ExitCode};

use brake::{Severity, Verdict};
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
        /// Evaluate suppression expiry at this date (YYYY-MM-DD).
        #[arg(long)]
        as_of: Option<String>,
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
        /// Minimum severity that should fail analyze.
        #[arg(long, default_value = "warning")]
        fail_on: String,
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

    if let Command::Check {
        since,
        config,
        severity,
        as_of,
        format,
        drift,
        ..
    } = &cli.command
    {
        let config_value = match brake::config::Config::from_path(config) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(Verdict::ToolFailure.exit_code() as u8);
            }
        };
        let threshold = match parse_severity(severity) {
            Ok(value) => value,
            Err(message) => {
                eprintln!("{message}");
                return ExitCode::from(Verdict::ToolFailure.exit_code() as u8);
            }
        };
        let report = brake::check::check_contracts(
            &PathBuf::from("."),
            &config_value,
            since.as_deref(),
            as_of.as_deref(),
            *drift,
        );
        let output = match render_report(&report, format) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(Verdict::ToolFailure.exit_code() as u8);
            }
        };
        print!("{output}");
        return ExitCode::from(report.exit_code(threshold) as u8);
    }

    if let Command::Analyze {
        path,
        config,
        fail_on,
        format,
        ..
    } = &cli.command
    {
        let config_value = match brake::config::Config::from_path(config) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(Verdict::ToolFailure.exit_code() as u8);
            }
        };
        let threshold = match parse_severity(fail_on) {
            Ok(value) => value,
            Err(message) => {
                eprintln!("{message}");
                return ExitCode::from(Verdict::ToolFailure.exit_code() as u8);
            }
        };
        let report = brake::check::check_contracts(path, &config_value, None, None, false);
        let output = match render_report(&report, format) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(Verdict::ToolFailure.exit_code() as u8);
            }
        };
        print!("{output}");
        return ExitCode::from(report.exit_code(threshold) as u8);
    }

    if let Command::Diff { config, format } = &cli.command {
        let config_value = match brake::config::Config::from_path(config) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(Verdict::ToolFailure.exit_code() as u8);
            }
        };
        let report =
            brake::check::check_contracts(&PathBuf::from("."), &config_value, None, None, false);
        let output = match render_report(&report, format) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(Verdict::ToolFailure.exit_code() as u8);
            }
        };
        print!("{output}");
        return ExitCode::from(Verdict::Clean.exit_code() as u8);
    }

    let unimplemented = match &cli.command {
        Command::Diff { .. } => "diff",
        Command::Explain { .. } => unreachable!("handled above"),
        Command::Check { .. } | Command::Analyze { .. } => unreachable!("handled above"),
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

    ExitCode::from(Verdict::ToolFailure.exit_code() as u8)
}

fn parse_severity(input: &str) -> Result<Severity, String> {
    match input.trim().to_ascii_lowercase().as_str() {
        "info" => Ok(Severity::Info),
        "warning" | "warn" => Ok(Severity::Warning),
        "error" | "err" => Ok(Severity::Error),
        _ => Err(format!(
            "unknown severity `{input}`; expected one of: info, warning, error"
        )),
    }
}

fn render_report(report: &brake::report::Report, requested_format: &str) -> Result<String, String> {
    match normalize_format(requested_format)? {
        OutputFormat::Text => Ok(brake::render::text::render(report)),
        OutputFormat::Json => Ok(brake::render::json::render(report)),
        OutputFormat::Sarif => Ok(brake::render::sarif::render(report)),
    }
}

fn normalize_format(requested_format: &str) -> Result<OutputFormat, String> {
    let normalized = requested_format.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "text" => Ok(OutputFormat::Text),
        "json" => Ok(OutputFormat::Json),
        "sarif" => Ok(OutputFormat::Sarif),
        "auto" => {
            if std::io::stdout().is_terminal() {
                Ok(OutputFormat::Text)
            } else {
                Ok(OutputFormat::Json)
            }
        }
        _ => Err(format!(
            "unknown format `{requested_format}`; expected one of: auto, text, json, sarif"
        )),
    }
}

enum OutputFormat {
    Text,
    Json,
    Sarif,
}
