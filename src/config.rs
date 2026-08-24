use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    pub defaults: Defaults,
    pub contracts: Vec<ContractConfig>,
    /// Declared consumer demand — `design/05-consumer-demand.md` §5.
    ///
    /// A glob in `source` has already been expanded and sorted byte-wise by
    /// the time a run reads this, so guarantee G3 holds over a directory
    /// listing.
    pub consumers: Vec<ConsumerConfig>,
    /// The `[consumers]` block. One block rather than a per-contract setting,
    /// because both knobs are statements about *this repository's knowledge of
    /// the world* rather than about an artifact.
    pub consumer_options: ConsumerOptions,
}

impl Config {
    pub fn from_path(path: &Path) -> Result<Self, ConfigError> {
        let contents = fs::read_to_string(path).map_err(|source| ConfigError::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&contents)
    }

    pub fn parse(contents: &str) -> Result<Self, ConfigError> {
        let raw: RawConfig = toml::from_str(contents).map_err(ConfigError::ParseToml)?;
        raw.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Defaults {
    pub compatibility: Compatibility,
    pub baseline: Option<Baseline>,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            compatibility: Compatibility::WireJson,
            baseline: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractConfig {
    pub name: String,
    pub format: ContractFormat,
    pub source: PathBuf,
    pub compatibility: Option<Compatibility>,
    pub baseline: Option<Baseline>,
    pub allow: Vec<Suppression>,
    pub generated: Option<GeneratedConfig>,
}

impl ContractConfig {
    pub fn effective_compatibility(&self, defaults: &Defaults) -> Compatibility {
        self.compatibility.unwrap_or(defaults.compatibility)
    }

    pub fn effective_baseline<'a>(&'a self, defaults: &'a Defaults) -> Option<&'a Baseline> {
        self.baseline.as_ref().or(defaults.baseline.as_ref())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Compatibility {
    Wire,
    WireJson,
    Surface,
    Strict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContractFormat {
    Openapi,
    Proto,
    Graphql,
    Asyncapi,
}

/// A consumer declaration format — `design/05-consumer-demand.md` §2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DemandFormat {
    /// Pact v2/v3/v4 HTTP interactions, JSON.
    Pact,
    /// The consumer's own `.graphql` query documents. The strongest of the
    /// three: a selection set *is* the field list, with no inference at all.
    GraphqlOperations,
    /// A hand- or codegen-written `*.brake-uses.toml`.
    Manifest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumerConfig {
    /// Optional in the file: a pact names itself.
    pub name: Option<String>,
    pub format: DemandFormat,
    /// Repository-relative. May contain `*`, expanded and sorted at run time.
    pub source: PathBuf,
    /// Which `[[contract]]` this constrains. Defaults to whatever the artifact
    /// names as its provider.
    pub provider: Option<String>,
}

/// The `[consumers]` block.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConsumerOptions {
    pub policy: ConsumerPolicy,
    pub completeness: Completeness,
}

/// What a declared consumer does to a finding's severity — §7.1.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConsumerPolicy {
    /// Severities unchanged; affected consumers are named on the finding.
    #[default]
    Annotate,
    /// A `warning` becomes an `error` when a declared consumer is affected.
    /// `param-removed` and `security-removed` are warnings precisely because
    /// brake could not tell whether anyone relied on them. Now it can.
    Escalate,
    /// An `error` becomes a `warning` when no declared consumer is affected.
    /// Constrained by §7.2, because it is the one that can lie.
    Triage,
}

/// Whether the declared consumer set is claimed to be exhaustive.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Completeness {
    /// There may be consumers brake has never heard of. The honest default.
    #[default]
    OpenWorld,
    /// An explicit, reviewable assertion by a human that the declared set is
    /// exhaustive. brake cannot verify that claim and does not pretend to.
    ClosedWorld,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Baseline {
    File(PathBuf),
    /// A ref and an explicit path. The only shape that takes a path, and the
    /// reason to prefer the others — see `design/02-contract-gates.md` §2.1.
    Git {
        spec: String,
    },
    GitMergeBase {
        reference: String,
    },
    /// A named release tag. The path comes from `source`.
    Tag {
        name: String,
    },
    /// The newest tag matching this glob that HEAD descends from.
    LatestTag {
        pattern: String,
    },
    /// Any revision — a commit, a branch, a tag.
    Rev {
        rev: String,
    },
}

impl Baseline {
    pub fn strategy_name(&self) -> &'static str {
        match self {
            Self::File(_) => "file",
            Self::Git { .. } => "git",
            Self::GitMergeBase { .. } => "git-merge-base",
            Self::Tag { .. } => "tag",
            Self::LatestTag { .. } => "latest-tag",
            Self::Rev { .. } => "rev",
        }
    }

    /// Does this baseline answer "has the published API broken since we
    /// shipped?" rather than "is this change safe to merge?"
    ///
    /// The distinction is not cosmetic: a release baseline does not forgive
    /// what is already on the trunk, so it is the wrong default for a commit
    /// hook and the right one for a release gate.
    #[must_use]
    pub fn is_release_baseline(&self) -> bool {
        matches!(
            self,
            Self::Tag { .. } | Self::LatestTag { .. } | Self::Rev { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suppression {
    pub rule: String,
    pub endpoint: Option<String>,
    pub field: Option<String>,
    pub reason: String,
    pub expires: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedConfig {
    pub command: String,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file {path}: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse brake.toml: {0}")]
    ParseToml(toml::de::Error),
    #[error("invalid brake.toml: {0}")]
    Validation(String),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    defaults: RawDefaults,
    #[serde(default, rename = "contract")]
    contracts: Vec<RawContract>,
    #[serde(default, rename = "consumer")]
    consumers: Vec<RawConsumer>,
    #[serde(default, rename = "consumers")]
    consumers_options: Option<RawConsumerOptions>,
}

impl RawConfig {
    fn validate(self) -> Result<Config, ConfigError> {
        let defaults = self.defaults.validate("defaults")?;
        let mut contracts = Vec::with_capacity(self.contracts.len());
        for contract in self.contracts {
            contracts.push(contract.validate()?);
        }
        let mut consumers = Vec::with_capacity(self.consumers.len());
        for consumer in self.consumers {
            consumers.push(consumer.validate()?);
        }
        Ok(Config {
            defaults,
            contracts,
            consumers,
            consumer_options: self
                .consumers_options
                .map(RawConsumerOptions::validate)
                .unwrap_or_default(),
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDefaults {
    compatibility: Option<Compatibility>,
    baseline: Option<RawBaseline>,
}

impl RawDefaults {
    fn validate(self, context: &str) -> Result<Defaults, ConfigError> {
        Ok(Defaults {
            compatibility: self.compatibility.unwrap_or(Compatibility::WireJson),
            baseline: self.baseline.map(|raw| raw.validate(context)).transpose()?,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawContract {
    name: String,
    format: ContractFormat,
    source: PathBuf,
    compatibility: Option<Compatibility>,
    baseline: Option<RawBaseline>,
    #[serde(default)]
    allow: Vec<RawSuppression>,
    generated: Option<RawGenerated>,
}

impl RawContract {
    fn validate(self) -> Result<ContractConfig, ConfigError> {
        if self.name.trim().is_empty() {
            return Err(ConfigError::Validation(
                "contract name cannot be empty".into(),
            ));
        }
        if self.source.as_os_str().is_empty() {
            return Err(ConfigError::Validation(format!(
                "contract `{}` has an empty source path",
                self.name
            )));
        }

        let mut allow = Vec::with_capacity(self.allow.len());
        for suppression in self.allow {
            allow.push(suppression.validate(&self.name)?);
        }

        Ok(ContractConfig {
            name: self.name.clone(),
            format: self.format,
            source: self.source,
            compatibility: self.compatibility,
            baseline: self
                .baseline
                .map(|raw| raw.validate(&format!("contract `{}`", self.name)))
                .transpose()?,
            allow,
            generated: self
                .generated
                .map(|raw| raw.validate(&self.name))
                .transpose()?,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConsumer {
    name: Option<String>,
    format: DemandFormat,
    source: PathBuf,
    provider: Option<String>,
}

impl RawConsumer {
    fn validate(self) -> Result<ConsumerConfig, ConfigError> {
        if self.source.as_os_str().is_empty() {
            return Err(ConfigError::Validation(
                "a `[[consumer]]` has an empty source path".into(),
            ));
        }
        // A demand source that is itself a URL is a configuration error,
        // refused at parse time rather than fetched. G1, over the demand axis.
        let text = self.source.to_string_lossy();
        if text.contains("://") {
            return Err(ConfigError::Validation(format!(
                "consumer source `{text}` looks like a URL. brake never fetches a demand \
                 source: have CI write the file and point `source` at the path"
            )));
        }
        if let Some(name) = &self.name
            && name.trim().is_empty()
        {
            return Err(ConfigError::Validation(
                "a `[[consumer]]` has an empty name".into(),
            ));
        }
        Ok(ConsumerConfig {
            name: self.name,
            format: self.format,
            source: self.source,
            provider: self.provider,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConsumerOptions {
    policy: Option<ConsumerPolicy>,
    completeness: Option<Completeness>,
}

impl RawConsumerOptions {
    fn validate(self) -> ConsumerOptions {
        ConsumerOptions {
            policy: self.policy.unwrap_or_default(),
            completeness: self.completeness.unwrap_or_default(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGenerated {
    command: String,
}

impl RawGenerated {
    fn validate(self, contract_name: &str) -> Result<GeneratedConfig, ConfigError> {
        if self.command.trim().is_empty() {
            return Err(ConfigError::Validation(format!(
                "contract `{contract_name}` generated.command cannot be empty"
            )));
        }
        Ok(GeneratedConfig {
            command: self.command,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBaseline {
    file: Option<PathBuf>,
    git: Option<String>,
    #[serde(rename = "git-merge-base")]
    git_merge_base: Option<String>,
    tag: Option<String>,
    #[serde(rename = "latest-tag")]
    latest_tag: Option<String>,
    rev: Option<String>,
}

impl RawBaseline {
    fn validate(self, context: &str) -> Result<Baseline, ConfigError> {
        // Table-driven so adding a shape cannot forget the arity check, which
        // is what stops `{ tag = "v1", rev = "abc" }` from silently picking one.
        let candidates: [(&str, Option<String>); 5] = [
            ("git", self.git),
            ("git-merge-base", self.git_merge_base),
            ("tag", self.tag),
            ("latest-tag", self.latest_tag),
            ("rev", self.rev),
        ];

        let mut chosen: Option<(&str, String)> = None;
        let mut set = usize::from(self.file.is_some());
        for (key, value) in candidates {
            let Some(value) = value else { continue };
            set += 1;
            if value.trim().is_empty() {
                return Err(ConfigError::Validation(format!(
                    "{context} baseline `{key}` cannot be empty"
                )));
            }
            chosen.get_or_insert((key, value));
        }

        if set != 1 {
            return Err(ConfigError::Validation(format!(
                "{context} baseline must set exactly one of `file`, `git`, `git-merge-base`, \
                 `tag`, `latest-tag`, or `rev`{}",
                if set > 1 { ", and it sets several" } else { "" }
            )));
        }

        if let Some(file) = self.file {
            if file.as_os_str().is_empty() {
                return Err(ConfigError::Validation(format!(
                    "{context} baseline file path cannot be empty"
                )));
            }
            return Ok(Baseline::File(file));
        }

        let (key, value) = chosen.expect("exactly one is set and it is not `file`");
        Ok(match key {
            "git" => Baseline::Git { spec: value },
            "git-merge-base" => Baseline::GitMergeBase { reference: value },
            "tag" => Baseline::Tag { name: value },
            "latest-tag" => Baseline::LatestTag { pattern: value },
            "rev" => Baseline::Rev { rev: value },
            other => unreachable!("unhandled baseline key `{other}`"),
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSuppression {
    rule: String,
    endpoint: Option<String>,
    field: Option<String>,
    reason: String,
    expires: Option<Expires>,
}

/// `expires`, however it was written.
///
/// TOML has a native date type, so `expires = 2026-09-01` without quotes is
/// the natural thing to write — and deserialising straight into `String`
/// rejected it with serde's `invalid type: map, expected a string`, which
/// tells a reader nothing about what to do. Both spellings are accepted and
/// normalised, because the tool knows perfectly well what was meant.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Expires {
    Text(String),
    Date(toml::value::Datetime),
}

impl std::fmt::Display for Expires {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(text) => formatter.write_str(text),
            // `Datetime`'s own rendering is the TOML spelling, which for a
            // local date is already YYYY-MM-DD.
            Self::Date(date) => write!(formatter, "{date}"),
        }
    }
}

impl RawSuppression {
    fn validate(self, contract_name: &str) -> Result<Suppression, ConfigError> {
        if self.rule.trim().is_empty() {
            return Err(ConfigError::Validation(format!(
                "contract `{contract_name}` suppression rule cannot be empty"
            )));
        }
        if self.reason.trim().is_empty() {
            return Err(ConfigError::Validation(format!(
                "contract `{contract_name}` suppression for rule `{}` must include a non-empty reason",
                self.rule
            )));
        }

        // An unreadable date silently never expires, which turns a
        // time-boxed exception into a permanent one.
        let expires = self.expires.map(|expires| expires.to_string());
        if let Some(expires) = &expires
            && crate::rules::parse_date(expires).is_none()
        {
            return Err(ConfigError::Validation(format!(
                "contract `{contract_name}` suppression for rule `{}` has an unreadable \
                 `expires` value `{expires}`; use YYYY-MM-DD",
                self.rule
            )));
        }
        if crate::rules::catalogue::lookup(&self.rule).is_none() {
            return Err(ConfigError::Validation(format!(
                "contract `{contract_name}` suppresses unknown rule `{}`; \
                 run `brake explain <rule-id>` to see the catalogue",
                self.rule
            )));
        }

        Ok(Suppression {
            rule: self.rule,
            endpoint: self.endpoint,
            field: self.field,
            reason: self.reason,
            expires,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expires_accepts_both_spellings_of_a_date() {
        // TOML has a native date type, so writing one unquoted is the natural
        // thing to do. Rejecting it with serde's `invalid type: map, expected
        // a string` told a reader nothing about what to do instead.
        let with = |expires: &str| {
            format!(
                "[[contract]]\nname=\"c\"\nformat=\"openapi\"\nsource=\"a.yaml\"\n\
                 [[contract.allow]]\nrule=\"endpoint-removed\"\nreason=\"r\"\n\
                 expires={expires}\n"
            )
        };

        for spelling in ["\"2026-09-01\"", "2026-09-01"] {
            let config = Config::parse(&with(spelling))
                .unwrap_or_else(|error| panic!("`expires={spelling}` should parse: {error}"));
            assert_eq!(
                config.contracts[0].allow[0].expires.as_deref(),
                Some("2026-09-01"),
                "both spellings must normalise to the same date"
            );
        }
    }

    #[test]
    fn expires_still_rejects_something_that_is_not_a_date() {
        let config = Config::parse(
            "[[contract]]\nname=\"c\"\nformat=\"openapi\"\nsource=\"a.yaml\"\n\
             [[contract.allow]]\nrule=\"endpoint-removed\"\nreason=\"r\"\n\
             expires=\"soon\"\n",
        );
        let error = config.expect_err("`soon` is not a date").to_string();
        assert!(error.contains("YYYY-MM-DD"), "{error}");
    }

    #[test]
    fn parses_defaults_contract_and_suppression() {
        let config = Config::parse(
            r#"
            [defaults]
            compatibility = "wire-json"
            baseline = { file = "api/default-openapi.baseline.yaml" }

            [[contract]]
            name = "payments"
            format = "openapi"
            source = "api/payments-openapi.yaml"

            [[contract.allow]]
            rule = "response-field-removed"
            endpoint = "GET /payments/{id}"
            field = "legacy_reference"
            reason = "Removed after deprecation window"
            expires = "2026-09-01"
        "#,
        )
        .expect("config should parse");

        assert_eq!(config.defaults.compatibility, Compatibility::WireJson);
        assert_eq!(
            config.defaults.baseline,
            Some(Baseline::File(PathBuf::from(
                "api/default-openapi.baseline.yaml"
            )))
        );
        assert_eq!(config.contracts.len(), 1);
        assert_eq!(config.contracts[0].name, "payments");
        assert_eq!(config.contracts[0].allow.len(), 1);
        assert!(config.contracts[0].generated.is_none());
    }

    #[test]
    fn suppression_requires_reason() {
        let error = Config::parse(
            r#"
            [[contract]]
            name = "payments"
            format = "openapi"
            source = "api/payments-openapi.yaml"

            [[contract.allow]]
            rule = "response-field-removed"
            endpoint = "GET /payments/{id}"
            field = "legacy_reference"
        "#,
        )
        .expect_err("suppression without reason must fail");

        assert!(error.to_string().contains("reason"));
    }

    #[test]
    fn baseline_requires_exactly_one_strategy() {
        let error = Config::parse(
            r#"
            [defaults]
            baseline = { file = "a.yaml", git = "origin/main:api/a.yaml" }
        "#,
        )
        .expect_err("multiple baseline strategies must fail");

        assert!(error.to_string().contains("exactly one"));
    }

    #[test]
    fn default_compatibility_is_wire_json() {
        let config = Config::parse(
            r#"
            [[contract]]
            name = "ledger"
            format = "openapi"
            source = "api/ledger-openapi.yaml"
        "#,
        )
        .expect("config should parse");

        assert_eq!(config.defaults.compatibility, Compatibility::WireJson);
        assert_eq!(
            config.contracts[0].effective_compatibility(&config.defaults),
            Compatibility::WireJson
        );
    }

    #[test]
    fn parses_generated_command() {
        let config = Config::parse(
            r#"
            [[contract]]
            name = "payments"
            format = "openapi"
            source = "api/payments-openapi.yaml"
            generated = { command = "echo generated" }
        "#,
        )
        .expect("config should parse");

        assert_eq!(
            config.contracts[0]
                .generated
                .as_ref()
                .expect("generated config")
                .command,
            "echo generated"
        );
    }
}
