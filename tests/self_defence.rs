//! The seven tests that defend the claims brake makes about itself.
//!
//! Each maps to a numbered guarantee in `design/02-contract-gates.md` §6.1,
//! and the set is enumerated in `design/03-implementation-plan.md` §6. They are
//! not optional: a deterministic tool must be *provably* deterministic or it is
//! a flaky test with a good reputation.
//!
//! Five defend the contract axis. Two more, at the end, defend the same
//! guarantees over the *demand* axis that `design/05-consumer-demand.md` §8
//! adds: a URL in a pact is data, and a declared consumer brake could not read
//! is never a clean run.
//!
//! Fixtures are built under `tempfile::tempdir()`, never read from the
//! surrounding checkout. A test that inspects the ambient repository passes on
//! every laptop and fails in CI, which checks out a detached HEAD.

use std::fs;
use std::path::{Path, PathBuf};

use brake::check::{Options, Scope, check};
use brake::config::{Baseline, Compatibility, Config, ContractConfig, ContractFormat, Defaults};
use brake::render::{github, gitlab, json, sarif, text};
use tempfile::{TempDir, tempdir};

const OPENAPI: &str = r#"
openapi: 3.1.0
paths:
  /payments/{id}:
    get:
      operationId: getPayment
      parameters:
        - name: id
          in: path
          required: true
          schema: { type: string }
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                type: object
                required: [id]
                properties:
                  id: { type: string }
                  legacy_reference: { type: string }
            application/xml:
              schema: { type: string }
"#;

const PROTO: &str = r#"
syntax = "proto3";
package payments;
message Req { string id = 1; }
message Payment { string id = 1; int32 amount = 2; }
service PaymentService { rpc Get(Req) returns (Payment); }
"#;

const GRAPHQL: &str = r#"
type Query {
  payment(id: ID!): Payment!
}
type Payment {
  id: ID!
  legacyReference: String
}
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

fn config(format: ContractFormat, source: &str, baseline: &str) -> Config {
    Config {
        defaults: Defaults {
            compatibility: Compatibility::Strict,
            baseline: None,
        },
        contracts: vec![ContractConfig {
            name: "payments".to_owned(),
            format,
            source: PathBuf::from(source),
            compatibility: None,
            baseline: Some(Baseline::File(PathBuf::from(baseline))),
            allow: Vec::new(),
            generated: None,
        }],
        ..Config::default()
    }
}

fn render_all(root: &Path, config: &Config) -> (String, String, String, String, String) {
    let report = check(root, config, &Scope::All, &Options::default());
    (
        text::render(&report),
        json::render(&report),
        sarif::render(&report),
        github::render(&report),
        gitlab::render(&report),
    )
}

/// G4 — byte-stability. Two runs on identical inputs produce identical bytes
/// in every output format, for every ingester.
#[test]
fn g4_two_runs_on_the_same_inputs_produce_the_same_bytes() {
    let cases: &[(ContractFormat, &str, &str, &str, &str)] = &[
        (
            ContractFormat::Openapi,
            "api/openapi.yaml",
            "api/openapi.baseline.yaml",
            OPENAPI,
            &OPENAPI_CHANGED,
        ),
        (
            ContractFormat::Proto,
            "api/payments.proto",
            "api/payments.baseline.proto",
            PROTO,
            PROTO_CHANGED,
        ),
        (
            ContractFormat::Graphql,
            "api/schema.graphql",
            "api/schema.baseline.graphql",
            GRAPHQL,
            GRAPHQL_CHANGED,
        ),
    ];

    for (format, source, baseline, base_body, head_body) in cases {
        let checkout = repo(&[(baseline, base_body), (source, head_body)]);
        let config = config(*format, source, baseline);

        let first = render_all(checkout.path(), &config);
        let second = render_all(checkout.path(), &config);

        assert_eq!(
            first.0, second.0,
            "text output differs between runs: {source}"
        );
        assert_eq!(
            first.1, second.1,
            "json output differs between runs: {source}"
        );
        assert_eq!(
            first.2, second.2,
            "sarif output differs between runs: {source}"
        );
        assert_eq!(
            first.3, second.3,
            "github output differs between runs: {source}"
        );
        assert_eq!(
            first.4, second.4,
            "gitlab output differs between runs: {source}"
        );
        assert!(
            !first.1.contains("\"line\":null") || !first.1.is_empty(),
            "sanity: the run produced output"
        );
    }
}

/// A second checkout at a different path must also produce the same bytes,
/// which is the half of G4 that catches an absolute path leaking into output.
#[test]
fn g4_output_does_not_depend_on_where_the_repository_lives() {
    let one = repo(&[
        ("api/openapi.baseline.yaml", OPENAPI),
        ("api/openapi.yaml", &OPENAPI_CHANGED),
    ]);
    let two = repo(&[
        ("api/openapi.baseline.yaml", OPENAPI),
        ("api/openapi.yaml", &OPENAPI_CHANGED),
    ]);
    let config = config(
        ContractFormat::Openapi,
        "api/openapi.yaml",
        "api/openapi.baseline.yaml",
    );

    assert_ne!(one.path(), two.path(), "the two checkouts must differ");
    assert_eq!(
        render_all(one.path(), &config),
        render_all(two.path(), &config),
        "output embeds the checkout path"
    );
}

/// G3 — order-independence. The verdict does not depend on YAML key order.
#[test]
fn g3_shuffling_yaml_mapping_keys_does_not_change_the_output() {
    let ordered = r#"
openapi: 3.1.0
info:
  title: payments
  version: "1.0"
paths:
  /payments:
    get:
      operationId: listPayments
      deprecated: false
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                type: object
                required: [id]
                properties:
                  alpha: { type: string }
                  beta: { type: integer }
                  id: { type: string }
            application/xml:
              schema: { type: string }
"#;
    // The same document, with every mapping's keys in a different order.
    let shuffled = r#"
paths:
  /payments:
    get:
      responses:
        "200":
          content:
            application/xml:
              schema: { type: string }
            application/json:
              schema:
                properties:
                  id: { type: string }
                  beta: { type: integer }
                  alpha: { type: string }
                required: [id]
                type: object
          description: ok
      deprecated: false
      operationId: listPayments
info:
  version: "1.0"
  title: payments
openapi: 3.1.0
"#;

    let checkout = repo(&[
        ("api/openapi.baseline.yaml", ordered),
        ("api/openapi.yaml", shuffled),
    ]);
    let config = config(
        ContractFormat::Openapi,
        "api/openapi.yaml",
        "api/openapi.baseline.yaml",
    );
    let report = check(checkout.path(), &config, &Scope::All, &Options::default());

    assert!(
        report.findings.is_empty(),
        "key order changed the verdict: {:?}",
        report.findings
    );

    // And the reverse comparison agrees, which rules out a one-way collapse.
    let reversed = repo(&[
        ("api/openapi.baseline.yaml", shuffled),
        ("api/openapi.yaml", ordered),
    ]);
    assert!(
        check(reversed.path(), &config, &Scope::All, &Options::default())
            .findings
            .is_empty()
    );
}

/// G1 — hermeticity. A `$ref` resolving to a URL produces
/// `contract-unreachable`; it is never fetched, under any flag.
#[test]
fn g1_a_remote_ref_is_refused_and_no_request_is_made() {
    // Pointed at a port nothing is listening on: if brake ever tried to
    // resolve this, the run would fail differently — a connection error, or a
    // hang — rather than reporting the ref as refused.
    let remote = r#"
openapi: 3.1.0
paths:
  /payments:
    get:
      operationId: listPayments
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                $ref: 'http://127.0.0.1:9/schemas.yaml#/components/schemas/Payment'
"#;
    let checkout = repo(&[
        ("api/openapi.baseline.yaml", OPENAPI),
        ("api/openapi.yaml", remote),
    ]);
    let config = config(
        ContractFormat::Openapi,
        "api/openapi.yaml",
        "api/openapi.baseline.yaml",
    );

    let started = std::time::Instant::now();
    let report = check(checkout.path(), &config, &Scope::All, &Options::default());
    let elapsed = started.elapsed();

    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.rule_id == "contract-unreachable"),
        "a remote ref must be reported, not fetched: {:?}",
        report.findings
    );
    assert_eq!(report.exit_code(brake::Severity::Error), 1);
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "the run took {elapsed:?}, which suggests something waited on a socket"
    );

    // Every scheme, and every flag combination, refuses alike.
    for scheme in ["https://example.invalid/s.yaml", "//example.invalid/s.yaml"] {
        let body = remote.replace("http://127.0.0.1:9/schemas.yaml", scheme);
        let checkout = repo(&[
            ("api/openapi.baseline.yaml", OPENAPI),
            ("api/openapi.yaml", &body),
        ]);
        let report = check(
            checkout.path(),
            &config,
            &Scope::All,
            &Options {
                drift: true,
                ..Options::default()
            },
        );
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.rule_id == "contract-unreachable"),
            "`{scheme}` was not refused"
        );
    }
}

/// G2 — filesystem bound. A `$ref` escaping the directory containing `source`
/// is an error, not a read.
#[test]
fn g2_a_ref_escaping_the_source_tree_is_an_error_not_a_read() {
    const CANARY: &str = "THIS-FILE-MUST-NEVER-BE-READ-BY-BRAKE";

    let checkout = repo(&[
        ("api/openapi.baseline.yaml", OPENAPI),
        (
            "api/openapi.yaml",
            r#"
openapi: 3.1.0
paths:
  /payments:
    get:
      operationId: listPayments
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                $ref: '../../secrets.yaml#/components/schemas/Payment'
"#,
        ),
    ]);
    // A real, readable file at the escaping path, so the test proves brake
    // declined to read it rather than merely failing to find it.
    let secrets = checkout
        .path()
        .parent()
        .expect("temp parent")
        .join("secrets.yaml");
    fs::write(
        &secrets,
        format!(
            "components:\n  schemas:\n    Payment:\n      type: string\n      title: {CANARY}\n"
        ),
    )
    .expect("write the canary");

    let config = config(
        ContractFormat::Openapi,
        "api/openapi.yaml",
        "api/openapi.baseline.yaml",
    );
    let report = check(checkout.path(), &config, &Scope::All, &Options::default());

    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.rule_id == "contract-unreachable"),
        "an escaping ref must be an error: {:?}",
        report.findings
    );
    for rendered in [
        text::render(&report),
        json::render(&report),
        sarif::render(&report),
    ] {
        assert!(
            !rendered.contains(CANARY),
            "brake read a file outside the source directory"
        );
    }

    let _ = fs::remove_file(&secrets);
}

/// §6.2 — honest failure. A configured-but-unresolvable baseline exits `2`.
///
/// This is the failure that would make every other test meaningless: a gate
/// that returns clean when it cannot see the baseline is worse than no gate.
#[test]
fn honest_failure_a_missing_baseline_exits_two_not_zero() {
    let checkout = repo(&[("api/openapi.yaml", OPENAPI)]);
    let config = config(
        ContractFormat::Openapi,
        "api/openapi.yaml",
        "api/openapi.baseline.yaml",
    );

    let report = check(checkout.path(), &config, &Scope::All, &Options::default());

    assert_eq!(
        report.exit_code(brake::Severity::Error),
        2,
        "a gate that cannot see its baseline must not report clean"
    );
    assert!(!report.unavailable.is_empty());

    // The same for a git baseline that cannot be resolved.
    let mut git_config = config.clone();
    git_config.contracts[0].baseline = Some(Baseline::Git {
        spec: "no-such-ref:api/openapi.yaml".to_owned(),
    });
    assert_eq!(
        check(
            checkout.path(),
            &git_config,
            &Scope::All,
            &Options::default()
        )
        .exit_code(brake::Severity::Error),
        2
    );

    // And the distinction that matters: an *unconfigured* baseline is a user
    // who has not opted in, and exits 0.
    let mut unconfigured = config;
    unconfigured.contracts[0].baseline = None;
    let report = check(
        checkout.path(),
        &unconfigured,
        &Scope::All,
        &Options::default(),
    );
    assert_eq!(
        report.exit_code(brake::Severity::Error),
        0,
        "an unconfigured baseline is not a tool failure"
    );
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.rule_id == "baseline-unconfigured")
    );
}

/// Not one of the five, but the same class: an unmodelled construct must never
/// be the reason a run looks clean.
#[test]
fn an_unverifiable_payload_is_never_reported_as_verified() {
    let with_external_ref = r#"
openapi: 3.1.0
paths:
  /payments:
    get:
      operationId: listPayments
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                $ref: 'common.yaml#/components/schemas/Payment'
"#;
    let checkout = repo(&[
        ("api/openapi.baseline.yaml", with_external_ref),
        ("api/openapi.yaml", with_external_ref),
    ]);
    let config = config(
        ContractFormat::Openapi,
        "api/openapi.yaml",
        "api/openapi.baseline.yaml",
    );

    let report = check(checkout.path(), &config, &Scope::All, &Options::default());
    let partial = report
        .findings
        .iter()
        .find(|finding| finding.rule_id == "contract-partial")
        .expect("identical-but-unreadable schemas must report partial, not clean");
    assert!(
        partial.message.contains("common.yaml"),
        "the finding must name the construct: {}",
        partial.message
    );
}

static OPENAPI_CHANGED: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    OPENAPI
        .replace("                  legacy_reference: { type: string }\n", "")
        .replace(
            "            application/xml:\n              schema: { type: string }\n",
            "",
        )
});

const PROTO_CHANGED: &str = r#"
syntax = "proto3";
package payments;
message Req { string id = 1; }
message Payment { string id = 7; }
service PaymentService { rpc Get(Req) returns (Payment); }
"#;

const GRAPHQL_CHANGED: &str = r#"
type Query {
  payment(id: ID!): Payment
}
type Payment {
  id: ID!
}
"#;

// ── the demand axis: design/05-consumer-demand.md §8 ────────────────────────

/// A pact declaring exactly what `OPENAPI` produces, so any finding below is
/// about the URLs in it rather than about the payload.
const PACT_FULL_OF_URLS: &str = r#"{
  "consumer": { "name": "web-checkout" },
  "provider": { "name": "payments" },
  "_links": {
    "self": { "href": "http://broker.invalid/pacts/provider/payments/latest" },
    "pb:publish-verification-results": { "href": "https://broker.invalid/verify" }
  },
  "interactions": [
    {
      "description": "a request for payment 42",
      "request": { "method": "GET", "path": "/payments/42" },
      "response": {
        "status": 200,
        "headers": { "Content-Type": "application/json" },
        "body": {
          "id": "42",
          "legacy_reference": "L-1",
          "customer_id": { "$ref": "http://example.invalid/schema.json" }
        }
      }
    }
  ]
}"#;

fn demand_config(consumer_source: &str) -> Config {
    Config::parse(&format!(
        "[[contract]]\nname = \"payments\"\nformat = \"openapi\"\n\
         source = \"api/openapi.yaml\"\n\
         baseline = {{ file = \"api/openapi.baseline.yaml\" }}\n\
         \n[[consumer]]\nformat = \"pact\"\nsource = \"{consumer_source}\"\n"
    ))
    .expect("the config in this test must parse")
}

/// G1, over the demand axis — a URL anywhere in a pact is data.
///
/// `_links`, `pb:publish`, a `$ref` inside an example body: none of them is
/// ever dereferenced, under any flag. The hosts here are `.invalid`, which is
/// reserved by RFC 2606 and can never resolve, so a run that tried to fetch
/// one would hang or fail rather than agree with itself.
#[test]
fn g1_a_pact_carrying_broker_links_and_a_remote_ref_opens_no_socket() {
    let checkout = repo(&[
        ("api/openapi.yaml", OPENAPI),
        ("api/openapi.baseline.yaml", OPENAPI),
        ("pacts/web-checkout.json", PACT_FULL_OF_URLS),
    ]);
    let config = demand_config("pacts/web-checkout.json");

    let report = check(checkout.path(), &config, &Scope::All, &Options::default());

    // The verdict came from the bytes on disk: `customer_id` is reported as a
    // field the contract does not produce, rather than resolved over the wire.
    assert!(
        report.findings.iter().any(|finding| {
            finding.rule_id == "consumer-field-unmet"
                && finding.subject.as_deref() == Some("customer_id")
        }),
        "{:?}",
        report
            .findings
            .iter()
            .map(|finding| &finding.message)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        json::render(&report),
        json::render(&check(
            checkout.path(),
            &config,
            &Scope::All,
            &Options::default()
        )),
        "two runs against a host that cannot exist agree, which they could not \
         do if either had waited on it"
    );
}

/// §6.2 honest failure, over the demand axis.
///
/// The CI-pull workflow of §5.1 rests entirely on this: a prior step writes
/// the directory and a failed pull leaves the declared file absent. That has
/// to be loud rather than clean, or the pipeline reports a verification that
/// never happened.
#[test]
fn honest_failure_a_declared_consumer_that_is_absent_never_reads_as_clean() {
    let checkout = repo(&[
        ("api/openapi.yaml", OPENAPI),
        ("api/openapi.baseline.yaml", OPENAPI),
    ]);
    let report = check(
        checkout.path(),
        &demand_config("pacts/pulled-by-ci.json"),
        &Scope::All,
        &Options::default(),
    );

    assert_eq!(
        report.exit_code(brake::Severity::Warning),
        1,
        "a declaration brake could not read must not be reported as satisfied"
    );
    let unreachable = report
        .findings
        .iter()
        .find(|finding| finding.rule_id == "consumer-unreachable")
        .expect("consumer-unreachable");
    assert_eq!(unreachable.severity, brake::Severity::Error);
    assert!(
        unreachable.message.contains("pulled-by-ci.json"),
        "the finding must name the file: {}",
        unreachable.message
    );
}
