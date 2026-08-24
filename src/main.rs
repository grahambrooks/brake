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
        /// Verify against only these declared consumers, by name. Repeatable.
        ///
        /// Mirrors `--contract`. A path scope that names a consumer
        /// declaration selects the contracts that declaration constrains, so a
        /// hook run on a pact-updating commit verifies the right thing.
        #[arg(long)]
        consumer: Vec<String>,
        /// Override every contract's baseline.
        ///
        /// A revision (`v1.2.0`, `origin/main`, `8743cba`), a tag glob
        /// (`latest-tag:v*`), or the explicit `ref:path` form.
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
        /// Output format: auto, text, json, sarif, github, gitlab.
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
        /// Verify against only these declared consumers, by name. Repeatable.
        #[arg(long)]
        consumer: Vec<String>,
        /// Override the compatibility level.
        #[arg(long)]
        compatibility: Option<String>,
        /// Output format: auto, text, json, sarif, github, gitlab.
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
        /// Override every contract's baseline. See `brake check --help`.
        #[arg(long)]
        baseline: Option<String>,
        /// Output format: auto, text, json, sarif, github, gitlab.
        #[arg(long, short, default_value = "auto")]
        format: String,
    },
    /// Discover the contracts here and write a brake.toml declaring them.
    ///
    /// A file counts as a contract only if brake can actually parse it, so a
    /// CI workflow that happens to be called `api-something.yaml` is not
    /// mistaken for an API.
    Init {
        /// Repository root. Defaults to the working directory.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Print the configuration instead of writing it.
        #[arg(long)]
        dry_run: bool,
        /// Overwrite an existing brake.toml.
        #[arg(long)]
        force: bool,
    },
    /// Serve the ruleset over MCP, for a coding agent.
    ///
    /// The same checks `brake check` runs, consulted while an API is being
    /// edited rather than when it is committed. stdio transport; it never
    /// listens on a port and never runs a declared generator command.
    ///
    /// Needs the `mcp` feature, which is not on by default because the server
    /// requires an async runtime. The subcommand is listed either way: a
    /// capability that silently does not exist is one nobody can discover, and
    /// brake's whole posture is to say what it cannot do rather than go quiet.
    Mcp {
        /// Repository root. Defaults to the working directory.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Evaluate suppression expiry at this date (YYYY-MM-DD).
        #[arg(long)]
        as_of: Option<String>,
    },
    /// Report who consumes what, and what the verdict rested on.
    ///
    /// Non-gating: it always exits 0, joining `diff` in that family. Each
    /// declaration is listed with its path and a content digest, because brake
    /// does not measure freshness — a pact from eighteen months ago and one
    /// from this morning are the same bytes to a file reader.
    Consumers {
        /// Path to brake.toml.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Report only these contracts, by name. Repeatable.
        #[arg(long)]
        contract: Vec<String>,
        /// Report only these consumers, by name. Repeatable.
        #[arg(long)]
        consumer: Vec<String>,
        /// Output format: auto, text, json.
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

        Command::Init {
            path,
            dry_run,
            force,
        } => init(&path, dry_run, force),

        Command::Mcp { path, as_of } => serve_mcp(path, as_of),

        Command::Check {
            paths,
            since,
            config,
            contract,
            consumer,
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
                baseline: baseline.as_deref().map(parse_baseline),
                only: contract,
                consumers: consumer,
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
            consumer,
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
                consumers: consumer,
            };

            let report = brake::check::check(&path, &config_value, &Scope::All, &options);
            emit(&report, &format)?;
            Ok(ExitCode::from(
                report.exit_code(parse_severity(&fail_on)?) as u8
            ))
        }

        Command::Consumers {
            config,
            contract,
            consumer,
            format,
        } => {
            let (root, config_value) = load(config.as_deref())?;
            let inventory =
                brake::demand::inventory::build(&root, &config_value, &contract, &consumer);
            let rendered = match normalize_format(&format)? {
                OutputFormat::Text => brake::demand::inventory::render_text(&inventory),
                OutputFormat::Json
                | OutputFormat::Sarif
                | OutputFormat::Github
                | OutputFormat::Gitlab => brake::demand::inventory::render_json(&inventory),
            };
            print!("{rendered}");
            // Non-gating, by design: it answers a question, it does not judge.
            Ok(ExitCode::from(Verdict::Clean.exit_code() as u8))
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
                baseline: baseline.as_deref().map(parse_baseline),
                only: contract,
                consumers: Vec::new(),
            };

            let report = brake::check::check(&root, &config_value, &Scope::All, &options);
            emit(&report, &format)?;
            // Never gates, by design.
            Ok(ExitCode::from(Verdict::Clean.exit_code() as u8))
        }
    }
}

/// `brake init` — discover contracts and write the configuration.
fn init(path: &Path, dry_run: bool, force: bool) -> Result<ExitCode, String> {
    let discovery = brake::init::discover(path);
    let reference = brake::init::default_baseline_reference(path);
    let rendered = brake::init::render_config(&discovery, &reference);

    if dry_run {
        print!("{rendered}");
        return Ok(ExitCode::from(Verdict::Clean.exit_code() as u8));
    }

    let destination = path.join("brake.toml");
    if destination.exists() && !force {
        // Overwriting would discard hand-written suppressions and their
        // reasons, which are the most expensive thing in the file.
        return Err(format!(
            "`{}` already exists. Pass --force to overwrite it, or --dry-run to \
             see what init would write",
            destination.display()
        ));
    }

    // Report what was found before writing, so a wrong guess is visible at the
    // moment it can still be corrected.
    if discovery.contracts.is_empty() {
        println!(
            "no API contracts found under `{}`.\n\n\
             brake looks for OpenAPI, protobuf and GraphQL files it can parse — a \
             file is\nonly a contract if the ingester that would gate it can \
             actually read it. The\nconfiguration below has a commented example \
             to fill in.\n",
            path.display()
        );
    } else {
        println!(
            "found {} contract{}:",
            discovery.contracts.len(),
            if discovery.contracts.len() == 1 {
                ""
            } else {
                "s"
            }
        );
        for contract in &discovery.contracts {
            println!(
                "  {:<40} {:<8} as `{}`",
                contract.source,
                brake::init::format_name(contract.format),
                contract.name
            );
        }
        if !discovery.baselines_skipped.is_empty() {
            println!(
                "\nskipped {} checked-in baseline{} — a baseline is a previous \
                 version, not a contract to gate:",
                discovery.baselines_skipped.len(),
                if discovery.baselines_skipped.len() == 1 {
                    ""
                } else {
                    "s"
                }
            );
            for skipped in &discovery.baselines_skipped {
                println!("  {skipped}");
            }
        }
        if discovery.truncated {
            println!(
                "\nnote: stopped after examining a large number of files, so this \
                 may be incomplete."
            );
        }
        println!();
    }

    std::fs::write(&destination, &rendered)
        .map_err(|error| format!("failed to write `{}`: {error}", destination.display()))?;
    println!("wrote {}", display_relative(path, &destination));

    match discovery.contracts.first() {
        Some(contract) => println!(
            "\nnext:\n  brake check {}\n\n\
             The baseline is the merge-base with `{reference}`, so a contract that \
             is new\nto this branch reports as new rather than failing.",
            contract.source
        ),
        None => println!("\nnext: declare a contract in brake.toml, then run `brake check`"),
    }
    Ok(ExitCode::from(Verdict::Clean.exit_code() as u8))
}

fn display_relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).map_or_else(
        |_| path.display().to_string(),
        |rest| rest.display().to_string(),
    )
}

/// The `mcp` feature is off: say so, and say how to get it.
///
/// Exiting `2` with an actionable message is the same contract every other
/// unavailable path here follows — a tool that cannot answer says which, and
/// what to do about it.
#[cfg(not(feature = "mcp"))]
fn serve_mcp(_path: PathBuf, _as_of: Option<String>) -> Result<ExitCode, String> {
    Err(format!(
        "this build of brake {} has no MCP server.\n\n\
         It is behind the `mcp` feature, which is off by default because the \
         server needs an\nasync runtime that the rest of brake does not. To get it:\n\n    \
         cargo install brake --features mcp\n\n\
         or, from a checkout:\n\n    \
         cargo run --features mcp -- mcp .\n\n\
         See design/04-mcp-interface.md.",
        brake::VERSION
    ))
}

/// Run the MCP server on stdio.
///
/// The runtime is built here rather than with `#[tokio::main]` so the async
/// surface stays inside this one function: every other path through the binary
/// is synchronous, and so is every tool handler.
#[cfg(feature = "mcp")]
fn serve_mcp(path: PathBuf, as_of: Option<String>) -> Result<ExitCode, String> {
    let root = path
        .canonicalize()
        .map_err(|error| format!("cannot read `{}`: {error}", path.display()))?;

    let context = brake::mcp::handlers::Context::new(root, as_of.unwrap_or_else(today));

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to start the async runtime: {error}"))?;

    runtime
        .block_on(brake::mcp::serve_stdio(context))
        .map_err(|error| format!("the MCP server stopped: {error}"))?;
    Ok(ExitCode::from(Verdict::Clean.exit_code() as u8))
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
        "{}\n\nseverity:      {}\nfires from:    {}\n\n{}\n\n{}",
        definition.id,
        format!("{:?}", definition.severity).to_lowercase(),
        level_name(definition.level),
        definition.summary,
        definition.explanation,
    );

    // The ways out, generic here because there is no finding to bind them to.
    // A finding renders the same strategies with its own field named.
    let remedies: Vec<_> = definition
        .remedies
        .iter()
        .filter_map(|id| brake::rules::strategies::lookup(id))
        .collect();
    if !remedies.is_empty() {
        println!("\nways to make the change safely:");
        for strategy in remedies {
            println!(
                "  {}\n    {}\n    costs: {}",
                strategy.id,
                strategy
                    .summary
                    .replace("{subject}", "the field")
                    .replace("{endpoint}", "the endpoint"),
                strategy.cost
            );
        }
        println!(
            "\nbrake does not choose between these: which one fits depends on whether you\n\
             control every consumer and whether you have a version scheme, and it cannot\n\
             see either."
        );
    }

    println!("\n{}", definition.help_uri());
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
                    "no brake.toml found in `{}` or any parent directory.\n\n\
                     Run `brake init` to discover the contracts here and write one, \
                     or\n`brake init --dry-run` to see what it would write first. \
                     Pass --config to\npoint at a file elsewhere.",
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
        OutputFormat::Github => brake::render::github::render(report),
        OutputFormat::Gitlab => brake::render::gitlab::render(report),
    };
    print!("{rendered}");
    Ok(())
}

/// Interpret a `--baseline` value.
///
/// The flag is a convenience over `brake.toml`, so it accepts the same ideas
/// in one string: a bare revision is the common case, `latest-tag:` names the
/// glob form, and anything containing a `:` after a known ref is the explicit
/// `ref:path` shape.
fn parse_baseline(input: &str) -> Baseline {
    let input = input.trim();
    if let Some(pattern) = input.strip_prefix("latest-tag:") {
        return Baseline::LatestTag {
            pattern: pattern.to_owned(),
        };
    }
    if let Some(reference) = input.strip_prefix("merge-base:") {
        return Baseline::GitMergeBase {
            reference: reference.to_owned(),
        };
    }
    // `ref:path` is distinguishable because a path needs a separator; a bare
    // revision never contains one.
    if input.contains(':') {
        return Baseline::Git {
            spec: input.to_owned(),
        };
    }
    Baseline::Rev {
        rev: input.to_owned(),
    }
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
        "github" | "github-actions" => Ok(OutputFormat::Github),
        "gitlab" | "codequality" | "code-quality" => Ok(OutputFormat::Gitlab),
        "auto" => {
            if std::io::stdout().is_terminal() {
                Ok(OutputFormat::Text)
            } else {
                Ok(OutputFormat::Json)
            }
        }
        other => Err(format!(
            "unknown format `{other}`; expected one of: auto, text, json, sarif, github, gitlab"
        )),
    }
}

enum OutputFormat {
    Text,
    Json,
    Sarif,
    Github,
    Gitlab,
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
    fn baseline_flag_reads_the_shapes_it_documents() {
        assert!(matches!(
            parse_baseline("v1.2.0"),
            Baseline::Rev { rev } if rev == "v1.2.0"
        ));
        assert!(matches!(
            parse_baseline("latest-tag:v*"),
            Baseline::LatestTag { pattern } if pattern == "v*"
        ));
        assert!(matches!(
            parse_baseline("merge-base:origin/main"),
            Baseline::GitMergeBase { reference } if reference == "origin/main"
        ));
        assert!(matches!(
            parse_baseline("origin/main:api/openapi.yaml"),
            Baseline::Git { spec } if spec == "origin/main:api/openapi.yaml"
        ));
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
