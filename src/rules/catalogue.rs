use crate::Severity;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub id: &'static str,
    pub severity: Severity,
    pub summary: &'static str,
    pub explanation: &'static str,
}

pub static RULES: &[Rule] = &[
    Rule {
        id: "endpoint-removed",
        severity: Severity::Error,
        summary: "A baseline endpoint is absent in the head contract.",
        explanation: "Removing an endpoint breaks existing consumers that still call it.",
    },
    Rule {
        id: "method-removed",
        severity: Severity::Error,
        summary: "A method on an existing path is absent in the head contract.",
        explanation: "Removing a method breaks consumers using that HTTP verb on the path.",
    },
    Rule {
        id: "endpoint-path-changed",
        severity: Severity::Error,
        summary: "An operationId stayed, but its path template changed.",
        explanation: "Changing a path for an existing operation breaks callers pinned to the previous route.",
    },
    Rule {
        id: "contract-unreachable",
        severity: Severity::Error,
        summary: "The configured contract source could not be read or parsed.",
        explanation: "A contract that cannot be read cannot be verified; the run must fail honestly.",
    },
    Rule {
        id: "param-added-required",
        severity: Severity::Error,
        summary: "A new required request parameter or field was added.",
        explanation: "Adding a required input breaks callers that do not send it.",
    },
    Rule {
        id: "param-became-required",
        severity: Severity::Error,
        summary: "An existing request parameter or field became required.",
        explanation: "Tightening optional input to required breaks existing callers.",
    },
    Rule {
        id: "param-removed",
        severity: Severity::Warning,
        summary: "A request parameter was removed.",
        explanation: "Removing request parameters can break callers still sending them depending on server validation.",
    },
    Rule {
        id: "param-type-narrowed",
        severity: Severity::Error,
        summary: "A request parameter or body type was narrowed.",
        explanation: "Narrowing accepted request shapes breaks clients that send previously accepted values.",
    },
    Rule {
        id: "response-type-changed",
        severity: Severity::Error,
        summary: "A response type changed incompatibly.",
        explanation: "Changing response shape incompatibly breaks consumers that deserialize the baseline schema.",
    },
    Rule {
        id: "response-enum-extended",
        severity: Severity::Warning,
        summary: "A response enum gained new values.",
        explanation: "Enum extension can break exhaustive matching in generated or strict clients.",
    },
    Rule {
        id: "response-status-removed",
        severity: Severity::Error,
        summary: "A documented response status code was removed.",
        explanation: "Removing response statuses breaks consumers depending on previously documented outcomes.",
    },
    Rule {
        id: "stale-allow",
        severity: Severity::Error,
        summary: "A suppression no longer matches any finding.",
        explanation: "Stale suppressions hide real regressions and should be removed once no longer needed.",
    },
    Rule {
        id: "expired-allow",
        severity: Severity::Error,
        summary: "A suppression is past its expiry date.",
        explanation: "Expired suppressions must fail the check to enforce time-bounded exceptions.",
    },
    Rule {
        id: "generated-drift",
        severity: Severity::Error,
        summary: "Generated contract output differs from the checked-in artifact.",
        explanation: "Generated drift means the committed contract is stale relative to its declared generator output.",
    },
];

pub fn lookup(id: &str) -> Option<&'static Rule> {
    RULES.iter().find(|rule| rule.id == id)
}

#[cfg(test)]
mod tests {
    use super::{RULES, lookup};

    #[test]
    fn every_rule_has_non_placeholder_explanation() {
        for rule in RULES {
            assert!(
                !rule.summary.trim().is_empty(),
                "empty summary for {}",
                rule.id
            );
            assert!(
                !rule.explanation.trim().is_empty(),
                "empty explanation for {}",
                rule.id
            );
        }
    }

    #[test]
    fn lookup_finds_known_rule() {
        let rule = lookup("endpoint-removed").expect("known rule should resolve");
        assert_eq!(rule.id, "endpoint-removed");
    }
}
