//! GitHub Actions workflow command renderer.
//!
//! Emits `::error`, `::warning`, and `::notice` lines so findings render directly
//! as inline pull request annotations in GitHub Actions without requiring a
//! separate SARIF upload step.

use crate::Severity;
use crate::report::Report;
use crate::rules::Finding;

pub fn render(report: &Report) -> String {
    let mut out = String::new();

    for finding in &report.findings {
        out.push_str(&render_finding(finding));
        out.push('\n');
    }

    for unavailable in &report.unavailable {
        let contract = unavailable.contract.as_deref().unwrap_or("<unknown>");
        let message = format!(
            "brake could not check `{contract}`: {} (tool failure, exiting 2)",
            unavailable.message
        );
        out.push_str(&format!(
            "::error title=tool-failure::{}\n",
            escape_message(&message)
        ));
    }

    out
}

fn render_finding(finding: &Finding) -> String {
    let command = match finding.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "notice",
    };

    let mut params = Vec::new();

    if let Some(span) = &finding.span {
        params.push(format!("file={}", escape_param(&span.file)));
        params.push(format!("line={}", span.line));
        params.push(format!("col={}", span.column));
    } else if let Some(path) = &finding.path {
        params.push(format!("file={}", escape_param(path)));
    }

    params.push(format!("title={}", escape_param(finding.rule_id)));

    let formatted_params = if params.is_empty() {
        String::new()
    } else {
        format!(" {}", params.join(","))
    };

    let body = escape_message(&finding.message);
    format!("::{command}{formatted_params}::{body}")
}

fn escape_param(s: &str) -> String {
    s.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
        .replace(':', "%3A")
        .replace(',', "%2C")
}

fn escape_message(s: &str) -> String {
    s.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::Span;
    use crate::report::Unavailable;

    #[test]
    fn renders_finding_with_span() {
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
        assert_eq!(
            rendered.trim(),
            "::error file=api/payments.yaml,line=142,col=9,title=response-field-removed::response field `customer_id` was removed"
        );
    }

    #[test]
    fn renders_unavailable() {
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
        assert_eq!(
            rendered.trim(),
            "::error title=tool-failure::brake could not check `orders`: file missing (tool failure, exiting 2)"
        );
    }

    #[test]
    fn renders_notice_and_escapes_special_characters() {
        let finding = Finding {
            rule_id: "response-field-added",
            severity: Severity::Info,
            contract: "payments".to_owned(),
            message: "100% added\nnew field".to_owned(),
            method: Some("GET".to_owned()),
            path: Some("/payments,items".to_owned()),
            pointer: "/components/schemas/Payment".to_owned(),
            subject: Some("items".to_owned()),
            span: None,
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
        assert_eq!(
            rendered.trim(),
            "::notice file=/payments%2Citems,title=response-field-added::100%25 added%0Anew field"
        );
    }
}
