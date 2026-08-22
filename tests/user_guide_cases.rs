use std::fs;
use std::path::PathBuf;

use brake::Severity;
use brake::check::check_contracts;
use brake::config::{Baseline, Compatibility, Config, ContractConfig, ContractFormat, Defaults};
use tempfile::tempdir;

struct UserGuideCase {
    id: &'static str,
    format: ContractFormat,
    source: &'static str,
    baseline_source: &'static str,
    baseline_body: &'static str,
    head_body: &'static str,
    expected_exit: i32,
    expected_rule: Option<&'static str>,
}

#[test]
fn user_guide_case_matrix() {
    let cases = vec![
        UserGuideCase {
            id: "openapi-clean",
            format: ContractFormat::Openapi,
            source: "api/openapi.yaml",
            baseline_source: "api/openapi.baseline.yaml",
            baseline_body: r#"
openapi: 3.1.0
paths:
  /payments/{id}:
    get:
      operationId: getPayment
      responses:
        "200":
          description: ok
"#,
            head_body: r#"
openapi: 3.1.0
paths:
  /payments/{id}:
    get:
      operationId: getPayment
      responses:
        "200":
          description: ok
"#,
            expected_exit: 0,
            expected_rule: None,
        },
        UserGuideCase {
            id: "openapi-endpoint-removed",
            format: ContractFormat::Openapi,
            source: "api/openapi.yaml",
            baseline_source: "api/openapi.baseline.yaml",
            baseline_body: r#"
openapi: 3.1.0
paths:
  /payments/{id}:
    get:
      operationId: getPayment
      responses:
        "200":
          description: ok
"#,
            head_body: r#"
openapi: 3.1.0
paths:
  /payments:
    get:
      operationId: listPayments
      responses:
        "200":
          description: ok
"#,
            expected_exit: 1,
            expected_rule: Some("endpoint-removed"),
        },
        UserGuideCase {
            id: "protobuf-clean",
            format: ContractFormat::Proto,
            source: "api/payments.proto",
            baseline_source: "api/payments.baseline.proto",
            baseline_body: r#"
syntax = "proto3";
package payments;

message GetPaymentRequest {
  string id = 1;
}

message Payment {
  string id = 1;
}

service PaymentService {
  rpc GetPayment(GetPaymentRequest) returns (Payment);
}
"#,
            head_body: r#"
syntax = "proto3";
package payments;

message GetPaymentRequest {
  string id = 1;
}

message Payment {
  string id = 1;
}

service PaymentService {
  rpc GetPayment(GetPaymentRequest) returns (Payment);
}
"#,
            expected_exit: 0,
            expected_rule: None,
        },
        UserGuideCase {
            id: "protobuf-rpc-removed",
            format: ContractFormat::Proto,
            source: "api/payments.proto",
            baseline_source: "api/payments.baseline.proto",
            baseline_body: r#"
syntax = "proto3";
package payments;

message GetPaymentRequest {
  string id = 1;
}

message Payment {
  string id = 1;
}

service PaymentService {
  rpc GetPayment(GetPaymentRequest) returns (Payment);
}
"#,
            head_body: r#"
syntax = "proto3";
package payments;

message GetPaymentRequest {
  string id = 1;
}

message Payment {
  string id = 1;
}

service PaymentService {}
"#,
            expected_exit: 1,
            expected_rule: Some("endpoint-removed"),
        },
        UserGuideCase {
            id: "graphql-clean",
            format: ContractFormat::Graphql,
            source: "api/schema.graphql",
            baseline_source: "api/schema.baseline.graphql",
            baseline_body: r#"
type Query {
  payment(id: ID!): Payment!
}

type Payment {
  id: ID!
}
"#,
            head_body: r#"
type Query {
  payment(id: ID!): Payment!
}

type Payment {
  id: ID!
}
"#,
            expected_exit: 0,
            expected_rule: None,
        },
        UserGuideCase {
            id: "graphql-query-removed",
            format: ContractFormat::Graphql,
            source: "api/schema.graphql",
            baseline_source: "api/schema.baseline.graphql",
            baseline_body: r#"
type Query {
  payment(id: ID!): Payment!
}

type Payment {
  id: ID!
}
"#,
            head_body: r#"
type Query {
  health: String!
}
"#,
            expected_exit: 1,
            expected_rule: Some("endpoint-removed"),
        },
    ];

    for case in cases {
        let repo = tempdir().expect("tempdir");
        fs::create_dir_all(repo.path().join("api")).expect("mkdir api");
        fs::write(repo.path().join(case.baseline_source), case.baseline_body).expect("baseline");
        fs::write(repo.path().join(case.source), case.head_body).expect("head");

        let config = Config {
            defaults: Defaults {
                compatibility: Compatibility::WireJson,
                baseline: None,
            },
            contracts: vec![ContractConfig {
                name: case.id.to_owned(),
                format: case.format,
                source: PathBuf::from(case.source),
                compatibility: None,
                baseline: Some(Baseline::File(PathBuf::from(case.baseline_source))),
                allow: Vec::new(),
                generated: None,
            }],
        };

        let report = check_contracts(repo.path(), &config, None, None, false);
        assert_eq!(
            report.exit_code(Severity::Error),
            case.expected_exit,
            "case {} exit mismatch",
            case.id
        );

        if let Some(rule_id) = case.expected_rule {
            assert!(
                report
                    .findings
                    .iter()
                    .any(|finding| finding.rule_id == rule_id),
                "case {} expected finding {}",
                case.id,
                rule_id
            );
        } else {
            assert!(
                report.findings.is_empty(),
                "case {} expected no findings",
                case.id
            );
        }
    }
}
