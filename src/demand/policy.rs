//! Attribution, and what a declared consumer does to a severity — §7.
//!
//! **There is no `consumer-break` rule.** A break is already a finding;
//! attribution is *evidence attached to it*. One broken field must not produce
//! one `response-field-removed` plus three `consumer-break`s, because a
//! developer then has to work out that four findings are one problem.

use std::collections::BTreeSet;

use super::load::BoundConsumer;
use crate::Severity;
use crate::config::{Completeness, ConsumerOptions, ConsumerPolicy};
use crate::contract::EndpointKey;
use crate::rules::{ConsumerRef, Finding, catalogue};

/// Attach the declared consumers each finding is evidence against.
///
/// A change is attributed to a consumer when the consumer's usage set contains
/// the change's `(endpoint, subject)` — which is why the subject is carried on
/// the finding explicitly rather than recovered from a JSON pointer. The
/// attribution join is that field, used a second time.
pub fn attribute(findings: &mut [Finding], consumers: &[BoundConsumer]) {
    for finding in findings.iter_mut() {
        // A consumer finding already names the consumer it is about, and
        // adding the same reference twice would double-count it.
        if finding.rule_id.starts_with("consumer-") {
            continue;
        }
        let (Some(method), Some(path)) = (&finding.method, &finding.path) else {
            continue;
        };
        let key = EndpointKey {
            method: method.clone(),
            path: path.clone(),
        };

        let mut affected: Vec<ConsumerRef> = consumers
            .iter()
            .filter_map(|consumer| {
                let usages = consumer.usage_index.get(&key)?;
                // No subject means the finding is about the endpoint itself,
                // and using the endpoint is enough to be affected by it.
                let relevant = finding
                    .subject
                    .as_ref()
                    .is_none_or(|subject| usages.subjects.contains(subject));
                relevant.then(|| ConsumerRef {
                    consumer: consumer.consumer.clone(),
                    source: consumer.source.clone(),
                    span: usages.span.clone(),
                })
            })
            .collect();
        affected.sort();
        affected.dedup();
        finding.affects = affected;
    }
}

/// Apply the configured policy — §7.1.
#[must_use]
pub fn apply(
    findings: Vec<Finding>,
    options: ConsumerOptions,
    declared_consumers: usize,
) -> Vec<Finding> {
    findings
        .into_iter()
        .map(|finding| match options.policy {
            ConsumerPolicy::Annotate => finding,
            ConsumerPolicy::Escalate => escalate(finding),
            ConsumerPolicy::Triage => triage(finding, options.completeness, declared_consumers),
        })
        .collect()
}

/// `param-removed` and `security-removed` are warnings precisely because brake
/// could not tell whether anyone relied on them. Now it can.
fn escalate(finding: Finding) -> Finding {
    if finding.severity != Severity::Warning || finding.affects.is_empty() {
        return finding;
    }
    let named = finding
        .affects
        .iter()
        .map(|reference| format!("`{}`", reference.consumer))
        .collect::<Vec<_>>()
        .join(", ");
    Finding {
        severity: Severity::Error,
        note: Some(format!(
            "escalated: {named} declared this, so brake no longer has to guess whether \
             anyone relies on it"
        )),
        ..finding
    }
}

/// Downgrade a break no declared consumer can observe — the one policy that
/// can lie, and therefore the constrained one.
///
/// Four constraints make it honest, and all four are enforced here:
///
/// 1. `completeness = "closed-world"`, an explicit, reviewable assertion by a
///    human that the declared set is exhaustive. brake cannot verify that
///    claim and does not pretend to.
/// 2. Only rules the catalogue marks observable by demand. A pact says nothing
///    about `operation-id-changed`, `security-scheme-changed` or
///    `path-parameter-renamed` — those break generated client *code*, which no
///    consumer declaration models. A rule demand cannot see is never
///    downgraded on the strength of demand's silence.
/// 3. The floor is `warning`. Nothing is downgraded to nothing, and nothing is
///    suppressed: a suppression still requires a reason, as it should.
/// 4. Every downgraded finding renders the assumption it rests on.
fn triage(finding: Finding, completeness: Completeness, declared: usize) -> Finding {
    if completeness != Completeness::ClosedWorld
        || finding.severity != Severity::Error
        || !finding.affects.is_empty()
    {
        return finding;
    }
    if !catalogue::lookup(finding.rule_id).is_some_and(|rule| rule.observable_by_demand) {
        return finding;
    }
    Finding {
        severity: Severity::Warning,
        note: Some(format!(
            "no declared consumer uses this — {declared} consumer{} declared, and brake \
             cannot know that is all of them",
            if declared == 1 { "" } else { "s" }
        )),
        ..finding
    }
}

/// Endpoints in the contract no declared consumer uses.
///
/// The one rule here that reports a suspected *absence*, which the thesis
/// forbids at commit time. Its caller is `analyze` and `brake consumers`, and
/// it is gated behind an explicit closed-world declaration — without one it is
/// a confident statement about consumers brake has never heard of.
#[must_use]
pub fn unused_surface(
    contract_name: &str,
    endpoints: impl Iterator<Item = EndpointKey>,
    consumers: &[BoundConsumer],
) -> Vec<Finding> {
    let used: BTreeSet<EndpointKey> = consumers
        .iter()
        .flat_map(|consumer| consumer.usage_index.keys().cloned())
        .collect();

    let mut findings: Vec<Finding> = endpoints
        .filter(|key| !used.contains(key))
        .map(|key| {
            let mut finding = crate::rules::synthetic(
                "consumer-surface-unused",
                contract_name,
                format!("no declared consumer uses `{} {}`", key.method, key.path),
            );
            finding.method = Some(key.method);
            finding.path = Some(key.path);
            finding
        })
        .collect();
    findings.sort();
    findings.dedup();
    findings
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::contract::Span;
    use crate::demand::Usages;

    fn consumer(subjects: &[&str]) -> BoundConsumer {
        let mut usages = Usages::empty(Span::new("pacts/web-checkout.json", 88, 1, "/i/0"));
        usages
            .subjects
            .extend(subjects.iter().map(|subject| (*subject).to_owned()));
        BoundConsumer {
            consumer: "web-checkout".to_owned(),
            source: "pacts/web-checkout.json".to_owned(),
            usage_index: BTreeMap::from([(
                EndpointKey {
                    method: "GET".to_owned(),
                    path: "/payments/{id}".to_owned(),
                },
                usages,
            )]),
        }
    }

    fn finding(rule_id: &'static str, severity: Severity, subject: Option<&str>) -> Finding {
        let mut finding = crate::rules::synthetic(rule_id, "payments", "something".to_owned());
        finding.severity = severity;
        finding.method = Some("GET".to_owned());
        finding.path = Some("/payments/{id}".to_owned());
        finding.subject = subject.map(ToOwned::to_owned);
        finding
    }

    #[test]
    fn a_field_a_consumer_reads_is_attributed_to_it() {
        let mut findings = vec![finding(
            "response-field-removed",
            Severity::Error,
            Some("customer_id"),
        )];
        attribute(&mut findings, &[consumer(&["customer_id", "id"])]);
        assert_eq!(findings[0].affects.len(), 1);
        assert_eq!(findings[0].affects[0].consumer, "web-checkout");
        assert_eq!(findings[0].affects[0].span.line, 88);
    }

    #[test]
    fn a_field_no_consumer_reads_is_not_attributed() {
        let mut findings = vec![finding(
            "response-field-removed",
            Severity::Error,
            Some("internal_note"),
        )];
        attribute(&mut findings, &[consumer(&["customer_id"])]);
        assert!(findings[0].affects.is_empty());
    }

    #[test]
    fn escalate_raises_a_warning_only_when_someone_is_affected() {
        let options = ConsumerOptions {
            policy: ConsumerPolicy::Escalate,
            completeness: Completeness::OpenWorld,
        };
        let mut affected = finding("param-removed", Severity::Warning, Some("expand"));
        attribute(
            std::slice::from_mut(&mut affected),
            &[consumer(&["expand"])],
        );
        let raised = apply(vec![affected], options, 1);
        assert_eq!(raised[0].severity, Severity::Error);
        assert!(
            raised[0]
                .note
                .as_ref()
                .is_some_and(|note| note.contains("web-checkout"))
        );

        let untouched = apply(
            vec![finding("param-removed", Severity::Warning, Some("other"))],
            options,
            1,
        );
        assert_eq!(untouched[0].severity, Severity::Warning);
    }

    #[test]
    fn triage_needs_a_closed_world_declaration() {
        let open = ConsumerOptions {
            policy: ConsumerPolicy::Triage,
            completeness: Completeness::OpenWorld,
        };
        let findings = apply(
            vec![finding(
                "response-field-removed",
                Severity::Error,
                Some("x"),
            )],
            open,
            3,
        );
        assert_eq!(
            findings[0].severity,
            Severity::Error,
            "an open world cannot justify a downgrade"
        );
    }

    #[test]
    fn triage_downgrades_to_warning_and_prints_its_assumption() {
        let closed = ConsumerOptions {
            policy: ConsumerPolicy::Triage,
            completeness: Completeness::ClosedWorld,
        };
        let findings = apply(
            vec![finding(
                "response-field-removed",
                Severity::Error,
                Some("x"),
            )],
            closed,
            3,
        );
        assert_eq!(findings[0].severity, Severity::Warning);
        let note = findings[0].note.as_deref().expect("an assumption");
        assert!(note.contains("3 consumers declared"), "{note}");
        assert!(note.contains("cannot know that is all of them"), "{note}");
    }

    #[test]
    fn triage_leaves_a_rule_demand_cannot_observe_alone() {
        let closed = ConsumerOptions {
            policy: ConsumerPolicy::Triage,
            completeness: Completeness::ClosedWorld,
        };
        for rule in [
            "operation-id-changed",
            "security-scheme-changed",
            "path-parameter-renamed",
        ] {
            let findings = apply(vec![finding(rule, Severity::Error, None)], closed, 3);
            assert_eq!(
                findings[0].severity,
                Severity::Error,
                "`{rule}` breaks generated client code, which no consumer declaration models"
            );
        }
    }

    #[test]
    fn nothing_is_ever_downgraded_below_warning() {
        let closed = ConsumerOptions {
            policy: ConsumerPolicy::Triage,
            completeness: Completeness::ClosedWorld,
        };
        let findings = apply(
            vec![finding("param-removed", Severity::Warning, None)],
            closed,
            0,
        );
        assert_eq!(findings[0].severity, Severity::Warning);
        assert!(findings[0].note.is_none(), "a warning is already the floor");
    }
}
