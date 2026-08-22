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
        Baseline::File(file) => resolve_file_baseline(repo_root, contract, file),
        Baseline::Git { spec } => resolve_git_baseline(repo_root, contract, spec),
        Baseline::GitMergeBase { reference } => {
            resolve_git_merge_base_baseline(repo_root, contract, reference)
        }
    }
}

fn resolve_file_baseline(
    repo_root: &Path,
    contract: &ContractConfig,
    file: &Path,
) -> Result<ResolvedBaseline, BaselineError> {
    let path = repo_root.join(file);
    let bytes = fs::read(&path).map_err(|source| BaselineError::ReadFile {
        contract: contract.name.clone(),
        path: path.clone(),
        source,
    })?;
    Ok(ResolvedBaseline { path, bytes })
}

fn resolve_git_baseline(
    repo_root: &Path,
    contract: &ContractConfig,
    spec: &str,
) -> Result<ResolvedBaseline, BaselineError> {
    let repo = gix::open(repo_root).map_err(|source| BaselineError::OpenGitRepository {
        contract: contract.name.clone(),
        path: repo_root.to_path_buf(),
        details: source.to_string(),
    })?;

    let id = repo
        .rev_parse_single(spec)
        .map_err(|source| BaselineError::ResolveGitSpec {
            contract: contract.name.clone(),
            spec: spec.to_owned(),
            details: source.to_string(),
        })?;
    let object = id
        .object()
        .map_err(|source| BaselineError::ResolveGitSpec {
            contract: contract.name.clone(),
            spec: spec.to_owned(),
            details: source.to_string(),
        })?;
    let mut blob = object
        .try_into_blob()
        .map_err(|source| BaselineError::ResolveGitSpec {
            contract: contract.name.clone(),
            spec: spec.to_owned(),
            details: source.to_string(),
        })?;

    Ok(ResolvedBaseline {
        path: PathBuf::from(format!("git:{spec}")),
        bytes: blob.take_data(),
    })
}

fn resolve_git_merge_base_baseline(
    repo_root: &Path,
    contract: &ContractConfig,
    reference: &str,
) -> Result<ResolvedBaseline, BaselineError> {
    let repo = gix::open(repo_root).map_err(|source| BaselineError::OpenGitRepository {
        contract: contract.name.clone(),
        path: repo_root.to_path_buf(),
        details: source.to_string(),
    })?;
    let head = repo
        .head_id()
        .map_err(|source| BaselineError::ResolveMergeBase {
            contract: contract.name.clone(),
            reference: reference.to_owned(),
            details: source.to_string(),
        })?;
    let reference_id =
        repo.rev_parse_single(reference)
            .map_err(|source| BaselineError::ResolveMergeBase {
                contract: contract.name.clone(),
                reference: reference.to_owned(),
                details: source.to_string(),
            })?;
    let merge_base = repo
        .merge_base(head.detach(), reference_id.detach())
        .map_err(|source| BaselineError::ResolveMergeBase {
            contract: contract.name.clone(),
            reference: reference.to_owned(),
            details: source.to_string(),
        })?;

    let object = merge_base
        .object()
        .map_err(|source| BaselineError::ResolveMergeBase {
            contract: contract.name.clone(),
            reference: reference.to_owned(),
            details: source.to_string(),
        })?;
    let commit = object
        .try_into_commit()
        .map_err(|source| BaselineError::ResolveMergeBase {
            contract: contract.name.clone(),
            reference: reference.to_owned(),
            details: source.to_string(),
        })?;
    let tree = commit
        .tree()
        .map_err(|source| BaselineError::ResolveMergeBase {
            contract: contract.name.clone(),
            reference: reference.to_owned(),
            details: source.to_string(),
        })?;
    let entry = tree
        .lookup_entry_by_path(&contract.source)
        .map_err(|source| BaselineError::ResolveMergeBasePath {
            contract: contract.name.clone(),
            reference: reference.to_owned(),
            path: contract.source.clone(),
            details: source.to_string(),
        })?
        .ok_or_else(|| BaselineError::ResolveMergeBasePath {
            contract: contract.name.clone(),
            reference: reference.to_owned(),
            path: contract.source.clone(),
            details: "path not found in merge-base tree".to_owned(),
        })?;

    let object = entry
        .object()
        .map_err(|source| BaselineError::ResolveMergeBasePath {
            contract: contract.name.clone(),
            reference: reference.to_owned(),
            path: contract.source.clone(),
            details: source.to_string(),
        })?;
    let mut blob =
        object
            .try_into_blob()
            .map_err(|source| BaselineError::ResolveMergeBasePath {
                contract: contract.name.clone(),
                reference: reference.to_owned(),
                path: contract.source.clone(),
                details: source.to_string(),
            })?;

    Ok(ResolvedBaseline {
        path: PathBuf::from(format!(
            "merge-base:{reference}:{}",
            contract.source.display()
        )),
        bytes: blob.take_data(),
    })
}

#[derive(Debug, Error)]
pub enum BaselineError {
    #[error("contract `{contract}` has no configured baseline")]
    MissingBaseline { contract: String },
    #[error("failed to read baseline file for contract `{contract}` at {path}: {source}")]
    ReadFile {
        contract: String,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to open git repository at {path} for contract `{contract}`: {details}")]
    OpenGitRepository {
        contract: String,
        path: PathBuf,
        details: String,
    },
    #[error("failed to resolve git baseline spec `{spec}` for contract `{contract}`: {details}")]
    ResolveGitSpec {
        contract: String,
        spec: String,
        details: String,
    },
    #[error(
        "failed to resolve merge-base against `{reference}` for contract `{contract}`: {details}"
    )]
    ResolveMergeBase {
        contract: String,
        reference: String,
        details: String,
    },
    #[error(
        "failed to read `{path}` from merge-base with `{reference}` for contract `{contract}`: {details}"
    )]
    ResolveMergeBasePath {
        contract: String,
        reference: String,
        path: PathBuf,
        details: String,
    },
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

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

    fn run_git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git command should launch");
        assert!(
            output.status.success(),
            "git {:?} failed: {}\n{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn run_git_output(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git command should launch");
        assert!(
            output.status.success(),
            "git {:?} failed: {}\n{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn write_and_commit(repo: &Path, path: &str, contents: &str, message: &str) {
        let absolute = repo.join(path);
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(&absolute, contents).expect("write file");
        run_git(repo, &["add", path]);
        run_git(repo, &["commit", "-m", message]);
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
    fn resolves_git_baseline_from_ref_and_path_spec() {
        let root = tempdir().expect("tempdir");
        run_git(root.path(), &["init", "-b", "main"]);
        run_git(root.path(), &["config", "user.name", "Brake Test"]);
        run_git(root.path(), &["config", "user.email", "brake@example.com"]);

        write_and_commit(
            root.path(),
            "api/payments-openapi.yaml",
            "openapi: 3.1.0\ninfo:\n  title: baseline\n",
            "baseline",
        );
        write_and_commit(
            root.path(),
            "api/payments-openapi.yaml",
            "openapi: 3.1.0\ninfo:\n  title: head\n",
            "head",
        );

        let defaults = Defaults {
            compatibility: Compatibility::WireJson,
            baseline: None,
        };
        let contract = contract_with(Some(Baseline::Git {
            spec: "HEAD~1:api/payments-openapi.yaml".to_owned(),
        }));

        let resolved =
            resolve_for_contract(root.path(), &defaults, &contract).expect("git baseline");
        let content = String::from_utf8(resolved.bytes).expect("utf-8 baseline");
        assert!(content.contains("title: baseline"));
    }

    #[test]
    fn resolves_git_merge_base_baseline_against_reference() {
        let root = tempdir().expect("tempdir");
        run_git(root.path(), &["init", "-b", "main"]);
        run_git(root.path(), &["config", "user.name", "Brake Test"]);
        run_git(root.path(), &["config", "user.email", "brake@example.com"]);

        write_and_commit(
            root.path(),
            "api/payments-openapi.yaml",
            "openapi: 3.1.0\ninfo:\n  title: base\n",
            "base",
        );
        let base_sha = run_git_output(root.path(), &["rev-parse", "HEAD"]);

        write_and_commit(
            root.path(),
            "api/payments-openapi.yaml",
            "openapi: 3.1.0\ninfo:\n  title: main\n",
            "main-update",
        );

        run_git(
            root.path(),
            &["checkout", "-b", "feature", base_sha.as_str()],
        );
        write_and_commit(
            root.path(),
            "README.md",
            "# feature branch change\n",
            "feature-change",
        );

        let defaults = Defaults {
            compatibility: Compatibility::WireJson,
            baseline: Some(Baseline::GitMergeBase {
                reference: "refs/heads/main".to_owned(),
            }),
        };
        let contract = contract_with(None);
        let resolved =
            resolve_for_contract(root.path(), &defaults, &contract).expect("merge-base baseline");

        let content = String::from_utf8(resolved.bytes).expect("utf-8 baseline");
        assert!(content.contains("title: base"));
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
    fn errors_for_invalid_git_spec() {
        let root = tempdir().expect("tempdir");
        run_git(root.path(), &["init", "-b", "main"]);
        run_git(root.path(), &["config", "user.name", "Brake Test"]);
        run_git(root.path(), &["config", "user.email", "brake@example.com"]);
        write_and_commit(
            root.path(),
            "api/payments-openapi.yaml",
            "openapi: 3.1.0\n",
            "init",
        );

        let defaults = Defaults {
            compatibility: Compatibility::WireJson,
            baseline: None,
        };
        let contract = contract_with(Some(Baseline::Git {
            spec: "does-not-exist:api/payments-openapi.yaml".to_owned(),
        }));

        let error = resolve_for_contract(root.path(), &defaults, &contract)
            .expect_err("invalid git spec must error");
        assert!(matches!(error, BaselineError::ResolveGitSpec { .. }));
    }
}
