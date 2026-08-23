//! Snapshot tests for the rendered diagnostic.
//!
//! Diagnostic rendering is exactly what snapshot tests are for
//! (`design/03-implementation-plan.md` §4, O5): the thing being asserted is
//! "does this read well", which no `assert!` expresses. They double as
//! byte-stability tests, since a snapshot that drifts run to run cannot pass.
//!
//! Review a change with `cargo insta review`, or accept with `INSTA_FORCE_PASS=1
//! cargo test && cargo insta accept`.

use std::fs;
use std::path::PathBuf;

use brake::check::{Options, Scope, check};
use brake::config::{Baseline, Compatibility, Config, ContractConfig, ContractFormat, Defaults};
use brake::render::{json, sarif, text};
use tempfile::{TempDir, tempdir};

const BASELINE: &str = r#"openapi: 3.1.0
info:
  title: payments
  version: "1.0"
paths:
  /payments/{id}:
    get:
      operationId: getPayment
      responses:
        "200":
          description: The payment.
          content:
            application/json:
              schema:
                type: object
                required: [id, customer_id]
                properties:
                  id:
                    type: string
                  customer_id:
                    type: string
                  status:
                    type: string
                    enum: [pending, settled]
"#;

const HEAD: &str = r#"openapi: 3.1.0
info:
  title: payments
  version: "1.0"
paths:
  /payments/{id}:
    get:
      operationId: getPayment
      responses:
        "200":
          description: The payment.
          content:
            application/json:
              schema:
                type: object
                required: [id]
                properties:
                  id:
                    type: string
                  status:
                    type: string
                    enum: [pending, settled, failed]
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

fn report_for(level: Compatibility, baseline: &str, head: &str) -> brake::report::Report {
    let checkout = repo(&[
        ("api/openapi.baseline.yaml", baseline),
        ("api/openapi.yaml", head),
    ]);
    let config = Config {
        defaults: Defaults {
            compatibility: level,
            baseline: None,
        },
        contracts: vec![ContractConfig {
            name: "payments".to_owned(),
            format: ContractFormat::Openapi,
            source: PathBuf::from("api/openapi.yaml"),
            compatibility: None,
            baseline: Some(Baseline::File(PathBuf::from("api/openapi.baseline.yaml"))),
            allow: Vec::new(),
            generated: None,
        }],
    };
    check(checkout.path(), &config, &Scope::All, &Options::default())
}

#[test]
fn text_diagnostic_for_a_response_field_removal() {
    let report = report_for(Compatibility::WireJson, BASELINE, HEAD);
    insta::assert_snapshot!("text_response_field_removed", text::render(&report));
}

#[test]
fn text_diagnostic_when_the_contract_is_clean() {
    let report = report_for(Compatibility::WireJson, BASELINE, BASELINE);
    insta::assert_snapshot!("text_clean", text::render(&report));
}

#[test]
fn text_diagnostic_for_a_tool_failure() {
    let checkout = repo(&[("api/openapi.yaml", HEAD)]);
    let config = Config {
        defaults: Defaults {
            compatibility: Compatibility::WireJson,
            baseline: None,
        },
        contracts: vec![ContractConfig {
            name: "payments".to_owned(),
            format: ContractFormat::Openapi,
            source: PathBuf::from("api/openapi.yaml"),
            compatibility: None,
            baseline: Some(Baseline::File(PathBuf::from("api/absent.baseline.yaml"))),
            allow: Vec::new(),
            generated: None,
        }],
    };
    let report = check(checkout.path(), &config, &Scope::All, &Options::default());
    insta::assert_snapshot!("text_tool_failure", text::render(&report));
}

#[test]
fn text_diagnostic_for_an_unverifiable_payload() {
    let unreadable = BASELINE.replace(
        "              schema:\n                type: object\n                required: [id, customer_id]",
        "              schema:\n                $ref: 'shared/common.yaml#/components/schemas/Payment'\n                x-unused:\n                  required: [id, customer_id]",
    );
    let report = report_for(Compatibility::WireJson, &unreadable, &unreadable);
    insta::assert_snapshot!("text_contract_partial", text::render(&report));
}

#[test]
fn json_output_for_a_response_field_removal() {
    let report = report_for(Compatibility::WireJson, BASELINE, HEAD);
    let value: serde_json::Value =
        serde_json::from_str(&json::render(&report)).expect("valid JSON");
    insta::assert_snapshot!(
        "json_response_field_removed",
        serde_json::to_string_pretty(&value).expect("pretty")
    );
}

#[test]
fn sarif_output_for_a_response_field_removal() {
    let report = report_for(Compatibility::WireJson, BASELINE, HEAD);
    let value: serde_json::Value =
        serde_json::from_str(&sarif::render(&report)).expect("valid SARIF");
    insta::assert_snapshot!(
        "sarif_response_field_removed",
        serde_json::to_string_pretty(&value).expect("pretty")
    );
}

/// A head that trips exactly one new rule at each level, so the snapshot is a
/// living answer to "what does picking a level actually get me".
///
/// Against `BASELINE`: a newly required query parameter (`wire`), a response
/// field that became optional (`wire-json`), the path parameter renamed
/// (`surface`), and a new optional response field (`strict`).
const LEVELS_HEAD: &str = r#"openapi: 3.1.0
info:
  title: payments
  version: "1.0"
paths:
  /payments/{payment_id}:
    get:
      operationId: getPayment
      parameters:
        - name: tenant
          in: query
          required: true
          schema:
            type: string
      responses:
        "200":
          description: The payment.
          content:
            application/json:
              schema:
                type: object
                required: [id]
                properties:
                  id:
                    type: string
                  customer_id:
                    type: string
                  settled_at:
                    type: string
                  status:
                    type: string
                    enum: [pending, settled]
"#;

#[test]
fn the_same_change_at_each_compatibility_level() {
    let mut rendered = String::new();
    for (name, level) in [
        ("wire", Compatibility::Wire),
        ("wire-json", Compatibility::WireJson),
        ("surface", Compatibility::Surface),
        ("strict", Compatibility::Strict),
    ] {
        let report = report_for(level, BASELINE, LEVELS_HEAD);
        rendered.push_str(&format!("=== {name} ===\n"));
        for finding in &report.findings {
            rendered.push_str(&format!("{} {}\n", finding.rule_id, finding.message));
        }
        rendered.push('\n');
    }
    insta::assert_snapshot!("levels", rendered);
}
