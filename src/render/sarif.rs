use std::collections::BTreeSet;

use serde_json::json;

use crate::report::Report;
use crate::rules::catalogue;

pub fn render(report: &Report) -> String {
    let rules = report
        .findings
        .iter()
        .map(|finding| finding.rule_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(catalogue::lookup)
        .map(|rule| {
            json!({
                "id": rule.id,
                "shortDescription": { "text": rule.summary },
                "fullDescription": { "text": rule.explanation },
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
        }));
    }

    let payload = json!({
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {
                "driver": {
                    "name": "brake",
                    "version": crate::VERSION,
                    "rules": rules,
                }
            },
            "results": results,
        }]
    });

    format!("{payload}\n")
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

    #[test]
    fn renders_sarif_payload() {
        let report = Report::new(
            vec![Finding {
                rule_id: "endpoint-removed",
                severity: Severity::Error,
                message: "endpoint removed".to_owned(),
                method: Some("GET".to_owned()),
                path: Some("/pets/{id}".to_owned()),
                span: Some(Span {
                    file: "api/openapi.yaml".to_owned(),
                    line: 12,
                    column: 5,
                    pointer: "/paths/~1pets~1{id}/get".to_owned(),
                }),
            }],
            vec![Unavailable {
                contract: Some("pets".to_owned()),
                message: "baseline file missing".to_owned(),
            }],
            1,
        );

        let rendered = render(&report);
        let value: serde_json::Value = serde_json::from_str(&rendered).expect("valid SARIF JSON");

        assert_eq!(value["version"], "2.1.0");
        assert_eq!(value["runs"][0]["tool"]["driver"]["name"], "brake");
        assert_eq!(value["runs"][0]["results"][0]["ruleId"], "endpoint-removed");
        assert_eq!(
            value["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["artifactLocation"]
                ["uri"],
            "api/openapi.yaml"
        );
        assert_eq!(
            value["runs"][0]["results"][1]["ruleId"],
            "contract-unreachable"
        );
    }
}
