use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::config::{Baseline, ContractConfig, Defaults};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBaseline {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

pub fn resolve_for_contract(
    repo_root: &Path,
    defaults: &Defaults,
    contract: &ContractConfig,
) -> Result<ResolvedBaseline, BaselineError> {
    let baseline =
        contract
            .effective_baseline(defaults)
            .ok_or_else(|| BaselineError::MissingBaseline {
                contract: contract.name.clone(),
            })?;

    match baseline {
        Baseline::File(file) => {
            let path = repo_root.join(file);
            let bytes = fs::read(&path).map_err(|source| BaselineError::ReadFile {
                contract: contract.name.clone(),
                path: path.clone(),
                source,
            })?;
            Ok(ResolvedBaseline { path, bytes })
        }
        Baseline::Git { .. } | Baseline::GitMergeBase { .. } => {
            Err(BaselineError::UnsupportedBaselineStrategy {
                contract: contract.name.clone(),
                strategy: baseline.strategy_name(),
            })
        }
    }
}

#[derive(Debug, Error)]
pub enum BaselineError {
    #[error("contract `{contract}` has no configured baseline")]
    MissingBaseline { contract: String },
    #[error(
        "contract `{contract}` uses baseline strategy `{strategy}`, which is not implemented in M1"
    )]
    UnsupportedBaselineStrategy {
        contract: String,
        strategy: &'static str,
    },
    #[error("failed to read baseline file for contract `{contract}` at {path}: {source}")]
    ReadFile {
        contract: String,
        path: PathBuf,
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tempfile::tempdir;

    use super::*;
    use crate::config::{Compatibility, ContractFormat};

    fn contract_with(baseline: Option<Baseline>) -> ContractConfig {
        ContractConfig {
            name: "payments".to_owned(),
            format: ContractFormat::Openapi,
            source: PathBuf::from("api/payments-openapi.yaml"),
            compatibility: None,
            baseline,
            allow: Vec::new(),
        }
    }

    #[test]
    fn resolves_file_baseline_from_contract_override() {
        let root = tempdir().expect("tempdir");
        let baseline_path = root.path().join("api/payments.baseline.yaml");
        fs::create_dir_all(baseline_path.parent().expect("parent")).expect("mkdir");
        fs::write(&baseline_path, b"openapi: 3.1.0").expect("write baseline");

        let defaults = Defaults {
            compatibility: Compatibility::WireJson,
            baseline: Some(Baseline::File(PathBuf::from("api/default.baseline.yaml"))),
        };
        let contract = contract_with(Some(Baseline::File(PathBuf::from(
            "api/payments.baseline.yaml",
        ))));

        let resolved =
            resolve_for_contract(root.path(), &defaults, &contract).expect("file baseline");

        assert_eq!(resolved.path, baseline_path);
        assert_eq!(resolved.bytes, b"openapi: 3.1.0");
    }

    #[test]
    fn resolves_file_baseline_from_defaults() {
        let root = tempdir().expect("tempdir");
        let baseline_path = root.path().join("api/default.baseline.yaml");
        fs::create_dir_all(baseline_path.parent().expect("parent")).expect("mkdir");
        fs::write(&baseline_path, b"openapi: 3.1.0").expect("write baseline");

        let defaults = Defaults {
            compatibility: Compatibility::WireJson,
            baseline: Some(Baseline::File(PathBuf::from("api/default.baseline.yaml"))),
        };
        let contract = contract_with(None);

        let resolved =
            resolve_for_contract(root.path(), &defaults, &contract).expect("file baseline");

        assert_eq!(resolved.path, baseline_path);
    }

    #[test]
    fn errors_when_baseline_missing() {
        let root = tempdir().expect("tempdir");
        let defaults = Defaults {
            compatibility: Compatibility::WireJson,
            baseline: None,
        };
        let contract = contract_with(None);

        let error = resolve_for_contract(root.path(), &defaults, &contract)
            .expect_err("missing baseline must error");
        assert!(matches!(error, BaselineError::MissingBaseline { .. }));
    }

    #[test]
    fn errors_for_unimplemented_git_strategies() {
        let root = tempdir().expect("tempdir");
        let defaults = Defaults {
            compatibility: Compatibility::WireJson,
            baseline: Some(Baseline::GitMergeBase {
                reference: "origin/main".to_owned(),
            }),
        };
        let contract = contract_with(None);

        let error = resolve_for_contract(root.path(), &defaults, &contract)
            .expect_err("git strategy is not in M1");
        assert!(matches!(
            error,
            BaselineError::UnsupportedBaselineStrategy { .. }
        ));
    }
}
