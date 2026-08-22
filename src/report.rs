use crate::Severity;
use crate::rules::Finding;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub findings: Vec<Finding>,
    pub unavailable: Vec<Unavailable>,
    pub contracts_checked: usize,
}

impl Report {
    #[must_use]
    pub fn new(
        findings: Vec<Finding>,
        unavailable: Vec<Unavailable>,
        contracts_checked: usize,
    ) -> Self {
        Self {
            findings,
            unavailable,
            contracts_checked,
        }
    }

    /// The CI contract: 0 clean, 1 findings, 2 tool failure.
    #[must_use]
    pub fn exit_code(&self, threshold: Severity) -> i32 {
        if !self.unavailable.is_empty() {
            return 2;
        }
        if self
            .findings
            .iter()
            .any(|finding| finding.severity >= threshold)
        {
            return 1;
        }
        0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unavailable {
    pub contract: Option<String>,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::Span;

    fn finding(severity: Severity) -> Finding {
        Finding {
            rule_id: "endpoint-removed",
            severity,
            message: "endpoint was removed".to_owned(),
            method: Some("GET".to_owned()),
            path: Some("/payments/{id}".to_owned()),
            span: Some(Span {
                file: "api/openapi.yaml".to_owned(),
                line: 10,
                column: 5,
                pointer: "/paths/~1payments~1{id}/get".to_owned(),
            }),
        }
    }

    #[test]
    fn exits_clean_when_no_findings_match_threshold() {
        let report = Report::new(vec![finding(Severity::Info)], Vec::new(), 1);
        assert_eq!(report.exit_code(Severity::Warning), 0);
    }

    #[test]
    fn exits_findings_when_threshold_is_matched() {
        let report = Report::new(vec![finding(Severity::Error)], Vec::new(), 1);
        assert_eq!(report.exit_code(Severity::Warning), 1);
    }

    #[test]
    fn exits_tool_failure_when_any_unavailable_exists() {
        let report = Report::new(
            vec![finding(Severity::Error)],
            vec![Unavailable {
                contract: Some("payments".to_owned()),
                message: "baseline missing".to_owned(),
            }],
            1,
        );
        assert_eq!(report.exit_code(Severity::Error), 2);
    }
}
