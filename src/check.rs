use std::fs;
use std::path::Path;

use crate::baseline;
use crate::compare;
use crate::config::{ContractConfig, Defaults};
use crate::contract::openapi;
use crate::report::{Report, Unavailable};
use crate::rules;

pub fn check_contract(repo_root: &Path, defaults: &Defaults, contract: &ContractConfig) -> Report {
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
    let findings = rules::evaluate(&changes);
    Report::new(findings, Vec::new(), 1)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tempfile::tempdir;

    use super::check_contract;
    use crate::Severity;
    use crate::config::{Baseline, Compatibility, ContractConfig, ContractFormat, Defaults};
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
        }
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
        let report = check_contract(repo.path(), &defaults, &contract_fixture());

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
        let report = check_contract(repo.path(), &defaults, &contract_fixture());

        assert_eq!(report.exit_code(Severity::Error), 0);
    }
}
