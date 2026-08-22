use crate::report::Report;

pub fn render(report: &Report) -> String {
    let mut out = String::new();

    for finding in &report.findings {
        let mut line = format!(
            "{}[{}]: {}",
            severity_label(finding.severity),
            finding.rule_id,
            finding.message
        );
        if let Some(span) = &finding.span {
            line.push_str(&format!(
                "\n  --> {}:{}:{}",
                span.file, span.line, span.column
            ));
        }
        out.push_str(&line);
        out.push('\n');
    }

    for unavailable in &report.unavailable {
        let contract = unavailable.contract.as_deref().unwrap_or("<unknown>");
        out.push_str(&format!(
            "error[unavailable]: contract `{contract}` is unavailable: {}\n",
            unavailable.message
        ));
    }

    if report.findings.is_empty() && report.unavailable.is_empty() {
        out.push_str("clean: no findings at the selected threshold\n");
    }

    out
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
    fn renders_findings_with_source_location() {
        let report = Report::new(
            vec![Finding {
                rule_id: "endpoint-removed",
                severity: Severity::Error,
                message: "endpoint `GET /payments/{id}` was removed".to_owned(),
                method: Some("GET".to_owned()),
                path: Some("/payments/{id}".to_owned()),
                span: Some(Span {
                    file: "api/openapi.yaml".to_owned(),
                    line: 42,
                    column: 9,
                    pointer: "/paths/~1payments~1{id}/get".to_owned(),
                }),
            }],
            vec![Unavailable {
                contract: Some("ledger".to_owned()),
                message: "baseline file missing".to_owned(),
            }],
            2,
        );

        let rendered = render(&report);
        assert!(rendered.contains("error[endpoint-removed]"));
        assert!(rendered.contains("api/openapi.yaml:42:9"));
        assert!(rendered.contains("contract `ledger` is unavailable"));
    }

    #[test]
    fn renders_clean_message_when_nothing_to_report() {
        let report = Report::new(Vec::new(), Vec::new(), 0);
        let rendered = render(&report);
        assert!(rendered.contains("clean: no findings"));
    }
}
