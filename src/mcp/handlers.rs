//! The tools, as plain synchronous functions.
//!
//! No async, no `rmcp`, no protocol. Each takes deserialised arguments and
//! returns JSON, so the whole tool surface is testable without a transport and
//! every verdict comes from the same library call the CLI makes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::check::{self, Document, Options as CheckOptions, Scope, Trust};
use crate::config::{Baseline, Compatibility, Config, ContractConfig, ContractFormat};
use crate::report::Report;
use crate::rules::{Finding, catalogue, strategies};
use crate::{Severity, Verdict};

/// Everything a tool call needs that does not come from its arguments.
#[derive(Debug, Clone)]
pub struct Context {
    /// The repository the server was started in. Every path resolves under it.
    pub repo_root: PathBuf,
    /// The date suppression expiry is evaluated against.
    pub as_of: String,
}

impl Context {
    #[must_use]
    pub fn new(repo_root: PathBuf, as_of: String) -> Self {
        Self { repo_root, as_of }
    }

    /// The check options every tool uses.
    ///
    /// `trust` is what keeps a tool call away from the `--drift` subprocess:
    /// the core refuses an untrusted caller whatever `drift` says, so no edit
    /// here can turn it on by flipping a flag.
    fn check_options(&self, compatibility: Option<Compatibility>) -> CheckOptions {
        CheckOptions {
            as_of: Some(self.as_of.clone()),
            drift: false,
            // A tool call is not a whole-repository run, and a suppression it
            // did not look at is not dead.
            report_stale: false,
            compatibility,
            trust: Trust::Untrusted,
            ..CheckOptions::default()
        }
    }
}

/// Why a tool could not answer.
///
/// Distinct from "the API broke": a finding is an answer, and only a failure
/// to determine one is an error. `design/04-mcp-interface.md` §6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolFailure {
    pub message: String,
}

impl ToolFailure {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ToolFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

type ToolResult = Result<Value, ToolFailure>;

/// The tools this server exposes, in the order they are listed.
pub const TOOL_NAMES: &[&str] = &[
    "check_change",
    "compare_contracts",
    "explain_rule",
    "check_repository",
    "who_consumes",
];

// ── check_change ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CheckChangeArgs {
    pub format: String,
    /// The full proposed document, as text.
    ///
    /// Text rather than a path because an agent holds an unsaved draft — the
    /// case this whole interface exists for.
    pub proposed: String,
    /// Which configured contract to compare against. Optional: with
    /// `baseline_document` instead, no `brake.toml` is needed at all.
    #[serde(default)]
    pub contract: Option<String>,
    /// An inline baseline, for an agent with no configured repository.
    #[serde(default)]
    pub baseline_document: Option<String>,
    #[serde(default)]
    pub compatibility: Option<String>,
}

pub fn check_change(context: &Context, args: CheckChangeArgs) -> ToolResult {
    let format = parse_format(&args.format)?;
    let level = args.compatibility.as_deref().map(parse_level).transpose()?;

    // An inline baseline with no contract named: two documents, and no
    // repository involved at all.
    if let (Some(baseline), None) = (&args.baseline_document, &args.contract) {
        let level = level.unwrap_or(Compatibility::WireJson);
        let report = check::check_documents(
            format,
            Document {
                label: "baseline",
                bytes: baseline.as_bytes(),
            },
            Document {
                label: "proposed",
                bytes: args.proposed.as_bytes(),
            },
            level,
        );
        return answer(&report, level);
    }

    let config = load_config(&context.repo_root)?;
    let contract = select_contract(&config, args.contract.as_deref())?;
    if contract.format != format {
        return Err(ToolFailure::new(format!(
            "contract `{}` is declared as `{}`, but `{}` was supplied",
            contract.name,
            format_name(contract.format),
            args.format
        )));
    }

    // The proposed document stands in for the file on disk, and the run is the
    // one the commit hook makes for that file. So the draft gets every rule,
    // suppression and consumer check the hook applies. See
    // design/09-one-core-three-surfaces.md.
    let mut options = context.check_options(level);
    options.only = vec![contract.name.clone()];
    options
        .overrides
        .insert(contract.source.clone(), args.proposed.into_bytes());
    options.baseline_document = args.baseline_document.map(String::into_bytes);
    let scope = Scope::Paths(vec![contract.source.clone()]);
    let report = check::check(&context.repo_root, &config, &scope, &options);

    let level = level.unwrap_or_else(|| contract.effective_compatibility(&config.defaults));
    answer(&report, level)
}

/// Every declared consumer of one contract, bound to a contract document.
fn bind_declared(
    repo_root: &Path,
    config: &Config,
    contract: &str,
    document: &crate::Contract,
) -> Vec<crate::demand::load::BoundConsumer> {
    crate::demand::load::load(repo_root, config)
        .declared
        .iter()
        .filter(|declared| declared.provider == contract)
        .map(|declared| declared.bind(document))
        .collect()
}

// ── who_consumes ────────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
pub struct WhoConsumesArgs {
    /// Which configured contract. Required only when more than one is
    /// declared.
    #[serde(default)]
    pub contract: Option<String>,
    /// `GET /payments/{id}`, or just the path. Omit for every endpoint.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// A field, status, parameter or media type. Omit for the whole endpoint.
    #[serde(default)]
    pub field: Option<String>,
}

/// Who declared that they consume this endpoint or field.
///
/// The most valuable thing here for an agent: it can ask who reads a field
/// *before* writing the edit, rather than being told afterwards by a hook.
pub fn who_consumes(context: &Context, args: WhoConsumesArgs) -> ToolResult {
    let config = load_config(&context.repo_root)?;
    let contract = select_contract(&config, args.contract.as_deref())?;
    let source = check::display_path(&contract.source);
    let bytes = std::fs::read(context.repo_root.join(&contract.source))
        .map_err(|error| ToolFailure::new(format!("cannot read `{source}`: {error}")))?;
    let document = crate::parse(contract.format, &source, &bytes)
        .map_err(|error| ToolFailure::new(format!("`{source}` did not parse: {error}")))?;

    let wanted = args.endpoint.as_deref().map(str::trim);
    let bound = bind_declared(&context.repo_root, &config, &contract.name, &document);

    let mut consumers = Vec::new();
    for consumer in &bound {
        let mut uses = Vec::new();
        for (key, usages) in &consumer.usage_index {
            if let Some(wanted) = wanted
                && !matches_endpoint(wanted, key)
            {
                continue;
            }
            if let Some(field) = &args.field
                && !usages.subjects.contains(field)
            {
                continue;
            }
            uses.push(json!({
                "endpoint": format!("{} {}", key.method, key.path),
                "statuses": usages.statuses.iter().collect::<Vec<_>>(),
                "reads": usages.reads.iter().collect::<Vec<_>>(),
                "sends": usages.sends.iter().collect::<Vec<_>>(),
                "declared_at": format!("{}:{}", consumer.source, usages.span.line),
            }));
        }
        if uses.is_empty() {
            continue;
        }
        consumers.push(json!({
            "consumer": consumer.consumer,
            "source": consumer.source,
            "uses": uses,
        }));
    }

    Ok(json!({
        "contract": contract.name,
        "endpoint": args.endpoint,
        "field": args.field,
        "consumers": consumers,
        "declared_consumers": bound.len(),
        // Without this the answer reads as a census, and it is a list of files
        // somebody remembered to declare.
        "note": CENSUS_CAVEAT,
    }))
}

/// `GET /payments/{id}`, or a bare path.
fn matches_endpoint(wanted: &str, key: &crate::contract::EndpointKey) -> bool {
    let full = format!("{} {}", key.method, key.path);
    wanted.eq_ignore_ascii_case(&full) || wanted == key.path
}

// ── compare_contracts ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CompareContractsArgs {
    pub format: String,
    pub base: String,
    pub head: String,
    #[serde(default)]
    pub compatibility: Option<String>,
}

pub fn compare_contracts(_context: &Context, args: CompareContractsArgs) -> ToolResult {
    let format = parse_format(&args.format)?;
    let level = args
        .compatibility
        .as_deref()
        .map(parse_level)
        .transpose()?
        .unwrap_or(Compatibility::WireJson);
    let report = check::check_documents(
        format,
        Document {
            label: "baseline",
            bytes: args.base.as_bytes(),
        },
        Document {
            label: "head",
            bytes: args.head.as_bytes(),
        },
        level,
    );
    answer(&report, level)
}

// ── explain_rule ────────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
pub struct ExplainRuleArgs {
    #[serde(default)]
    pub rule: Option<String>,
}

pub fn explain_rule(_context: &Context, args: ExplainRuleArgs) -> ToolResult {
    let Some(id) = args.rule else {
        return Ok(json!({ "rules": rule_catalogue() }));
    };
    let rule = catalogue::lookup(&id).ok_or_else(|| {
        ToolFailure::new(format!(
            "unknown rule `{id}`. Call this tool with no argument, or read \
             `brake://rules`, for the catalogue"
        ))
    })?;
    Ok(describe_rule(rule))
}

// ── check_repository ────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
pub struct CheckRepositoryArgs {
    /// Restrict to these contracts by name — `--contract`.
    #[serde(default)]
    pub contracts: Vec<String>,
    /// Restrict to these consumers by name — `--consumer`.
    #[serde(default)]
    pub consumers: Vec<String>,
    /// Check only the contracts among these repository-relative paths, as the
    /// commit hook does — `brake check <paths>`.
    #[serde(default)]
    pub paths: Vec<PathBuf>,
    /// Check only what changed since the merge-base with this ref — `--since`.
    #[serde(default)]
    pub since: Option<String>,
    /// Compare against this baseline instead of the configured one, in the
    /// one-string form `--baseline` takes.
    #[serde(default)]
    pub baseline: Option<String>,
    #[serde(default)]
    pub compatibility: Option<String>,
}

pub fn check_repository(context: &Context, args: CheckRepositoryArgs) -> ToolResult {
    let config = load_config(&context.repo_root)?;
    let level = args.compatibility.as_deref().map(parse_level).transpose()?;

    let scope = match (args.paths.is_empty(), args.since) {
        (false, Some(_)) => {
            return Err(ToolFailure::new(
                "`paths` and `since` choose the scope two different ways; supply one",
            ));
        }
        (false, None) => Scope::Paths(args.paths),
        (true, Some(reference)) => Scope::Since(reference),
        (true, None) => Scope::All,
    };

    let mut options = context.check_options(level);
    options.only = args.contracts;
    options.consumers = args.consumers;
    options.baseline = args.baseline.as_deref().map(Baseline::from_flag);
    // Only a run over everything can say a suppression matched nothing.
    options.report_stale = scope == Scope::All;

    let report = check::check(&context.repo_root, &config, &scope, &options);
    Ok(render_report(
        &report,
        level.unwrap_or(config.defaults.compatibility),
    ))
}

// ── resources ───────────────────────────────────────────────────────────────

/// The resource URIs this server serves, with their descriptions.
pub const RESOURCES: &[(&str, &str)] = &[
    (
        "brake://rules",
        "Every rule brake can report, with its severity, the compatibility level it \
         fires from, and the ways to make the change safely.",
    ),
    (
        "brake://strategies",
        "The API evolution strategies brake names when a change breaks a consumer, \
         each with what it costs. Read this before drafting a change, not after.",
    ),
    (
        "brake://config",
        "The resolved brake.toml for the repository the server was started in.",
    ),
    (
        "brake://consumers",
        "The declared consumers of each contract, with the file and content \
         digest each declaration came from. brake knows about the consumers \
         declared in brake.toml and no others.",
    ),
];

pub fn read_resource(context: &Context, uri: &str) -> Result<String, ToolFailure> {
    match uri {
        "brake://rules" => Ok(pretty(&json!({ "rules": rule_catalogue() }))),
        "brake://strategies" => Ok(pretty(&json!({
            "strategies": strategies::STRATEGIES
                .iter()
                .map(|strategy| json!({
                    "id": strategy.id,
                    // Unbound: there is no finding to bind to here.
                    "summary": strategy
                        .summary
                        .replace("{subject}", "the field")
                        .replace("{endpoint}", "the endpoint"),
                    "cost": strategy.cost,
                }))
                .collect::<Vec<_>>(),
            "note": CHOICE_IS_NOT_BRAKES,
        }))),
        "brake://config" => Ok(pretty(&match load_config(&context.repo_root) {
            Ok(config) => json!({
                "configured": true,
                "default_compatibility": level_name(config.defaults.compatibility),
                "contracts": config
                    .contracts
                    .iter()
                    .map(|contract| json!({
                        "name": contract.name,
                        "format": format_name(contract.format),
                        "source": check::display_path(&contract.source),
                        "compatibility": level_name(
                            contract.effective_compatibility(&config.defaults)
                        ),
                        "baseline": contract
                            .effective_baseline(&config.defaults)
                            .map(Baseline::strategy_name),
                        // Stated so an agent does not have to infer it from
                        // the baseline kind.
                        "gates": contract
                            .effective_baseline(&config.defaults)
                            .map_or("nothing — no baseline is configured", |baseline| {
                                if baseline.is_release_baseline() {
                                    "the delta since the last release"
                                } else {
                                    "this change against the trunk"
                                }
                            }),
                    }))
                    .collect::<Vec<_>>(),
            }),
            Err(failure) => json!({
                "configured": false,
                "reason": failure.message,
                "note": "`compare_contracts` needs no configuration and works on two documents.",
            }),
        })),
        "brake://consumers" => Ok(match load_config(&context.repo_root) {
            Ok(config) => {
                let inventory =
                    crate::demand::inventory::build(&context.repo_root, &config, &[], &[]);
                crate::demand::inventory::render_json(&inventory)
            }
            Err(failure) => pretty(&json!({
                "configured": false,
                "reason": failure.message,
            })),
        }),
        // A URI outside the served set is refused rather than treated as a
        // path: this server reads contracts, not arbitrary files.
        other => Err(ToolFailure::new(format!(
            "unknown resource `{other}`. This server serves: {}",
            RESOURCES
                .iter()
                .map(|(uri, _)| *uri)
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

// ── the prompt ──────────────────────────────────────────────────────────────

pub const PROMPT_NAME: &str = "review-api-change";

#[derive(Debug, Deserialize)]
pub struct ReviewPromptArgs {
    pub format: String,
    pub base: String,
    pub head: String,
    #[serde(default)]
    pub compatibility: Option<String>,
}

/// The `review-api-change` prompt.
///
/// The framing is brake's rather than the agent's, which is most of the value:
/// the difference between "here are some warnings" and "here is what a
/// consumer of this API experiences, and here are the ways to give them what
/// you want without that".
pub fn review_api_change(context: &Context, args: ReviewPromptArgs) -> Result<String, ToolFailure> {
    let verdict = compare_contracts(
        context,
        CompareContractsArgs {
            format: args.format,
            base: args.base,
            head: args.head,
            compatibility: args.compatibility,
        },
    )?;

    let findings = verdict["findings"].as_array().map_or(0, Vec::len);
    let unverified = verdict["unverified"].as_array().map_or(0, Vec::len);

    let mut prompt = String::from(
        "You are reviewing a change to an API contract on behalf of the people who \
         consume it. They cannot see this diff and did not agree to it.\n\n",
    );

    if findings == 0 && unverified == 0 {
        prompt.push_str(
            "brake found nothing that would break a consumer at this compatibility \
             level. Say so plainly, and do not manufacture concerns to fill the \
             space.\n\n",
        );
    } else {
        if findings > 0 {
            prompt.push_str(
                "For each finding below: say what a consumer experiences when this \
                 ships, then give the listed strategies. brake does not choose \
                 between them and neither should you without saying why — but you \
                 can see the repository and it cannot, so a recommendation grounded \
                 in what you can see there is worth more than the bare list.\n\n",
            );
        }
        if unverified > 0 {
            prompt.push_str(
                "Some of the payload could not be modelled, and is listed under \
                 `unverified`. That is not a pass: say which parts were not \
                 checked, and do not describe the change as safe.\n\n",
            );
        }
    }

    prompt.push_str(&pretty(&verdict));
    Ok(prompt)
}

// ── shared shaping ──────────────────────────────────────────────────────────

const CENSUS_CAVEAT: &str = "brake knows about the consumers declared in brake.toml and no others; an empty \
     answer is not proof that nobody uses this";

const CHOICE_IS_NOT_BRAKES: &str = "which strategy fits depends on whether you control every consumer and whether you \
     have a version scheme; brake can see neither, and does not choose";

/// A check's report as a tool answer.
///
/// A document that did not parse, or a baseline that could not be found, is
/// a broken gate to CI and an unanswerable question to an agent: §6 keeps
/// "brake could not tell" apart from "the API broke", so they are tool errors
/// here rather than findings.
fn answer(report: &Report, level: Compatibility) -> ToolResult {
    let unanswerable: Vec<&str> = report
        .unavailable
        .iter()
        .map(|item| item.message.as_str())
        .chain(
            report
                .findings
                .iter()
                .filter(|finding| {
                    matches!(
                        finding.rule_id,
                        "contract-unreachable" | "baseline-unconfigured"
                    )
                })
                .map(|finding| finding.message.as_str()),
        )
        .collect();
    if !unanswerable.is_empty() {
        return Err(ToolFailure::new(unanswerable.join("; ")));
    }
    Ok(render_report(report, level))
}

/// The response shape of `design/04-mcp-interface.md` §3.1.
///
/// `verdict` is a required field and `unverified` is a separate key, and both
/// choices are §6: a human skims a warning, an agent acts on the absence of
/// one. An empty `findings` array with a non-empty `unverified` is not a pass,
/// and the shape has to make that hard to misread.
fn render_findings(findings: &[Finding], level: Compatibility, contracts_checked: usize) -> Value {
    let (unverified, real): (Vec<_>, Vec<_>) = findings
        .iter()
        .partition(|finding| finding.rule_id == "contract-partial");

    let verdict = if real.iter().any(|f| f.severity >= Severity::Warning) {
        "findings"
    } else if unverified.is_empty() {
        "clean"
    } else {
        // Not "clean": part of the payload was never checked.
        "unverified"
    };

    json!({
        "verdict": verdict,
        "compatibility": level_name(level),
        "contracts_checked": contracts_checked,
        "findings": real.iter().map(|finding| finding_json(finding)).collect::<Vec<_>>(),
        "unverified": unverified
            .iter()
            .map(|finding| json!({
                "pointer": finding.pointer,
                "reason": finding.message,
            }))
            .collect::<Vec<_>>(),
    })
}

fn render_report(report: &Report, level: Compatibility) -> Value {
    let mut value = render_findings(&report.findings, level, report.contracts_checked);
    if !report.unavailable.is_empty() {
        // A gate that could not run is not a gate that found nothing.
        value["verdict"] = json!("unavailable");
        value["unavailable"] = json!(
            report
                .unavailable
                .iter()
                .map(|item| json!({
                    "contract": item.contract,
                    "message": item.message,
                }))
                .collect::<Vec<_>>()
        );
    }
    value
}

/// One finding: the object `--format json` emits, plus what an agent needs that
/// CI does not — the rule's rationale and a suppression it could propose.
fn finding_json(finding: &Finding) -> Value {
    let mut value = crate::render::json::finding(finding);
    let rule = catalogue::lookup(finding.rule_id);
    value["rationale"] = json!(rule.map(|rule| rule.explanation));
    value["suggested_suppression"] = json!(finding.suggest_suppression(None, None));
    if !finding.remediations().is_empty() {
        value["choice_is_not_brakes"] = json!(CHOICE_IS_NOT_BRAKES);
    }
    value
}

fn rule_catalogue() -> Vec<Value> {
    catalogue::RULES.iter().map(describe_rule).collect()
}

fn describe_rule(rule: &catalogue::Rule) -> Value {
    json!({
        "id": rule.id,
        "severity": severity_name(rule.severity),
        "fires_from": level_name(rule.level),
        "summary": rule.summary,
        "rationale": rule.explanation,
        "help_uri": rule.help_uri(),
        "remediation": rule
            .remedies
            .iter()
            .filter_map(|id| strategies::lookup(id))
            .map(|strategy| json!({
                "strategy": strategy.id,
                "summary": strategy
                    .summary
                    .replace("{subject}", "the field")
                    .replace("{endpoint}", "the endpoint"),
                "cost": strategy.cost,
            }))
            .collect::<Vec<_>>(),
    })
}

fn load_config(repo_root: &Path) -> Result<Config, ToolFailure> {
    let path = repo_root.join("brake.toml");
    if !path.is_file() {
        return Err(ToolFailure::new(format!(
            "no brake.toml in `{}`. `compare_contracts` needs no configuration and \
             works on two documents",
            repo_root.display()
        )));
    }
    Config::from_path(&path).map_err(|error| ToolFailure::new(error.to_string()))
}

fn select_contract<'a>(
    config: &'a Config,
    name: Option<&str>,
) -> Result<&'a ContractConfig, ToolFailure> {
    let named = |name: &str| {
        config
            .contracts
            .iter()
            .find(|contract| contract.name == name)
    };
    match name {
        Some(name) => named(name).ok_or_else(|| {
            ToolFailure::new(format!(
                "no contract named `{name}`. Configured: {}",
                config
                    .contracts
                    .iter()
                    .map(|contract| contract.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }),
        // Guessing which of several contracts was meant would silently compare
        // the wrong artifact.
        None => match config.contracts.as_slice() {
            [only] => Ok(only),
            [] => Err(ToolFailure::new(
                "brake.toml declares no contracts".to_owned(),
            )),
            many => Err(ToolFailure::new(format!(
                "brake.toml declares {} contracts, so `contract` is required. \
                 Configured: {}",
                many.len(),
                many.iter()
                    .map(|contract| contract.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        },
    }
}

fn parse_format(input: &str) -> Result<ContractFormat, ToolFailure> {
    ContractFormat::parse(input).map_err(ToolFailure::new)
}

fn parse_level(input: &str) -> Result<Compatibility, ToolFailure> {
    Compatibility::parse(input).map_err(ToolFailure::new)
}

#[must_use]
pub fn format_name(format: ContractFormat) -> &'static str {
    format.name()
}

#[must_use]
pub fn level_name(level: Compatibility) -> &'static str {
    level.name()
}

fn severity_name(severity: Severity) -> &'static str {
    severity.name()
}

/// The verdict a `check_repository` result maps to, for a caller that wants
/// the CLI's exit code rather than the string.
#[must_use]
pub fn verdict_of(value: &Value) -> Verdict {
    match value["verdict"].as_str() {
        Some("findings") => Verdict::Findings,
        Some("unavailable") => Verdict::ToolFailure,
        _ => Verdict::Clean,
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// The JSON Schemas advertised for the tools.
///
/// Hand-written rather than derived: the descriptions are the only
/// documentation an agent reads before calling, so they are worth writing
/// deliberately.
#[must_use]
pub fn tool_schemas() -> BTreeMap<&'static str, Value> {
    let format = json!({
        "type": "string",
        "enum": ["openapi", "proto", "graphql", "asyncapi", "mcp"],
        "description": "The contract format of the documents supplied.",
    });
    let compatibility = json!({
        "type": "string",
        "enum": ["wire", "wire-json", "surface", "strict"],
        "description": "Which breaks to report. Each level is a superset of the one \
    below: `wire` catches removals and narrowing, `wire-json` adds field-level \
    response breaks, `surface` adds what breaks generated client code, `strict` \
    reports additive changes too. Defaults to the configured level, or `wire-json`.",
    });

    BTreeMap::from([
        (
            "check_change",
            json!({
                "type": "object",
                "properties": {
                    "format": format,
                    "proposed": {
                        "type": "string",
                        "description": "The full proposed document, as text. Text \
            rather than a path, so a draft that has not been written to disk can be checked.",
                    },
                    "contract": {
                        "type": "string",
                        "description": "Which contract in brake.toml to compare \
            against. Required only when more than one is configured.",
                    },
                    "baseline_document": {
                        "type": "string",
                        "description": "An inline baseline. With no `contract`, the two \
            documents are compared with no brake.toml and no repository. With a \
            `contract`, it replaces that contract's configured baseline.",
                    },
                    "compatibility": compatibility.clone(),
                },
                "required": ["format", "proposed"],
            }),
        ),
        (
            "compare_contracts",
            json!({
                "type": "object",
                "properties": {
                    "format": format.clone(),
                    "base": { "type": "string", "description": "The previous document, as text." },
                    "head": { "type": "string", "description": "The new document, as text." },
                    "compatibility": compatibility.clone(),
                },
                "required": ["format", "base", "head"],
            }),
        ),
        (
            "explain_rule",
            json!({
                "type": "object",
                "properties": {
                    "rule": {
                        "type": "string",
                        "description": "A rule id, for example \
            `response-field-removed`. Omit to list the whole catalogue.",
                    },
                },
            }),
        ),
        (
            "check_repository",
            json!({
                "type": "object",
                "properties": {
                    "contracts": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Restrict the run to these contracts by name.",
                    },
                    "consumers": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Restrict consumer verification to these \
            declared consumers by name.",
                    },
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Check only the contracts among these \
            repository-relative paths, as the commit hook does. Omit for every contract.",
                    },
                    "since": {
                        "type": "string",
                        "description": "Check only the contracts that changed since \
            the merge-base with this ref, as a pull-request check does.",
                    },
                    "baseline": {
                        "type": "string",
                        "description": "Compare against this baseline instead of the \
            configured one: a revision (`v1.2.0`), `merge-base:<ref>`, \
            `latest-tag:<glob>`, or `<ref>:<path>`.",
                    },
                    "compatibility": compatibility,
                },
            }),
        ),
        (
            "who_consumes",
            json!({
                "type": "object",
                "properties": {
                    "contract": {
                        "type": "string",
                        "description": "Which contract in brake.toml. Required \
            only when more than one is configured.",
                    },
                    "endpoint": {
                        "type": "string",
                        "description": "`GET /payments/{id}`, or just the path. \
            Omit for every endpoint the contract documents.",
                    },
                    "field": {
                        "type": "string",
                        "description": "A response field, request field, status \
            code, parameter or media type. Omit for the whole endpoint.",
                    },
                },
            }),
        ),
    ])
}

/// One-line descriptions, shown to an agent choosing between the tools.
#[must_use]
pub fn tool_descriptions() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        (
            "check_change",
            "Check whether a proposed API contract would break its consumers, before \
writing it. Returns each break with the ways to make the same change safely. Use \
this while drafting a change to an OpenAPI, protobuf or GraphQL document.",
        ),
        (
            "compare_contracts",
            "Compare two API contract documents and report what would break a \
consumer. Needs no configuration and no repository — use it to review a diff.",
        ),
        (
            "explain_rule",
            "Explain why a brake rule exists, what it catches, and the ways to make \
the change it flags without breaking a consumer. Omit the rule id to list them all.",
        ),
        (
            "check_repository",
            "Check every API contract configured in this repository against its \
baseline. Answers 'what is our compatibility posture?' rather than 'is this \
change safe?'.",
        ),
        (
            "who_consumes",
            "Name the declared consumers of an endpoint or a field, with the \
interaction that declares it. Call this BEFORE proposing the removal or rename \
of anything in a contract: it answers who breaks, while the edit can still be \
reconsidered. An empty answer means nobody *declared* it, not that nobody uses it.",
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::{TempDir, tempdir};

    const BASE: &str = r#"
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
                type: object
                required: [id, customer_id]
                properties:
                  id: { type: string }
                  customer_id: { type: string }
"#;

    const HEAD_BREAKS: &str = r#"
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
                type: object
                required: [id]
                properties:
                  id: { type: string }
"#;

    fn repo(files: &[(&str, &str)]) -> TempDir {
        let repo = tempdir().expect("tempdir");
        for (path, body) in files {
            let full = repo.path().join(path);
            fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
            fs::write(full, body).expect("write");
        }
        repo
    }

    fn context(root: &Path) -> Context {
        Context::new(root.to_path_buf(), "2026-08-23".to_owned())
    }

    fn configured() -> TempDir {
        repo(&[
            (
                "brake.toml",
                "[[contract]]\nname=\"payments\"\nformat=\"openapi\"\n\
                 source=\"api/c.yaml\"\nbaseline={file=\"api/c.baseline.yaml\"}\n",
            ),
            ("api/c.baseline.yaml", BASE),
            ("api/c.yaml", BASE),
        ])
    }

    #[test]
    fn compare_contracts_needs_no_configuration() {
        let empty = tempdir().expect("tempdir");
        let value = compare_contracts(
            &context(empty.path()),
            CompareContractsArgs {
                format: "openapi".to_owned(),
                base: BASE.to_owned(),
                head: HEAD_BREAKS.to_owned(),
                compatibility: None,
            },
        )
        .expect("comparison");

        assert_eq!(value["verdict"], "findings");
        assert_eq!(value["findings"][0]["rule"], "response-field-removed");
    }

    #[test]
    fn a_finding_carries_the_ways_out_bound_to_its_subject() {
        let empty = tempdir().expect("tempdir");
        let value = compare_contracts(
            &context(empty.path()),
            CompareContractsArgs {
                format: "openapi".to_owned(),
                base: BASE.to_owned(),
                head: HEAD_BREAKS.to_owned(),
                compatibility: None,
            },
        )
        .expect("comparison");

        let finding = &value["findings"][0];
        assert_eq!(finding["subject"], "customer_id");
        assert_eq!(
            finding["remediation"][0]["strategy"],
            "deprecate-then-remove"
        );
        assert!(
            finding["remediation"][0]["summary"]
                .as_str()
                .expect("summary")
                .contains("`customer_id`")
        );
        // An agent handed one confident recommendation will follow it.
        assert!(finding["choice_is_not_brakes"].is_string());
    }

    #[test]
    fn an_identical_document_is_clean() {
        let empty = tempdir().expect("tempdir");
        let value = compare_contracts(
            &context(empty.path()),
            CompareContractsArgs {
                format: "openapi".to_owned(),
                base: BASE.to_owned(),
                head: BASE.to_owned(),
                compatibility: None,
            },
        )
        .expect("comparison");

        assert_eq!(value["verdict"], "clean");
        assert!(value["findings"].as_array().expect("array").is_empty());
    }

    #[test]
    fn an_unverifiable_payload_is_never_reported_clean() {
        // Identical on both sides, and unreadable on both sides. A verdict of
        // "clean" here would tell an automated caller that an unverified
        // change is fine.
        let unreadable = r#"
openapi: 3.1.0
paths:
  /payments:
    get:
      operationId: listPayments
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                $ref: 'common.yaml#/components/schemas/Payment'
"#;
        let empty = tempdir().expect("tempdir");
        let value = compare_contracts(
            &context(empty.path()),
            CompareContractsArgs {
                format: "openapi".to_owned(),
                base: unreadable.to_owned(),
                head: unreadable.to_owned(),
                compatibility: None,
            },
        )
        .expect("comparison");

        assert_eq!(
            value["verdict"], "unverified",
            "an unmodelled payload must not read as clean: {value}"
        );
        assert!(!value["unverified"].as_array().expect("array").is_empty());
        assert!(
            value["findings"].as_array().expect("array").is_empty(),
            "and it is not mixed in with the real findings"
        );
    }

    #[test]
    fn check_change_compares_a_draft_against_the_configured_baseline() {
        let repo = configured();
        let value = check_change(
            &context(repo.path()),
            CheckChangeArgs {
                format: "openapi".to_owned(),
                proposed: HEAD_BREAKS.to_owned(),
                contract: Some("payments".to_owned()),
                baseline_document: None,
                compatibility: None,
            },
        )
        .expect("check");

        assert_eq!(value["verdict"], "findings");
        assert_eq!(value["findings"][0]["contract"], "payments");
        // The draft was never written to disk.
        let on_disk = fs::read_to_string(repo.path().join("api/c.yaml")).expect("read");
        assert_eq!(on_disk, BASE, "the tool must not write the proposal");
    }

    #[test]
    fn check_change_takes_the_only_contract_without_being_told() {
        let repo = configured();
        let value = check_change(
            &context(repo.path()),
            CheckChangeArgs {
                format: "openapi".to_owned(),
                proposed: HEAD_BREAKS.to_owned(),
                contract: None,
                baseline_document: None,
                compatibility: None,
            },
        )
        .expect("check");
        assert_eq!(value["verdict"], "findings");
    }

    #[test]
    fn check_change_refuses_to_guess_between_several_contracts() {
        let repo = repo(&[
            (
                "brake.toml",
                "[[contract]]\nname=\"a\"\nformat=\"openapi\"\nsource=\"api/a.yaml\"\n\
                 baseline={file=\"api/a.baseline.yaml\"}\n\
                 [[contract]]\nname=\"b\"\nformat=\"openapi\"\nsource=\"api/b.yaml\"\n\
                 baseline={file=\"api/b.baseline.yaml\"}\n",
            ),
            ("api/a.baseline.yaml", BASE),
            ("api/a.yaml", BASE),
            ("api/b.baseline.yaml", BASE),
            ("api/b.yaml", BASE),
        ]);

        let failure = check_change(
            &context(repo.path()),
            CheckChangeArgs {
                format: "openapi".to_owned(),
                proposed: HEAD_BREAKS.to_owned(),
                contract: None,
                baseline_document: None,
                compatibility: None,
            },
        )
        .expect_err("ambiguous");

        assert!(
            failure.message.contains("`contract` is required"),
            "{failure}"
        );
        // And it names them, so the next call can succeed.
        assert!(failure.message.contains('a') && failure.message.contains('b'));
    }

    #[test]
    fn the_compatibility_level_changes_the_answer() {
        let empty = tempdir().expect("tempdir");
        let compare = |level: &str| {
            compare_contracts(
                &context(empty.path()),
                CompareContractsArgs {
                    format: "openapi".to_owned(),
                    base: BASE.to_owned(),
                    head: HEAD_BREAKS.to_owned(),
                    compatibility: Some(level.to_owned()),
                },
            )
            .expect("comparison")
        };

        assert_eq!(compare("wire")["verdict"], "clean");
        assert_eq!(compare("wire-json")["verdict"], "findings");
    }

    #[test]
    fn a_bad_document_is_a_failure_not_a_clean_verdict() {
        let empty = tempdir().expect("tempdir");
        let failure = compare_contracts(
            &context(empty.path()),
            CompareContractsArgs {
                format: "openapi".to_owned(),
                base: BASE.to_owned(),
                head: "this: is: not: openapi".to_owned(),
                compatibility: None,
            },
        )
        .expect_err("unparseable");
        // Names which of the two documents failed.
        assert!(
            failure.message.contains("failed to parse") && failure.message.contains("`head`"),
            "{failure}"
        );
    }

    #[test]
    fn a_remote_ref_is_refused_over_mcp_too() {
        let empty = tempdir().expect("tempdir");
        let failure = compare_contracts(
            &context(empty.path()),
            CompareContractsArgs {
                format: "openapi".to_owned(),
                base: BASE.to_owned(),
                head: r#"
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
                $ref: 'https://example.invalid/schema.yaml#/P'
"#
                .to_owned(),
                compatibility: None,
            },
        )
        .expect_err("remote ref");
        assert!(failure.message.contains("network"), "{failure}");
    }

    #[test]
    fn explain_rule_covers_the_catalogue_and_rejects_an_unknown_id() {
        let empty = tempdir().expect("tempdir");
        let context = context(empty.path());

        let all = explain_rule(&context, ExplainRuleArgs { rule: None }).expect("catalogue");
        assert_eq!(
            all["rules"].as_array().expect("array").len(),
            catalogue::RULES.len()
        );

        let one = explain_rule(
            &context,
            ExplainRuleArgs {
                rule: Some("response-field-removed".to_owned()),
            },
        )
        .expect("rule");
        assert_eq!(one["id"], "response-field-removed");
        assert!(one["remediation"].as_array().expect("array").len() == 3);

        assert!(
            explain_rule(
                &context,
                ExplainRuleArgs {
                    rule: Some("no-such-rule".to_owned())
                }
            )
            .is_err()
        );
    }

    #[test]
    fn check_repository_reports_the_posture() {
        let repo = configured();
        fs::write(repo.path().join("api/c.yaml"), HEAD_BREAKS).expect("write");

        let value = check_repository(&context(repo.path()), CheckRepositoryArgs::default())
            .expect("analyze");
        assert_eq!(value["verdict"], "findings");
        assert_eq!(value["contracts_checked"], 1);
    }

    #[test]
    fn an_unresolvable_baseline_is_unavailable_not_clean() {
        let repo = repo(&[
            (
                "brake.toml",
                "[[contract]]\nname=\"payments\"\nformat=\"openapi\"\n\
                 source=\"api/c.yaml\"\nbaseline={file=\"api/absent.yaml\"}\n",
            ),
            ("api/c.yaml", BASE),
        ]);

        let value = check_repository(&context(repo.path()), CheckRepositoryArgs::default())
            .expect("analyze");
        assert_eq!(
            value["verdict"], "unavailable",
            "a gate that could not run must not report clean"
        );
        assert_eq!(verdict_of(&value), Verdict::ToolFailure);
    }

    #[test]
    fn resources_serve_the_catalogue_and_refuse_anything_else() {
        let repo = configured();
        let context = context(repo.path());

        for (uri, _) in RESOURCES {
            let body = read_resource(&context, uri).expect(uri);
            assert!(!body.is_empty(), "{uri} served nothing");
        }

        assert!(
            read_resource(&context, "brake://rules")
                .expect("rules")
                .contains("response-field-removed")
        );
        assert!(
            read_resource(&context, "brake://strategies")
                .expect("strategies")
                .contains("deprecate-then-remove")
        );

        // Not a path this server will read.
        assert!(read_resource(&context, "file:///etc/passwd").is_err());
        assert!(read_resource(&context, "brake://config/../../etc/passwd").is_err());
    }

    #[test]
    fn the_config_resource_says_what_each_contract_gates() {
        let repo = configured();
        let body = read_resource(&context(repo.path()), "brake://config").expect("config");
        assert!(body.contains("\"configured\": true"), "{body}");
        assert!(body.contains("this change against the trunk"), "{body}");
    }

    #[test]
    fn the_config_resource_is_honest_when_there_is_none() {
        let empty = tempdir().expect("tempdir");
        let body = read_resource(&context(empty.path()), "brake://config").expect("config");
        assert!(body.contains("\"configured\": false"), "{body}");
        // And points at the tool that still works.
        assert!(body.contains("compare_contracts"), "{body}");
    }

    #[test]
    fn the_prompt_frames_a_clean_result_without_inventing_concerns() {
        let empty = tempdir().expect("tempdir");
        let prompt = review_api_change(
            &context(empty.path()),
            ReviewPromptArgs {
                format: "openapi".to_owned(),
                base: BASE.to_owned(),
                head: BASE.to_owned(),
                compatibility: None,
            },
        )
        .expect("prompt");

        assert!(prompt.contains("do not manufacture concerns"), "{prompt}");
    }

    #[test]
    fn the_prompt_carries_the_findings_and_their_strategies() {
        let empty = tempdir().expect("tempdir");
        let prompt = review_api_change(
            &context(empty.path()),
            ReviewPromptArgs {
                format: "openapi".to_owned(),
                base: BASE.to_owned(),
                head: HEAD_BREAKS.to_owned(),
                compatibility: None,
            },
        )
        .expect("prompt");

        assert!(prompt.contains("response-field-removed"), "{prompt}");
        assert!(prompt.contains("deprecate-then-remove"), "{prompt}");
        assert!(prompt.contains("brake does not choose"), "{prompt}");
    }

    #[test]
    fn every_tool_has_a_schema_and_a_description() {
        let schemas = tool_schemas();
        let descriptions = tool_descriptions();
        for name in TOOL_NAMES {
            assert!(schemas.contains_key(name), "{name}: no schema");
            let description = descriptions.get(name).unwrap_or_else(|| panic!("{name}"));
            // The description is the only documentation an agent reads before
            // choosing a tool.
            assert!(description.len() > 60, "{name}: description too thin");
        }
        assert_eq!(schemas.len(), TOOL_NAMES.len());
    }

    #[test]
    fn no_tool_argument_can_request_drift() {
        // The load-bearing exclusion of design/04-mcp-interface.md §5.1. If
        // this ever fails, an agent that can write brake.toml has arbitrary
        // command execution.
        //
        // Asserted on property *names*, not the serialised blob: "generated
        // client code" legitimately appears in a description, and a test that
        // trips on prose gets weakened rather than heeded.
        for (name, schema) in tool_schemas() {
            let properties = schema["properties"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            for property in properties.keys() {
                assert!(
                    !property.contains("drift") && !property.contains("generated"),
                    "`{name}` accepts `{property}`, which would reach the subprocess path"
                );
            }
        }
    }
}
