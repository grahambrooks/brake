pub mod catalogue;

use crate::Severity;
use crate::compare::Change;
use crate::config::Compatibility;
use crate::config::Suppression;
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

pub fn evaluate(changes: &[Change], level: Compatibility) -> Vec<Finding> {
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
        .into_iter()
        .filter(|finding| level >= minimum_level(finding.rule_id))
        .collect()
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

pub fn apply_suppressions(
    findings: Vec<Finding>,
    suppressions: &[Suppression],
    as_of: Option<&str>,
) -> Vec<Finding> {
    let mut output = Vec::new();
    let mut matched = vec![false; suppressions.len()];

    for finding in findings {
        let mut suppressed = false;
        for (index, suppression) in suppressions.iter().enumerate() {
            if suppression.rule != finding.rule_id {
                continue;
            }
            if let Some(endpoint) = &suppression.endpoint
                && finding_endpoint(&finding).as_deref() != Some(endpoint.as_str())
            {
                continue;
            }
            if let Some(field) = &suppression.field
                && !finding.message.contains(field)
            {
                continue;
            }

            matched[index] = true;
            if suppression_is_expired(suppression, as_of) {
                output.push(Finding {
                    rule_id: "expired-allow",
                    severity: Severity::Error,
                    message: format!(
                        "suppression for `{}` expired on `{}`",
                        suppression.rule,
                        suppression.expires.as_deref().unwrap_or("unknown")
                    ),
                    method: finding.method.clone(),
                    path: finding.path.clone(),
                    span: finding.span.clone(),
                });
            } else {
                suppressed = true;
            }
            break;
        }

        if !suppressed {
            output.push(finding);
        }
    }

    for (index, suppression) in suppressions.iter().enumerate() {
        if !matched[index] {
            output.push(Finding {
                rule_id: "stale-allow",
                severity: Severity::Error,
                message: format!(
                    "suppression for rule `{}` no longer matches any finding",
                    suppression.rule
                ),
                method: None,
                path: None,
                span: None,
            });
        }
    }

    output
}

fn minimum_level(rule_id: &str) -> Compatibility {
    match rule_id {
        "response-status-removed" | "response-enum-extended" => Compatibility::WireJson,
        _ => Compatibility::Wire,
    }
}

fn finding_endpoint(finding: &Finding) -> Option<String> {
    Some(format!(
        "{} {}",
        finding.method.as_ref()?,
        finding.path.as_ref()?
    ))
}

fn suppression_is_expired(suppression: &Suppression, as_of: Option<&str>) -> bool {
    let Some(expires) = suppression.expires.as_deref() else {
        return false;
    };
    let Some(as_of) = as_of else {
        return false;
    };
    parse_date(expires)
        .zip(parse_date(as_of))
        .is_some_and(|(expiry, now)| now > expiry)
}

fn parse_date(date: &str) -> Option<(u32, u32, u32)> {
    let mut parts = date.split('-');
    let year = parts.next()?.parse().ok()?;
    let month = parts.next()?.parse().ok()?;
    let day = parts.next()?.parse().ok()?;
    Some((year, month, day))
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
        let findings = evaluate(&changes, Compatibility::Wire);

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
        let findings = evaluate(&changes, Compatibility::Wire);

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
        let findings = evaluate(&changes, Compatibility::Wire);

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

        let findings = evaluate(&changes, Compatibility::WireJson);
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

    #[test]
    fn filters_rules_by_compatibility_level() {
        let changes = vec![Change::ResponseStatusRemoved {
            method: "GET".to_owned(),
            path: "/payments/{id}".to_owned(),
            status: "200".to_owned(),
            span: sample_span(),
        }];

        let wire_findings = evaluate(&changes, Compatibility::Wire);
        let wire_json_findings = evaluate(&changes, Compatibility::WireJson);
        assert!(wire_findings.is_empty());
        assert_eq!(wire_json_findings.len(), 1);
        assert_eq!(wire_json_findings[0].rule_id, "response-status-removed");
    }

    #[test]
    fn applies_suppressions_with_stale_and_expired_detection() {
        let findings = vec![Finding {
            rule_id: "endpoint-removed",
            severity: Severity::Error,
            message: "endpoint `GET /payments/{id}` was removed".to_owned(),
            method: Some("GET".to_owned()),
            path: Some("/payments/{id}".to_owned()),
            span: Some(sample_span()),
        }];
        let suppressions = vec![
            Suppression {
                rule: "endpoint-removed".to_owned(),
                endpoint: Some("GET /payments/{id}".to_owned()),
                field: None,
                reason: "accepted".to_owned(),
                expires: Some("2026-01-01".to_owned()),
            },
            Suppression {
                rule: "method-removed".to_owned(),
                endpoint: Some("POST /payments".to_owned()),
                field: None,
                reason: "obsolete".to_owned(),
                expires: None,
            },
        ];

        let filtered = apply_suppressions(findings, &suppressions, Some("2026-02-01"));
        assert!(filtered.iter().any(|f| f.rule_id == "expired-allow"));
        assert!(filtered.iter().any(|f| f.rule_id == "stale-allow"));
    }
}
