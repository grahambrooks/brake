use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub defaults: Defaults,
    pub contracts: Vec<ContractConfig>,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Baseline {
    File(PathBuf),
    Git { spec: String },
    GitMergeBase { reference: String },
}

impl Baseline {
    pub fn strategy_name(&self) -> &'static str {
        match self {
            Self::File(_) => "file",
            Self::Git { .. } => "git",
            Self::GitMergeBase { .. } => "git-merge-base",
        }
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
}

impl RawConfig {
    fn validate(self) -> Result<Config, ConfigError> {
        let defaults = self.defaults.validate("defaults")?;
        let mut contracts = Vec::with_capacity(self.contracts.len());
        for contract in self.contracts {
            contracts.push(contract.validate()?);
        }
        Ok(Config {
            defaults,
            contracts,
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
}

impl RawBaseline {
    fn validate(self, context: &str) -> Result<Baseline, ConfigError> {
        let mut set = 0usize;
        if self.file.is_some() {
            set += 1;
        }
        if self.git.is_some() {
            set += 1;
        }
        if self.git_merge_base.is_some() {
            set += 1;
        }
        if set != 1 {
            return Err(ConfigError::Validation(format!(
                "{context} baseline must set exactly one of `file`, `git`, or `git-merge-base`"
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
        if let Some(git) = self.git {
            if git.trim().is_empty() {
                return Err(ConfigError::Validation(format!(
                    "{context} baseline git spec cannot be empty"
                )));
            }
            return Ok(Baseline::Git { spec: git });
        }
        if let Some(reference) = self.git_merge_base {
            if reference.trim().is_empty() {
                return Err(ConfigError::Validation(format!(
                    "{context} baseline git-merge-base reference cannot be empty"
                )));
            }
            return Ok(Baseline::GitMergeBase { reference });
        }

        Err(ConfigError::Validation(format!(
            "{context} baseline must not be empty"
        )))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSuppression {
    rule: String,
    endpoint: Option<String>,
    field: Option<String>,
    reason: String,
    expires: Option<String>,
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
        if let Some(expires) = &self.expires
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
            expires: self.expires,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
