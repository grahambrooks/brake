//! Machine-readable output, one object per finding.
//!
//! Key order is stable because `serde_json::json!` preserves declaration order
//! and every collection reaching here is already sorted. Byte-stability across
//! runs is guarantee G4.

use serde_json::json;

use crate::report::Report;
use crate::rules::catalogue;

pub fn render(report: &Report) -> String {
    let findings = report
        .findings
        .iter()
        .map(|finding| {
            let span = finding.span.as_ref();
            json!({
                "rule": finding.rule_id,
                "severity": severity_label(finding.severity),
                "contract": finding.contract,
                "method": finding.method,
                "path": finding.path,
                "pointer": finding.pointer,
                "file": span.map(|span| span.file.clone()),
                "line": span.map(|span| span.line),
                "column": span.map(|span| span.column),
                "message": finding.message,
                "help_uri": catalogue::lookup(finding.rule_id).map(catalogue::Rule::help_uri),
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

    fn report() -> Report {
        Report::new(
            vec![Finding {
                rule_id: "response-field-removed",
                severity: Severity::Error,
                contract: "payments".to_owned(),
                message: "response field removed: field `customer_id`".to_owned(),
                method: Some("GET".to_owned()),
                path: Some("/payments/{id}".to_owned()),
                pointer: "/paths/~1payments~1{id}/get/responses/200/customer_id".to_owned(),
                span: Some(Span::new("api/openapi.yaml", 142, 9, "/paths")),
            }],
            vec![Unavailable {
                contract: Some("ledger".to_owned()),
                message: "baseline missing".to_owned(),
            }],
            2,
        )
    }

    #[test]
    fn emits_the_documented_key_set() {
        let value: serde_json::Value =
            serde_json::from_str(&render(&report())).expect("valid JSON");
        let finding = &value["findings"][0];

        for key in [
            "rule", "severity", "contract", "method", "path", "pointer", "file", "line", "message",
        ] {
            assert!(!finding[key].is_null(), "missing documented key `{key}`");
        }
        assert_eq!(finding["rule"], "response-field-removed");
        assert_eq!(finding["contract"], "payments");
        assert_eq!(finding["file"], "api/openapi.yaml");
        assert_eq!(finding["line"], 142);
    }

    #[test]
    fn names_the_contract_so_findings_are_attributable() {
        let value: serde_json::Value =
            serde_json::from_str(&render(&report())).expect("valid JSON");
        assert_eq!(value["findings"][0]["contract"], "payments");
        assert_eq!(value["unavailable"][0]["contract"], "ledger");
        assert_eq!(value["contracts_checked"], 2);
    }

    #[test]
    fn output_is_byte_stable() {
        assert_eq!(render(&report()), render(&report()));
    }
}
