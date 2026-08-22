pub mod catalogue;

use crate::Severity;
use crate::compare::Change;
use crate::contract::Span;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub rule_id: &'static str,
    pub severity: Severity,
    pub message: String,
    pub method: Option<String>,
    pub path: Option<String>,
    pub span: Option<Span>,
}

pub fn evaluate(changes: &[Change]) -> Vec<Finding> {
    let mut findings = Vec::with_capacity(changes.len());
    for change in changes {
        match change {
            Change::EndpointRemoved { method, path, span } => findings.push(Finding {
                rule_id: "endpoint-removed",
                severity: Severity::Error,
                message: format!("endpoint `{method} {path}` was removed"),
                method: Some(method.clone()),
                path: Some(path.clone()),
                span: Some(span.clone()),
            }),
            Change::MethodRemoved { method, path, span } => findings.push(Finding {
                rule_id: "method-removed",
                severity: Severity::Error,
                message: format!("method `{method}` was removed from path `{path}`"),
                method: Some(method.clone()),
                path: Some(path.clone()),
                span: Some(span.clone()),
            }),
            Change::EndpointPathChanged {
                operation_id,
                method,
                from_path,
                to_path,
                span,
            } => findings.push(Finding {
                rule_id: "endpoint-path-changed",
                severity: Severity::Error,
                message: format!(
                    "operationId `{operation_id}` moved from `{method} {from_path}` to `{method} {to_path}`"
                ),
                method: Some(method.clone()),
                path: Some(to_path.clone()),
                span: Some(span.clone()),
            }),
            Change::ParamAddedRequired {
                method,
                path,
                parameter,
                span,
            } => findings.push(Finding {
                rule_id: "param-added-required",
                severity: Severity::Error,
                message: format!(
                    "required parameter `{parameter}` was added to `{method} {path}`"
                ),
                method: Some(method.clone()),
                path: Some(path.clone()),
                span: Some(span.clone()),
            }),
            Change::ParamBecameRequired {
                method,
                path,
                parameter,
                span,
            } => findings.push(Finding {
                rule_id: "param-became-required",
                severity: Severity::Error,
                message: format!(
                    "parameter `{parameter}` became required in `{method} {path}`"
                ),
                method: Some(method.clone()),
                path: Some(path.clone()),
                span: Some(span.clone()),
            }),
            Change::ParamRemoved {
                method,
                path,
                parameter,
                span,
            } => findings.push(Finding {
                rule_id: "param-removed",
                severity: Severity::Warning,
                message: format!("parameter `{parameter}` was removed from `{method} {path}`"),
                method: Some(method.clone()),
                path: Some(path.clone()),
                span: Some(span.clone()),
            }),
            Change::ParamTypeNarrowed {
                method,
                path,
                target,
                reason,
                span,
            } => findings.push(Finding {
                rule_id: "param-type-narrowed",
                severity: Severity::Error,
                message: format!("{target} narrowed in `{method} {path}`: {reason}"),
                method: Some(method.clone()),
                path: Some(path.clone()),
                span: Some(span.clone()),
            }),
            Change::ResponseTypeChanged {
                method,
                path,
                status,
                reason,
                span,
            } => findings.push(Finding {
                rule_id: "response-type-changed",
                severity: Severity::Error,
                message: format!(
                    "response type changed for `{method} {path}` status `{status}`: {reason}"
                ),
                method: Some(method.clone()),
                path: Some(path.clone()),
                span: Some(span.clone()),
            }),
            Change::ResponseEnumExtended {
                method,
                path,
                status,
                span,
            } => findings.push(Finding {
                rule_id: "response-enum-extended",
                severity: Severity::Warning,
                message: format!(
                    "response enum was extended for `{method} {path}` status `{status}`"
                ),
                method: Some(method.clone()),
                path: Some(path.clone()),
                span: Some(span.clone()),
            }),
            Change::ResponseStatusRemoved {
                method,
                path,
                status,
                span,
            } => findings.push(Finding {
                rule_id: "response-status-removed",
                severity: Severity::Error,
                message: format!(
                    "response status `{status}` was removed from `{method} {path}`"
                ),
                method: Some(method.clone()),
                path: Some(path.clone()),
                span: Some(span.clone()),
            }),
        }
    }

    findings
}

pub fn contract_unreachable(contract: &str, details: &str, span: Option<Span>) -> Finding {
    Finding {
        rule_id: "contract-unreachable",
        severity: Severity::Error,
        message: format!("contract `{contract}` is unreachable: {details}"),
        method: None,
        path: None,
        span,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_span() -> Span {
        Span {
            file: "api/openapi.yaml".to_owned(),
            line: 2,
            column: 3,
            pointer: "/paths/~1payments/get".to_owned(),
        }
    }

    #[test]
    fn maps_endpoint_removed_to_rule() {
        let changes = vec![Change::EndpointRemoved {
            method: "GET".to_owned(),
            path: "/payments/{id}".to_owned(),
            span: sample_span(),
        }];
        let findings = evaluate(&changes);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule_id, "endpoint-removed");
        assert_eq!(findings[0].severity, Severity::Error);
    }

    #[test]
    fn maps_method_removed_to_rule() {
        let changes = vec![Change::MethodRemoved {
            method: "POST".to_owned(),
            path: "/payments".to_owned(),
            span: sample_span(),
        }];
        let findings = evaluate(&changes);

        assert_eq!(findings[0].rule_id, "method-removed");
        assert_eq!(findings[0].severity, Severity::Error);
    }

    #[test]
    fn maps_endpoint_path_changed_to_rule() {
        let changes = vec![Change::EndpointPathChanged {
            operation_id: "getPayment".to_owned(),
            method: "GET".to_owned(),
            from_path: "/payments/{id}".to_owned(),
            to_path: "/payments/{payment_id}".to_owned(),
            span: sample_span(),
        }];
        let findings = evaluate(&changes);

        assert_eq!(findings[0].rule_id, "endpoint-path-changed");
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(findings[0].message.contains("operationId `getPayment`"));
    }

    #[test]
    fn creates_contract_unreachable_finding() {
        let finding = contract_unreachable("payments", "file not found", Some(sample_span()));

        assert_eq!(finding.rule_id, "contract-unreachable");
        assert_eq!(finding.severity, Severity::Error);
    }

    #[test]
    fn maps_request_response_changes_to_rules() {
        let changes = vec![
            Change::ParamAddedRequired {
                method: "POST".to_owned(),
                path: "/payments".to_owned(),
                parameter: "query:mode".to_owned(),
                span: sample_span(),
            },
            Change::ResponseEnumExtended {
                method: "GET".to_owned(),
                path: "/payments/{id}".to_owned(),
                status: "200".to_owned(),
                span: sample_span(),
            },
        ];

        let findings = evaluate(&changes);
        assert!(
            findings
                .iter()
                .any(|finding| finding.rule_id == "param-added-required")
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.rule_id == "response-enum-extended")
        );
    }
}
