//! GitLab Code Quality report renderer.
//!
//! Emits an array of Code Quality issue objects conforming to the GitLab CI
//! Code Quality specification, allowing MR widget annotations.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde_json::json;

use crate::Severity;
use crate::report::Report;
use crate::rules::Finding;

pub fn render(report: &Report) -> String {
    let mut items = Vec::new();

    for finding in &report.findings {
        items.push(render_finding(finding));
    }

    for unavailable in &report.unavailable {
        let contract = unavailable.contract.as_deref().unwrap_or("<unknown>");
        let message = format!(
            "brake could not check `{contract}`: {} (tool failure, exiting 2)",
            unavailable.message
        );
        let fingerprint = compute_fingerprint("tool-failure", contract, &message, 0);
        items.push(json!({
            "description": message,
            "check_name": "tool-failure",
            "fingerprint": fingerprint,
            "severity": "blocker",
            "location": {
                "path": contract,
                "lines": {
                    "begin": 1
                }
            }
        }));
    }

    let payload = serde_json::Value::Array(items);
    format!("{payload}\n")
}

fn render_finding(finding: &Finding) -> serde_json::Value {
    let severity = match finding.severity {
        Severity::Error => "critical",
        Severity::Warning => "minor",
        Severity::Info => "info",
    };

    let path = finding
        .span
        .as_ref()
        .map(|s| s.file.clone())
        .or_else(|| finding.path.clone())
        .unwrap_or_else(|| finding.contract.clone());

    let line = finding.span.as_ref().map_or(1, |s| s.line);

    let fingerprint =
        compute_fingerprint(finding.rule_id, &finding.contract, &finding.pointer, line);

    json!({
        "description": finding.message,
        "check_name": finding.rule_id,
        "fingerprint": fingerprint,
        "severity": severity,
        "location": {
            "path": path,
            "lines": {
                "begin": line
            }
        }
    })
}

fn compute_fingerprint(rule_id: &str, contract: &str, pointer: &str, line: usize) -> String {
    let mut hasher = DefaultHasher::new();
    rule_id.hash(&mut hasher);
    contract.hash(&mut hasher);
    pointer.hash(&mut hasher);
    line.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::Span;
    use crate::report::{Report, Unavailable};

    #[test]
    fn renders_gitlab_code_quality_format() {
        let finding = Finding {
            rule_id: "response-field-removed",
            severity: Severity::Error,
            contract: "payments".to_owned(),
            message: "response field `customer_id` was removed".to_owned(),
            method: Some("GET".to_owned()),
            path: Some("/payments/{id}".to_owned()),
            pointer: "/components/schemas/Payment/properties/customer_id".to_owned(),
            subject: Some("customer_id".to_owned()),
            span: Some(Span {
                file: "api/payments.yaml".to_owned(),
                line: 142,
                column: 9,
                pointer: "/components/schemas/Payment/properties/customer_id".to_owned(),
            }),
            affects: Vec::new(),
            note: None,
        };

        let report = Report {
            contracts_checked: 1,
            findings: vec![finding],
            unavailable: Vec::new(),
            sources: std::collections::BTreeMap::new(),
        };

        let rendered = render(&report);
        let parsed: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON array");
        assert!(parsed.is_array());
        let item = &parsed[0];
        assert_eq!(item["check_name"], "response-field-removed");
        assert_eq!(item["severity"], "critical");
        assert_eq!(item["location"]["path"], "api/payments.yaml");
        assert_eq!(item["location"]["lines"]["begin"], 142);
        assert!(item["fingerprint"].is_string());
    }

    #[test]
    fn renders_unavailable_as_blocker() {
        let report = Report {
            contracts_checked: 0,
            findings: Vec::new(),
            unavailable: vec![Unavailable {
                contract: Some("orders".to_owned()),
                message: "file missing".to_owned(),
            }],
            sources: std::collections::BTreeMap::new(),
        };

        let rendered = render(&report);
        let parsed: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON array");
        assert_eq!(parsed[0]["severity"], "blocker");
        assert_eq!(parsed[0]["check_name"], "tool-failure");
    }

    #[test]
    fn maps_warning_and_info_severities_correctly() {
        let warning_finding = Finding {
            rule_id: "response-field-optional",
            severity: Severity::Warning,
            contract: "payments".to_owned(),
            message: "field became optional".to_owned(),
            method: Some("GET".to_owned()),
            path: Some("/payments".to_owned()),
            pointer: "/components/schemas/Payment".to_owned(),
            subject: Some("status".to_owned()),
            span: None,
            affects: Vec::new(),
            note: None,
        };
        let info_finding = Finding {
            rule_id: "response-field-added",
            severity: Severity::Info,
            contract: "payments".to_owned(),
            message: "field added".to_owned(),
            method: Some("GET".to_owned()),
            path: Some("/payments".to_owned()),
            pointer: "/components/schemas/Payment".to_owned(),
            subject: Some("tag".to_owned()),
            span: None,
            affects: Vec::new(),
            note: None,
        };
        let report = Report {
            contracts_checked: 1,
            findings: vec![warning_finding, info_finding],
            unavailable: Vec::new(),
            sources: std::collections::BTreeMap::new(),
        };

        let rendered = render(&report);
        let parsed: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON array");
        assert_eq!(parsed[0]["severity"], "minor");
        assert_eq!(parsed[1]["severity"], "info");
    }
}
