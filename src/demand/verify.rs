//! The join, and verification as the comparator run sideways.
//!
//! `compare/types.rs` already answers "does head still satisfy what base
//! promised?". Put the consumer's expectation on the *base* side and the head
//! contract on the head side and those are precisely the questions to ask of a
//! consumer — `design/05-consumer-demand.md` §4. There is no second
//! comparator here, and that is the point: as the type comparison improves,
//! consumer verification improves with it and stays consistent with the
//! baseline diff.
//!
//! No baseline is involved. Demand is compared against `head` only: a
//! consumer's expectation is a statement about the contract as it is now.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    BindIssue, Bound, Demand, PathBinding, Route, Usage, UsageKind, Usages, bind_path,
    normalise_media,
};
use crate::compare::types::{self, TypeDirection, TypeIssue};
use crate::contract::{
    Constraints, Contract, Endpoint, EndpointKey, Field, MEDIA_ANY, Parameter, Payload, Span,
    TypeRef,
};
use crate::rules::{ConsumerRef, Finding, catalogue};

/// Headers a pact carries as fixture credentials rather than as a statement
/// about the contract's security scheme.
///
/// §3: inferring security from a fixture is how a tool starts being wrong
/// confidently, so a contract requiring one of these is never reported as
/// rejecting a consumer that did not record it.
const CREDENTIAL_HEADERS: &[&str] = &["authorization", "proxy-authorization", "cookie"];

/// An endpoint a route bound to, with the values its template segments took.
type BoundRoute = (EndpointKey, BTreeMap<String, String>);

/// Bind a demand to a contract, producing the expectation of §3.
#[must_use]
pub fn bind(demand: &Demand, contract: &Contract) -> Bound {
    let mut expectation = Contract::empty();
    let mut issues = Vec::new();
    let mut usage_index: BTreeMap<EndpointKey, Usages> = BTreeMap::new();

    // What the ingester itself could not model reaches the verdict as
    // `consumer-partial` rather than as silence.
    for unmodelled in &demand.unmodelled {
        issues.push(BindIssue {
            rule: "consumer-partial",
            message: format!(
                "`{}` was not modelled: {}",
                unmodelled.pointer,
                unmodelled.kind.describe()
            ),
            endpoint: None,
            subject: None,
            span: unmodelled.span.clone(),
        });
    }

    let templates: Vec<String> = contract
        .endpoints
        .keys()
        .map(|key| key.path.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    // Each route is bound once, so an unbindable one is reported once rather
    // than once per interaction that mentions it.
    let mut resolved: BTreeMap<Route, Option<BoundRoute>> = BTreeMap::new();

    for usage in &demand.usages {
        if !resolved.contains_key(&usage.route) {
            let outcome = resolve_route(&usage.route, contract, &templates, &mut issues, usage);
            resolved.insert(usage.route.clone(), outcome);
        }
        let Some((key, path_parameters)) = resolved[&usage.route].clone() else {
            continue;
        };
        let actual = &contract.endpoints[&key];

        usage_index
            .entry(key.clone())
            .or_insert_with(|| Usages::new(usage.span.clone()));

        // The endpoint exists in the expectation whether or not the consumer
        // declared anything about its payloads: `Endpoint` alone is a usage.
        let expected = expectation
            .endpoints
            .entry(key.clone())
            .or_insert_with(|| empty_endpoint(usage.span.clone()));

        // A consumer calling `/payments/abc` has declared that it sends `abc`
        // for `id`. That is what lets a later narrowing of `id` to `integer`
        // be reported as breaking *that consumer* rather than in the abstract.
        //
        // Every distinct value is recorded, because two interactions calling
        // `/payments/1` and `/payments/abc` have declared two things — but the
        // *same* value twice is one declaration, not two findings.
        for (name, value) in &path_parameters {
            if is_template_placeholder(value) {
                continue;
            }
            let ty = infer_scalar(value);
            if !expected
                .parameters
                .iter()
                .any(|existing| existing.name == *name && existing.ty == ty)
            {
                expected.parameters.push(Parameter {
                    name: name.clone(),
                    location: "path".to_owned(),
                    required: true,
                    deprecated: false,
                    ty,
                    span: usage.span.clone(),
                });
            }
            usage_index
                .get_mut(&key)
                .expect("just inserted")
                .subjects
                .insert(name.clone());
        }

        match &usage.kind {
            UsageKind::Endpoint => {}
            UsageKind::Parameter {
                name,
                location,
                value,
            } => {
                let ty = value
                    .as_deref()
                    .filter(|value| !is_template_placeholder(value))
                    .map_or_else(unconstrained_string, infer_scalar);
                // Two interactions sending the same value for the same
                // parameter are one declaration, not two: recording both would
                // report the same problem twice, at two different lines.
                if !expected.parameters.iter().any(|existing| {
                    existing.name == *name && existing.location == *location && existing.ty == ty
                }) {
                    expected.parameters.push(Parameter {
                        name: name.clone(),
                        location: location.clone(),
                        required: true,
                        deprecated: false,
                        ty,
                        span: usage.span.clone(),
                    });
                }
                usage_index
                    .get_mut(&key)
                    .expect("just inserted")
                    .subjects
                    .insert(name.clone());
            }
            UsageKind::Request { media_type, ty } => {
                let Some(request) = &actual.request else {
                    issues.push(issue_for(
                        "consumer-request-rejected",
                        format!(
                            "the contract documents no request body for `{key_display}`, and \
                             `{consumer}` sends one",
                            key_display = display(&key),
                            consumer = demand.consumer
                        ),
                        &key,
                        None,
                        usage,
                    ));
                    continue;
                };
                if request.media_types.is_empty() {
                    // The contract declares a body and no schema for it. That
                    // is unverifiable, not unmet: saying "rejected" here would
                    // be a confident claim brake cannot support. A consumer
                    // that declared no shape either has nothing to verify, so
                    // there is nothing to report.
                    if !declares_nothing(ty) {
                        issues.push(issue_for(
                            "consumer-partial",
                            format!(
                                "`{key_display}` declares a request body with no schema, so what \
                                 `{consumer}` sends was not verified",
                                key_display = display(&key),
                                consumer = demand.consumer
                            ),
                            &key,
                            None,
                            usage,
                        ));
                    }
                    continue;
                }
                let Some(resolved_media) = resolve_media(request, media_type) else {
                    issues.push(issue_for(
                        "consumer-request-rejected",
                        format!(
                            "`{consumer}` sends `{media_type}` to `{key_display}`, which the \
                             contract does not accept (it accepts: {accepted})",
                            consumer = demand.consumer,
                            key_display = display(&key),
                            accepted = list(request.media_types.keys())
                        ),
                        &key,
                        Some(normalise_media(media_type)),
                        usage,
                    ));
                    continue;
                };
                let head_ty = &request.media_types[&resolved_media];
                let mut notes = Vec::new();
                let reconciled = reconcile(ty, head_ty, TypeDirection::Request, &mut notes);
                note_issues(&notes, &key, usage, &mut issues);

                let payload = expected
                    .request
                    .get_or_insert_with(|| empty_payload(usage.span.clone()));
                payload.media_types.insert(resolved_media, reconciled);

                let mut names = BTreeSet::new();
                let mut paths = BTreeSet::new();
                collect_names(ty, "", &mut names, &mut paths);
                let entry = usage_index.get_mut(&key).expect("just inserted");
                entry.subjects.extend(names);
                entry.sends.extend(paths);
            }
            UsageKind::Response {
                status,
                media_type,
                ty,
            } => {
                let Some(resolved_status) = resolve_status(&actual.responses, status) else {
                    issues.push(issue_for(
                        "consumer-status-unmet",
                        format!(
                            "`{consumer}` expects status `{status}` from `{key_display}`, which \
                             the contract does not document (it documents: {documented})",
                            consumer = demand.consumer,
                            key_display = display(&key),
                            documented = list(actual.responses.keys())
                        ),
                        &key,
                        Some(status.clone()),
                        usage,
                    ));
                    continue;
                };
                let response = &actual.responses[&resolved_status];
                if response.media_types.is_empty() {
                    // A documented status with no schema, read by a consumer
                    // that recorded no body — a 404, usually — is satisfied,
                    // not unverified. Warning about it would be noise on the
                    // most common interaction in any pact directory.
                    if !declares_nothing(ty) {
                        issues.push(issue_for(
                            "consumer-partial",
                            format!(
                                "`{key_display}` documents `{resolved_status}` with no schema, \
                                 so what `{consumer}` reads was not verified",
                                key_display = display(&key),
                                consumer = demand.consumer
                            ),
                            &key,
                            Some(status.clone()),
                            usage,
                        ));
                    }
                    entry_for(&mut usage_index, &key)
                        .statuses
                        .insert(status.clone());
                    continue;
                }
                let Some(resolved_media) = resolve_media(response, media_type) else {
                    issues.push(issue_for(
                        "consumer-status-unmet",
                        format!(
                            "`{consumer}` reads `{status} {media_type}` from `{key_display}`, and \
                             the contract documents `{resolved_status}` only as {documented}",
                            consumer = demand.consumer,
                            key_display = display(&key),
                            documented = list(response.media_types.keys())
                        ),
                        &key,
                        Some(normalise_media(media_type)),
                        usage,
                    ));
                    continue;
                };
                let head_ty = &response.media_types[&resolved_media];
                let mut notes = Vec::new();
                let reconciled = reconcile(ty, head_ty, TypeDirection::Response, &mut notes);
                note_issues(&notes, &key, usage, &mut issues);

                let payload = expected
                    .responses
                    .entry(resolved_status.clone())
                    .or_insert_with(|| empty_payload(usage.span.clone()));
                payload.media_types.insert(resolved_media, reconciled);

                let mut names = BTreeSet::new();
                let mut paths = BTreeSet::new();
                collect_names(ty, "", &mut names, &mut paths);
                let entry = usage_index.get_mut(&key).expect("just inserted");
                entry.subjects.insert(status.clone());
                entry.subjects.extend(names);
                entry.statuses.insert(status.clone());
                entry.reads.extend(paths);
            }
        }
    }

    issues.sort_by(|a, b| (a.rule, &a.message, &a.span).cmp(&(b.rule, &b.message, &b.span)));
    issues.dedup();

    Bound {
        expectation,
        issues,
        usage_index,
    }
}

/// Bind one usage's route, reporting why it could not be bound.
fn resolve_route(
    route: &Route,
    contract: &Contract,
    templates: &[String],
    issues: &mut Vec<BindIssue>,
    usage: &Usage,
) -> Option<BoundRoute> {
    match bind_path(&route.path, templates.iter().map(String::as_str)) {
        PathBinding::Bound {
            template,
            parameters,
        } => {
            let key = EndpointKey {
                method: route.method.clone(),
                path: template,
            };
            if contract.endpoints.contains_key(&key) {
                return Some((key, parameters));
            }
            issues.push(BindIssue {
                rule: "consumer-endpoint-unmet",
                message: format!(
                    "the contract documents `{}` but not the `{}` method a consumer calls",
                    key.path, key.method
                ),
                endpoint: Some(key),
                subject: None,
                span: usage.span.clone(),
            });
            None
        }
        PathBinding::Ambiguous(candidates) => {
            issues.push(BindIssue {
                rule: "consumer-path-ambiguous",
                message: format!(
                    "`{route}` matches {} equally, so the expectation was not verified \
                     against either",
                    list(candidates.iter())
                ),
                endpoint: None,
                subject: None,
                span: usage.span.clone(),
            });
            None
        }
        PathBinding::Unmatched => {
            issues.push(BindIssue {
                rule: "consumer-endpoint-unmet",
                message: format!(
                    "a consumer calls `{route}`, which the contract does not document"
                ),
                endpoint: None,
                subject: None,
                span: usage.span.clone(),
            });
            None
        }
    }
}

fn issue_for(
    rule: &'static str,
    message: String,
    key: &EndpointKey,
    subject: Option<String>,
    usage: &Usage,
) -> BindIssue {
    BindIssue {
        rule,
        message,
        endpoint: Some(key.clone()),
        subject,
        span: usage.span.clone(),
    }
}

fn note_issues(notes: &[String], key: &EndpointKey, usage: &Usage, issues: &mut Vec<BindIssue>) {
    for note in notes {
        issues.push(issue_for(
            "consumer-partial",
            format!("`{}`: {note}", display(key)),
            key,
            None,
            usage,
        ));
    }
}

// ── verification ────────────────────────────────────────────────────────────

/// Verify one demand against the head contract, as findings.
///
/// `contract` is the configured contract name, so a repository with several
/// produces findings somebody can attribute.
#[must_use]
pub fn verify(demand: &Demand, contract_name: &str, head: &Contract) -> Vec<Finding> {
    let bound = bind(demand, head);
    let reference = ConsumerRef {
        consumer: demand.consumer.clone(),
        source: demand.source.clone(),
        span: Span::new(&demand.source, 1, 1, ""),
    };

    let mut findings: Vec<Finding> = bound
        .issues
        .iter()
        .map(|issue| {
            consumer_finding(
                issue.rule,
                contract_name,
                &prefix(&demand.consumer, &issue.message),
                issue.endpoint.as_ref(),
                issue.subject.clone(),
                &issue.span,
                &reference,
            )
        })
        .collect();

    for (key, expected) in &bound.expectation.endpoints {
        let Some(actual) = head.endpoints.get(key) else {
            continue;
        };
        findings.extend(verify_parameters(
            demand,
            contract_name,
            key,
            expected,
            actual,
            &reference,
        ));
        findings.extend(verify_payloads(
            demand,
            contract_name,
            key,
            expected,
            actual,
            &reference,
        ));
    }

    findings.sort();
    findings.dedup();
    findings
}

fn verify_parameters(
    demand: &Demand,
    contract_name: &str,
    key: &EndpointKey,
    expected: &Endpoint,
    actual: &Endpoint,
    reference: &ConsumerRef,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    let sent: BTreeMap<(String, String), &Parameter> = expected
        .parameters
        .iter()
        .map(|parameter| {
            (
                (
                    parameter.location.to_ascii_lowercase(),
                    parameter.name.to_ascii_lowercase(),
                ),
                parameter,
            )
        })
        .collect();

    // A parameter value the contract would reject. This is the one place a
    // literal value is checked rather than a shape, and it is why binding
    // records `{id}` = `abc` at all: narrowing `id` to `integer` is then
    // authoritative for *this* consumer rather than in the abstract.
    for parameter in &expected.parameters {
        let Some(declared) = actual.parameters.iter().find(|candidate| {
            candidate.name.eq_ignore_ascii_case(&parameter.name)
                && candidate.location.eq_ignore_ascii_case(&parameter.location)
        }) else {
            continue;
        };
        if let TypeRef::Scalar { ty, .. } = &parameter.ty
            && ty == "string"
            && let Some(reason) = unsatisfied(&parameter.ty, &declared.ty)
        {
            findings.push(consumer_finding(
                "consumer-request-rejected",
                contract_name,
                &format!(
                    "`{consumer}` sends `{name}` in the {location} of `{endpoint}`, and {reason}",
                    consumer = demand.consumer,
                    name = parameter.name,
                    location = parameter.location,
                    endpoint = display(key),
                ),
                Some(key),
                Some(parameter.name.clone()),
                &parameter.span,
                reference,
            ));
        }
    }

    // A required parameter the consumer does not send. Credential headers are
    // excluded: a pact carrying a bearer token says nothing about which scheme
    // the contract should require.
    for declared in &actual.parameters {
        if !declared.required
            || CREDENTIAL_HEADERS.contains(&declared.name.to_ascii_lowercase().as_str())
        {
            continue;
        }
        let looked_up = (
            declared.location.to_ascii_lowercase(),
            declared.name.to_ascii_lowercase(),
        );
        if sent.contains_key(&looked_up) {
            continue;
        }
        findings.push(consumer_finding(
            "consumer-request-rejected",
            contract_name,
            &format!(
                "`{endpoint}` requires `{name}` in the {location}, and `{consumer}` does not \
                 send it",
                endpoint = display(key),
                name = declared.name,
                location = declared.location,
                consumer = demand.consumer,
            ),
            Some(key),
            Some(declared.name.clone()),
            &expected.span,
            reference,
        ));
    }

    findings
}

fn verify_payloads(
    demand: &Demand,
    contract_name: &str,
    key: &EndpointKey,
    expected: &Endpoint,
    actual: &Endpoint,
    reference: &ConsumerRef,
) -> Vec<Finding> {
    let mut findings = Vec::new();

    if let (Some(expected_request), Some(actual_request)) = (&expected.request, &actual.request) {
        for (media_type, expected_ty) in &expected_request.media_types {
            let Some(actual_ty) = actual_request.media_types.get(media_type) else {
                continue;
            };
            findings.extend(map_issues(
                types::compare(expected_ty, actual_ty, TypeDirection::Request),
                demand,
                contract_name,
                key,
                &format!("request body (`{media_type}`)"),
                &expected_request.span,
                reference,
            ));
        }
    }

    for (status, expected_response) in &expected.responses {
        let Some(actual_response) = actual.responses.get(status) else {
            continue;
        };
        for (media_type, expected_ty) in &expected_response.media_types {
            let Some(actual_ty) = actual_response.media_types.get(media_type) else {
                continue;
            };
            findings.extend(map_issues(
                types::compare(expected_ty, actual_ty, TypeDirection::Response),
                demand,
                contract_name,
                key,
                &format!("response `{status}`"),
                &expected_response.span,
                reference,
            ));
        }
    }

    findings
}

/// The `TypeIssue` → consumer rule table of §4.
///
/// Deliberately partial. Three cases are *not* mapped, and each omission is
/// load-bearing:
///
/// - `ResponseFieldOptional`. The expectation marks every field the consumer
///   observed as `required` — that is what pact's own verification asserts —
///   and a contract marks almost none of them so. Mapping it would fire on
///   every field of every interaction, which is a false positive per field.
/// - The additive issues (`ResponseFieldAdded`, `ResponseEnumExtended`,
///   `RequestFieldAddedOptional`, `ResponseVariantAdded`). A contract that
///   produces *more* than a consumer reads has not broken it.
/// - `FieldRenamed` / `FieldNumberChanged`. Those are wire-identity facts
///   between two versions of a contract; there is no second version here.
fn consumer_rule(issue: &TypeIssue) -> Option<&'static str> {
    match issue {
        TypeIssue::ResponseFieldRemoved { .. } | TypeIssue::ResponseTypeChanged { .. } => {
            Some("consumer-field-unmet")
        }
        TypeIssue::RequestFieldAddedRequired { .. }
        | TypeIssue::RequestTypeNarrowed { .. }
        | TypeIssue::RequestVariantRemoved { .. } => Some("consumer-request-rejected"),
        TypeIssue::Partial { .. } => Some("consumer-partial"),
        _ => None,
    }
}

fn map_issues(
    located: Vec<types::Located>,
    demand: &Demand,
    contract_name: &str,
    key: &EndpointKey,
    context: &str,
    fallback: &Span,
    reference: &ConsumerRef,
) -> Vec<Finding> {
    located
        .into_iter()
        .filter_map(|located| {
            let rule = consumer_rule(&located.issue)?;
            let (pointer, detail, subject) = describe(&located.issue);
            // The span points at the *interaction*: that is the evidence. A
            // head-side issue carries the contract's span instead, which is
            // the right file for making the change and the wrong one for
            // saying who is affected.
            let span = located
                .span
                .filter(|span| span.file == demand.source)
                .unwrap_or_else(|| fallback.clone());
            let where_ = if pointer.is_empty() {
                context.to_owned()
            } else {
                format!("{context} at `{pointer}`")
            };
            Some(consumer_finding(
                rule,
                contract_name,
                &format!(
                    "`{consumer}` and `{endpoint}` disagree: {where_} — {detail}",
                    consumer = demand.consumer,
                    endpoint = display(key),
                ),
                Some(key),
                subject,
                &span,
                reference,
            ))
        })
        .collect()
}

fn describe(issue: &TypeIssue) -> (String, String, Option<String>) {
    let tail = |pointer: &str| {
        pointer
            .rsplit('/')
            .find(|segment| !segment.is_empty())
            .map(|segment| segment.replace("~1", "/").replace("~0", "~"))
    };
    match issue {
        TypeIssue::ResponseFieldRemoved { pointer, field } => (
            pointer.clone(),
            format!("the contract does not produce field `{field}`"),
            Some(field.clone()),
        ),
        TypeIssue::ResponseTypeChanged { pointer, reason } => (
            pointer.clone(),
            format!("the contract produces a different shape: {reason}"),
            tail(pointer),
        ),
        TypeIssue::RequestFieldAddedRequired { pointer, field } => (
            pointer.clone(),
            format!("the contract requires field `{field}`, which is not sent"),
            Some(field.clone()),
        ),
        TypeIssue::RequestTypeNarrowed { pointer, reason } => (
            pointer.clone(),
            format!("the contract would reject the request: {reason}"),
            tail(pointer),
        ),
        TypeIssue::RequestVariantRemoved { pointer } => (
            pointer.clone(),
            "the contract no longer accepts this variant".to_owned(),
            tail(pointer),
        ),
        TypeIssue::Partial { pointer, detail } => (
            pointer.clone(),
            format!("not fully verified — {detail}"),
            tail(pointer),
        ),
        other => (String::new(), format!("{other:?}"), None),
    }
}

/// Build a consumer finding.
///
/// `pointer` is a JSON pointer into the *demand* artifact rather than the
/// contract, because that is where the span is and the two must agree: a SARIF
/// fingerprint built from a pointer into a different file than the location it
/// carries would move whenever the contract did.
fn consumer_finding(
    rule_id: &'static str,
    contract: &str,
    message: &str,
    endpoint: Option<&EndpointKey>,
    subject: Option<String>,
    span: &Span,
    reference: &ConsumerRef,
) -> Finding {
    let rule = catalogue::lookup(rule_id).expect("consumer rules are catalogued");
    Finding {
        rule_id: rule.id,
        severity: rule.severity,
        contract: contract.to_owned(),
        message: message.to_owned(),
        method: endpoint.map(|key| key.method.clone()),
        path: endpoint.map(|key| key.path.clone()),
        pointer: span.pointer.clone(),
        subject,
        span: Some(span.clone()),
        affects: vec![ConsumerRef {
            span: span.clone(),
            ..reference.clone()
        }],
        note: None,
    }
}

fn prefix(consumer: &str, message: &str) -> String {
    if message.contains(&format!("`{consumer}`")) {
        message.to_owned()
    } else {
        format!("`{consumer}`: {message}")
    }
}

// ── reconciliation ──────────────────────────────────────────────────────────

/// Line the consumer's inferred type up with the contract's, so the comparison
/// only reports what the demand actually says.
///
/// A demand is *silent* about nullability, string formats and value bounds: a
/// pact example is one recorded value, not a schema. Comparing an inferred
/// `string` against a `string` with `format: uuid` as though the demand had
/// declared "no format" would report a narrowing the consumer never claimed —
/// a false positive, which §1 of the thesis calls the thing that gets a hook
/// uninstalled. So those are copied across, and only the parts the demand
/// genuinely asserts — which fields exist, and their broad shape — are
/// compared.
///
/// What it cannot line up it says so about, and the note becomes
/// `consumer-partial`.
fn reconcile(
    base: &TypeRef,
    head: &TypeRef,
    direction: TypeDirection,
    notes: &mut Vec<String>,
) -> TypeRef {
    // "The field is there" and nothing more: adopt the contract's shape, so
    // only absence is ever reported.
    if matches!(base, TypeRef::Scalar { ty, .. } if ty == super::ANY_SCALAR) {
        return head.clone();
    }

    match (base, head) {
        (_, TypeRef::Unknown(_)) | (TypeRef::Unknown(_), _) => head.clone(),
        (_, TypeRef::OneOf { .. }) if !matches!(base, TypeRef::OneOf { .. }) => {
            notes.push(
                "the contract declares a union here, which a consumer example cannot be \
                 checked against"
                    .to_owned(),
            );
            head.clone()
        }
        (_, TypeRef::Cycle(name)) => {
            notes.push(format!(
                "the contract's type recurses at `{name}`, which was not followed"
            ));
            head.clone()
        }
        (
            TypeRef::Scalar { ty, .. },
            TypeRef::Scalar {
                format,
                nullable,
                constraints,
                ty: head_ty,
            },
        ) => TypeRef::Scalar {
            // The type *name* is the one thing a recorded value does assert,
            // so a `string` where the contract now says `integer` is still a
            // narrowing. Everything else comes from the contract.
            ty: if ty == "null" {
                head_ty.clone()
            } else {
                ty.clone()
            },
            format: format.clone(),
            nullable: *nullable,
            constraints: constraints.clone(),
        },
        // A recorded value is a member, not the member list: the consumer has
        // not declared which of an enum's values it needs.
        (TypeRef::Scalar { .. }, TypeRef::Enum { .. }) => head.clone(),
        (
            TypeRef::Array { items, .. },
            TypeRef::Array {
                items: head_items,
                nullable,
            },
        ) => TypeRef::Array {
            items: Box::new(reconcile(items, head_items, direction, notes)),
            nullable: *nullable,
        },
        (
            TypeRef::Object { fields, .. },
            TypeRef::Object {
                fields: head_fields,
                additional,
                nullable,
            },
        ) => {
            let mut reconciled = BTreeMap::new();
            for (name, field) in fields {
                match head_fields.get(name) {
                    Some(head_field) => {
                        reconciled.insert(
                            name.clone(),
                            Field {
                                ty: reconcile(&field.ty, &head_field.ty, direction, notes),
                                required: field.required,
                                deprecated: false,
                                number: head_field.number,
                                span: field.span.clone(),
                            },
                        );
                    }
                    // A field the consumer sends that the contract does not
                    // document is only rejected where the contract actually
                    // closes its object. An API that ignores unknown input
                    // tolerates it, and reporting it there would be the
                    // confident-nonsense failure mode.
                    None if direction == TypeDirection::Request && *additional => {}
                    None => {
                        reconciled.insert(name.clone(), field.clone());
                    }
                }
            }
            TypeRef::Object {
                fields: reconciled,
                additional: *additional,
                nullable: *nullable,
            }
        }
        _ => base.clone(),
    }
}

// ── value checking ──────────────────────────────────────────────────────────

/// Why the contract would reject this recorded value, if it would.
fn unsatisfied(sent: &TypeRef, declared: &TypeRef) -> Option<String> {
    let TypeRef::Scalar { constraints, .. } = sent else {
        return None;
    };
    // The recorded value travels as the scalar's pattern slot: a URL segment
    // or a header is always a string on the wire, so its *text* is the only
    // thing there is to check, and checking the inferred type name instead
    // would report `42` as failing `integer`.
    let value = constraints.pattern.as_deref()?;

    match declared {
        TypeRef::Scalar {
            ty, constraints, ..
        } => {
            let numeric = match ty.as_str() {
                "integer" => value.parse::<i64>().is_ok(),
                "number" => value.parse::<f64>().is_ok(),
                "boolean" => matches!(value, "true" | "false"),
                _ => true,
            };
            if !numeric {
                return Some(format!(
                    "the contract declares it `{ty}`, which `{value}` is not"
                ));
            }
            if let Some(max) = constraints.max_length
                && value.chars().count() as u64 > max
            {
                return Some(format!(
                    "the contract caps it at {max} characters and `{value}` is longer"
                ));
            }
            if let Some(min) = constraints.min_length
                && (value.chars().count() as u64) < min
            {
                return Some(format!(
                    "the contract requires at least {min} characters and `{value}` is shorter"
                ));
            }
            None
        }
        TypeRef::Enum { values, .. } if !values.contains(value) => Some(format!(
            "the contract accepts only {}, not `{value}`",
            list(values.iter())
        )),
        _ => None,
    }
}

// ── small helpers ───────────────────────────────────────────────────────────

fn empty_endpoint(span: Span) -> Endpoint {
    Endpoint {
        operation_id: None,
        deprecated: false,
        sunset: None,
        parameters: Vec::new(),
        request: None,
        responses: BTreeMap::new(),
        security: Vec::new(),
        span,
    }
}

fn empty_payload(span: Span) -> Payload {
    Payload {
        media_types: BTreeMap::new(),
        span,
    }
}

/// A scalar carrying the literal value that produced it.
///
/// The value rides in `constraints.pattern` so the contract model needs no new
/// field for something only demand has. [`unsatisfied`] is the only reader.
#[must_use]
pub fn infer_scalar(value: &str) -> TypeRef {
    TypeRef::Scalar {
        ty: "string".to_owned(),
        format: None,
        nullable: false,
        constraints: Constraints {
            pattern: Some(value.to_owned()),
            ..Constraints::default()
        },
    }
}

fn unconstrained_string() -> TypeRef {
    TypeRef::Scalar {
        ty: "string".to_owned(),
        format: None,
        nullable: false,
        constraints: Constraints::default(),
    }
}

/// The subjects recorded for one endpoint, created if this is the first.
fn entry_for<'a>(
    index: &'a mut BTreeMap<EndpointKey, Usages>,
    key: &EndpointKey,
) -> &'a mut Usages {
    index.get_mut(key).expect("the endpoint was recorded first")
}

/// Does this expectation say anything a comparison could check?
///
/// A pact interaction with no body — a 404, usually — produces an empty
/// object. It declares that the call happens and that the status comes back,
/// and nothing about the payload, so there is nothing to verify and nothing
/// left unverified.
fn declares_nothing(ty: &TypeRef) -> bool {
    matches!(ty, TypeRef::Object { fields, .. } if fields.is_empty())
}

/// `{id}` in a manifest is a template marker, not a value somebody sent.
fn is_template_placeholder(value: &str) -> bool {
    value.starts_with('{') && value.ends_with('}')
}

fn resolve_status(responses: &BTreeMap<String, Payload>, wanted: &str) -> Option<String> {
    if responses.contains_key(wanted) {
        return Some(wanted.to_owned());
    }
    if let Some(first) = wanted.chars().next() {
        for candidate in [format!("{first}XX"), format!("{first}xx")] {
            if responses.contains_key(&candidate) {
                return Some(candidate);
            }
        }
    }
    responses
        .contains_key("default")
        .then(|| "default".to_owned())
}

fn resolve_media(payload: &Payload, wanted: &str) -> Option<String> {
    let wanted = normalise_media(wanted);
    if let Some(exact) = payload
        .media_types
        .keys()
        .find(|key| normalise_media(key) == wanted)
    {
        return Some(exact.clone());
    }
    if payload.media_types.contains_key(MEDIA_ANY) {
        return Some(MEDIA_ANY.to_owned());
    }
    if wanted.is_empty() || wanted == MEDIA_ANY {
        // A demand with no media type of its own — a GraphQL operation, a
        // manifest — is checked against JSON where the contract has it, and
        // against the only body otherwise. Both are deterministic.
        if let Some(json) = payload
            .media_types
            .keys()
            .find(|key| normalise_media(key) == "application/json")
        {
            return Some(json.clone());
        }
        return payload.media_types.keys().next().cloned();
    }
    None
}

/// Every field name in a type, and every dotted path to one.
fn collect_names(
    ty: &TypeRef,
    prefix: &str,
    names: &mut BTreeSet<String>,
    paths: &mut BTreeSet<String>,
) {
    match ty {
        TypeRef::Object { fields, .. } => {
            for (name, field) in fields {
                let path = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}.{name}")
                };
                names.insert(name.clone());
                if matches!(field.ty, TypeRef::Object { .. } | TypeRef::Array { .. }) {
                    collect_names(&field.ty, &path, names, paths);
                } else {
                    paths.insert(path);
                }
            }
        }
        TypeRef::Array { items, .. } => collect_names(items, prefix, names, paths),
        _ => {
            if !prefix.is_empty() {
                paths.insert(prefix.to_owned());
            }
        }
    }
}

fn display(key: &EndpointKey) -> String {
    format!("{} {}", key.method, key.path)
}

fn list<T: std::fmt::Display>(items: impl Iterator<Item = T>) -> String {
    let rendered: Vec<String> = items.map(|item| format!("`{item}`")).collect();
    if rendered.is_empty() {
        "nothing".to_owned()
    } else {
        rendered.join(", ")
    }
}
