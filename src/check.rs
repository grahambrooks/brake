//! The orchestration: config → baseline → ingest → compare → rules → report.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::Severity;
use crate::baseline::{self, BaselineError};
use crate::compare;
use crate::config::{Config, ContractConfig, ContractFormat, Defaults};
use crate::contract::{self, Contract};
use crate::report::{Report, Unavailable};
use crate::rules::{self, Finding};

/// How long a `--drift` generator command may run before it is killed.
///
/// A hook that hangs is a hook that gets uninstalled, and an unbounded wait on
/// a subprocess is the easiest way to hang one.
const DRIFT_TIMEOUT: Duration = Duration::from_secs(120);

/// What a run covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Every configured contract — `brake analyze`.
    All,
    /// Only the contracts among these paths — the pre-commit hook, and the
    /// primary surface. Scoping to the change is what lets the gate be adopted
    /// on a repository that already has findings.
    Paths(Vec<PathBuf>),
    /// Only the contracts that differ from the merge-base with this ref.
    Since(String),
}

/// Everything a run needs beyond the config.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// The date suppression expiry is evaluated against. `None` means expiry
    /// is not evaluated at all; the CLI passes today unless `--as-of` is given.
    pub as_of: Option<String>,
    /// Run declared generator commands. Off by default, and unreachable
    /// without the flag, so `brake check` stays safe on an untrusted
    /// repository.
    pub drift: bool,
    /// Report suppressions that matched nothing. Only correct for a run that
    /// covered everything.
    pub report_stale: bool,
    /// Override the configured compatibility level for every contract.
    pub compatibility: Option<crate::config::Compatibility>,
    /// Override the configured baseline for every contract.
    pub baseline: Option<crate::config::Baseline>,
    /// Restrict to these contract names.
    pub only: Vec<String>,
}

/// Run a check over the given scope.
pub fn check(repo_root: &Path, config: &Config, scope: &Scope, options: &Options) -> Report {
    let selected = match select_contracts(repo_root, config, scope) {
        Ok(selected) => selected,
        Err(message) => {
            let mut report = Report::new(
                Vec::new(),
                vec![Unavailable {
                    contract: None,
                    message,
                }],
                0,
            );
            report.finalise();
            return report;
        }
    };

    let mut report = Report::default();
    for contract in &selected.contracts {
        if !options.only.is_empty() && !options.only.contains(&contract.name) {
            continue;
        }
        report.absorb(check_contract(
            repo_root,
            &config.defaults,
            contract,
            options,
        ));
    }
    report.findings.extend(selected.notices);
    report.finalise();
    report
}

/// Check one contract against its baseline.
pub fn check_contract(
    repo_root: &Path,
    defaults: &Defaults,
    contract: &ContractConfig,
    options: &Options,
) -> Report {
    let mut report = Report::new(Vec::new(), Vec::new(), 1);

    let head_relative = display_path(&contract.source);
    let head_path = repo_root.join(&contract.source);
    let head_bytes = match fs::read(&head_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            report.findings.push(rules::contract_unreachable(
                &contract.name,
                &format!("failed to read `{head_relative}`: {error}"),
                None,
            ));
            return report;
        }
    };

    let baseline_override = options.baseline.clone();
    let resolved = match baseline::resolve_for_contract(
        repo_root,
        defaults,
        contract,
        baseline_override.as_ref(),
    ) {
        Ok(resolved) => resolved,
        // An *unconfigured* baseline is a user who has not opted in; an
        // *unresolvable* one is a broken gate. Conflating them is how a gate
        // silently stops gating, so they take different exits.
        Err(BaselineError::MissingBaseline { .. }) => {
            report.findings.push(rules::synthetic(
                "baseline-unconfigured",
                &contract.name,
                format!(
                    "contract `{}` has no baseline, so nothing was compared. Add one under \
                     `[[contract]]` or `[defaults]`, for example \
                     `baseline = {{ git-merge-base = \"origin/main\" }}`",
                    contract.name
                ),
            ));
            return report;
        }
        // The baseline resolved, but this contract is not in it: the contract
        // is new. Nothing existed to break, so this is a note, not a failure.
        Err(error @ BaselineError::AbsentFromBaseline { .. }) => {
            report.findings.push(rules::synthetic(
                "contract-new",
                &contract.name,
                format!("{error}. Nothing was compared"),
            ));
            return report;
        }
        Err(error) => {
            report.unavailable.push(Unavailable {
                contract: Some(contract.name.clone()),
                message: error.to_string(),
            });
            return report;
        }
    };

    let base_contract = match ingest(contract.format, &resolved.label, &resolved.bytes) {
        Ok(parsed) => parsed,
        Err(error) => {
            report
                .findings
                .push(rules::contract_unreachable(&contract.name, &error, None));
            return report;
        }
    };
    let head_contract = match ingest(contract.format, &head_relative, &head_bytes) {
        Ok(parsed) => parsed,
        Err(error) => {
            report
                .findings
                .push(rules::contract_unreachable(&contract.name, &error, None));
            return report;
        }
    };

    // Carried so the text renderer can quote the offending line, including
    // from a baseline that only exists as a git blob.
    if let Ok(text) = String::from_utf8(resolved.bytes.clone()) {
        report.sources.insert(resolved.label.clone(), text);
    }
    if let Ok(text) = String::from_utf8(head_bytes.clone()) {
        report.sources.insert(head_relative.clone(), text);
    }

    let level = options
        .compatibility
        .unwrap_or_else(|| contract.effective_compatibility(defaults));

    let mut changes = compare::compare_contracts(&base_contract, &head_contract);
    // An unmodelled construct on an endpoint that is byte-identical on both
    // sides is still an unverified path, and must not read as clean.
    changes.extend(compare::partial_changes(&head_contract));
    changes.sort();
    changes.dedup();

    let findings = rules::evaluate(&changes, &contract.name, level);
    report.findings = rules::apply_suppressions(
        findings,
        &contract.name,
        &contract.allow,
        options.as_of.as_deref(),
        options.report_stale,
    );

    if options.drift
        && let Some(generated) = &contract.generated
    {
        match run_drift(repo_root, contract, &head_bytes, &generated.command) {
            Ok(Some(finding)) => report.findings.push(finding),
            Ok(None) => {}
            // A generator that could not be run tells us nothing about drift.
            // Reporting no-drift here would be a clean result brake cannot
            // justify, which is the one thing it must never do.
            Err(message) => report.unavailable.push(Unavailable {
                contract: Some(contract.name.clone()),
                message,
            }),
        }
    }

    report
}

fn ingest(format: ContractFormat, source: &str, bytes: &[u8]) -> Result<Contract, String> {
    match format {
        ContractFormat::Openapi => {
            contract::openapi::ingest(source, bytes).map_err(|error| error.to_string())
        }
        ContractFormat::Proto => {
            contract::proto::ingest(source, bytes).map_err(|error| error.to_string())
        }
        ContractFormat::Graphql => {
            contract::graphql::ingest(source, bytes).map_err(|error| error.to_string())
        }
    }
}

struct Selection<'a> {
    contracts: Vec<&'a ContractConfig>,
    notices: Vec<Finding>,
}

fn select_contracts<'a>(
    repo_root: &Path,
    config: &'a Config,
    scope: &Scope,
) -> Result<Selection<'a>, String> {
    match scope {
        Scope::All => Ok(Selection {
            contracts: config.contracts.iter().collect(),
            notices: Vec::new(),
        }),
        Scope::Paths(paths) => Ok(select_by_path(repo_root, config, paths)),
        Scope::Since(reference) => Ok(Selection {
            contracts: select_since(repo_root, config, reference)?,
            notices: Vec::new(),
        }),
    }
}

/// Select the contracts among the given paths.
///
/// A pre-commit hook passes every changed file it was configured to watch, so
/// most of them will not be contracts. A file that *looks* like a contract and
/// is not configured gets an info: silently not checking a new API file is
/// exactly the kind of gap this tool exists to close.
fn select_by_path<'a>(repo_root: &Path, config: &'a Config, paths: &[PathBuf]) -> Selection<'a> {
    let wanted = paths
        .iter()
        .map(|path| normalise_against_root(repo_root, path))
        .collect::<Vec<_>>();

    let contracts = config
        .contracts
        .iter()
        .filter(|contract| wanted.contains(&display_path(&contract.source)))
        .collect::<Vec<_>>();

    let mut notices = Vec::new();
    for path in &wanted {
        let configured = config
            .contracts
            .iter()
            .any(|contract| &display_path(&contract.source) == path);
        if !configured && looks_like_a_contract(repo_root, path) {
            notices.push(rules::about_file(
                "contract-unconfigured",
                path,
                format!(
                    "`{path}` parses as an API contract but no `[[contract]]` in brake.toml \
                     declares it, so it was not checked. `brake init` will declare it"
                ),
            ));
        }
    }

    Selection { contracts, notices }
}

/// Is this file one brake could gate, if it were declared?
///
/// Answered by parsing it, not by looking at its name. The first version of
/// this asked whether the path contained `api`, which called
/// `.github/workflows/api-tests.yaml` an API and printed a notice about it on
/// every commit that touched CI. `01-thesis.md` says false positives are how a
/// hook gets uninstalled — a *loud* one costs more than a quiet one, not less.
///
/// Shares its implementation with `brake init`, so the two cannot disagree
/// about what a contract is.
fn looks_like_a_contract(repo_root: &Path, path: &str) -> bool {
    crate::init::identify(&repo_root.join(path)).is_some()
}

fn select_since<'a>(
    repo_root: &Path,
    config: &'a Config,
    reference: &str,
) -> Result<Vec<&'a ContractConfig>, String> {
    let repo = gix::open(repo_root)
        .map_err(|error| format!("failed to open git repository for --since: {error}"))?;
    let head = repo
        .head_id()
        .map_err(|error| format!("failed to resolve HEAD for --since: {error}"))?;
    let reference_id = repo
        .rev_parse_single(reference)
        .map_err(|error| format!("failed to resolve --since ref `{reference}`: {error}"))?;
    let merge_base = repo
        .merge_base(head.detach(), reference_id.detach())
        .map_err(|error| format!("failed to compute merge-base for --since: {error}"))?;
    let tree = merge_base
        .object()
        .map_err(|error| format!("failed to read the merge-base object: {error}"))?
        .try_into_commit()
        .map_err(|error| format!("failed to decode the merge-base commit: {error}"))?
        .tree()
        .map_err(|error| format!("failed to read the merge-base tree: {error}"))?;

    let mut in_scope = Vec::new();
    for contract in &config.contracts {
        let baseline_bytes = match tree.lookup_entry_by_path(&contract.source) {
            Ok(Some(entry)) => {
                let mut blob = entry
                    .object()
                    .map_err(|error| {
                        format!(
                            "failed to read `{}` from the merge-base: {error}",
                            display_path(&contract.source)
                        )
                    })?
                    .try_into_blob()
                    .map_err(|error| {
                        format!(
                            "failed to decode `{}` from the merge-base: {error}",
                            display_path(&contract.source)
                        )
                    })?;
                Some(blob.take_data())
            }
            Ok(None) => None,
            Err(error) => {
                return Err(format!(
                    "failed to look up `{}` in the merge-base: {error}",
                    display_path(&contract.source)
                ));
            }
        };
        if baseline_bytes != fs::read(repo_root.join(&contract.source)).ok() {
            in_scope.push(contract);
        }
    }
    Ok(in_scope)
}

/// Run a declared generator and compare its stdout to the committed artifact.
///
/// `Err` means the generator could not be run at all, which is unavailable
/// rather than clean. `Ok(None)` means it ran and matched.
fn run_drift(
    repo_root: &Path,
    contract: &ContractConfig,
    committed: &[u8],
    command: &str,
) -> Result<Option<Finding>, String> {
    let temp = tempfile::tempdir()
        .map_err(|error| format!("failed to create a directory for --drift: {error}"))?;
    let stdout_path = temp.path().join("brake-generated-stdout");
    let stdout_file = fs::File::create(&stdout_path)
        .map_err(|error| format!("failed to capture generator output: {error}"))?;

    let shell = if cfg!(windows) { "cmd" } else { "sh" };
    let shell_flag = if cfg!(windows) { "/C" } else { "-c" };
    let mut child = Command::new(shell)
        .arg(shell_flag)
        .arg(command)
        .current_dir(temp.path())
        .env("BRAKE_REPO_ROOT", repo_root)
        // Writing stdout to a file rather than a pipe means a generator that
        // produces more than a pipe buffer cannot deadlock against us.
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            format!(
                "generator for contract `{}` could not be started (`{command}`): {error}",
                contract.name
            )
        })?;

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if started.elapsed() > DRIFT_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "generator for contract `{}` did not finish within {}s",
                        contract.name,
                        DRIFT_TIMEOUT.as_secs()
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => {
                return Err(format!(
                    "failed to wait for the generator for contract `{}`: {error}",
                    contract.name
                ));
            }
        }
    };

    let produced = fs::read(&stdout_path)
        .map_err(|error| format!("failed to read generator output: {error}"))?;

    if !status.success() {
        return Err(format!(
            "generator for contract `{}` exited with {}",
            contract.name,
            status
                .code()
                .map_or_else(|| "a signal".to_owned(), |code| format!("status {code}"))
        ));
    }

    if produced == committed {
        return Ok(None);
    }
    Ok(Some(rules::Finding {
        rule_id: "generated-drift",
        severity: Severity::Error,
        contract: contract.name.clone(),
        message: format!(
            "`{}` differs from the output of its declared generator ({} bytes committed, {} bytes generated)",
            display_path(&contract.source),
            committed.len(),
            produced.len()
        ),
        method: None,
        path: Some(display_path(&contract.source)),
        pointer: String::new(),
        subject: None,
        span: None,
    }))
}

/// A repository-relative path with `/` separators.
///
/// Output must never contain an absolute path: guarantee G4 is byte-stability
/// across machines, and G6 normalises separators.
#[must_use]
pub fn display_path(path: &Path) -> String {
    let mut out = String::new();
    for component in path.components() {
        if let Component::Normal(segment) = component {
            if !out.is_empty() {
                out.push('/');
            }
            out.push_str(&segment.to_string_lossy());
        }
    }
    out
}

/// Reduce a path the user typed to the repository-relative form used in config.
fn normalise_against_root(repo_root: &Path, path: &Path) -> String {
    let absolute_root = repo_root.canonicalize();
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        repo_root.join(path)
    };
    if let (Ok(root), Ok(candidate)) = (&absolute_root, candidate.canonicalize())
        && let Ok(relative) = candidate.strip_prefix(root)
    {
        return display_path(relative);
    }
    display_path(path)
}

/// Every configured contract, for `brake diff` and for listing.
#[must_use]
pub fn contract_sources(config: &Config) -> BTreeMap<String, String> {
    config
        .contracts
        .iter()
        .map(|contract| (contract.name.clone(), display_path(&contract.source)))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    use tempfile::{TempDir, tempdir};

    use super::*;
    use crate::config::{
        Baseline, Compatibility, Config, ContractConfig, ContractFormat, Defaults,
    };
    use crate::render::text;

    const SPEC: &str = r#"
openapi: 3.1.0
paths:
  /payments/{id}:
    get:
      operationId: getPayment
      responses:
        "200":
          description: ok
"#;

    fn contract_at(name: &str, source: &str, baseline: &str) -> ContractConfig {
        ContractConfig {
            name: name.to_owned(),
            format: ContractFormat::Openapi,
            source: PathBuf::from(source),
            compatibility: None,
            baseline: Some(Baseline::File(PathBuf::from(baseline))),
            allow: Vec::new(),
            generated: None,
        }
    }

    fn config_of(contracts: Vec<ContractConfig>) -> Config {
        Config {
            defaults: Defaults {
                compatibility: Compatibility::WireJson,
                baseline: None,
            },
            contracts,
        }
    }

    fn repo_with(files: &[(&str, &str)]) -> TempDir {
        let repo = tempdir().expect("tempdir");
        for (path, body) in files {
            let full = repo.path().join(path);
            fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
            fs::write(full, body).expect("write");
        }
        repo
    }

    fn run_git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git should launch");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn removed_endpoint_fails_with_a_located_diagnostic() {
        let repo = repo_with(&[
            ("api/openapi.baseline.yaml", SPEC),
            (
                "api/openapi.yaml",
                "openapi: 3.1.0\npaths:\n  /payments:\n    get:\n      operationId: listPayments\n      responses:\n        \"200\":\n          description: ok\n",
            ),
        ]);
        let config = config_of(vec![contract_at(
            "payments",
            "api/openapi.yaml",
            "api/openapi.baseline.yaml",
        )]);

        let report = check(repo.path(), &config, &Scope::All, &Options::default());

        assert_eq!(report.exit_code(Severity::Error), 1);
        let rendered = text::render(&report);
        assert!(rendered.contains("endpoint-removed"), "{rendered}");
        assert!(rendered.contains("api/openapi.baseline.yaml"), "{rendered}");
    }

    #[test]
    fn an_unchanged_contract_exits_clean() {
        let repo = repo_with(&[
            ("api/openapi.baseline.yaml", SPEC),
            ("api/openapi.yaml", SPEC),
        ]);
        let config = config_of(vec![contract_at(
            "payments",
            "api/openapi.yaml",
            "api/openapi.baseline.yaml",
        )]);

        let report = check(repo.path(), &config, &Scope::All, &Options::default());
        assert_eq!(
            report.exit_code(Severity::Error),
            0,
            "{:?}",
            report.findings
        );
    }

    #[test]
    fn paths_scope_the_run_to_the_contracts_named() {
        let broken = "openapi: 3.1.0\npaths: {}\n";
        let repo = repo_with(&[
            ("api/a.baseline.yaml", SPEC),
            ("api/a.yaml", broken),
            ("api/b.baseline.yaml", SPEC),
            ("api/b.yaml", SPEC),
        ]);
        let config = config_of(vec![
            contract_at("a", "api/a.yaml", "api/a.baseline.yaml"),
            contract_at("b", "api/b.yaml", "api/b.baseline.yaml"),
        ]);

        // `b` is untouched, so a run scoped to it must not report `a`.
        let scoped = check(
            repo.path(),
            &config,
            &Scope::Paths(vec![PathBuf::from("api/b.yaml")]),
            &Options::default(),
        );
        assert_eq!(scoped.contracts_checked, 1);
        assert_eq!(
            scoped.exit_code(Severity::Error),
            0,
            "a scoped run leaked a finding from another contract: {:?}",
            scoped.findings
        );

        // The whole repository still sees it.
        let everything = check(repo.path(), &config, &Scope::All, &Options::default());
        assert_eq!(everything.contracts_checked, 2);
        assert_eq!(everything.exit_code(Severity::Error), 1);
    }

    #[test]
    fn the_ratchet_holds_a_repository_with_existing_findings_passes_an_unrelated_change() {
        // `legacy` is already broken against its baseline and stays broken.
        // A commit touching only `fresh` must pass, which is the whole
        // adoption argument for scoping.
        let broken = "openapi: 3.1.0\npaths: {}\n";
        let repo = repo_with(&[
            ("api/legacy.baseline.yaml", SPEC),
            ("api/legacy.yaml", broken),
            ("api/fresh.baseline.yaml", SPEC),
            ("api/fresh.yaml", SPEC),
        ]);
        let config = config_of(vec![
            contract_at("legacy", "api/legacy.yaml", "api/legacy.baseline.yaml"),
            contract_at("fresh", "api/fresh.yaml", "api/fresh.baseline.yaml"),
        ]);

        let report = check(
            repo.path(),
            &config,
            &Scope::Paths(vec![PathBuf::from("api/fresh.yaml")]),
            &Options::default(),
        );
        assert_eq!(report.exit_code(Severity::Error), 0);

        // And a commit that does add a new break is still blocked.
        fs::write(repo.path().join("api/fresh.yaml"), broken).expect("write");
        let report = check(
            repo.path(),
            &config,
            &Scope::Paths(vec![PathBuf::from("api/fresh.yaml")]),
            &Options::default(),
        );
        assert_eq!(report.exit_code(Severity::Error), 1);
    }

    #[test]
    fn paths_accept_the_forms_a_hook_actually_passes() {
        let repo = repo_with(&[
            ("api/openapi.baseline.yaml", SPEC),
            ("api/openapi.yaml", SPEC),
        ]);
        let config = config_of(vec![contract_at(
            "payments",
            "api/openapi.yaml",
            "api/openapi.baseline.yaml",
        )]);

        for path in [
            repo.path().join("api/openapi.yaml"),
            PathBuf::from("./api/openapi.yaml"),
            PathBuf::from("api/openapi.yaml"),
        ] {
            let report = check(
                repo.path(),
                &config,
                &Scope::Paths(vec![path.clone()]),
                &Options::default(),
            );
            assert_eq!(
                report.contracts_checked, 1,
                "path form not matched: {path:?}"
            );
        }
    }

    #[test]
    fn an_unconfigured_contract_file_is_pointed_out_but_does_not_fail() {
        let repo = repo_with(&[("api/new-openapi.yaml", SPEC)]);
        let config = config_of(Vec::new());

        let report = check(
            repo.path(),
            &config,
            &Scope::Paths(vec![PathBuf::from("api/new-openapi.yaml")]),
            &Options::default(),
        );
        assert_eq!(report.exit_code(Severity::Error), 0);
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.rule_id == "contract-unconfigured"),
            "a new API file that nothing gates should be pointed out: {:?}",
            report.findings
        );
    }

    #[test]
    fn an_unconfigured_baseline_exits_zero_with_an_info() {
        let repo = repo_with(&[("api/openapi.yaml", SPEC)]);
        let mut contract = contract_at("payments", "api/openapi.yaml", "unused");
        contract.baseline = None;
        let config = config_of(vec![contract]);

        let report = check(repo.path(), &config, &Scope::All, &Options::default());
        assert_eq!(
            report.exit_code(Severity::Error),
            0,
            "a user who has not opted in has not broken the gate"
        );
        assert_eq!(report.findings[0].rule_id, "baseline-unconfigured");
        assert_eq!(report.findings[0].severity, Severity::Info);
    }

    #[test]
    fn a_contract_added_by_this_change_is_new_not_broken() {
        let repo = repo_with(&[("api/openapi.yaml", SPEC)]);
        run_git(repo.path(), &["init", "-b", "main"]);
        run_git(repo.path(), &["config", "user.name", "Brake Test"]);
        run_git(repo.path(), &["config", "user.email", "brake@example.com"]);
        fs::write(repo.path().join("README.md"), "# repo\n").expect("write");
        run_git(repo.path(), &["add", "README.md"]);
        run_git(repo.path(), &["commit", "-m", "before the api existed"]);
        run_git(repo.path(), &["add", "api/openapi.yaml"]);
        run_git(repo.path(), &["commit", "-m", "add the api"]);

        let mut config = config_of(vec![contract_at("payments", "api/openapi.yaml", "unused")]);
        config.contracts[0].baseline = Some(Baseline::GitMergeBase {
            reference: "HEAD~1".to_owned(),
        });

        let report = check(repo.path(), &config, &Scope::All, &Options::default());
        assert_eq!(
            report.exit_code(Severity::Error),
            0,
            "a newly added contract cannot have broken anything: {report:?}"
        );
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.rule_id == "contract-new")
        );
    }

    #[test]
    fn a_configured_but_missing_baseline_exits_two() {
        let repo = repo_with(&[("api/openapi.yaml", SPEC)]);
        let config = config_of(vec![contract_at(
            "payments",
            "api/openapi.yaml",
            "api/nonexistent.baseline.yaml",
        )]);

        let report = check(repo.path(), &config, &Scope::All, &Options::default());
        assert_eq!(
            report.exit_code(Severity::Error),
            2,
            "a gate that cannot see its baseline must not report clean"
        );
    }

    #[test]
    fn since_scopes_to_contracts_the_branch_changed() {
        let repo = repo_with(&[("api/a.yaml", SPEC), ("api/b.yaml", SPEC)]);
        run_git(repo.path(), &["init", "-b", "main"]);
        run_git(repo.path(), &["config", "user.name", "Brake Test"]);
        run_git(repo.path(), &["config", "user.email", "brake@example.com"]);
        run_git(repo.path(), &["add", "."]);
        run_git(repo.path(), &["commit", "-m", "base"]);

        fs::write(
            repo.path().join("api/a.yaml"),
            "openapi: 3.1.0\npaths:\n  /renamed:\n    get:\n      operationId: getPayment\n      responses:\n        \"200\":\n          description: ok\n",
        )
        .expect("write");
        run_git(repo.path(), &["add", "api/a.yaml"]);
        run_git(repo.path(), &["commit", "-m", "change a"]);

        let mut config = config_of(vec![
            contract_at("a", "api/a.yaml", "unused"),
            contract_at("b", "api/b.yaml", "unused"),
        ]);
        config.defaults.baseline = Some(Baseline::GitMergeBase {
            reference: "HEAD~1".to_owned(),
        });
        for contract in &mut config.contracts {
            contract.baseline = None;
        }

        let report = check(
            repo.path(),
            &config,
            &Scope::Since("HEAD~1".to_owned()),
            &Options::default(),
        );
        assert_eq!(report.contracts_checked, 1);
        assert_eq!(report.exit_code(Severity::Error), 1);
    }

    #[test]
    fn spans_never_contain_an_absolute_path() {
        let repo = repo_with(&[
            ("api/openapi.baseline.yaml", SPEC),
            ("api/openapi.yaml", "openapi: 3.1.0\npaths: {}\n"),
        ]);
        let config = config_of(vec![contract_at(
            "payments",
            "api/openapi.yaml",
            "api/openapi.baseline.yaml",
        )]);

        let report = check(repo.path(), &config, &Scope::All, &Options::default());
        for finding in &report.findings {
            let Some(span) = &finding.span else { continue };
            assert!(
                !span.file.starts_with('/')
                    && !span.file.contains(':')
                    && !span.file.contains('\\'),
                "span path is not repository-relative: {}",
                span.file
            );
        }
        let root = repo.path().to_string_lossy().to_string();
        assert!(
            !crate::render::json::render(&report).contains(&root),
            "output must not embed the absolute checkout path"
        );
    }

    #[test]
    fn drift_reports_when_generated_output_differs() {
        let repo = repo_with(&[
            ("api/openapi.baseline.yaml", SPEC),
            ("api/openapi.yaml", SPEC),
        ]);
        let mut contract = contract_at("payments", "api/openapi.yaml", "api/openapi.baseline.yaml");
        contract.generated = Some(crate::config::GeneratedConfig {
            command: "printf 'not-the-checked-in-spec'".to_owned(),
        });
        let config = config_of(vec![contract]);

        let report = check(
            repo.path(),
            &config,
            &Scope::All,
            &Options {
                drift: true,
                ..Options::default()
            },
        );
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.rule_id == "generated-drift")
        );
    }

    #[test]
    fn drift_passes_when_generated_output_matches() {
        let repo = repo_with(&[
            ("api/openapi.baseline.yaml", SPEC),
            ("api/openapi.yaml", SPEC),
        ]);
        let mut contract = contract_at("payments", "api/openapi.yaml", "api/openapi.baseline.yaml");
        contract.generated = Some(crate::config::GeneratedConfig {
            command: "cat \"$BRAKE_REPO_ROOT/api/openapi.yaml\"".to_owned(),
        });
        let config = config_of(vec![contract]);

        let report = check(
            repo.path(),
            &config,
            &Scope::All,
            &Options {
                drift: true,
                ..Options::default()
            },
        );
        assert_eq!(
            report.exit_code(Severity::Error),
            0,
            "{:?}",
            report.findings
        );
    }

    #[test]
    fn a_generator_that_cannot_run_is_unavailable_not_clean() {
        let repo = repo_with(&[
            ("api/openapi.baseline.yaml", SPEC),
            ("api/openapi.yaml", SPEC),
        ]);
        let mut contract = contract_at("payments", "api/openapi.yaml", "api/openapi.baseline.yaml");
        contract.generated = Some(crate::config::GeneratedConfig {
            command: "brake-no-such-generator-command".to_owned(),
        });
        let config = config_of(vec![contract]);

        let report = check(
            repo.path(),
            &config,
            &Scope::All,
            &Options {
                drift: true,
                ..Options::default()
            },
        );
        assert_eq!(
            report.exit_code(Severity::Error),
            2,
            "a generator that did not run tells us nothing about drift: {report:?}"
        );
    }

    #[test]
    fn the_drift_subprocess_is_unreachable_without_the_flag() {
        let marker = tempdir().expect("tempdir");
        let witness = marker.path().join("the-generator-ran");
        let repo = repo_with(&[
            ("api/openapi.baseline.yaml", SPEC),
            ("api/openapi.yaml", SPEC),
        ]);
        let mut contract = contract_at("payments", "api/openapi.yaml", "api/openapi.baseline.yaml");
        contract.generated = Some(crate::config::GeneratedConfig {
            command: format!("touch {}", witness.display()),
        });
        let config = config_of(vec![contract]);

        check(repo.path(), &config, &Scope::All, &Options::default());
        assert!(
            !witness.exists(),
            "brake check must not execute a config-declared command without --drift"
        );
    }

    #[test]
    fn an_unmodelled_construct_is_reported_rather_than_compared_clean() {
        let with_ref = r#"
openapi: 3.1.0
paths:
  /p:
    get:
      operationId: getP
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                $ref: 'common.yaml#/components/schemas/Payment'
"#;
        let repo = repo_with(&[
            ("api/openapi.baseline.yaml", with_ref),
            ("api/openapi.yaml", with_ref),
        ]);
        let config = config_of(vec![contract_at(
            "payments",
            "api/openapi.yaml",
            "api/openapi.baseline.yaml",
        )]);

        let report = check(repo.path(), &config, &Scope::All, &Options::default());
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.rule_id == "contract-partial"),
            "identical-but-unreadable schemas must not report clean: {:?}",
            report.findings
        );
    }

    #[test]
    fn a_remote_ref_is_contract_unreachable_not_a_silent_pass() {
        let remote = r#"
openapi: 3.1.0
paths:
  /p:
    get:
      operationId: getP
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                $ref: 'https://example.com/schema.yaml#/Payment'
"#;
        let repo = repo_with(&[
            ("api/openapi.baseline.yaml", SPEC),
            ("api/openapi.yaml", remote),
        ]);
        let config = config_of(vec![contract_at(
            "payments",
            "api/openapi.yaml",
            "api/openapi.baseline.yaml",
        )]);

        let report = check(repo.path(), &config, &Scope::All, &Options::default());
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.rule_id == "contract-unreachable"),
            "a remote ref must be refused: {:?}",
            report.findings
        );
        assert_eq!(report.exit_code(Severity::Error), 1);
    }

    #[test]
    fn only_restricts_to_the_named_contracts() {
        let broken = "openapi: 3.1.0\npaths: {}\n";
        let repo = repo_with(&[
            ("api/a.baseline.yaml", SPEC),
            ("api/a.yaml", broken),
            ("api/b.baseline.yaml", SPEC),
            ("api/b.yaml", SPEC),
        ]);
        let config = config_of(vec![
            contract_at("a", "api/a.yaml", "api/a.baseline.yaml"),
            contract_at("b", "api/b.yaml", "api/b.baseline.yaml"),
        ]);

        let report = check(
            repo.path(),
            &config,
            &Scope::All,
            &Options {
                only: vec!["b".to_owned()],
                ..Options::default()
            },
        );
        assert_eq!(report.contracts_checked, 1);
        assert_eq!(report.exit_code(Severity::Error), 0);
    }

    #[test]
    fn the_compatibility_override_changes_the_verdict() {
        let base = r#"
openapi: 3.1.0
paths:
  /p:
    get:
      operationId: getP
      responses:
        "200":
          description: ok
"#;
        let head = base.replace("getP", "fetchP");
        let repo = repo_with(&[
            ("api/openapi.baseline.yaml", base),
            ("api/openapi.yaml", &head),
        ]);
        let config = config_of(vec![contract_at(
            "payments",
            "api/openapi.yaml",
            "api/openapi.baseline.yaml",
        )]);

        let wire_json = check(repo.path(), &config, &Scope::All, &Options::default());
        assert_eq!(wire_json.exit_code(Severity::Error), 0);

        let surface = check(
            repo.path(),
            &config,
            &Scope::All,
            &Options {
                compatibility: Some(Compatibility::Surface),
                ..Options::default()
            },
        );
        assert_eq!(
            surface.exit_code(Severity::Error),
            1,
            "an operationId rename breaks generated clients: {:?}",
            surface.findings
        );
    }
}
