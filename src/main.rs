//! The `brake` CLI.
//!
//! This binary parses arguments, renders output, and exits. It does not decide
//! whether something is a breaking change — that lives in the library, so it
//! can be tested without spawning a process and so `forge` reaches the same
//! code. See `design/03-implementation-plan.md` §1.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use brake::check::{Options, Scope};
use brake::config::{Baseline, Compatibility, Config};
use brake::report::Report;
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
        /// Scope to contracts changed since the merge-base with this git ref.
        #[arg(long)]
        since: Option<String>,
        /// Path to brake.toml.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Check only these contracts, by name. Repeatable.
        #[arg(long)]
        contract: Vec<String>,
        /// Override every contract's baseline with this git ref and path,
        /// for example `origin/main:api/openapi.yaml`.
        #[arg(long)]
        baseline: Option<String>,
        /// Override the compatibility level: wire, wire-json, surface, strict.
        #[arg(long)]
        compatibility: Option<String>,
        /// Minimum severity to fail on.
        #[arg(long, default_value = "warning")]
        severity: String,
        /// Evaluate suppression expiry at this date (YYYY-MM-DD).
        /// Defaults to today.
        #[arg(long)]
        as_of: Option<String>,
        /// Output format: auto, text, json, sarif.
        #[arg(long, short, default_value = "auto")]
        format: String,
        /// Also run declared generator commands and check for drift.
        ///
        /// The only place brake executes a subprocess, and off by default so
        /// `brake check` stays safe against an untrusted repository.
        #[arg(long)]
        drift: bool,
    },
    /// Check every configured contract in the repository.
    Analyze {
        /// Repository root.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Path to brake.toml.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Check only these contracts, by name. Repeatable.
        #[arg(long)]
        contract: Vec<String>,
        /// Override the compatibility level.
        #[arg(long)]
        compatibility: Option<String>,
        /// Output format: auto, text, json, sarif.
        #[arg(long, short, default_value = "auto")]
        format: String,
        /// Minimum severity that should fail analyze.
        #[arg(long, default_value = "warning")]
        fail_on: String,
        /// Evaluate suppression expiry at this date (YYYY-MM-DD).
        #[arg(long)]
        as_of: Option<String>,
        /// Also run declared generator commands and check for drift.
        #[arg(long)]
        drift: bool,
    },
    /// Report every change with its classification, without failing.
    ///
    /// For pull-request descriptions and changelog drafting: it always exits
    /// 0, and reports at `strict` so additive changes are listed too.
    Diff {
        /// Path to brake.toml.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Report only these contracts, by name. Repeatable.
        #[arg(long)]
        contract: Vec<String>,
        /// Override every contract's baseline with this git ref and path.
        #[arg(long)]
        baseline: Option<String>,
        /// Output format: auto, text, json, sarif.
        #[arg(long, short, default_value = "auto")]
        format: String,
    },
    /// Explain why a rule exists and what to do about it.
    Explain {
        /// A rule ID, for example `response-field-removed`. Omit to list all.
        rule: Option<String>,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("brake: {message}");
            ExitCode::from(Verdict::ToolFailure.exit_code() as u8)
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    match cli.command {
        Command::Explain { rule } => explain(rule.as_deref()),

        Command::Check {
            paths,
            since,
            config,
            contract,
            baseline,
            compatibility,
            severity,
            as_of,
            format,
            drift,
        } => {
            let (root, config_value) = load(config.as_deref())?;
            let scope = match (since, paths.is_empty()) {
                (Some(reference), _) => Scope::Since(reference),
                (None, false) => Scope::Paths(paths),
                (None, true) => Scope::All,
            };
            let options = Options {
                // Expiry is the single documented clock dependency, and
                // `--as-of` overrides it so the expiry path stays testable.
                as_of: Some(as_of.unwrap_or_else(today)),
                drift,
                // A scoped run has not looked at everything, so it cannot know
                // a suppression is dead.
                report_stale: matches!(scope, Scope::All),
                compatibility: compatibility.as_deref().map(parse_level).transpose()?,
                baseline: baseline.map(|spec| Baseline::Git { spec }),
                only: contract,
            };

            let report = brake::check::check(&root, &config_value, &scope, &options);
            emit(&report, &format)?;
            Ok(ExitCode::from(
                report.exit_code(parse_severity(&severity)?) as u8
            ))
        }

        Command::Analyze {
            path,
            config,
            contract,
            compatibility,
            format,
            fail_on,
            as_of,
            drift,
        } => {
            let config_path = config.unwrap_or_else(|| path.join("brake.toml"));
            let config_value = Config::from_path(&config_path).map_err(|e| e.to_string())?;
            let options = Options {
                as_of: Some(as_of.unwrap_or_else(today)),
                drift,
                // analyze covers everything, so a suppression matching nothing
                // really is dead.
                report_stale: true,
                compatibility: compatibility.as_deref().map(parse_level).transpose()?,
                baseline: None,
                only: contract,
            };

            let report = brake::check::check(&path, &config_value, &Scope::All, &options);
            emit(&report, &format)?;
            Ok(ExitCode::from(
                report.exit_code(parse_severity(&fail_on)?) as u8
            ))
        }

        Command::Diff {
            config,
            contract,
            baseline,
            format,
        } => {
            let (root, config_value) = load(config.as_deref())?;
            let options = Options {
                as_of: Some(today()),
                drift: false,
                report_stale: false,
                // Report everything, including additive changes, because the
                // point is a complete description of the change.
                compatibility: Some(Compatibility::Strict),
                baseline: baseline.map(|spec| Baseline::Git { spec }),
                only: contract,
            };

            let report = brake::check::check(&root, &config_value, &Scope::All, &options);
            emit(&report, &format)?;
            // Never gates, by design.
            Ok(ExitCode::from(Verdict::Clean.exit_code() as u8))
        }
    }
}

fn explain(rule: Option<&str>) -> Result<ExitCode, String> {
    let Some(rule) = rule else {
        for definition in brake::rules::catalogue::RULES {
            println!(
                "{:<28} {:<10} {:<10} {}",
                definition.id,
                format!("{:?}", definition.severity).to_lowercase(),
                level_name(definition.level),
                definition.summary
            );
        }
        return Ok(ExitCode::from(Verdict::Clean.exit_code() as u8));
    };

    let Some(definition) = brake::rules::catalogue::lookup(rule) else {
        return Err(format!(
            "unknown rule id `{rule}`. Run `brake explain` with no argument to list the catalogue"
        ));
    };

    println!(
        "{}\n\nseverity:      {}\nfires from:    {}\n\n{}\n\n{}\n\n{}",
        definition.id,
        format!("{:?}", definition.severity).to_lowercase(),
        level_name(definition.level),
        definition.summary,
        definition.explanation,
        definition.help_uri(),
    );
    Ok(ExitCode::from(Verdict::Clean.exit_code() as u8))
}

/// Find `brake.toml` and the repository root it sits in.
///
/// Walking up means `brake check` works from a subdirectory, which is where a
/// developer usually is when a hook fires.
fn load(config: Option<&Path>) -> Result<(PathBuf, Config), String> {
    if let Some(explicit) = config {
        let root = explicit
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_path_buf();
        let parsed = Config::from_path(explicit).map_err(|e| e.to_string())?;
        return Ok((root, parsed));
    }

    let start =
        std::env::current_dir().map_err(|error| format!("no working directory: {error}"))?;
    let mut directory = start.as_path();
    loop {
        let candidate = directory.join("brake.toml");
        if candidate.is_file() {
            let parsed = Config::from_path(&candidate).map_err(|e| e.to_string())?;
            return Ok((directory.to_path_buf(), parsed));
        }
        match directory.parent() {
            Some(parent) => directory = parent,
            None => {
                return Err(format!(
                    "no brake.toml found in `{}` or any parent directory. \
                     Create one, or pass --config",
                    start.display()
                ));
            }
        }
    }
}

fn emit(report: &Report, requested_format: &str) -> Result<(), String> {
    let rendered = match normalize_format(requested_format)? {
        OutputFormat::Text => brake::render::text::render(report),
        OutputFormat::Json => brake::render::json::render(report),
        OutputFormat::Sarif => brake::render::sarif::render(report),
    };
    print!("{rendered}");
    Ok(())
}

fn parse_severity(input: &str) -> Result<Severity, String> {
    match input.trim().to_ascii_lowercase().as_str() {
        "info" => Ok(Severity::Info),
        "warning" | "warn" => Ok(Severity::Warning),
        "error" | "err" => Ok(Severity::Error),
        other => Err(format!(
            "unknown severity `{other}`; expected one of: info, warning, error"
        )),
    }
}

fn parse_level(input: &str) -> Result<Compatibility, String> {
    match input.trim().to_ascii_lowercase().as_str() {
        "wire" => Ok(Compatibility::Wire),
        "wire-json" | "wirejson" => Ok(Compatibility::WireJson),
        "surface" => Ok(Compatibility::Surface),
        "strict" => Ok(Compatibility::Strict),
        other => Err(format!(
            "unknown compatibility level `{other}`; expected one of: \
             wire, wire-json, surface, strict"
        )),
    }
}

fn level_name(level: Compatibility) -> &'static str {
    match level {
        Compatibility::Wire => "wire",
        Compatibility::WireJson => "wire-json",
        Compatibility::Surface => "surface",
        Compatibility::Strict => "strict",
    }
}

/// Today, as `YYYY-MM-DD` UTC.
///
/// Suppression expiry is the one place brake reads the clock — §6.1 guarantee
/// 5 — and the library never does it, so every other path stays trivially
/// deterministic. Days-from-civil, inverted; no calendar dependency needed for
/// a value that is only ever compared for order.
fn today() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let days = (seconds / 86_400) as i64 + 719_468;

    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let year = if month <= 2 { year + 1 } else { year };

    format!("{year:04}-{month:02}-{day:02}")
}

fn normalize_format(requested_format: &str) -> Result<OutputFormat, String> {
    match requested_format.trim().to_ascii_lowercase().as_str() {
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
        other => Err(format!(
            "unknown format `{other}`; expected one of: auto, text, json, sarif"
        )),
    }
}

enum OutputFormat {
    Text,
    Json,
    Sarif,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_and_level_parse_their_documented_spellings() {
        assert_eq!(parse_severity("Error").expect("error"), Severity::Error);
        assert_eq!(parse_severity(" warn ").expect("warn"), Severity::Warning);
        assert!(parse_severity("critical").is_err());

        assert_eq!(
            parse_level("wire-json").expect("wire-json"),
            Compatibility::WireJson
        );
        assert_eq!(
            parse_level("STRICT").expect("strict"),
            Compatibility::Strict
        );
        assert!(parse_level("loose").is_err());
    }

    #[test]
    fn today_is_a_well_formed_date_the_rules_can_parse() {
        let today = today();
        assert!(
            brake::rules::parse_date(&today).is_some(),
            "today() produced an unparseable date: {today}"
        );
        assert_eq!(today.len(), 10, "{today}");
    }

    #[test]
    fn level_names_round_trip() {
        for level in [
            Compatibility::Wire,
            Compatibility::WireJson,
            Compatibility::Surface,
            Compatibility::Strict,
        ] {
            assert_eq!(parse_level(level_name(level)).expect("round trip"), level);
        }
    }
}
