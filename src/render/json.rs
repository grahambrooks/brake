use serde_json::json;

use crate::report::Report;

pub fn render(report: &Report) -> String {
    let findings = report
        .findings
        .iter()
        .map(|finding| {
            json!({
                "rule_id": finding.rule_id,
                "severity": severity_label(finding.severity),
                "message": finding.message,
                "method": finding.method,
                "path": finding.path,
                "span": finding.span.as_ref().map(|span| json!({
                    "file": span.file,
                    "line": span.line,
                    "column": span.column,
                    "pointer": span.pointer,
                })),
            })
        })
        .collect::<Vec<_>>();

    let unavailable = report
        .unavailable
        .iter()
        .map(|item| {
            json!({
                "contract": item.contract,
                "message": item.message,
            })
        })
        .collect::<Vec<_>>();

    let payload = json!({
        "contracts_checked": report.contracts_checked,
        "findings": findings,
        "unavailable": unavailable,
    });

    format!("{payload}\n")
}

fn severity_label(severity: crate::Severity) -> &'static str {
    match severity {
        crate::Severity::Info => "info",
        crate::Severity::Warning => "warning",
        crate::Severity::Error => "error",
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
    fn renders_structured_payload() {
        let report = Report::new(
            vec![Finding {
                rule_id: "endpoint-removed",
                severity: Severity::Error,
                message: "endpoint removed".to_owned(),
                method: Some("GET".to_owned()),
                path: Some("/pets/{id}".to_owned()),
                span: Some(Span {
                    file: "api/openapi.yaml".to_owned(),
                    line: 7,
                    column: 3,
                    pointer: "/paths/~1pets~1{id}/get".to_owned(),
                }),
            }],
            vec![Unavailable {
                contract: Some("pets".to_owned()),
                message: "baseline missing".to_owned(),
            }],
            1,
        );

        let rendered = render(&report);
        let value: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON");

        assert_eq!(value["contracts_checked"], 1);
        assert_eq!(value["findings"][0]["rule_id"], "endpoint-removed");
        assert_eq!(value["findings"][0]["severity"], "error");
        assert_eq!(value["findings"][0]["span"]["file"], "api/openapi.yaml");
        assert_eq!(value["unavailable"][0]["contract"], "pets");
    }
}
