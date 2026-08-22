use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::config::{Baseline, ContractConfig, Defaults};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBaseline {
    /// How the baseline is named in output: a repository-relative path for a
    /// file, or a `git:`/`merge-base:` descriptor for a blob. Never absolute —
    /// an absolute path in a span would break guarantees G4 and G6.
    pub label: String,
    pub bytes: Vec<u8>,
}

/// Resolve the baseline for a contract, honouring a command-line override.
pub fn resolve_for_contract(
    repo_root: &Path,
    defaults: &Defaults,
    contract: &ContractConfig,
    override_baseline: Option<&Baseline>,
) -> Result<ResolvedBaseline, BaselineError> {
    let baseline = override_baseline
        .or_else(|| contract.effective_baseline(defaults))
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
        path: file.to_path_buf(),
        source,
    })?;
    Ok(ResolvedBaseline {
        label: crate::check::display_path(file),
        bytes,
    })
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
        label: format!("git:{spec}"),
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
        // The ref resolved; the file simply was not there yet. A contract
        // added in this change has nothing to be compared against, and that is
        // not a broken gate — failing here would make every new API file a CI
        // failure on the commit that introduces it.
        .ok_or_else(|| BaselineError::AbsentFromBaseline {
            contract: contract.name.clone(),
            reference: format!("the merge-base with `{reference}`"),
            path: contract.source.clone(),
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
        label: format!(
            "merge-base:{reference}:{}",
            crate::check::display_path(&contract.source)
        ),
        bytes: blob.take_data(),
    })
}

#[derive(Debug, Error)]
pub enum BaselineError {
    #[error("contract `{contract}` has no configured baseline")]
    MissingBaseline { contract: String },
    #[error(
        "contract `{contract}` source `{path}` does not exist in {reference}, \
         so there is no previous version to compare against"
    )]
    AbsentFromBaseline {
        contract: String,
        reference: String,
        path: PathBuf,
    },
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
            generated: None,
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
            resolve_for_contract(root.path(), &defaults, &contract, None).expect("file baseline");

        assert_eq!(resolved.label, "api/payments.baseline.yaml");
        assert!(
            !resolved
                .label
                .contains(&root.path().to_string_lossy().to_string()),
            "a baseline label must never carry the absolute checkout path"
        );
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
            resolve_for_contract(root.path(), &defaults, &contract, None).expect("file baseline");

        assert_eq!(resolved.label, "api/default.baseline.yaml");
        assert!(baseline_path.exists());
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
            resolve_for_contract(root.path(), &defaults, &contract, None).expect("git baseline");
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
        let resolved = resolve_for_contract(root.path(), &defaults, &contract, None)
            .expect("merge-base baseline");

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

        let error = resolve_for_contract(root.path(), &defaults, &contract, None)
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

        let error = resolve_for_contract(root.path(), &defaults, &contract, None)
            .expect_err("invalid git spec must error");
        assert!(matches!(error, BaselineError::ResolveGitSpec { .. }));
    }
}
