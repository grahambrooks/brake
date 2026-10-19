//! The orchestration: config → baseline → ingest → compare → rules → report.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::Severity;
use crate::baseline::{self, BaselineError};
use crate::compare;
use crate::config::{Completeness, Config, ContractConfig, ContractFormat, Defaults};
use crate::contract::{self, Contract};
use crate::demand::load::BoundConsumer;
use crate::demand::{self, policy};
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
    /// Restrict to these consumer names — `--consumer`, which mirrors
    /// `--contract`. Empty means every declared consumer.
    pub consumers: Vec<String>,
    /// Documents read from memory instead of the working tree, keyed by
    /// repository-relative path.
    ///
    /// An agent's unsaved draft goes here, so the draft gets the whole check —
    /// suppressions, consumer rules, cross-file `$ref`s — rather than whatever
    /// subset an adapter could reassemble. Only the head side reads them: an
    /// edit to the working tree must not change what "before" means. See
    /// `design/09-one-core-three-surfaces.md`.
    pub overrides: BTreeMap<PathBuf, Vec<u8>>,
    /// A baseline supplied as a document rather than resolved from the
    /// repository. It has no siblings brake can locate, so a cross-file `$ref`
    /// in it is reported as unmodelled.
    pub baseline_document: Option<Vec<u8>>,
    /// Whether this caller may run a declared generator. `drift` asks for it;
    /// this decides whether it is allowed, so the refusal lives here rather
    /// than in every adapter that must remember not to ask.
    pub trust: Trust,
}

/// Whether a run may spawn the one subprocess brake has, the `--drift`
/// generator.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Trust {
    /// The caller is the person who owns the repository: the CLI, or a
    /// library caller. `drift` is honoured.
    #[default]
    Trusted,
    /// The inputs may be untrusted, as they are for an MCP tool call. A
    /// declared generator is never run, whatever `drift` says.
    Untrusted,
}

/// Read a head-side document: the override if there is one, else the file.
fn read_head(repo_root: &Path, relative: &Path, options: &Options) -> std::io::Result<Vec<u8>> {
    match override_for(&options.overrides, relative) {
        Some(bytes) => Ok(bytes.clone()),
        None => fs::read(repo_root.join(relative)),
    }
}

fn override_for<'a>(
    overrides: &'a BTreeMap<PathBuf, Vec<u8>>,
    relative: &Path,
) -> Option<&'a Vec<u8>> {
    let wanted = lexical(relative);
    overrides
        .iter()
        .find(|(path, _)| lexical(path) == wanted)
        .map(|(_, bytes)| bytes)
}

/// A path reduced to `/`-separated normal components, without touching the
/// filesystem: an overridden document need not exist on disk.
fn lexical(path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::ParentDir => {
                parts.pop();
            }
            _ => {}
        }
    }
    parts.join("/")
}

/// The head side's resolver: the working tree, with the overrides on top.
fn head_resolver(repo_root: &Path, document_dir: &str, options: &Options) -> OverlayResolver {
    OverlayResolver {
        disk: contract::FileSystemResolver::new(repo_root.join(document_dir)),
        document_dir: PathBuf::from(document_dir),
        overrides: options.overrides.clone(),
    }
}

/// Sibling documents for a head contract, from the overrides first.
///
/// So a draft of a shared schema is read along with the draft that `$ref`s
/// it, and an untouched sibling still comes from disk.
struct OverlayResolver {
    disk: contract::FileSystemResolver,
    document_dir: PathBuf,
    overrides: BTreeMap<PathBuf, Vec<u8>>,
}

impl contract::DocumentResolver for OverlayResolver {
    fn resolve(&self, relative_path: &str) -> Option<Vec<u8>> {
        override_for(&self.overrides, &self.document_dir.join(relative_path))
            .cloned()
            .or_else(|| self.disk.resolve(relative_path))
    }
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

    // Demand is the third input, and it is compared against `head` only: a
    // consumer's expectation is a statement about the contract as it is now,
    // so no baseline is involved. See design/05-consumer-demand.md §4.
    let demand = demand_pass(repo_root, config, &selected.contracts, scope, options);
    report.findings.extend(demand.findings);
    report.sources.extend(demand.sources);

    // Attribution is evidence attached to a finding, not a second finding —
    // §6.4. Every existing rule gains an `affects` list, and that is the whole
    // change.
    policy::attribute(&mut report.findings, &demand.bound);
    report.findings = policy::apply(
        std::mem::take(&mut report.findings),
        config.consumer_options,
        demand.declared,
    );

    report.finalise();
    report
}

/// What the demand axis contributed to a run.
#[derive(Debug, Default)]
struct DemandPass {
    findings: Vec<Finding>,
    sources: BTreeMap<String, String>,
    /// Every declared consumer, bound, for the attribution join.
    bound: Vec<BoundConsumer>,
    /// How many declarations were read — the number the `triage` note prints,
    /// so a downgrade always states what it rests on.
    declared: usize,
}

fn demand_pass(
    repo_root: &Path,
    config: &Config,
    selected: &[&ContractConfig],
    scope: &Scope,
    options: &Options,
) -> DemandPass {
    let mut pass = DemandPass::default();
    if config.consumers.is_empty() {
        // Nothing declared, so there is nothing to verify and nothing to
        // attribute. The undeclared sweep below is still worth running: a pact
        // in the tree that nothing declares is the gap this rule exists for.
        pass.findings
            .extend(undeclared_notices(repo_root, config, scope));
        return pass;
    }

    let loaded = demand::load::load(repo_root, config);
    pass.findings.extend(loaded.findings.clone());
    pass.sources.extend(loaded.sources.clone());

    let wanted = |consumer: &str| {
        options.consumers.is_empty() || options.consumers.iter().any(|name| name == consumer)
    };
    let declared: Vec<_> = loaded
        .declared
        .iter()
        .filter(|declared| wanted(&declared.consumer))
        .collect();
    pass.declared = declared.len();

    for contract in selected {
        if !options.only.is_empty() && !options.only.contains(&contract.name) {
            continue;
        }
        let source = display_path(&contract.source);
        let Ok(bytes) = read_head(repo_root, &contract.source, options) else {
            continue;
        };
        let resolver = head_resolver(repo_root, &parent_dir(&contract.source), options);
        let Ok(head) = ingest(contract.format, &source, &bytes, &resolver) else {
            // `contract-unreachable` is already on the report from the
            // baseline pass; reporting it twice would read as two problems.
            continue;
        };

        let mut bound_here = Vec::new();
        for entry in &declared {
            if entry.provider != contract.name {
                continue;
            }
            pass.findings
                .extend(demand::verify::verify(&entry.demand, &contract.name, &head));
            bound_here.push(entry.bind(&head));
        }

        // The one rule that reports a suspected absence, which the thesis
        // forbids at commit time. `analyze` and `brake consumers` only, and
        // only under an explicit closed-world declaration.
        if matches!(scope, Scope::All)
            && config.consumer_options.completeness == Completeness::ClosedWorld
        {
            pass.findings.extend(policy::unused_surface(
                &contract.name,
                head.endpoints.keys().cloned(),
                &bound_here,
            ));
        }
        pass.bound.extend(bound_here);
    }

    pass.findings
        .extend(undeclared_notices(repo_root, config, scope));
    pass
}

/// Files in the tree that parse as a demand and nothing declares.
fn undeclared_notices(repo_root: &Path, config: &Config, scope: &Scope) -> Vec<Finding> {
    match scope {
        Scope::Paths(paths) => {
            let wanted: Vec<String> = paths
                .iter()
                .map(|path| normalise_against_root(repo_root, path))
                .collect();
            demand::load::undeclared(repo_root, config, Some(&wanted))
        }
        Scope::All => demand::load::undeclared(repo_root, config, None),
        // A `--since` run is scoped to what changed, and walking the whole
        // tree for it would report files the change never touched.
        Scope::Since(_) => Vec::new(),
    }
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
    let head_bytes = match read_head(repo_root, &contract.source, options) {
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

    let level = options
        .compatibility
        .unwrap_or_else(|| contract.effective_compatibility(defaults));

    let baseline_override = options.baseline.clone();
    let resolution = match &options.baseline_document {
        Some(bytes) => Ok(baseline::ResolvedBaseline {
            label: "baseline".to_owned(),
            bytes: bytes.clone(),
            origin: baseline::BaselineOrigin::Isolated,
        }),
        None => baseline::resolve_for_contract(
            repo_root,
            defaults,
            contract,
            baseline_override.as_ref(),
        ),
    };
    let resolved = match resolution {
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
            // Not linted: a user with no baseline has not opted in to gating
            // at all, and an error here would fail a run they never asked for.
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
            report.findings.extend(lint_without_baseline(
                repo_root,
                contract,
                &head_relative,
                &head_bytes,
                level,
                options,
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

    // Two resolvers, not one. A cross-file `$ref` on the baseline side has to
    // be read from wherever the baseline came from — the commit, not the
    // working tree. Sharing one working-tree resolver put today's shared
    // schema on both sides, so a field deleted from a file two contracts
    // `$ref` was missing from base and head alike and the run reported clean.
    let document_dir = parent_dir(&contract.source);
    let base_resolver = resolved.origin.resolver(repo_root, &document_dir);
    let head_resolver = head_resolver(repo_root, &document_dir, options);
    let base_contract = match ingest(
        contract.format,
        &resolved.label,
        &resolved.bytes,
        base_resolver.as_ref(),
    ) {
        Ok(parsed) => parsed,
        Err(error) => {
            report
                .findings
                .push(rules::contract_unreachable(&contract.name, &error, None));
            return report;
        }
    };
    let head_contract = match ingest(contract.format, &head_relative, &head_bytes, &head_resolver) {
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

    report.findings = evaluate_pair(&base_contract, &head_contract, contract, level, options);

    // Asked for, and allowed. An untrusted caller never reaches the one
    // subprocess brake has, however it sets `drift`.
    if options.drift
        && options.trust == Trust::Trusted
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

/// Every rule over one base and head pair, with the contract's suppressions.
///
/// The one place a pair becomes findings. `check_contract` reaches it from
/// the repository and `check_documents` from two documents in memory, so the
/// two cannot disagree about which rules run.
fn evaluate_pair(
    base: &Contract,
    head: &Contract,
    contract: &ContractConfig,
    level: crate::config::Compatibility,
    options: &Options,
) -> Vec<Finding> {
    let mut changes = compare::compare_contracts(base, head);
    // An unmodelled construct on an endpoint that is byte-identical on both
    // sides is still an unverified path, and must not read as clean.
    changes.extend(compare::partial_changes(head));
    changes.sort();
    changes.dedup();

    let mut findings = rules::evaluate(&changes, &contract.name, level);
    // Structural lint reads the head contract on its own, but reports only the
    // defects the baseline does not already have. See
    // design/07-structural-lint.md.
    findings.extend(rules::lint::introduced(base, head, &contract.name, level));
    findings.sort();
    findings.dedup();

    rules::apply_suppressions(
        findings,
        &contract.name,
        &contract.allow,
        options.as_of.as_deref(),
        options.report_stale,
    )
}

/// One document supplied from memory, and how findings should name it.
///
/// The label is what a span's `file` says, so a CI annotation points at the
/// file the caller meant: a path for `brake compare`, a role for an MCP tool
/// that was handed text.
#[derive(Debug, Clone, Copy)]
pub struct Document<'a> {
    pub label: &'a str,
    pub bytes: &'a [u8],
}

/// Check two documents, with no configuration and no repository.
///
/// MCP `compare_contracts` and `brake compare`. Both documents are parsed with
/// no resolver, so a `$ref` into a sibling file is reported as unmodelled
/// rather than read from whatever directory the caller happens to be in: the
/// caller supplied two documents, not a tree.
///
/// A document that does not parse is reported as `contract-unreachable`, as
/// it is for a configured contract.
#[must_use]
pub fn check_documents(
    format: ContractFormat,
    base: Document<'_>,
    head: Document<'_>,
    level: crate::config::Compatibility,
) -> Report {
    const NAME: &str = "inline";
    let mut report = Report::new(Vec::new(), Vec::new(), 1);
    let none = contract::SingleDocumentResolver;
    let parsed = ingest(format, base.label, base.bytes, &none).and_then(|base_contract| {
        ingest(format, head.label, head.bytes, &none)
            .map(|head_contract| (base_contract, head_contract))
    });
    match parsed {
        Ok((base_contract, head_contract)) => {
            // So the text renderer can quote the offending line, as it does
            // for a file.
            for document in [base, head] {
                if let Ok(text) = std::str::from_utf8(document.bytes) {
                    report
                        .sources
                        .insert(document.label.to_owned(), text.to_owned());
                }
            }
            let contract = ContractConfig {
                name: NAME.to_owned(),
                format,
                source: PathBuf::from(head.label),
                compatibility: Some(level),
                baseline: None,
                allow: Vec::new(),
                generated: None,
            };
            report.findings = evaluate_pair(
                &base_contract,
                &head_contract,
                &contract,
                level,
                &Options::default(),
            );
        }
        Err(error) => report
            .findings
            .push(rules::contract_unreachable(NAME, &error, None)),
    }
    report.finalise();
    report
}

/// Structural lint for a contract that is new in this change.
///
/// Lint needs no baseline, and a contract absent from the baseline introduces
/// every defect it has. Returning early without it let a first draft that
/// breaks every generated client through as a note.
fn lint_without_baseline(
    repo_root: &Path,
    contract: &ContractConfig,
    head_relative: &str,
    head_bytes: &[u8],
    level: crate::config::Compatibility,
    options: &Options,
) -> Vec<Finding> {
    let resolver = head_resolver(repo_root, &parent_dir(&contract.source), options);
    match ingest(contract.format, head_relative, head_bytes, &resolver) {
        Ok(head) => rules::apply_suppressions(
            rules::lint::lint(&head, &contract.name, level),
            &contract.name,
            &contract.allow,
            options.as_of.as_deref(),
            // Every compatibility rule is absent here by construction, so an
            // `allow` aimed at one would read as stale when it is not.
            false,
        ),
        // A contract that does not parse cannot be linted, and saying nothing
        // would read as a clean lint.
        Err(error) => vec![rules::contract_unreachable(&contract.name, &error, None)],
    }
}

fn ingest(
    format: ContractFormat,
    source: &str,
    bytes: &[u8],
    resolver: &dyn contract::DocumentResolver,
) -> Result<Contract, String> {
    match format {
        ContractFormat::Openapi => contract::openapi::ingest_with_resolver(source, bytes, resolver)
            .map_err(|error| error.to_string()),
        ContractFormat::Proto => contract::proto::ingest_with_resolver(source, bytes, resolver)
            .map_err(|error| error.to_string()),
        ContractFormat::Graphql => contract::graphql::ingest_with_resolver(source, bytes, resolver)
            .map_err(|error| error.to_string()),
        ContractFormat::Asyncapi => {
            contract::asyncapi::ingest_with_resolver(source, bytes, resolver)
                .map_err(|error| error.to_string())
        }
        ContractFormat::Mcp => contract::mcp::ingest_with_resolver(source, bytes, resolver)
            .map_err(|error| error.to_string()),
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

    // A path scope that names a consumer declaration selects the contracts
    // that declaration constrains, so a hook run on a pact-updating commit
    // verifies the right thing rather than nothing.
    let via_demand: std::collections::BTreeSet<String> = wanted
        .iter()
        .flat_map(|path| demand::load::providers_for_path(repo_root, config, path))
        .collect();

    let contracts = config
        .contracts
        .iter()
        .filter(|contract| {
            wanted.contains(&display_path(&contract.source)) || via_demand.contains(&contract.name)
        })
        .collect::<Vec<_>>();

    let mut notices = Vec::new();
    for path in &wanted {
        let configured = config
            .contracts
            .iter()
            .any(|contract| &display_path(&contract.source) == path);
        // A consumer declaration is not an unconfigured contract; the
        // `consumer-undeclared` sweep is what reports those.
        if !configured && demand::identify(&repo_root.join(path)).is_some() {
            continue;
        }
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

/// Build the shell invocation for a declared generator.
///
/// On Windows the command must be appended *verbatim*. Rust escapes arguments
/// using MSVC's rules, and `cmd.exe` does not parse those — so a command
/// containing quotes, which any path with a space needs, reaches cmd mangled
/// and fails. `raw_arg` is the documented way to hand cmd a command line
/// unaltered, and without it `--drift` is broken on Windows for anyone whose
/// generator quotes a path.
fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        let mut builder = Command::new("cmd");
        builder.arg("/C");
        builder.raw_arg(command);
        builder
    }
    #[cfg(not(windows))]
    {
        let mut builder = Command::new("sh");
        builder.arg("-c").arg(command);
        builder
    }
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

    let mut child = shell_command(command)
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
        affects: Vec::new(),
        note: None,
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

/// The directory a contract document sits in, repository-relative and
/// `/`-separated. Empty when the document is at the repository root.
///
/// A cross-file `$ref` is resolved relative to this, so it is also the
/// boundary such a reference cannot reach past.
#[must_use]
pub fn parent_dir(source: &Path) -> String {
    source.parent().map(display_path).unwrap_or_default()
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
            ..Config::default()
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

    /// A shell command that creates `path` as a marker, in whichever shell
    /// `--drift` uses.
    ///
    /// `mkdir` rather than `touch`: it is a builtin in both `sh` and `cmd`,
    /// needs no redirection, and depends on nothing being on PATH.
    fn create_file_command(path: &std::path::Path) -> String {
        format!("mkdir \"{}\"", path.display())
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
            // Both, because an empty `findings` with a non-zero exit means the
            // generator did not run at all — which is what a shell mismatch
            // looks like, and the findings list alone does not say so.
            "findings: {:?}\nunavailable: {:?}",
            report.findings,
            report.unavailable
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

    const DUPLICATE_OPERATION_ID: &str = r#"
openapi: 3.1.0
info: { title: Payments, version: "1" }
paths:
  /payments:
    get:
      operationId: listPayments
      responses: { "200": { description: ok } }
  /refunds:
    get:
      operationId: listPayments
      responses: { "200": { description: ok } }
"#;

    fn lint_rules(report: &Report) -> Vec<&'static str> {
        report
            .findings
            .iter()
            .map(|finding| finding.rule_id)
            .filter(|rule| *rule == "operation-id-duplicate")
            .collect()
    }

    fn surface(mut contract: ContractConfig) -> ContractConfig {
        contract.compatibility = Some(crate::config::Compatibility::Surface);
        contract
    }

    #[test]
    fn a_structural_defect_already_in_the_baseline_fails_no_commit() {
        let repo = repo_with(&[
            ("api/openapi.baseline.yaml", DUPLICATE_OPERATION_ID),
            ("api/openapi.yaml", DUPLICATE_OPERATION_ID),
        ]);
        let config = config_of(vec![surface(contract_at(
            "payments",
            "api/openapi.yaml",
            "api/openapi.baseline.yaml",
        ))]);

        let report = check(repo.path(), &config, &Scope::All, &Options::default());
        assert_eq!(lint_rules(&report), Vec::<&str>::new(), "{report:?}");
        assert_eq!(report.exit_code(Severity::Error), 0);
    }

    #[test]
    fn a_structural_defect_the_change_adds_fails_the_commit() {
        // The baseline's second operation had its own id; the change reuses one.
        let duplicated = "operationId: listPayments";
        let second = DUPLICATE_OPERATION_ID.rfind(duplicated).expect("fixture");
        let baseline = format!(
            "{}operationId: listRefunds{}",
            &DUPLICATE_OPERATION_ID[..second],
            &DUPLICATE_OPERATION_ID[second + duplicated.len()..]
        );
        let repo = repo_with(&[
            ("api/openapi.baseline.yaml", &baseline),
            ("api/openapi.yaml", DUPLICATE_OPERATION_ID),
        ]);
        let config = config_of(vec![surface(contract_at(
            "payments",
            "api/openapi.yaml",
            "api/openapi.baseline.yaml",
        ))]);

        let report = check(repo.path(), &config, &Scope::All, &Options::default());
        assert_eq!(
            lint_rules(&report),
            vec!["operation-id-duplicate"],
            "{report:?}"
        );
    }

    #[test]
    fn a_new_contract_is_linted_in_full() {
        let repo = repo_with(&[("api/openapi.yaml", DUPLICATE_OPERATION_ID)]);
        run_git(repo.path(), &["init", "-b", "main"]);
        run_git(repo.path(), &["config", "user.name", "Brake Test"]);
        run_git(repo.path(), &["config", "user.email", "brake@example.com"]);
        fs::write(repo.path().join("README.md"), "# repo\n").expect("write");
        run_git(repo.path(), &["add", "README.md"]);
        run_git(repo.path(), &["commit", "-m", "before the api existed"]);

        let mut contract = surface(contract_at("payments", "api/openapi.yaml", "unused"));
        contract.baseline = Some(Baseline::Rev {
            rev: "HEAD".to_owned(),
        });
        let report = check(
            repo.path(),
            &config_of(vec![contract.clone()]),
            &Scope::All,
            &Options::default(),
        );
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.rule_id == "contract-new"),
            "{report:?}"
        );
        assert_eq!(lint_rules(&report), vec!["operation-id-duplicate"]);

        // And suppressible, like every finding on the gated path.
        contract.allow = vec![crate::config::Suppression {
            rule: "operation-id-duplicate".to_owned(),
            endpoint: None,
            field: None,
            reason: "two read models, one SDK method, by design".to_owned(),
            expires: None,
        }];
        let report = check(
            repo.path(),
            &config_of(vec![contract]),
            &Scope::All,
            &Options::default(),
        );
        assert_eq!(lint_rules(&report), Vec::<&str>::new(), "{report:?}");
    }

    #[test]
    fn an_unconfigured_baseline_is_not_linted() {
        let repo = repo_with(&[("api/openapi.yaml", DUPLICATE_OPERATION_ID)]);
        let mut contract = surface(contract_at("payments", "api/openapi.yaml", "unused"));
        contract.baseline = None;

        let report = check(
            repo.path(),
            &config_of(vec![contract]),
            &Scope::All,
            &Options::default(),
        );
        assert_eq!(lint_rules(&report), Vec::<&str>::new(), "{report:?}");
        assert_eq!(report.exit_code(Severity::Error), 0);
    }

    /// A shared schema is part of the contract, so the baseline's copy of it
    /// has to come from the baseline's own revision.
    ///
    /// Both sides used to be ingested through one working-tree resolver, which
    /// put today's `common/models.yaml` on the base side too. A field deleted
    /// from it was then missing from base and head alike, the comparison found
    /// nothing, and the run reported clean — the exact shape of false
    /// confidence the thesis forbids.
    #[test]
    fn a_break_in_a_shared_document_is_not_hidden_by_the_working_tree() {
        const ROOT: &str = r#"
openapi: 3.1.0
paths:
  /payments/{id}:
    get:
      operationId: getPayment
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                $ref: "./common/models.yaml#/components/schemas/Payment"
"#;
        const SHARED_BEFORE: &str = r#"
openapi: 3.1.0
components:
  schemas:
    Payment:
      type: object
      required: [id, customer_id]
      properties:
        id: { type: string }
        customer_id: { type: string }
"#;
        const SHARED_AFTER: &str = r#"
openapi: 3.1.0
components:
  schemas:
    Payment:
      type: object
      required: [id]
      properties:
        id: { type: string }
"#;

        let repo = repo_with(&[
            ("api/openapi.yaml", ROOT),
            ("api/common/models.yaml", SHARED_BEFORE),
        ]);
        run_git(repo.path(), &["init", "-b", "main"]);
        run_git(repo.path(), &["config", "user.name", "Brake Test"]);
        run_git(repo.path(), &["config", "user.email", "brake@example.com"]);
        run_git(repo.path(), &["add", "."]);
        run_git(repo.path(), &["commit", "-m", "the contract, in two files"]);

        // Only the shared document changes; the contract's own file does not.
        fs::write(repo.path().join("api/common/models.yaml"), SHARED_AFTER).expect("write");

        let mut config = config_of(vec![contract_at("payments", "api/openapi.yaml", "unused")]);
        config.contracts[0].baseline = Some(Baseline::Rev {
            rev: "HEAD".to_owned(),
        });

        let report = check(repo.path(), &config, &Scope::All, &Options::default());

        assert_eq!(
            report.exit_code(Severity::Error),
            1,
            "a field removed from a shared document is still a removal: {report:?}"
        );
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.rule_id == "response-field-removed"),
            "expected response-field-removed, got {:?}",
            report
                .findings
                .iter()
                .map(|finding| finding.rule_id)
                .collect::<Vec<_>>()
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
        // `--drift` runs the command through the platform shell, so the
        // fixture has to speak the one it will get. cmd does not expand
        // `$VAR`, which is why this test was failing on Windows CI while
        // `make check` stayed green on a developer's machine.
        contract.generated = Some(crate::config::GeneratedConfig {
            command: if cfg!(windows) {
                // cmd's own builtin, with cmd's own variable syntax and path
                // separators. `type` copies bytes verbatim, unlike `echo`,
                // which would append CRLF and break the byte comparison.
                "type \"%BRAKE_REPO_ROOT%\\api\\openapi.yaml\"".to_owned()
            } else {
                "cat \"$BRAKE_REPO_ROOT/api/openapi.yaml\"".to_owned()
            },
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
            // Both, because an empty `findings` with a non-zero exit means the
            // generator did not run at all — which is what a shell mismatch
            // looks like, and the findings list alone does not say so.
            "findings: {:?}\nunavailable: {:?}",
            report.findings,
            report.unavailable
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
    fn an_untrusted_caller_never_reaches_the_drift_subprocess() {
        let marker = tempdir().expect("tempdir");
        let witness = marker.path().join("the-generator-ran");
        let repo = repo_with(&[
            ("api/openapi.baseline.yaml", SPEC),
            ("api/openapi.yaml", SPEC),
        ]);
        let mut contract = contract_at("payments", "api/openapi.yaml", "api/openapi.baseline.yaml");
        contract.generated = Some(crate::config::GeneratedConfig {
            command: create_file_command(&witness),
        });
        let config = config_of(vec![contract]);

        // `drift` asked for, and refused: the refusal is the core's, not the
        // caller's to remember.
        let options = Options {
            drift: true,
            trust: Trust::Untrusted,
            ..Options::default()
        };
        check(repo.path(), &config, &Scope::All, &options);
        assert!(!witness.exists(), "an untrusted run spawned the generator");

        // The same request, trusted, does run it — so the guard above is
        // refusing, not failing to launch.
        let options = Options {
            drift: true,
            ..Options::default()
        };
        check(repo.path(), &config, &Scope::All, &options);
        assert!(
            witness.exists(),
            "a trusted --drift run did not spawn the generator"
        );
    }

    #[test]
    fn an_override_replaces_the_head_and_never_the_baseline() {
        let repo = repo_with(&[
            ("api/openapi.baseline.yaml", SPEC),
            ("api/openapi.yaml", SPEC),
        ]);
        let config = config_of(vec![contract_at(
            "payments",
            "api/openapi.yaml",
            "api/openapi.baseline.yaml",
        )]);
        let broken = SPEC.replace("/payments/{id}", "/refunds/{id}");
        assert_ne!(broken, SPEC, "the fixture must contain the path this moves");

        // The head, from memory: the disk copy is unchanged and clean.
        let mut options = Options::default();
        options.overrides.insert(
            PathBuf::from("api/openapi.yaml"),
            broken.clone().into_bytes(),
        );
        let report = check(repo.path(), &config, &Scope::All, &options);
        assert_eq!(report.exit_code(Severity::Error), 1, "{report:?}");

        // Overriding the baseline's *file* changes nothing: a baseline is
        // resolved from where it was declared, never from the overrides.
        let mut options = Options::default();
        options.overrides.insert(
            PathBuf::from("api/openapi.baseline.yaml"),
            broken.into_bytes(),
        );
        let report = check(repo.path(), &config, &Scope::All, &options);
        assert_eq!(report.exit_code(Severity::Error), 0, "{report:?}");
    }

    #[test]
    fn check_documents_is_hermetic_and_reports_what_it_cannot_read() {
        let base = SPEC;
        let head = SPEC.replace("/payments/{id}", "/refunds/{id}");
        assert_ne!(head, SPEC);
        let document = |label, bytes| Document { label, bytes };
        let report = check_documents(
            ContractFormat::Openapi,
            document("old.yaml", base.as_bytes()),
            document("new.yaml", head.as_bytes()),
            crate::config::Compatibility::WireJson,
        );
        assert_eq!(report.exit_code(Severity::Error), 1, "{report:?}");
        assert!(
            report
                .findings
                .iter()
                .filter_map(|finding| finding.span.as_ref())
                .all(|span| span.file == "old.yaml" || span.file == "new.yaml"),
            "spans must name the files the caller supplied: {report:?}"
        );

        let report = check_documents(
            ContractFormat::Openapi,
            document("old.yaml", base.as_bytes()),
            document("new.yaml", b"this: is: not: openapi"),
            crate::config::Compatibility::WireJson,
        );
        assert_eq!(report.findings[0].rule_id, "contract-unreachable");
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
            // `touch` does not exist in cmd, so on Windows this guard used to
            // pass because the command failed rather than because brake
            // refused to run it. A guard that cannot fail is not a guard.
            command: create_file_command(&witness),
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
