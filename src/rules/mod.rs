//! `Change` × `Level` → `Finding`.
//!
//! This module knows nothing about rendering. A `Finding` carries the facts; a
//! renderer decides how they look.

pub mod catalogue;
pub mod strategies;

use crate::Severity;
use crate::compare::{Change, ChangeKind};
use crate::config::{Compatibility, Suppression};
use crate::contract::Span;

/// A consumer this finding is evidence against, with the interaction that
/// says so.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ConsumerRef {
    pub consumer: String,
    /// The demand artifact, repository-relative.
    pub source: String,
    /// The interaction, not the contract.
    pub span: Span,
}

/// One thing brake found.
///
/// `#[non_exhaustive]` from the release that added [`Finding::affects`]:
/// adding a field to this struct breaks every downstream struct literal, and
/// brake is the tool that gates exactly that. It ships as a deliberate,
/// announced break so that it is the last one of its kind — taking the
/// medicine this crate prescribes. See `design/05-consumer-demand.md` §11, M13.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub struct Finding {
    pub rule_id: &'static str,
    pub severity: Severity,
    /// Which configured contract this came from. Without it, a repository with
    /// several contracts produces findings nobody can attribute.
    pub contract: String,
    pub message: String,
    pub method: Option<String>,
    pub path: Option<String>,
    /// A JSON pointer into the contract artifact, for SARIF fingerprints and
    /// for suppressions that target a field structurally.
    pub pointer: String,
    /// What the finding is about, as a bare name — the field, parameter,
    /// status or media type. `None` where the subject is the endpoint itself.
    ///
    /// Carried from the `Change` rather than parsed back out of `pointer`,
    /// which for a parameter ends in its index.
    pub subject: Option<String>,
    pub span: Option<Span>,
    /// Declared consumers this finding is evidence against, with the
    /// interaction that says so. Empty when no consumer is declared, and —
    /// importantly — also when none is affected. The two are not the same, and
    /// the `note` a `triage` downgrade prints is what keeps them
    /// distinguishable.
    pub affects: Vec<ConsumerRef>,
    /// The assumption a policy decision rests on, rendered under the finding.
    ///
    /// Set only by `triage`, which is the one policy that can lie: a
    /// downgraded finding has to say what it was downgraded on the strength
    /// of. See `design/05-consumer-demand.md` §7.2.
    pub note: Option<String>,
}

impl Finding {
    /// `GET /payments/{id}`, the form a suppression's `endpoint` matches.
    #[must_use]
    pub fn endpoint(&self) -> Option<String> {
        Some(format!("{} {}", self.method.as_ref()?, self.path.as_ref()?))
    }

    /// Ways to make this change without breaking a consumer.
    ///
    /// Catalogued per rule and bound to this finding's subject — see
    /// [`strategies`]. Empty for a rule where nothing is broken.
    #[must_use]
    pub fn remediations(&self) -> Vec<strategies::Remediation> {
        let Some(rule) = catalogue::lookup(self.rule_id) else {
            return Vec::new();
        };
        let subject = self.subject.as_deref();
        let endpoint = self.endpoint();
        rule.remedies
            .iter()
            .filter_map(|id| strategies::lookup(id))
            .map(|strategy| strategies::bind(strategy, subject, endpoint.as_deref()))
            .collect()
    }

    /// Format a suggested `[[contract.allow]]` TOML block for this finding.
    #[must_use]
    pub fn suggest_suppression(&self, reason: Option<&str>, expires: Option<&str>) -> String {
        let mut out = String::from("[[contract.allow]]\n");
        out.push_str(&format!("rule = {:?}\n", self.rule_id));
        if let Some(endpoint) = self.endpoint() {
            out.push_str(&format!("endpoint = {:?}\n", endpoint));
        }
        if let Some(field) = &self.subject {
            out.push_str(&format!("field = {:?}\n", field));
        }
        let reason_val = reason.unwrap_or("<reason for suppression>");
        out.push_str(&format!("reason = {:?}\n", reason_val));
        if let Some(exp) = expires {
            out.push_str(&format!("expires = {:?}\n", exp));
        }
        out
    }
}

/// Turn changes into findings, dropping those the selected level does not ask
/// about.
///
/// A rule outside the level does not fire at all rather than being downgraded
/// to a warning, because a warning is a thing a human has to read and dismiss.
#[must_use]
pub fn evaluate(changes: &[Change], contract: &str, level: Compatibility) -> Vec<Finding> {
    let mut findings = Vec::with_capacity(changes.len());
    for change in changes {
        let rule = catalogue::rule_for(change.kind);
        if level < rule.level {
            continue;
        }
        findings.push(Finding {
            affects: Vec::new(),
            note: None,
            rule_id: rule.id,
            severity: rule.severity,
            contract: contract.to_owned(),
            message: message_for(change),
            method: change.endpoint.as_ref().map(|key| key.method.clone()),
            path: change.endpoint.as_ref().map(|key| key.path.clone()),
            pointer: change.pointer.clone(),
            subject: change.subject.clone(),
            span: Some(change.span.clone()),
        });
    }
    findings.sort();
    findings.dedup();
    findings
}

fn message_for(change: &Change) -> String {
    let where_ = change
        .endpoint
        .as_ref()
        .map(|key| format!("{} {}", key.method, key.path));
    let detail = change.detail.trim();

    let core = match change.kind {
        ChangeKind::EndpointRemoved => format!(
            "endpoint `{}` was removed",
            where_.clone().unwrap_or_default()
        ),
        ChangeKind::MethodRemoved => {
            let key = change.endpoint.as_ref();
            format!(
                "method `{}` was removed from `{}`",
                key.map(|k| k.method.as_str()).unwrap_or_default(),
                key.map(|k| k.path.as_str()).unwrap_or_default()
            )
        }
        ChangeKind::EndpointPathChanged | ChangeKind::PathParameterRenamed => detail.to_owned(),
        ChangeKind::EndpointAdded => format!(
            "endpoint `{}` was added",
            where_.clone().unwrap_or_default()
        ),
        ChangeKind::OperationIdChanged => format!("operationId changed: {detail}"),
        ChangeKind::ParamAddedRequired => format!("required input added: {detail}"),
        ChangeKind::ParamBecameRequired => format!("`{detail}` became required"),
        ChangeKind::ParamRemoved => format!("parameter `{detail}` was removed"),
        ChangeKind::ParamTypeNarrowed => format!("request narrowed: {detail}"),
        ChangeKind::ParamLocationChanged => detail.to_owned(),
        ChangeKind::ParamAddedOptional => format!("optional input added: {detail}"),
        ChangeKind::RequestMediaTypeRemoved => {
            format!("request media type `{detail}` is no longer accepted")
        }
        ChangeKind::ResponseFieldRemoved => format!("response field removed: {detail}"),
        ChangeKind::ResponseFieldOptional => format!("response field became optional: {detail}"),
        ChangeKind::ResponseFieldAdded => format!("response field added: {detail}"),
        ChangeKind::ResponseTypeChanged => format!("response type changed: {detail}"),
        ChangeKind::ResponseEnumExtended => format!("response enum extended: {detail}"),
        ChangeKind::ResponseStatusRemoved => format!("response status `{detail}` was removed"),
        ChangeKind::ResponseStatusAdded => format!("response status `{detail}` was added"),
        ChangeKind::ResponseMediaTypeRemoved => format!("response media type removed: {detail}"),
        ChangeKind::FieldRenamed => format!("field renamed: {detail}"),
        ChangeKind::FieldNumberChanged => format!("wire number changed: {detail}"),
        ChangeKind::SecurityAdded => format!("security requirement added: {detail}"),
        ChangeKind::SecurityRemoved => format!("security requirement removed: {detail}"),
        ChangeKind::SecuritySchemeChanged => format!("security scheme changed: {detail}"),
        ChangeKind::RemovedWithoutDeprecation => {
            format!("{detail} was removed without being deprecated first")
        }
        ChangeKind::DeprecatedNoSunset => format!(
            "`{}` is deprecated with no `x-sunset` date",
            where_.clone().unwrap_or_default()
        ),
        ChangeKind::ContractPartial => format!("not fully verified — {detail}"),
    };

    match (&where_, change.kind) {
        // The endpoint is already in the text for these.
        (
            _,
            ChangeKind::EndpointRemoved
            | ChangeKind::MethodRemoved
            | ChangeKind::EndpointAdded
            | ChangeKind::EndpointPathChanged
            | ChangeKind::PathParameterRenamed
            | ChangeKind::DeprecatedNoSunset
            | ChangeKind::RemovedWithoutDeprecation,
        ) => core,
        (Some(endpoint), _) => format!("{core} in `{endpoint}`"),
        (None, _) => core,
    }
}

#[must_use]
pub fn contract_unreachable(contract: &str, details: &str, span: Option<Span>) -> Finding {
    Finding {
        affects: Vec::new(),
        note: None,
        rule_id: "contract-unreachable",
        severity: Severity::Error,
        contract: contract.to_owned(),
        message: format!("contract `{contract}` is unreachable: {details}"),
        method: None,
        path: None,
        pointer: String::new(),
        subject: None,
        span,
    }
}

/// A finding about a *file* rather than a configured contract.
///
/// `contract` is left empty because there is no contract: the whole point of
/// `contract-unconfigured` is that nothing declares this file. Putting the
/// path there rendered as `contract: .github/workflows/api-tests.yaml`, which
/// reads as though it were one.
#[must_use]
pub fn about_file(rule_id: &'static str, file: &str, message: String) -> Finding {
    Finding {
        path: Some(file.to_owned()),
        ..synthetic(rule_id, "", message)
    }
}

/// Build a finding for a rule that has no `Change` behind it — drift, and the
/// suppression-hygiene rules.
#[must_use]
pub fn synthetic(rule_id: &'static str, contract: &str, message: String) -> Finding {
    let rule = catalogue::lookup(rule_id).expect("synthetic findings use catalogued rules");
    Finding {
        affects: Vec::new(),
        note: None,
        rule_id: rule.id,
        severity: rule.severity,
        contract: contract.to_owned(),
        message,
        method: None,
        path: None,
        pointer: String::new(),
        subject: None,
        span: None,
    }
}

/// Apply a contract's suppressions.
///
/// `report_stale` is off for a scoped run: a suppression for a contract or an
/// endpoint that this run never looked at legitimately matches nothing, and
/// reporting it as dead would make suppressions unusable in a pre-commit hook.
#[must_use]
pub fn apply_suppressions(
    findings: Vec<Finding>,
    contract: &str,
    suppressions: &[Suppression],
    as_of: Option<&str>,
    report_stale: bool,
) -> Vec<Finding> {
    let mut output = Vec::new();
    let mut matched = vec![false; suppressions.len()];

    for finding in findings {
        // An integrity finding is never suppressible: a suppression that could
        // hide `contract-unreachable` would let the gate stop gating silently.
        if matches!(
            finding.rule_id,
            "contract-unreachable" | "stale-allow" | "expired-allow"
        ) {
            output.push(finding);
            continue;
        }

        let Some(index) = suppressions
            .iter()
            .position(|suppression| suppression_matches(suppression, &finding))
        else {
            output.push(finding);
            continue;
        };
        matched[index] = true;

        if suppression_is_expired(&suppressions[index], as_of) {
            output.push(Finding {
                rule_id: "expired-allow",
                severity: Severity::Error,
                contract: contract.to_owned(),
                message: format!(
                    "suppression for `{}` expired on `{}` — it no longer applies to: {}",
                    suppressions[index].rule,
                    suppressions[index].expires.as_deref().unwrap_or("unknown"),
                    finding.message
                ),
                ..finding
            });
        }
    }

    if report_stale {
        for (index, suppression) in suppressions.iter().enumerate() {
            if !matched[index] {
                output.push(synthetic(
                    "stale-allow",
                    contract,
                    format!(
                        "suppression for rule `{}`{} matched nothing (reason given: {})",
                        suppression.rule,
                        suppression
                            .endpoint
                            .as_ref()
                            .map(|endpoint| format!(" on `{endpoint}`"))
                            .unwrap_or_default(),
                        suppression.reason
                    ),
                ));
            }
        }
    }

    output.sort();
    output.dedup();
    output
}

fn suppression_matches(suppression: &Suppression, finding: &Finding) -> bool {
    if suppression.rule != finding.rule_id {
        return false;
    }
    if let Some(endpoint) = &suppression.endpoint
        && finding.endpoint().as_deref() != Some(endpoint.as_str())
    {
        return false;
    }
    if let Some(field) = &suppression.field
        && !pointer_names(&finding.pointer).any(|segment| segment == *field)
    {
        // Matching on the rendered message would let `field = "id"` suppress
        // anything whose message happens to contain those two letters.
        return false;
    }
    true
}

/// The decoded segments of a JSON pointer.
fn pointer_names(pointer: &str) -> impl Iterator<Item = String> + '_ {
    pointer
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| segment.replace("~1", "/").replace("~0", "~"))
}

fn suppression_is_expired(suppression: &Suppression, as_of: Option<&str>) -> bool {
    let (Some(expires), Some(as_of)) = (suppression.expires.as_deref(), as_of) else {
        return false;
    };
    match (parse_date(expires), parse_date(as_of)) {
        (Some(expiry), Some(now)) => now > expiry,
        // A date brake cannot read is treated as already expired rather than
        // as never expiring: the failure is loud instead of silent.
        _ => true,
    }
}

/// Parse `YYYY-MM-DD` into a comparable tuple.
///
/// Ordering is all that is needed, so no calendar arithmetic is involved.
#[must_use]
pub fn parse_date(date: &str) -> Option<(u32, u32, u32)> {
    let mut parts = date.trim().split('-');
    let year: u32 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some((year, month, day))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compare::ChangeKind;
    use crate::contract::EndpointKey;

    fn sample_span() -> Span {
        Span::new("api/openapi.yaml", 2, 3, "/paths/~1payments/get")
    }

    fn change(kind: ChangeKind, detail: &str, pointer: &str) -> Change {
        Change {
            kind,
            endpoint: Some(EndpointKey {
                method: "GET".to_owned(),
                path: "/payments/{id}".to_owned(),
            }),
            pointer: pointer.to_owned(),
            detail: detail.to_owned(),
            subject: None,
            span: sample_span(),
        }
    }

    fn finding(rule_id: &'static str, pointer: &str) -> Finding {
        Finding {
            affects: Vec::new(),
            note: None,
            rule_id,
            severity: Severity::Error,
            contract: "payments".to_owned(),
            message: "something happened".to_owned(),
            method: Some("GET".to_owned()),
            path: Some("/payments/{id}".to_owned()),
            pointer: pointer.to_owned(),
            subject: None,
            span: Some(sample_span()),
        }
    }

    fn suppression(rule: &str, endpoint: Option<&str>, field: Option<&str>) -> Suppression {
        Suppression {
            rule: rule.to_owned(),
            endpoint: endpoint.map(ToOwned::to_owned),
            field: field.map(ToOwned::to_owned),
            reason: "accepted after a deprecation window".to_owned(),
            expires: None,
        }
    }

    #[test]
    fn maps_a_change_to_its_rule_and_carries_the_contract() {
        let findings = evaluate(
            &[change(ChangeKind::EndpointRemoved, "", "/paths/~1p/get")],
            "payments",
            Compatibility::Wire,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule_id, "endpoint-removed");
        assert_eq!(findings[0].severity, Severity::Error);
        assert_eq!(findings[0].contract, "payments");
        assert!(findings[0].message.contains("GET /payments/{id}"));
    }

    #[test]
    fn a_finding_carries_the_ways_out_bound_to_its_subject() {
        let findings = evaluate(
            &[Change {
                kind: ChangeKind::ResponseFieldRemoved,
                endpoint: Some(EndpointKey {
                    method: "GET".to_owned(),
                    path: "/payments/{id}".to_owned(),
                }),
                pointer: "/paths/~1payments/get/responses/200/customer_id".to_owned(),
                detail: "field `customer_id`".to_owned(),
                subject: Some("customer_id".to_owned()),
                span: sample_span(),
            }],
            "payments",
            Compatibility::WireJson,
        );

        let remediations = findings[0].remediations();
        assert_eq!(remediations.len(), 3);
        assert_eq!(remediations[0].strategy, "deprecate-then-remove");
        assert!(
            remediations[0].summary.contains("`customer_id`"),
            "the strategy must name the field: {}",
            remediations[0].summary
        );
        assert!(
            remediations[2].summary.contains("`GET /payments/{id}`"),
            "and the endpoint where that is what it is about: {}",
            remediations[2].summary
        );
    }

    #[test]
    fn a_purely_additive_finding_suggests_nothing() {
        let findings = evaluate(
            &[change(
                ChangeKind::ResponseFieldAdded,
                "field `extra`",
                "/extra",
            )],
            "payments",
            Compatibility::Strict,
        );
        assert!(
            findings[0].remediations().is_empty(),
            "nothing was broken, so there is nothing to route around"
        );
    }

    #[test]
    fn a_subjectless_finding_still_reads_as_a_sentence() {
        let findings = evaluate(
            &[change(ChangeKind::EndpointRemoved, "", "/paths/~1p/get")],
            "payments",
            Compatibility::Wire,
        );
        for remediation in findings[0].remediations() {
            // `{id}` in a path template is not a placeholder; the two
            // named ones are.
            assert!(
                !remediation.summary.contains("{subject}")
                    && !remediation.summary.contains("{endpoint}")
                    && !remediation.summary.contains("``"),
                "unbound placeholder or empty backticks: {}",
                remediation.summary
            );
        }
    }

    #[test]
    fn wire_level_hides_a_wire_json_rule() {
        let changes = [change(ChangeKind::ResponseFieldRemoved, "field `x`", "/p")];
        assert!(evaluate(&changes, "c", Compatibility::Wire).is_empty());
        assert_eq!(evaluate(&changes, "c", Compatibility::WireJson).len(), 1);
    }

    #[test]
    fn surface_level_adds_generated_client_rules() {
        let changes = [change(
            ChangeKind::OperationIdChanged,
            "`a` became `b`",
            "/p",
        )];
        assert!(evaluate(&changes, "c", Compatibility::WireJson).is_empty());
        assert_eq!(evaluate(&changes, "c", Compatibility::Surface).len(), 1);
    }

    #[test]
    fn strict_level_adds_purely_additive_rules() {
        let changes = [change(ChangeKind::ResponseFieldAdded, "field `x`", "/p")];
        assert!(evaluate(&changes, "c", Compatibility::Surface).is_empty());
        assert_eq!(evaluate(&changes, "c", Compatibility::Strict).len(), 1);
    }

    #[test]
    fn each_level_is_a_superset_of_the_one_below() {
        let changes = [
            change(ChangeKind::EndpointRemoved, "", "/a"),
            change(ChangeKind::ResponseFieldRemoved, "field `x`", "/b"),
            change(ChangeKind::OperationIdChanged, "`a` became `b`", "/c"),
            change(ChangeKind::ResponseFieldAdded, "field `y`", "/d"),
        ];
        let counts = [
            Compatibility::Wire,
            Compatibility::WireJson,
            Compatibility::Surface,
            Compatibility::Strict,
        ]
        .map(|level| evaluate(&changes, "c", level).len());

        assert_eq!(counts, [1, 2, 3, 4], "levels must differ and nest");
    }

    #[test]
    fn suppression_hides_a_matching_finding() {
        let output = apply_suppressions(
            vec![finding("endpoint-removed", "/paths/~1p/get")],
            "payments",
            &[suppression(
                "endpoint-removed",
                Some("GET /payments/{id}"),
                None,
            )],
            None,
            true,
        );
        assert!(output.is_empty(), "{output:?}");
    }

    #[test]
    fn suppression_for_another_endpoint_does_not_match() {
        let output = apply_suppressions(
            vec![finding("endpoint-removed", "/paths/~1p/get")],
            "payments",
            &[suppression("endpoint-removed", Some("GET /other"), None)],
            None,
            false,
        );
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].rule_id, "endpoint-removed");
    }

    #[test]
    fn field_suppression_matches_a_pointer_segment_not_a_substring() {
        let target = finding(
            "response-field-removed",
            "/paths/~1p/get/responses/200/content/application~1json/legacy_reference",
        );
        let other = finding(
            "response-field-removed",
            "/paths/~1p/get/responses/200/content/application~1json/customer_id",
        );

        let suppression = suppression("response-field-removed", None, Some("legacy_reference"));
        assert!(
            apply_suppressions(
                vec![target],
                "c",
                std::slice::from_ref(&suppression),
                None,
                false
            )
            .is_empty()
        );
        // The same suppression must not swallow a different field.
        assert_eq!(
            apply_suppressions(vec![other], "c", &[suppression], None, false).len(),
            1
        );
    }

    #[test]
    fn a_short_field_name_does_not_suppress_unrelated_findings() {
        // `id` appears inside `customer_id`; a substring match would hide it.
        let unrelated = finding("response-field-removed", "/responses/200/customer_id");
        let output = apply_suppressions(
            vec![unrelated],
            "c",
            &[suppression("response-field-removed", None, Some("id"))],
            None,
            false,
        );
        assert_eq!(output.len(), 1);
    }

    #[test]
    fn integrity_findings_cannot_be_suppressed() {
        let output = apply_suppressions(
            vec![contract_unreachable("payments", "file missing", None)],
            "payments",
            &[suppression("contract-unreachable", None, None)],
            None,
            false,
        );
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].rule_id, "contract-unreachable");
    }

    #[test]
    fn expired_suppression_reports_instead_of_hiding() {
        let mut expiring = suppression("endpoint-removed", Some("GET /payments/{id}"), None);
        expiring.expires = Some("2026-01-01".to_owned());

        let output = apply_suppressions(
            vec![finding("endpoint-removed", "/p")],
            "payments",
            &[expiring.clone()],
            Some("2026-02-01"),
            true,
        );
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].rule_id, "expired-allow");

        // Before the expiry date it still suppresses.
        let output = apply_suppressions(
            vec![finding("endpoint-removed", "/p")],
            "payments",
            &[expiring],
            Some("2025-12-01"),
            true,
        );
        assert!(output.is_empty());
    }

    #[test]
    fn an_unreadable_expiry_date_expires_rather_than_never_expiring() {
        let mut broken = suppression("endpoint-removed", None, None);
        broken.expires = Some("soon".to_owned());

        let output = apply_suppressions(
            vec![finding("endpoint-removed", "/p")],
            "c",
            &[broken],
            Some("2026-02-01"),
            false,
        );
        assert_eq!(output[0].rule_id, "expired-allow");
    }

    #[test]
    fn stale_suppressions_are_reported_only_when_asked() {
        let unused = [suppression("method-removed", Some("POST /elsewhere"), None)];

        let scoped = apply_suppressions(Vec::new(), "c", &unused, None, false);
        assert!(
            scoped.is_empty(),
            "a scoped run must not call a suppression dead just because it was out of scope"
        );

        let whole_repository = apply_suppressions(Vec::new(), "c", &unused, None, true);
        assert_eq!(whole_repository.len(), 1);
        assert_eq!(whole_repository[0].rule_id, "stale-allow");
        assert!(whole_repository[0].message.contains("deprecation window"));
    }

    #[test]
    fn formats_suggested_suppression_block() {
        let mut f = finding("response-field-removed", "/responses/200/customer_id");
        f.subject = Some("customer_id".to_owned());
        let suggestion = f.suggest_suppression(Some("migration to v2"), Some("2026-12-31"));
        assert!(suggestion.contains("[[contract.allow]]"));
        assert!(suggestion.contains("rule = \"response-field-removed\""));
        assert!(suggestion.contains("endpoint = \"GET /payments/{id}\""));
        assert!(suggestion.contains("field = \"customer_id\""));
        assert!(suggestion.contains("reason = \"migration to v2\""));
        assert!(suggestion.contains("expires = \"2026-12-31\""));
    }

    #[test]
    fn parses_and_rejects_dates() {
        assert_eq!(parse_date("2026-09-01"), Some((2026, 9, 1)));
        assert!(parse_date("2026-13-01").is_none());
        assert!(parse_date("2026-09").is_none());
        assert!(parse_date("not-a-date").is_none());
    }
}
