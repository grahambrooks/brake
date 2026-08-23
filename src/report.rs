//! `Finding` → verdict, and the exit-code contract.
//!
//! `exit_code` lives in the library, not the CLI, because it *is* the CI
//! contract and it needs a test that does not spawn a process.

use std::collections::BTreeMap;

use crate::rules::Finding;
use crate::{Severity, Verdict};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub findings: Vec<Finding>,
    /// Things brake could not do. Any entry here means exit `2`: the gate is
    /// broken, which is a different problem from the API being broken.
    pub unavailable: Vec<Unavailable>,
    pub contracts_checked: usize,
    /// The text of every artifact a span points into, keyed by the span's
    /// `file`. Carried on the report so a renderer can show the offending
    /// line, including for a baseline that only exists as a git blob.
    pub sources: BTreeMap<String, String>,
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
            sources: BTreeMap::new(),
        }
    }

    /// Merge another report into this one, keeping the contract count additive.
    pub fn absorb(&mut self, other: Report) {
        self.findings.extend(other.findings);
        self.unavailable.extend(other.unavailable);
        self.contracts_checked += other.contracts_checked;
        self.sources.extend(other.sources);
    }

    /// Sort into a stable order, so two runs on the same inputs emit the same
    /// bytes regardless of the order contracts happened to be visited in.
    pub fn finalise(&mut self) {
        self.findings.sort();
        self.findings.dedup();
        self.unavailable.sort();
        self.unavailable.dedup();
    }

    #[must_use]
    pub fn verdict(&self, threshold: Severity) -> Verdict {
        if !self.unavailable.is_empty() {
            return Verdict::ToolFailure;
        }
        if self
            .findings
            .iter()
            .any(|finding| finding.severity >= threshold)
        {
            return Verdict::Findings;
        }
        Verdict::Clean
    }

    /// The §7.1 contract: 0 clean, 1 findings, 2 tool failure.
    #[must_use]
    pub fn exit_code(&self, threshold: Severity) -> i32 {
        self.verdict(threshold).exit_code()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
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
            contract: "payments".to_owned(),
            message: "endpoint was removed".to_owned(),
            method: Some("GET".to_owned()),
            path: Some("/payments/{id}".to_owned()),
            pointer: "/paths/~1payments~1{id}/get".to_owned(),
            subject: None,
            span: Some(Span::new(
                "api/openapi.yaml",
                10,
                5,
                "/paths/~1payments~1{id}/get",
            )),
        }
    }

    #[test]
    fn exits_clean_when_no_finding_reaches_the_threshold() {
        let report = Report::new(vec![finding(Severity::Info)], Vec::new(), 1);
        assert_eq!(report.exit_code(Severity::Warning), 0);
    }

    #[test]
    fn exits_one_when_a_finding_reaches_the_threshold() {
        let report = Report::new(vec![finding(Severity::Error)], Vec::new(), 1);
        assert_eq!(report.exit_code(Severity::Warning), 1);
    }

    #[test]
    fn exits_two_when_anything_is_unavailable() {
        let report = Report::new(
            vec![finding(Severity::Error)],
            vec![Unavailable {
                contract: Some("payments".to_owned()),
                message: "baseline missing".to_owned(),
            }],
            1,
        );
        assert_eq!(
            report.exit_code(Severity::Error),
            2,
            "a broken gate must never be reported as a broken API"
        );
    }

    #[test]
    fn absorb_keeps_the_contract_count_additive() {
        let mut report = Report::new(Vec::new(), Vec::new(), 1);
        report.absorb(Report::new(vec![finding(Severity::Error)], Vec::new(), 2));
        assert_eq!(report.contracts_checked, 3);
        assert_eq!(report.findings.len(), 1);
    }

    #[test]
    fn finalise_is_order_independent() {
        let mut one = Report::new(
            vec![finding(Severity::Error), finding(Severity::Info)],
            Vec::new(),
            1,
        );
        let mut two = Report::new(
            vec![finding(Severity::Info), finding(Severity::Error)],
            Vec::new(),
            1,
        );
        one.finalise();
        two.finalise();
        assert_eq!(one.findings, two.findings);
    }
}
