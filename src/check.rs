use std::fs;
use std::path::Path;
use std::process::Command;

use crate::Severity;
use crate::baseline;
use crate::compare;
use crate::config::{Config, ContractConfig, Defaults};
use crate::contract::openapi;
use crate::report::{Report, Unavailable};
use crate::rules;

pub fn check_contract(
    repo_root: &Path,
    defaults: &Defaults,
    contract: &ContractConfig,
    as_of: Option<&str>,
    drift: bool,
) -> Report {
    let resolved = match baseline::resolve_for_contract(repo_root, defaults, contract) {
        Ok(resolved) => resolved,
        Err(error) => {
            return Report::new(
                Vec::new(),
                vec![Unavailable {
                    contract: Some(contract.name.clone()),
                    message: error.to_string(),
                }],
                1,
            );
        }
    };

    let head_path = repo_root.join(&contract.source);
    let head_bytes = match fs::read(&head_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            let finding = rules::contract_unreachable(
                &contract.name,
                &format!("failed to read `{}`: {error}", head_path.display()),
                None,
            );
            return Report::new(vec![finding], Vec::new(), 1);
        }
    };

    let base_contract = match openapi::ingest(&resolved.path.to_string_lossy(), &resolved.bytes) {
        Ok(contract_data) => contract_data,
        Err(error) => {
            let finding = rules::contract_unreachable(&contract.name, &error.to_string(), None);
            return Report::new(vec![finding], Vec::new(), 1);
        }
    };
    let head_contract = match openapi::ingest(&head_path.to_string_lossy(), &head_bytes) {
        Ok(contract_data) => contract_data,
        Err(error) => {
            let finding = rules::contract_unreachable(&contract.name, &error.to_string(), None);
            return Report::new(vec![finding], Vec::new(), 1);
        }
    };

    let changes = compare::compare_contracts(&base_contract, &head_contract);
    let findings = rules::evaluate(&changes, contract.effective_compatibility(defaults));
    let mut findings = rules::apply_suppressions(findings, &contract.allow, as_of);
    if drift
        && let Some(generated) = &contract.generated
        && let Some(drift_finding) =
            generated_drift_finding(repo_root, contract, &head_bytes, &generated.command)
    {
        findings.push(drift_finding);
    }
    Report::new(findings, Vec::new(), 1)
}

pub fn check_contracts(
    repo_root: &Path,
    config: &Config,
    since: Option<&str>,
    as_of: Option<&str>,
    drift: bool,
) -> Report {
    let scoped_contracts = match select_contracts_in_scope(repo_root, config, since) {
        Ok(contracts) => contracts,
        Err(message) => {
            return Report::new(
                Vec::new(),
                vec![Unavailable {
                    contract: None,
                    message,
                }],
                0,
            );
        }
    };

    let mut findings = Vec::new();
    let mut unavailable = Vec::new();
    for contract in &scoped_contracts {
        let report = check_contract(repo_root, &config.defaults, contract, as_of, drift);
        findings.extend(report.findings);
        unavailable.extend(report.unavailable);
    }

    Report::new(findings, unavailable, scoped_contracts.len())
}

fn generated_drift_finding(
    repo_root: &Path,
    contract: &ContractConfig,
    committed_bytes: &[u8],
    command: &str,
) -> Option<rules::Finding> {
    let temp = tempfile::tempdir().ok()?;
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(temp.path())
        .env("BRAKE_REPO_ROOT", repo_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return Some(rules::Finding {
            rule_id: "generated-drift",
            severity: Severity::Error,
            message: format!(
                "generated command failed for contract `{}`: {}",
                contract.name,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            method: None,
            path: Some(contract.source.display().to_string()),
            span: None,
        });
    }
    if output.stdout != committed_bytes {
        return Some(rules::Finding {
            rule_id: "generated-drift",
            severity: Severity::Error,
            message: format!(
                "generated output drift for contract `{}` from `{}`",
                contract.name,
                contract.source.display()
            ),
            method: None,
            path: Some(contract.source.display().to_string()),
            span: None,
        });
    }
    None
}

fn select_contracts_in_scope<'a>(
    repo_root: &Path,
    config: &'a Config,
    since: Option<&str>,
) -> Result<Vec<&'a ContractConfig>, String> {
    if let Some(reference) = since {
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
        let merge_base_object = merge_base
            .object()
            .map_err(|error| format!("failed to read merge-base object: {error}"))?;
        let merge_base_commit = merge_base_object
            .try_into_commit()
            .map_err(|error| format!("failed to decode merge-base commit: {error}"))?;
        let merge_base_tree = merge_base_commit
            .tree()
            .map_err(|error| format!("failed to read merge-base tree: {error}"))?;

        let mut in_scope = Vec::new();
        for contract in &config.contracts {
            let baseline_bytes = match merge_base_tree.lookup_entry_by_path(&contract.source) {
                Ok(Some(entry)) => {
                    let object = entry.object().map_err(|error| {
                        format!(
                            "failed to read merge-base object `{}` for --since: {error}",
                            contract.source.display()
                        )
                    })?;
                    let mut blob = object.try_into_blob().map_err(|error| {
                        format!(
                            "failed to decode merge-base blob `{}` for --since: {error}",
                            contract.source.display()
                        )
                    })?;
                    Some(blob.take_data())
                }
                Ok(None) => None,
                Err(error) => {
                    return Err(format!(
                        "failed to read merge-base path `{}` for --since: {error}",
                        contract.source.display()
                    ));
                }
            };
            let head_bytes = fs::read(repo_root.join(&contract.source)).ok();
            if baseline_bytes != head_bytes {
                in_scope.push(contract);
            }
        }
        return Ok(in_scope);
    }

    Ok(config.contracts.iter().collect())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    use tempfile::tempdir;

    use super::{check_contract, check_contracts};
    use crate::Severity;
    use crate::config::{
        Baseline, Compatibility, Config, ContractConfig, ContractFormat, Defaults,
    };
    use crate::render::text;

    fn contract_fixture() -> ContractConfig {
        ContractConfig {
            name: "payments".to_owned(),
            format: ContractFormat::Openapi,
            source: PathBuf::from("api/payments-openapi.yaml"),
            compatibility: None,
            baseline: Some(Baseline::File(PathBuf::from(
                "api/payments-openapi.baseline.yaml",
            ))),
            allow: Vec::new(),
            generated: None,
        }
    }

    fn run_git(repo: &std::path::Path, args: &[&str]) {
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

    fn write_openapi(path: &std::path::Path, endpoint_path: &str) {
        fs::write(
            path,
            format!(
                r#"
openapi: 3.1.0
paths:
  {endpoint_path}:
        get:
          operationId: op
          responses:
            "200":
              description: ok
"#
            ),
        )
        .expect("write openapi file");
    }

    #[test]
    fn removed_endpoint_fails_with_diagnostic() {
        let repo = tempdir().expect("tempdir");
        let api_dir = repo.path().join("api");
        fs::create_dir_all(&api_dir).expect("mkdir api");

        fs::write(
            api_dir.join("payments-openapi.baseline.yaml"),
            r#"
openapi: 3.1.0
paths:
  /payments/{id}:
    get:
      operationId: getPayment
      responses:
        "200":
          description: ok
"#,
        )
        .expect("write baseline");
        fs::write(
            api_dir.join("payments-openapi.yaml"),
            r#"
openapi: 3.1.0
paths:
  /payments:
    get:
      operationId: listPayments
      responses:
        "200":
          description: ok
"#,
        )
        .expect("write head");

        let defaults = Defaults {
            compatibility: Compatibility::WireJson,
            baseline: None,
        };
        let report = check_contract(repo.path(), &defaults, &contract_fixture(), None, false);

        assert_eq!(report.exit_code(Severity::Error), 1);
        let rendered = text::render(&report);
        assert!(rendered.contains("error[endpoint-removed]"));
        assert!(rendered.contains("payments-openapi.baseline.yaml"));
    }

    #[test]
    fn restored_endpoint_exits_clean() {
        let repo = tempdir().expect("tempdir");
        let api_dir = repo.path().join("api");
        fs::create_dir_all(&api_dir).expect("mkdir api");

        let contract_body = r#"
openapi: 3.1.0
paths:
  /payments/{id}:
    get:
      operationId: getPayment
      responses:
        "200":
          description: ok
"#;
        fs::write(
            api_dir.join("payments-openapi.baseline.yaml"),
            contract_body,
        )
        .expect("write baseline");
        fs::write(api_dir.join("payments-openapi.yaml"), contract_body).expect("write head");

        let defaults = Defaults {
            compatibility: Compatibility::WireJson,
            baseline: None,
        };
        let report = check_contract(repo.path(), &defaults, &contract_fixture(), None, false);

        assert_eq!(report.exit_code(Severity::Error), 0);
    }

    #[test]
    fn since_scope_only_checks_changed_contracts() {
        let repo = tempdir().expect("tempdir");
        run_git(repo.path(), &["init", "-b", "main"]);
        run_git(repo.path(), &["config", "user.name", "Brake Test"]);
        run_git(repo.path(), &["config", "user.email", "brake@example.com"]);

        let api_dir = repo.path().join("api");
        fs::create_dir_all(&api_dir).expect("mkdir api");

        let a_path = api_dir.join("a.yaml");
        let b_path = api_dir.join("b.yaml");
        write_openapi(&a_path, "/a");
        write_openapi(&b_path, "/b");
        run_git(repo.path(), &["add", "api/a.yaml", "api/b.yaml"]);
        run_git(repo.path(), &["commit", "-m", "base"]);

        fs::write(
            &a_path,
            r#"
openapi: 3.1.0
paths:
  /a-renamed:
    get:
      operationId: op
      responses:
        "200":
          description: ok
"#,
        )
        .expect("update contract a");
        run_git(repo.path(), &["add", "api/a.yaml"]);
        run_git(repo.path(), &["commit", "-m", "change a"]);

        let config = Config {
            defaults: Defaults {
                compatibility: Compatibility::WireJson,
                baseline: Some(Baseline::GitMergeBase {
                    reference: "HEAD~1".to_owned(),
                }),
            },
            contracts: vec![
                ContractConfig {
                    name: "a".to_owned(),
                    format: ContractFormat::Openapi,
                    source: PathBuf::from("api/a.yaml"),
                    compatibility: None,
                    baseline: None,
                    allow: Vec::new(),
                    generated: None,
                },
                ContractConfig {
                    name: "b".to_owned(),
                    format: ContractFormat::Openapi,
                    source: PathBuf::from("api/b.yaml"),
                    compatibility: None,
                    baseline: None,
                    allow: Vec::new(),
                    generated: None,
                },
            ],
        };

        let report = check_contracts(repo.path(), &config, Some("HEAD~1"), None, false);
        assert_eq!(report.contracts_checked, 1);
        assert_eq!(report.exit_code(Severity::Error), 1);
    }

    #[test]
    fn drift_reports_generated_drift_when_output_differs() {
        let repo = tempdir().expect("tempdir");
        let api_dir = repo.path().join("api");
        fs::create_dir_all(&api_dir).expect("mkdir api");

        let contract_body = r#"
openapi: 3.1.0
paths:
  /payments/{id}:
    get:
      operationId: getPayment
      responses:
        "200":
          description: ok
"#;
        fs::write(
            api_dir.join("payments-openapi.baseline.yaml"),
            contract_body,
        )
        .expect("write baseline");
        fs::write(api_dir.join("payments-openapi.yaml"), contract_body).expect("write head");

        let defaults = Defaults {
            compatibility: Compatibility::WireJson,
            baseline: None,
        };
        let mut contract = contract_fixture();
        contract.generated = Some(crate::config::GeneratedConfig {
            command: "printf 'not-the-checked-in-spec'".to_owned(),
        });

        let report = check_contract(repo.path(), &defaults, &contract, None, true);
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.rule_id == "generated-drift")
        );
        assert_eq!(report.exit_code(Severity::Error), 1);
    }

    #[test]
    fn drift_passes_when_generated_output_matches() {
        let repo = tempdir().expect("tempdir");
        let api_dir = repo.path().join("api");
        fs::create_dir_all(&api_dir).expect("mkdir api");

        let contract_body = r#"
openapi: 3.1.0
paths:
  /payments/{id}:
    get:
      operationId: getPayment
      responses:
        "200":
          description: ok
"#;
        fs::write(
            api_dir.join("payments-openapi.baseline.yaml"),
            contract_body,
        )
        .expect("write baseline");
        fs::write(api_dir.join("payments-openapi.yaml"), contract_body).expect("write head");

        let defaults = Defaults {
            compatibility: Compatibility::WireJson,
            baseline: None,
        };
        let mut contract = contract_fixture();
        contract.generated = Some(crate::config::GeneratedConfig {
            command: "cat \"$BRAKE_REPO_ROOT/api/payments-openapi.yaml\"".to_owned(),
        });

        let report = check_contract(repo.path(), &defaults, &contract, None, true);
        assert!(
            !report
                .findings
                .iter()
                .any(|finding| finding.rule_id == "generated-drift")
        );
        assert_eq!(report.exit_code(Severity::Error), 0);
    }
}
