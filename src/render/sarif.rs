//! SARIF 2.1.0, for GitHub Code Scanning.
//!
//! `partialFingerprints` are the reason this format is worth emitting at all:
//! without them GitHub re-alerts on every commit, and a gate that cries wolf
//! on every push gets muted. The fingerprint derives from
//! `rule + contract + method + path + pointer`, none of which move when an
//! unrelated line above the finding changes.

use std::collections::BTreeSet;

use serde_json::json;

use crate::report::Report;
use crate::rules::{Finding, catalogue};

pub fn render(report: &Report) -> String {
    // Every rule that appears, plus `contract-unreachable` for the
    // unavailable entries below, so no result references an undeclared rule.
    let mut rule_ids = report
        .findings
        .iter()
        .map(|finding| finding.rule_id)
        .collect::<BTreeSet<_>>();
    if !report.unavailable.is_empty() {
        rule_ids.insert("contract-unreachable");
    }

    let rules = rule_ids
        .iter()
        .filter_map(|id| catalogue::lookup(id))
        .map(|rule| {
            json!({
                "id": rule.id,
                "name": rule.id,
                "shortDescription": { "text": rule.summary },
                "fullDescription": { "text": rule.explanation },
                "help": { "text": rule.explanation },
                "helpUri": rule.help_uri(),
                "defaultConfiguration": { "level": sarif_level(rule.severity) },
            })
        })
        .collect::<Vec<_>>();

    let mut results = report
        .findings
        .iter()
        .map(|finding| {
            let mut result = json!({
                "ruleId": finding.rule_id,
                "level": sarif_level(finding.severity),
                "message": { "text": finding.message },
                "partialFingerprints": {
                    "brakeFindingV1": fingerprint(finding),
                },
                "properties": {
                    "contract": finding.contract,
                },
            });

            if let Some(span) = &finding.span {
                result["locations"] = json!([{
                    "physicalLocation": {
                        "artifactLocation": { "uri": span.file },
                        "region": {
                            "startLine": span.line,
                            "startColumn": span.column,
                        }
                    }
                }]);
            }

            result
        })
        .collect::<Vec<_>>();

    for unavailable in &report.unavailable {
        results.push(json!({
            "ruleId": "contract-unreachable",
            "level": "error",
            "message": { "text": unavailable.message },
            "partialFingerprints": {
                "brakeFindingV1": stable_hash(&format!(
                    "contract-unreachable|{}",
                    unavailable.contract.as_deref().unwrap_or("")
                )),
            },
        }));
    }

    let payload = json!({
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {
                "driver": {
                    "name": "brake",
                    "informationUri": "https://github.com/grahambrooks/brake",
                    "semanticVersion": crate::VERSION,
                    "version": crate::VERSION,
                    "rules": rules,
                }
            },
            "results": results,
        }]
    });

    format!("{payload}\n")
}

/// Identity for a finding across commits.
///
/// Deliberately excludes the line number: a finding that has not changed but
/// has moved down the file is the same finding, and re-alerting on it is the
/// behaviour that makes people turn the integration off.
fn fingerprint(finding: &Finding) -> String {
    stable_hash(&format!(
        "{}|{}|{}|{}|{}",
        finding.rule_id,
        finding.contract,
        finding.method.as_deref().unwrap_or(""),
        finding.path.as_deref().unwrap_or(""),
        finding.pointer,
    ))
}

/// FNV-1a. Not cryptographic, and does not need to be: this is an identity for
/// deduplication, and a stable well-defined algorithm matters more than a
/// strong one. `DefaultHasher` is explicitly not stable across releases.
fn stable_hash(input: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in input.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn sarif_level(severity: crate::Severity) -> &'static str {
    match severity {
        crate::Severity::Error => "error",
        crate::Severity::Warning => "warning",
        crate::Severity::Info => "note",
    }
}

#[cfg(test)]
mod tests {
    use crate::Severity;
    use crate::contract::Span;
    use crate::report::{Report, Unavailable};
    use crate::rules::Finding;

    use super::render;

    fn finding(line: usize) -> Finding {
        Finding {
            rule_id: "response-field-removed",
            severity: Severity::Error,
            contract: "payments".to_owned(),
            message: "response field removed: field `customer_id`".to_owned(),
            method: Some("GET".to_owned()),
            path: Some("/payments/{id}".to_owned()),
            pointer: "/paths/~1payments~1{id}/get/responses/200/customer_id".to_owned(),
            span: Some(Span::new("api/openapi.yaml", line, 9, "/paths")),
        }
    }

    fn parsed(report: &Report) -> serde_json::Value {
        serde_json::from_str(&render(report)).expect("valid SARIF JSON")
    }

    #[test]
    fn emits_a_valid_looking_sarif_envelope() {
        let value = parsed(&Report::new(vec![finding(12)], Vec::new(), 1));
        assert_eq!(value["version"], "2.1.0");
        assert_eq!(value["runs"][0]["tool"]["driver"]["name"], "brake");
        assert_eq!(
            value["runs"][0]["results"][0]["ruleId"],
            "response-field-removed"
        );
        assert_eq!(
            value["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["artifactLocation"]
                ["uri"],
            "api/openapi.yaml"
        );
    }

    #[test]
    fn every_rule_a_result_references_is_declared_with_a_help_uri() {
        let report = Report::new(
            vec![finding(12)],
            vec![Unavailable {
                contract: Some("ledger".to_owned()),
                message: "baseline missing".to_owned(),
            }],
            2,
        );
        let value = parsed(&report);
        let declared = value["runs"][0]["tool"]["driver"]["rules"]
            .as_array()
            .expect("rules array")
            .iter()
            .map(|rule| rule["id"].as_str().expect("id").to_owned())
            .collect::<Vec<_>>();

        for result in value["runs"][0]["results"].as_array().expect("results") {
            let rule_id = result["ruleId"].as_str().expect("ruleId").to_owned();
            assert!(
                declared.contains(&rule_id),
                "result references undeclared rule `{rule_id}`"
            );
        }
        for rule in value["runs"][0]["tool"]["driver"]["rules"]
            .as_array()
            .expect("rules")
        {
            assert!(
                rule["helpUri"]
                    .as_str()
                    .is_some_and(|uri| uri.starts_with("https://")),
                "every reportingDescriptor needs a helpUri"
            );
        }
    }

    #[test]
    fn every_result_carries_a_partial_fingerprint() {
        let report = Report::new(
            vec![finding(12)],
            vec![Unavailable {
                contract: Some("ledger".to_owned()),
                message: "baseline missing".to_owned(),
            }],
            2,
        );
        for result in parsed(&report)["runs"][0]["results"]
            .as_array()
            .expect("results")
        {
            assert!(
                result["partialFingerprints"]["brakeFindingV1"].is_string(),
                "without a fingerprint GitHub re-alerts on every commit"
            );
        }
    }

    #[test]
    fn the_fingerprint_survives_the_finding_moving_down_the_file() {
        let at_twelve = parsed(&Report::new(vec![finding(12)], Vec::new(), 1));
        let at_two_hundred = parsed(&Report::new(vec![finding(200)], Vec::new(), 1));

        assert_eq!(
            at_twelve["runs"][0]["results"][0]["partialFingerprints"],
            at_two_hundred["runs"][0]["results"][0]["partialFingerprints"],
            "the same finding at a new line must not re-alert"
        );
    }

    #[test]
    fn different_fields_get_different_fingerprints() {
        let mut other = finding(12);
        other.pointer = "/paths/~1payments~1{id}/get/responses/200/other_field".to_owned();

        let one = parsed(&Report::new(vec![finding(12)], Vec::new(), 1));
        let two = parsed(&Report::new(vec![other], Vec::new(), 1));
        assert_ne!(
            one["runs"][0]["results"][0]["partialFingerprints"],
            two["runs"][0]["results"][0]["partialFingerprints"]
        );
    }

    #[test]
    fn output_is_byte_stable() {
        let report = Report::new(vec![finding(12)], Vec::new(), 1);
        assert_eq!(render(&report), render(&report));
    }
}
