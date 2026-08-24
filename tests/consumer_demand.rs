//! The contracts of `design/05-consumer-demand.md`.
//!
//! Every rule in §6 gets a positive and a negative test — it fires on the
//! mismatch, and stays quiet on a contract that satisfies the declaration —
//! plus the two tests §8 adds to the self-defence set, the three policies of
//! §7.1 and the interface of §9.
//!
//! Fixtures are built under `tempfile::tempdir()`, never read from the
//! surrounding checkout: a test that inspects the ambient repository passes on
//! every laptop and fails in CI, which checks out a detached HEAD.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use brake::Severity;
use brake::check::{Options, Scope, check};
use brake::config::Config;
use brake::report::Report;
use brake::rules::Finding;
use tempfile::{TempDir, tempdir};

const BRAKE: &str = env!("CARGO_BIN_EXE_brake");

/// The provider contract every case below is checked against.
const CONTRACT: &str = r#"
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
                  status: { type: string }
                  amount:
                    type: object
                    properties:
                      value: { type: integer }
                      currency: { type: string }
  /payments:
    post:
      operationId: createPayment
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
              required: [amount, idempotency_key]
              properties:
                amount:
                  type: object
                  properties:
                    value: { type: integer }
                    currency: { type: string }
                idempotency_key: { type: string }
      responses:
        "201":
          description: created
          content:
            application/json:
              schema:
                type: object
                properties:
                  id: { type: string }
"#;

/// A pact the contract above satisfies completely.
const SATISFIED: &str = r#"{
  "consumer": { "name": "web-checkout" },
  "provider": { "name": "payments" },
  "interactions": [
    {
      "description": "a request for payment 42",
      "request": {
        "method": "GET",
        "path": "/payments/42",
        "headers": { "Accept": "application/json" }
      },
      "response": {
        "status": 200,
        "headers": { "Content-Type": "application/json" },
        "body": {
          "id": "42",
          "status": "PAID",
          "amount": { "value": 100, "currency": "GBP" }
        }
      }
    },
    {
      "description": "creating a payment",
      "request": {
        "method": "POST",
        "path": "/payments",
        "headers": { "Content-Type": "application/json" },
        "body": {
          "amount": { "value": 100, "currency": "GBP" },
          "idempotency_key": "abc"
        }
      },
      "response": {
        "status": 201,
        "headers": { "Content-Type": "application/json" },
        "body": { "id": "42" }
      }
    }
  ],
  "metadata": { "pactSpecification": { "version": "3.0.0" } }
}"#;

fn config_toml(extra: &str) -> String {
    format!(
        "[[contract]]\nname = \"payments\"\nformat = \"openapi\"\n\
         source = \"api/payments-openapi.yaml\"\n\
         baseline = {{ file = \"api/payments-openapi.baseline.yaml\" }}\n{extra}"
    )
}

/// A repository with the contract, its baseline, and whatever else is given.
fn repo(files: &[(&str, &str)]) -> TempDir {
    let repo = tempdir().expect("tempdir");
    let mut all: Vec<(&str, &str)> = vec![
        ("api/payments-openapi.yaml", CONTRACT),
        ("api/payments-openapi.baseline.yaml", CONTRACT),
    ];
    all.extend_from_slice(files);
    for (path, body) in all {
        let full = repo.path().join(path);
        fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
        fs::write(full, body).expect("write");
    }
    repo
}

fn run(repo: &TempDir, scope: Scope) -> Report {
    let config = Config::from_path(&repo.path().join("brake.toml")).expect("brake.toml parses");
    check(repo.path(), &config, &scope, &Options::default())
}

/// Findings for one rule, most useful assertion first.
fn of(report: &Report, rule: &str) -> Vec<Finding> {
    report
        .findings
        .iter()
        .filter(|finding| finding.rule_id == rule)
        .cloned()
        .collect()
}

fn assert_quiet(report: &Report, rule: &str) {
    let found = of(report, rule);
    assert!(
        found.is_empty(),
        "`{rule}` must stay quiet on a contract that satisfies the declaration, got: {:?}",
        found.iter().map(|f| &f.message).collect::<Vec<_>>()
    );
}

fn brake_cli(cwd: &Path, args: &[&str]) -> Output {
    Command::new(BRAKE)
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("brake should launch")
}

fn one_consumer(source: &str) -> String {
    config_toml(&format!(
        "\n[[consumer]]\nformat = \"pact\"\nsource = \"{source}\"\n"
    ))
}

// ── §6.1 expectation ────────────────────────────────────────────────────────

#[test]
fn a_satisfied_pact_produces_no_consumer_finding_at_all() {
    let repo = repo(&[
        ("brake.toml", &one_consumer("pacts/web-checkout.json")),
        ("pacts/web-checkout.json", SATISFIED),
    ]);
    let report = run(&repo, Scope::All);

    let consumer_findings: Vec<_> = report
        .findings
        .iter()
        .filter(|finding| finding.rule_id.starts_with("consumer-"))
        .map(|finding| format!("{}: {}", finding.rule_id, finding.message))
        .collect();
    assert!(
        consumer_findings.is_empty(),
        "a pact the contract fully satisfies must be clean: {consumer_findings:?}"
    );
    assert_eq!(report.exit_code(Severity::Warning), 0);
}

#[test]
fn consumer_endpoint_unmet_fires_on_a_call_the_contract_does_not_document() {
    let pact = SATISFIED.replace("/payments/42", "/refunds/42");
    let repo = repo(&[
        ("brake.toml", &one_consumer("pacts/web-checkout.json")),
        ("pacts/web-checkout.json", &pact),
    ]);
    let report = run(&repo, Scope::All);

    let found = of(&report, "consumer-endpoint-unmet");
    assert_eq!(found.len(), 1, "{:?}", report.findings);
    assert!(
        found[0].message.contains("/refunds/42"),
        "{}",
        found[0].message
    );
    assert_eq!(found[0].severity, Severity::Error);
    assert_eq!(
        found[0].span.as_ref().expect("a span").file,
        "pacts/web-checkout.json",
        "the span points at the interaction, which is the evidence"
    );
}

#[test]
fn consumer_status_unmet_fires_on_a_status_the_contract_does_not_document() {
    let pact = SATISFIED.replace("\"status\": 200,", "\"status\": 404,");
    let repo = repo(&[
        ("brake.toml", &one_consumer("pacts/web-checkout.json")),
        ("pacts/web-checkout.json", &pact),
    ]);
    let report = run(&repo, Scope::All);

    let found = of(&report, "consumer-status-unmet");
    assert_eq!(found.len(), 1, "{:?}", report.findings);
    assert!(found[0].message.contains("404"), "{}", found[0].message);
}

#[test]
fn a_documented_status_class_satisfies_a_consumer_expecting_a_member_of_it() {
    // §3: the expected status is matched against the contract's `4XX` and
    // `default` classes before anything is reported.
    let contract = CONTRACT.replace(
        "      responses:\n        \"200\":",
        "      responses:\n        \"4XX\":\n          description: nope\n        \"200\":",
    );
    let pact = SATISFIED.replace("\"status\": 200,", "\"status\": 404,");
    let repo = repo(&[
        ("api/payments-openapi.yaml", &contract),
        ("api/payments-openapi.baseline.yaml", &contract),
        ("brake.toml", &one_consumer("pacts/web-checkout.json")),
        ("pacts/web-checkout.json", &pact),
    ]);
    assert_quiet(&run(&repo, Scope::All), "consumer-status-unmet");
}

#[test]
fn consumer_field_unmet_fires_on_a_field_the_contract_does_not_produce() {
    let pact = SATISFIED.replace(
        "\"id\": \"42\",\n          \"status\"",
        "\"id\": \"42\",\n          \"customer_id\": \"c-1\",\n          \"status\"",
    );
    let repo = repo(&[
        ("brake.toml", &one_consumer("pacts/web-checkout.json")),
        ("pacts/web-checkout.json", &pact),
    ]);
    let report = run(&repo, Scope::All);

    let found = of(&report, "consumer-field-unmet");
    assert_eq!(found.len(), 1, "{:?}", report.findings);
    assert!(
        found[0].message.contains("customer_id"),
        "{}",
        found[0].message
    );
    assert_eq!(found[0].subject.as_deref(), Some("customer_id"));
    assert_eq!(
        found[0].affects.len(),
        1,
        "a consumer finding names the consumer it is about"
    );
    assert_eq!(found[0].affects[0].consumer, "web-checkout");
}

#[test]
fn consumer_request_rejected_fires_when_a_required_field_is_not_sent() {
    let pact = SATISFIED.replace(",\n          \"idempotency_key\": \"abc\"", "");
    let repo = repo(&[
        ("brake.toml", &one_consumer("pacts/web-checkout.json")),
        ("pacts/web-checkout.json", &pact),
    ]);
    let report = run(&repo, Scope::All);

    let found = of(&report, "consumer-request-rejected");
    assert_eq!(found.len(), 1, "{:?}", report.findings);
    assert!(
        found[0].message.contains("idempotency_key"),
        "{}",
        found[0].message
    );
}

#[test]
fn consumer_request_rejected_fires_when_a_recorded_path_value_stops_being_accepted() {
    // §12.4: binding records that this consumer sends `id=abc`, so narrowing
    // `{id}` to `integer` is authoritative for *this* consumer rather than
    // being reported in the abstract.
    let contract = CONTRACT.replace(
        "        - name: id\n          in: path\n          required: true\n          schema: { type: string }",
        "        - name: id\n          in: path\n          required: true\n          schema: { type: integer }",
    );
    let narrowed = SATISFIED.replace("/payments/42", "/payments/abc");
    let repo = repo(&[
        ("api/payments-openapi.yaml", &contract),
        ("api/payments-openapi.baseline.yaml", &contract),
        ("brake.toml", &one_consumer("pacts/web-checkout.json")),
        ("pacts/web-checkout.json", &narrowed),
    ]);
    let report = run(&repo, Scope::All);

    let found = of(&report, "consumer-request-rejected");
    assert!(
        found
            .iter()
            .any(|finding| finding.message.contains("`abc`")),
        "{:?}",
        found.iter().map(|f| &f.message).collect::<Vec<_>>()
    );
}

#[test]
fn a_numeric_path_value_satisfies_an_integer_parameter() {
    // The negative of the case above: a URL segment is always a string on the
    // wire, so `42` against `type: integer` is satisfied and reporting it
    // would be the false positive that gets a hook uninstalled.
    let contract = CONTRACT.replace(
        "        - name: id\n          in: path\n          required: true\n          schema: { type: string }",
        "        - name: id\n          in: path\n          required: true\n          schema: { type: integer }",
    );
    let repo = repo(&[
        ("api/payments-openapi.yaml", &contract),
        ("api/payments-openapi.baseline.yaml", &contract),
        ("brake.toml", &one_consumer("pacts/web-checkout.json")),
        ("pacts/web-checkout.json", SATISFIED),
    ]);
    assert_quiet(&run(&repo, Scope::All), "consumer-request-rejected");
}

#[test]
fn a_contract_format_does_not_read_as_a_narrowing() {
    // A pact records one value, not a schema: it is silent about formats,
    // bounds and nullability, and comparing its silence as a claim would
    // report a narrowing the consumer never made.
    let contract = CONTRACT.replace(
        "                  id: { type: string }",
        "                  id: { type: string, format: uuid, maxLength: 36 }",
    );
    let repo = repo(&[
        ("api/payments-openapi.yaml", &contract),
        ("api/payments-openapi.baseline.yaml", &contract),
        ("brake.toml", &one_consumer("pacts/web-checkout.json")),
        ("pacts/web-checkout.json", SATISFIED),
    ]);
    let report = run(&repo, Scope::All);
    assert_quiet(&report, "consumer-field-unmet");
    assert_quiet(&report, "consumer-request-rejected");
}

// ── §6.2 integrity ──────────────────────────────────────────────────────────

#[test]
fn consumer_unreachable_fires_when_a_declared_source_is_absent() {
    let repo = repo(&[("brake.toml", &one_consumer("pacts/web-checkout.json"))]);
    let report = run(&repo, Scope::All);

    let found = of(&report, "consumer-unreachable");
    assert_eq!(found.len(), 1, "{:?}", report.findings);
    assert_eq!(found[0].severity, Severity::Error);
    assert_eq!(
        report.exit_code(Severity::Warning),
        1,
        "a declaration brake could not read must never be reported clean"
    );
}

#[test]
fn consumer_unreachable_fires_when_a_declared_source_does_not_parse() {
    let repo = repo(&[
        ("brake.toml", &one_consumer("pacts/web-checkout.json")),
        ("pacts/web-checkout.json", "{ not a pact"),
    ]);
    assert_eq!(of(&run(&repo, Scope::All), "consumer-unreachable").len(), 1);
}

#[test]
fn consumer_partial_names_an_interaction_that_could_not_be_modelled() {
    let pact = r#"{
  "consumer": { "name": "reporting" },
  "provider": { "name": "payments" },
  "interactions": [
    { "type": "Asynchronous/Messages", "description": "a payment settled event" }
  ]
}"#;
    let repo = repo(&[
        ("brake.toml", &one_consumer("pacts/reporting.json")),
        ("pacts/reporting.json", pact),
    ]);
    let report = run(&repo, Scope::All);

    let found = of(&report, "consumer-partial");
    assert_eq!(found.len(), 1, "{:?}", report.findings);
    assert!(
        found[0].message.contains("Asynchronous"),
        "the construct must be named, not reported anonymously: {}",
        found[0].message
    );
    assert_eq!(found[0].severity, Severity::Warning);
}

#[test]
fn a_modelled_pact_is_not_reported_as_partial() {
    let repo = repo(&[
        ("brake.toml", &one_consumer("pacts/web-checkout.json")),
        ("pacts/web-checkout.json", SATISFIED),
    ]);
    assert_quiet(&run(&repo, Scope::All), "consumer-partial");
}

#[test]
fn consumer_path_ambiguous_declines_to_guess() {
    let contract = CONTRACT.replace(
        "  /payments:\n",
        "  /payments/{kind}:\n    get:\n      operationId: getByKind\n      responses:\n        \"200\":\n          description: ok\n          content:\n            application/json:\n              schema: { type: object }\n  /payments:\n",
    );
    let repo = repo(&[
        ("api/payments-openapi.yaml", &contract),
        ("api/payments-openapi.baseline.yaml", &contract),
        ("brake.toml", &one_consumer("pacts/web-checkout.json")),
        ("pacts/web-checkout.json", SATISFIED),
    ]);
    let report = run(&repo, Scope::All);

    let found = of(&report, "consumer-path-ambiguous");
    assert_eq!(found.len(), 1, "{:?}", report.findings);
    assert!(
        found[0].message.contains("/payments/{id}")
            && found[0].message.contains("/payments/{kind}"),
        "both candidates must be named: {}",
        found[0].message
    );
    assert!(
        of(&report, "consumer-field-unmet").is_empty(),
        "an unbound path must not also be attributed to a guessed endpoint"
    );
}

#[test]
fn an_unambiguous_path_is_bound_without_complaint() {
    let repo = repo(&[
        ("brake.toml", &one_consumer("pacts/web-checkout.json")),
        ("pacts/web-checkout.json", SATISFIED),
    ]);
    assert_quiet(&run(&repo, Scope::All), "consumer-path-ambiguous");
}

#[test]
fn consumer_provider_unmatched_fires_on_a_provider_no_contract_declares() {
    let pact = SATISFIED.replace("\"name\": \"payments\"", "\"name\": \"ledger\"");
    let repo = repo(&[
        ("brake.toml", &one_consumer("pacts/web-checkout.json")),
        ("pacts/web-checkout.json", &pact),
    ]);
    let report = run(&repo, Scope::All);

    let found = of(&report, "consumer-provider-unmatched");
    assert_eq!(found.len(), 1, "{:?}", report.findings);
    assert!(found[0].message.contains("ledger"), "{}", found[0].message);
}

#[test]
fn a_matching_provider_is_not_reported() {
    let repo = repo(&[
        ("brake.toml", &one_consumer("pacts/web-checkout.json")),
        ("pacts/web-checkout.json", SATISFIED),
    ]);
    assert_quiet(&run(&repo, Scope::All), "consumer-provider-unmatched");
}

#[test]
fn consumer_undeclared_is_identified_by_parsing_not_by_filename() {
    let repo = repo(&[
        ("brake.toml", &config_toml("")),
        ("pacts/web-checkout.json", SATISFIED),
        // Neither of these is a consumer declaration, and a filename
        // heuristic would call at least one of them one.
        ("pacts/fixtures.json", r#"{"pact": "not really", "a": 1}"#),
        ("consumers/notes.toml", "title = \"who uses what\"\n"),
    ]);
    let report = run(&repo, Scope::All);

    let found = of(&report, "consumer-undeclared");
    assert_eq!(
        found.len(),
        1,
        "only the file that actually parses as a declaration: {:?}",
        found.iter().map(|f| &f.message).collect::<Vec<_>>()
    );
    assert_eq!(found[0].path.as_deref(), Some("pacts/web-checkout.json"));
    assert_eq!(found[0].severity, Severity::Info);
}

#[test]
fn a_declared_consumer_is_not_also_reported_as_undeclared() {
    let repo = repo(&[
        ("brake.toml", &one_consumer("pacts/web-checkout.json")),
        ("pacts/web-checkout.json", SATISFIED),
    ]);
    assert_quiet(&run(&repo, Scope::All), "consumer-undeclared");
}

// ── §6.3 advisory ───────────────────────────────────────────────────────────

#[test]
fn consumer_surface_unused_needs_an_explicit_closed_world_declaration() {
    let open = repo(&[
        ("brake.toml", &one_consumer("pacts/one-endpoint.json")),
        ("pacts/one-endpoint.json", &only_the_get()),
    ]);
    assert_quiet(&run(&open, Scope::All), "consumer-surface-unused");

    let closed = repo(&[
        (
            "brake.toml",
            &format!(
                "{}\n[consumers]\ncompleteness = \"closed-world\"\n",
                one_consumer("pacts/one-endpoint.json")
            ),
        ),
        ("pacts/one-endpoint.json", &only_the_get()),
    ]);
    let report = run(&closed, Scope::All);
    let found = of(&report, "consumer-surface-unused");
    assert_eq!(found.len(), 1, "{:?}", report.findings);
    assert!(
        found[0].message.contains("POST /payments"),
        "{}",
        found[0].message
    );
}

#[test]
fn consumer_surface_unused_never_fires_on_a_scoped_check() {
    // The thesis forbids reporting a suspected absence at commit time.
    let repo = repo(&[
        (
            "brake.toml",
            &format!(
                "{}\n[consumers]\ncompleteness = \"closed-world\"\n",
                one_consumer("pacts/one-endpoint.json")
            ),
        ),
        ("pacts/one-endpoint.json", &only_the_get()),
    ]);
    let report = run(
        &repo,
        Scope::Paths(vec![PathBuf::from("api/payments-openapi.yaml")]),
    );
    assert_quiet(&report, "consumer-surface-unused");
}

fn only_the_get() -> String {
    let end = SATISFIED
        .find("    {\n      \"description\": \"creating a payment\"")
        .expect("the second interaction");
    format!(
        "{}\n  ],\n  \"metadata\": {{ \"pactSpecification\": {{ \"version\": \"3.0.0\" }} }}\n}}",
        SATISFIED[..end].trim_end().trim_end_matches(',')
    )
}

// ── §6.4 there is no consumer-break rule ────────────────────────────────────

#[test]
fn attribution_is_evidence_on_a_finding_rather_than_a_second_finding() {
    assert!(
        brake::rules::catalogue::lookup("consumer-break").is_none(),
        "one broken field must not produce one break plus one attribution: §6.4"
    );

    let head = CONTRACT.replace("                  status: { type: string }\n", "");
    let repo = repo(&[
        ("api/payments-openapi.yaml", &head),
        ("brake.toml", &one_consumer("pacts/web-checkout.json")),
        ("pacts/web-checkout.json", SATISFIED),
    ]);
    let report = run(&repo, Scope::All);

    let removed = of(&report, "response-field-removed");
    assert_eq!(
        removed.len(),
        1,
        "one problem, one finding: {:?}",
        report
            .findings
            .iter()
            .map(|f| f.rule_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(removed[0].affects.len(), 1);
    assert_eq!(removed[0].affects[0].consumer, "web-checkout");
    assert_eq!(
        removed[0].affects[0].source, "pacts/web-checkout.json",
        "the attribution carries the interaction, not the contract"
    );
    assert!(removed[0].affects[0].span.line > 1);
}

#[test]
fn a_field_no_declared_consumer_reads_is_not_attributed() {
    let contract = CONTRACT.replace(
        "                  id: { type: string }",
        "                  id: { type: string }\n                  internal_note: { type: string }",
    );
    let repo = repo(&[
        ("api/payments-openapi.baseline.yaml", &contract),
        ("brake.toml", &one_consumer("pacts/web-checkout.json")),
        ("pacts/web-checkout.json", SATISFIED),
    ]);
    let report = run(&repo, Scope::All);

    let removed = of(&report, "response-field-removed");
    assert!(!removed.is_empty(), "{:?}", report.findings);
    assert!(
        removed
            .iter()
            .all(|finding| finding.subject.as_deref() == Some("internal_note")),
        "{removed:?}"
    );
    assert!(
        removed.iter().all(|finding| finding.affects.is_empty()),
        "no declared consumer reads it, and saying otherwise would be an invented claim"
    );
}

// ── §7.1 the three policies ─────────────────────────────────────────────────

/// A repository where `status` is removed and `web-checkout` reads it.
fn with_policy(policy: &str, completeness: &str) -> TempDir {
    let head = CONTRACT.replace("                  status: { type: string }\n", "");
    repo(&[
        ("api/payments-openapi.yaml", &head),
        (
            "brake.toml",
            &format!(
                "{}\n[consumers]\npolicy = \"{policy}\"\ncompleteness = \"{completeness}\"\n",
                one_consumer("pacts/web-checkout.json")
            ),
        ),
        ("pacts/web-checkout.json", SATISFIED),
    ])
}

#[test]
fn annotate_names_the_consumer_and_changes_nothing_else() {
    let repo = with_policy("annotate", "open-world");
    let report = run(&repo, Scope::All);
    let removed = of(&report, "response-field-removed");
    assert_eq!(removed[0].severity, Severity::Error);
    assert!(removed[0].note.is_none());
    assert_eq!(removed[0].affects.len(), 1);
}

#[test]
fn escalate_raises_a_warning_a_declared_consumer_is_affected_by() {
    // `param-removed` is a warning precisely because brake could not tell
    // whether anyone relied on it. With a declaration, it can.
    let head = CONTRACT.replace(
        "      parameters:\n        - name: id\n          in: path\n          required: true\n          schema: { type: string }\n",
        "",
    );
    let repo = repo(&[
        ("api/payments-openapi.yaml", &head),
        (
            "brake.toml",
            &format!(
                "{}\n[consumers]\npolicy = \"escalate\"\n",
                one_consumer("pacts/web-checkout.json")
            ),
        ),
        ("pacts/web-checkout.json", SATISFIED),
    ]);
    let report = run(&repo, Scope::All);
    let removed = of(&report, "param-removed");
    assert_eq!(removed.len(), 1, "{:?}", report.findings);
    assert_eq!(removed[0].severity, Severity::Error);
    assert!(
        removed[0]
            .note
            .as_deref()
            .is_some_and(|note| note.contains("web-checkout")),
        "{:?}",
        removed[0].note
    );
}

#[test]
fn triage_downgrades_only_what_it_is_allowed_to_and_prints_its_assumption() {
    let contract = CONTRACT.replace(
        "                  id: { type: string }",
        "                  id: { type: string }\n                  internal_note: { type: string }",
    );
    let repo = repo(&[
        ("api/payments-openapi.baseline.yaml", &contract),
        (
            "brake.toml",
            &format!(
                "{}\n[consumers]\npolicy = \"triage\"\ncompleteness = \"closed-world\"\n",
                one_consumer("pacts/web-checkout.json")
            ),
        ),
        ("pacts/web-checkout.json", SATISFIED),
    ]);
    let report = run(&repo, Scope::All);

    let removed = of(&report, "response-field-removed");
    assert!(!removed.is_empty(), "{:?}", report.findings);
    assert!(
        removed
            .iter()
            .all(|finding| finding.severity == Severity::Warning),
        "the floor is warning: nothing is downgraded to nothing, and nothing below it"
    );
    let note = removed[0].note.as_deref().expect("the assumption");
    assert!(note.contains("1 consumer declared"), "{note}");
    assert!(note.contains("cannot know that is all of them"), "{note}");
}

#[test]
fn triage_leaves_a_break_a_declared_consumer_is_affected_by_alone() {
    let repo = with_policy("triage", "closed-world");
    let report = run(&repo, Scope::All);
    let removed = of(&report, "response-field-removed");
    assert_eq!(removed[0].severity, Severity::Error);
    assert!(removed[0].note.is_none());
}

// ── §8 determinism and hermeticity ──────────────────────────────────────────
//
// The two tests §8 adds to the self-defence set — a pact full of URLs opening
// no socket, and an absent declaration exiting 1 — live in
// `tests/self_defence.rs` with the other five. They defend guarantees rather
// than rules, and splitting the set across two files is how one of them gets
// quietly deleted.

#[test]
fn g3_glob_expansion_and_interaction_order_do_not_move_the_verdict() {
    let repo = repo(&[
        (
            "brake.toml",
            &config_toml(
                "\n[[consumer]]\nformat = \"pact\"\nsource = \"services/*/pacts/*-payments.json\"\n",
            ),
        ),
        ("services/billing/pacts/billing-payments.json", SATISFIED),
        (
            "services/checkout/pacts/checkout-payments.json",
            &SATISFIED.replace("web-checkout", "checkout"),
        ),
    ]);

    let first = brake::render::json::render(&run(&repo, Scope::All));
    let second = brake::render::json::render(&run(&repo, Scope::All));
    assert_eq!(first, second, "guarantee G4 over the demand axis");

    // Both declarations were used, not just whichever the directory listed
    // first.
    let inventory = brake::demand::inventory::build(
        repo.path(),
        &Config::from_path(&repo.path().join("brake.toml")).expect("config"),
        &[],
        &[],
    );
    let names: BTreeSet<&str> = inventory.contracts[0]
        .consumers
        .iter()
        .map(|consumer| consumer.consumer.as_str())
        .collect();
    assert_eq!(names, BTreeSet::from(["checkout", "web-checkout"]));
}

// ── §9 interface ────────────────────────────────────────────────────────────

#[test]
fn brake_consumers_reports_what_the_verdict_rested_on_and_never_gates() {
    let repo = repo(&[
        ("brake.toml", &one_consumer("pacts/web-checkout.json")),
        ("pacts/web-checkout.json", SATISFIED),
    ]);
    let output = brake_cli(repo.path(), &["consumers", "--format", "text"]);
    let text = String::from_utf8_lossy(&output.stdout).into_owned();

    assert_eq!(output.status.code(), Some(0), "`consumers` never gates");
    assert!(
        text.contains("payments — api/payments-openapi.yaml"),
        "{text}"
    );
    assert!(text.contains("web-checkout"), "{text}");
    assert!(
        text.contains("sha256:"),
        "a digest is how a human sees what the verdict rested on: {text}"
    );
    assert!(
        text.contains("GET  /payments/{id}") || text.contains("GET  /payments/{id}"),
        "{text}"
    );
    assert!(
        text.contains("2 of 2 endpoints have a declared consumer."),
        "{text}"
    );
    assert!(
        text.contains("brake knows about the consumers declared in brake.toml and no others."),
        "without this the inventory reads as a complete census: {text}"
    );
}

#[test]
fn brake_consumers_filters_by_name() {
    let repo = repo(&[
        (
            "brake.toml",
            &config_toml(
                "\n[[consumer]]\nformat = \"pact\"\nsource = \"pacts/web-checkout.json\"\n\
                 \n[[consumer]]\nname = \"reporting\"\nformat = \"manifest\"\n\
                 source = \"consumers/reporting.brake-uses.toml\"\nprovider = \"payments\"\n",
            ),
        ),
        ("pacts/web-checkout.json", SATISFIED),
        (
            "consumers/reporting.brake-uses.toml",
            "consumer = \"reporting\"\nprovider = \"payments\"\n\n\
             [[uses]]\nendpoint = \"GET /payments/{id}\"\nstatuses = [\"200\"]\n\
             reads = [\"id\", \"amount.currency\"]\n",
        ),
    ]);

    let all = String::from_utf8_lossy(&brake_cli(repo.path(), &["consumers"]).stdout).into_owned();
    assert!(
        all.contains("web-checkout") && all.contains("reporting"),
        "{all}"
    );

    let one = String::from_utf8_lossy(
        &brake_cli(repo.path(), &["consumers", "--consumer", "reporting"]).stdout,
    )
    .into_owned();
    assert!(one.contains("reporting"), "{one}");
    assert!(!one.contains("web-checkout"), "{one}");
}

#[test]
fn a_path_scope_naming_a_pact_selects_the_contract_it_constrains() {
    let head = CONTRACT.replace("                  status: { type: string }\n", "");
    let repo = repo(&[
        ("api/payments-openapi.yaml", &head),
        ("brake.toml", &one_consumer("pacts/web-checkout.json")),
        ("pacts/web-checkout.json", SATISFIED),
    ]);

    let output = brake_cli(
        repo.path(),
        &["check", "pacts/web-checkout.json", "--format", "json"],
    );
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        text.contains("response-field-removed"),
        "a hook run on a pact-updating commit has to verify the contract it constrains: {text}"
    );
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn the_consumer_flag_restricts_the_run() {
    let broken = SATISFIED.replace("web-checkout", "noisy").replace(
        "\"status\": \"PAID\",",
        "\"status\": \"PAID\", \"nope\": 1,",
    );
    let repo = repo(&[
        (
            "brake.toml",
            &config_toml(
                "\n[[consumer]]\nformat = \"pact\"\nsource = \"pacts/web-checkout.json\"\n\
                 \n[[consumer]]\nformat = \"pact\"\nsource = \"pacts/noisy.json\"\n",
            ),
        ),
        ("pacts/web-checkout.json", SATISFIED),
        ("pacts/noisy.json", &broken),
    ]);

    let config = Config::from_path(&repo.path().join("brake.toml")).expect("config");
    let scoped = check(
        repo.path(),
        &config,
        &Scope::All,
        &Options {
            consumers: vec!["web-checkout".to_owned()],
            ..Options::default()
        },
    );
    assert_quiet(&scoped, "consumer-field-unmet");

    let everything = check(repo.path(), &config, &Scope::All, &Options::default());
    assert_eq!(of(&everything, "consumer-field-unmet").len(), 1);
}

#[test]
fn the_text_renderer_names_who_breaks_and_where_they_said_so() {
    let repo = with_policy("annotate", "open-world");
    let report = run(&repo, Scope::All);
    let text = brake::render::text::render(&report);
    assert!(
        text.contains("breaks web-checkout — pacts/web-checkout.json:"),
        "{text}"
    );
}

#[test]
fn json_and_sarif_carry_the_attribution_too() {
    let repo = with_policy("annotate", "open-world");
    let report = run(&repo, Scope::All);

    let json: serde_json::Value =
        serde_json::from_str(&brake::render::json::render(&report)).expect("valid JSON");
    let removed = json["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .find(|finding| finding["rule"] == "response-field-removed")
        .expect("the removal");
    assert_eq!(removed["affects"][0]["consumer"], "web-checkout");
    assert_eq!(removed["affects"][0]["source"], "pacts/web-checkout.json");
    assert!(
        removed["affects"][0]["line"]
            .as_u64()
            .is_some_and(|line| line > 1)
    );

    let sarif: serde_json::Value =
        serde_json::from_str(&brake::render::sarif::render(&report)).expect("valid SARIF");
    let result = sarif["runs"][0]["results"]
        .as_array()
        .expect("results")
        .iter()
        .find(|result| result["ruleId"] == "response-field-removed")
        .expect("the removal");
    assert_eq!(
        result["relatedLocations"][0]["physicalLocation"]["artifactLocation"]["uri"],
        "pacts/web-checkout.json",
        "SARIF related locations are exactly what this is for"
    );
    assert!(
        result["partialFingerprints"]["brakeFindingV1"].is_string(),
        "attribution must not disturb the fingerprint, or every finding re-alerts"
    );
}

#[test]
fn attribution_does_not_move_the_fingerprint() {
    let without = {
        let head = CONTRACT.replace("                  status: { type: string }\n", "");
        let repo = repo(&[
            ("api/payments-openapi.yaml", &head),
            ("brake.toml", &config_toml("")),
        ]);
        run(&repo, Scope::All)
    };
    let with = run(&with_policy("annotate", "open-world"), Scope::All);

    let fingerprint = |report: &Report| {
        let value: serde_json::Value =
            serde_json::from_str(&brake::render::sarif::render(report)).expect("valid SARIF");
        value["runs"][0]["results"]
            .as_array()
            .expect("results")
            .iter()
            .find(|result| result["ruleId"] == "response-field-removed")
            .expect("the removal")["partialFingerprints"]["brakeFindingV1"]
            .clone()
    };
    assert_eq!(
        fingerprint(&without),
        fingerprint(&with),
        "attribution appearing on an existing finding must not re-alert"
    );
}

// ── M14: beyond pact ────────────────────────────────────────────────────────

const SCHEMA: &str = r#"
type Query {
  payment(id: ID!): Payment!
}
type Payment {
  id: ID!
  status: String
}
"#;

#[test]
fn a_graphql_operation_document_produces_the_same_shape_of_finding() {
    // The proof that the demand model generalised rather than encoding pact's
    // shape under another name: the same join, no format-specific branch.
    let repo = tempdir().expect("tempdir");
    for (path, body) in [
        ("api/schema.graphql", SCHEMA),
        ("api/schema.baseline.graphql", SCHEMA),
        (
            "brake.toml",
            "[[contract]]\nname = \"payments\"\nformat = \"graphql\"\n\
             source = \"api/schema.graphql\"\n\
             baseline = { file = \"api/schema.baseline.graphql\" }\n\
             \n[[consumer]]\nformat = \"graphql-operations\"\n\
             source = \"services/web-checkout/queries.graphql\"\nprovider = \"payments\"\n",
        ),
        (
            "services/web-checkout/queries.graphql",
            "query PaymentById($id: ID!) { payment(id: $id) { id status settledAt } }",
        ),
    ] {
        let full = repo.path().join(path);
        fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
        fs::write(full, body).expect("write");
    }

    let config = Config::from_path(&repo.path().join("brake.toml")).expect("config");
    let report = check(repo.path(), &config, &Scope::All, &Options::default());

    let found: Vec<_> = report
        .findings
        .iter()
        .filter(|finding| finding.rule_id == "consumer-field-unmet")
        .collect();
    assert_eq!(found.len(), 1, "{:?}", report.findings);
    assert_eq!(found[0].subject.as_deref(), Some("settledAt"));
    assert_eq!(found[0].affects[0].consumer, "web-checkout");
    assert_eq!(
        found[0].span.as_ref().expect("a span").file,
        "services/web-checkout/queries.graphql"
    );
}

#[test]
fn a_graphql_selection_the_schema_satisfies_is_quiet() {
    let repo = tempdir().expect("tempdir");
    for (path, body) in [
        ("api/schema.graphql", SCHEMA),
        ("api/schema.baseline.graphql", SCHEMA),
        (
            "brake.toml",
            "[[contract]]\nname = \"payments\"\nformat = \"graphql\"\n\
             source = \"api/schema.graphql\"\n\
             baseline = { file = \"api/schema.baseline.graphql\" }\n\
             \n[[consumer]]\nformat = \"graphql-operations\"\n\
             source = \"services/web-checkout/queries.graphql\"\nprovider = \"payments\"\n",
        ),
        (
            "services/web-checkout/queries.graphql",
            "query PaymentById($id: ID!) { payment(id: $id) { id status } }",
        ),
    ] {
        let full = repo.path().join(path);
        fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
        fs::write(full, body).expect("write");
    }

    let config = Config::from_path(&repo.path().join("brake.toml")).expect("config");
    let report = check(repo.path(), &config, &Scope::All, &Options::default());
    assert!(
        report
            .findings
            .iter()
            .all(|finding| !finding.rule_id.starts_with("consumer-")),
        "{:?}",
        report
            .findings
            .iter()
            .map(|finding| &finding.message)
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_manifest_reaches_the_same_join_as_a_pact() {
    let repo = repo(&[
        (
            "brake.toml",
            &config_toml(
                "\n[[consumer]]\nname = \"reporting\"\nformat = \"manifest\"\n\
                 source = \"consumers/reporting.brake-uses.toml\"\nprovider = \"payments\"\n",
            ),
        ),
        (
            "consumers/reporting.brake-uses.toml",
            "consumer = \"reporting\"\nprovider = \"payments\"\n\n\
             [[uses]]\nendpoint = \"GET /payments/{id}\"\nstatuses = [\"200\"]\n\
             reads = [\"id\", \"amount.currency\", \"settled_at\"]\n",
        ),
    ]);
    let report = run(&repo, Scope::All);

    let found = of(&report, "consumer-field-unmet");
    assert_eq!(found.len(), 1, "{:?}", report.findings);
    assert_eq!(found[0].subject.as_deref(), Some("settled_at"));
    assert_eq!(found[0].affects[0].consumer, "reporting");
}

// ── configuration ───────────────────────────────────────────────────────────

#[test]
fn a_consumer_source_that_is_a_url_is_refused_at_parse_time() {
    let error = Config::parse(
        "[[contract]]\nname=\"p\"\nformat=\"openapi\"\nsource=\"a.yaml\"\n\
         [[consumer]]\nformat=\"pact\"\nsource=\"https://broker.example.com/pacts/x.json\"\n",
    )
    .expect_err("a URL is a configuration error, not a fetch");
    assert!(error.to_string().contains("never fetches"), "{error}");
}

#[test]
fn the_consumers_block_parses_both_knobs() {
    let config = Config::parse(
        "[[contract]]\nname=\"p\"\nformat=\"openapi\"\nsource=\"a.yaml\"\n\
         [[consumer]]\nformat=\"pact\"\nsource=\"pacts/x.json\"\n\
         [consumers]\npolicy = \"escalate\"\ncompleteness = \"closed-world\"\n",
    )
    .expect("the documented spelling must parse");
    assert_eq!(
        config.consumer_options.policy,
        brake::config::ConsumerPolicy::Escalate
    );
    assert_eq!(
        config.consumer_options.completeness,
        brake::config::Completeness::ClosedWorld
    );
    assert_eq!(config.consumers.len(), 1);
    assert_eq!(config.consumers[0].name, None, "a pact names itself");
}

#[test]
fn every_consumer_rule_in_the_design_is_in_the_catalogue() {
    for id in [
        "consumer-endpoint-unmet",
        "consumer-status-unmet",
        "consumer-field-unmet",
        "consumer-request-rejected",
        "consumer-unreachable",
        "consumer-partial",
        "consumer-path-ambiguous",
        "consumer-provider-unmatched",
        "consumer-undeclared",
        "consumer-surface-unused",
    ] {
        let rule = brake::rules::catalogue::lookup(id)
            .unwrap_or_else(|| panic!("`{id}` is specified in design/05-consumer-demand.md §6"));
        assert!(!rule.explanation.trim().is_empty());
        assert!(
            !rule.observable_by_demand,
            "`{id}` is already about demand; downgrading it on demand's silence is nonsense"
        );
    }
}

// ── a schemaless response ───────────────────────────────────────────────────

/// The contract above, with a documented `404` that carries no schema.
fn with_a_schemaless_404() -> String {
    CONTRACT.replace(
        "  /payments:\n",
        "        \"404\":\n          description: no such payment\n  /payments:\n",
    )
}

/// One GET interaction expecting a 404, with whatever body is given.
fn pact_expecting_404(body: &str) -> String {
    format!(
        "{{\n  \"consumer\": {{ \"name\": \"web-checkout\" }},\n  \
         \"provider\": {{ \"name\": \"payments\" }},\n  \"interactions\": [\n    {{\n      \
         \"description\": \"a payment that does not exist\",\n      \
         \"request\": {{ \"method\": \"GET\", \"path\": \"/payments/42\" }},\n      \
         \"response\": {{ \"status\": 404{body} }}\n    }}\n  ]\n}}"
    )
}

#[test]
fn a_consumer_that_recorded_no_body_is_satisfied_by_a_schemaless_response() {
    // A 404 interaction with no body is the most common thing in any pact
    // directory. It declares that the call happens and that the status comes
    // back; warning that its payload was "not verified" would be noise on
    // every one of them.
    let contract = with_a_schemaless_404();
    let repo = repo(&[
        ("api/payments-openapi.yaml", &contract),
        ("api/payments-openapi.baseline.yaml", &contract),
        ("brake.toml", &one_consumer("pacts/web-checkout.json")),
        ("pacts/web-checkout.json", &pact_expecting_404("")),
    ]);
    let report = run(&repo, Scope::All);
    assert_quiet(&report, "consumer-partial");
    assert_quiet(&report, "consumer-status-unmet");
}

#[test]
fn a_consumer_that_did_record_a_body_is_told_the_response_has_no_schema() {
    // The other half: brake cannot verify what it cannot see, and reporting
    // clean here would be reporting a verification that did not happen.
    let contract = with_a_schemaless_404();
    let repo = repo(&[
        ("api/payments-openapi.yaml", &contract),
        ("api/payments-openapi.baseline.yaml", &contract),
        ("brake.toml", &one_consumer("pacts/web-checkout.json")),
        (
            "pacts/web-checkout.json",
            &pact_expecting_404(", \"body\": { \"error\": \"not found\" }"),
        ),
    ]);
    let found = of(&run(&repo, Scope::All), "consumer-partial");
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].message.contains("no schema"),
        "{}",
        found[0].message
    );
}

#[test]
fn only_an_error_says_breaks() {
    // A warning that a consumer declared something is not a break, and the
    // renderer must not say it is: overstating one finding teaches a team to
    // discount the rest.
    let pact = r#"{
  "consumer": { "name": "reporting" },
  "provider": { "name": "payments" },
  "interactions": [
    { "type": "Asynchronous/Messages", "description": "a payment settled event" }
  ]
}"#;
    let repo = repo(&[
        ("brake.toml", &one_consumer("pacts/reporting.json")),
        ("pacts/reporting.json", pact),
    ]);
    let text = brake::render::text::render(&run(&repo, Scope::All));
    assert!(
        text.contains("declared by reporting — pacts/reporting.json:"),
        "{text}"
    );
    assert!(!text.contains("breaks reporting"), "{text}");
}

#[test]
fn every_distinct_path_value_a_consumer_sends_is_checked() {
    // Two interactions calling `/payments/1` and `/payments/abc` have declared
    // two things. Recording only the first would let a narrowing that rejects
    // the second pass unnoticed.
    let contract = CONTRACT.replace(
        "        - name: id\n          in: path\n          required: true\n          schema: { type: string }",
        "        - name: id\n          in: path\n          required: true\n          schema: { type: integer }",
    );
    let pact = SATISFIED.replace(
        "\"description\": \"creating a payment\",\n      \"request\": {\n        \"method\": \"POST\",\n        \"path\": \"/payments\",",
        "\"description\": \"a second payment\",\n      \"request\": {\n        \"method\": \"GET\",\n        \"path\": \"/payments/abc\",",
    );
    let repo = repo(&[
        ("api/payments-openapi.yaml", &contract),
        ("api/payments-openapi.baseline.yaml", &contract),
        ("brake.toml", &one_consumer("pacts/web-checkout.json")),
        ("pacts/web-checkout.json", &pact),
    ]);
    let found = of(&run(&repo, Scope::All), "consumer-request-rejected");
    assert!(
        found
            .iter()
            .any(|finding| finding.message.contains("`abc`")),
        "the second interaction's value must be checked too: {:?}",
        found.iter().map(|f| &f.message).collect::<Vec<_>>()
    );
    assert!(
        !found.iter().any(|finding| finding.message.contains("`42`")),
        "and `42` satisfies `integer`, so it must not be reported: {:?}",
        found.iter().map(|f| &f.message).collect::<Vec<_>>()
    );
}
