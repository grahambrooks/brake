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
    insta::assert_snapshot!(
        "text_tool_failure",
        // The OS supplies the reason a file could not be read, and words it
        // differently on each platform. The snapshot is about brake's framing
        // — "could not check X … this is a tool failure, not an API break" —
        // so the platform's half is redacted rather than pinned.
        normalise_os_errors(&text::render(&report))
    );
}

/// Replace an OS-supplied error phrase with a marker.
///
/// `No such file or directory (os error 2)` on Unix,
/// `The system cannot find the file specified. (os error 2)` on Windows.
fn normalise_os_errors(rendered: &str) -> String {
    let mut out = String::new();
    for line in rendered.lines() {
        match line.find("(os error") {
            Some(index) => {
                let head = &line[..index];
                let cut = head.rfind(": ").map_or(head.len(), |at| at + 2);
                out.push_str(&head[..cut]);
                out.push_str("<os error>");
            }
            None => out.push_str(line),
        }
        out.push('\n');
    }
    out
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
    let mut value: serde_json::Value =
        serde_json::from_str(&sarif::render(&report)).expect("valid SARIF");

    // SARIF reports the tool version, and it should. Pinning it in a snapshot
    // means `make release` — which bumps the version *after* running the gate
    // — leaves CI red on the release commit. Redacted here so the snapshot
    // covers the shape, and asserted separately so the field is still checked.
    let driver = &mut value["runs"][0]["tool"]["driver"];
    assert_eq!(driver["version"], brake::VERSION);
    assert_eq!(driver["semanticVersion"], brake::VERSION);
    driver["version"] = serde_json::json!("[version]");
    driver["semanticVersion"] = serde_json::json!("[version]");

    insta::assert_snapshot!(
        "sarif_response_field_removed",
        serde_json::to_string_pretty(&value).expect("pretty")
    );
}

/// Nothing may pin platform-specific text either.
///
/// An OS error phrase or a backslash path passes on the machine that wrote it
/// and fails on the next platform CI runs — which is how the Windows job
/// stayed red while `make check` was green locally.
#[test]
fn no_snapshot_pins_platform_specific_text() {
    for path in snapshot_files() {
        let body = fs::read_to_string(&path).expect("read snapshot");
        assert!(
            !body.contains("(os error"),
            "{} pins an OS error phrase, which is worded differently per platform",
            path.display()
        );
        // A colon immediately followed by a backslash is a Windows drive
        // letter and nothing else. Checking for a bare backslash would trip
        // on insta's own header, which escapes the quotes in `expression:`.
        assert!(
            !body.contains(":\\"),
            "{} pins an absolute Windows path",
            path.display()
        );
    }
}

fn snapshot_files() -> Vec<PathBuf> {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
    fs::read_dir(&directory)
        .expect("snapshots directory")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "snap")
        })
        .collect()
}

/// Nothing else may pin the version either.
///
/// A snapshot that embeds it is a release-time landmine: it passes when the
/// gate runs and fails after the bump, on a commit already tagged and pushed.
#[test]
fn no_snapshot_embeds_the_crate_version() {
    for path in snapshot_files() {
        let body = fs::read_to_string(&path).expect("read snapshot");
        assert!(
            !body.contains(brake::VERSION),
            "{} pins the crate version, so the next `make release` will break it",
            path.display()
        );
    }
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
