//! `Contract` × `Contract` → `Change`.
//!
//! Format-agnostic, and that is the architectural bet of the whole tool: if a
//! `match` on format appears in this module, the ingest normalisation is
//! under-specified and the fix belongs in `contract/`, not here. See
//! `design/03-implementation-plan.md` §2.

use std::collections::{BTreeMap, BTreeSet};

use crate::contract::{
    Contract, Endpoint, EndpointKey, Parameter, Payload, SecurityScheme, Span, TypeRef,
};

pub mod types;
use types::{TypeDirection, TypeIssue};

/// What changed between two contracts, with enough location to point at it.
///
/// One `ChangeKind` maps to exactly one rule ID, so `rules/` is a table rather
/// than a second copy of this vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Change {
    pub kind: ChangeKind,
    pub endpoint: Option<EndpointKey>,
    /// A JSON pointer into the contract artifact the span refers to.
    pub pointer: String,
    /// The specifics — a field name, a status code, a reason. Rules compose
    /// the human-readable message from this; `compare` does no presentation.
    pub detail: String,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangeKind {
    // §5.1 endpoint surface
    EndpointRemoved,
    MethodRemoved,
    EndpointPathChanged,
    EndpointAdded,
    OperationIdChanged,
    PathParameterRenamed,

    // §5.2 request compatibility
    ParamAddedRequired,
    ParamBecameRequired,
    ParamRemoved,
    ParamTypeNarrowed,
    ParamLocationChanged,
    ParamAddedOptional,
    RequestMediaTypeRemoved,

    // §5.3 response compatibility
    ResponseFieldRemoved,
    ResponseFieldOptional,
    ResponseFieldAdded,
    ResponseTypeChanged,
    ResponseEnumExtended,
    ResponseStatusRemoved,
    ResponseStatusAdded,
    ResponseMediaTypeRemoved,

    // Wire identity, for formats that have it
    FieldRenamed,
    FieldNumberChanged,

    // §5.4 security
    SecurityAdded,
    SecurityRemoved,
    SecuritySchemeChanged,

    // §5.5 deprecation hygiene
    RemovedWithoutDeprecation,
    DeprecatedNoSunset,

    // §5.6 integrity
    ContractPartial,
}

/// Compare two contracts. Level gating happens in `rules/`, not here: this
/// reports everything it can see and the level decides what is worth saying.
#[must_use]
pub fn compare_contracts(base: &Contract, head: &Contract) -> Vec<Change> {
    let mut changes = compare_endpoint_sets(base, head);
    changes.extend(compare_endpoint_details(base, head));
    changes.extend(compare_security_schemes(base, head));
    changes.sort();
    changes.dedup();
    changes
}

/// The endpoint set alone — what M1 shipped, kept as its own entry point
/// because it is the cheapest useful comparison.
#[must_use]
pub fn compare_endpoint_sets(base: &Contract, head: &Contract) -> Vec<Change> {
    let base_operations = operation_id_index(base);
    let head_operations = operation_id_index(head);

    let mut moved = BTreeSet::new();
    let mut changes = Vec::new();

    for (operation_id, base_key) in &base_operations {
        let Some(head_key) = head_operations.get(operation_id) else {
            continue;
        };
        if base_key.path == head_key.path && base_key.method == head_key.method {
            continue;
        }

        moved.insert(base_key.clone());
        let head_endpoint = &head.endpoints[head_key];
        // A path template whose shape is unchanged but whose parameter names
        // moved breaks generated clients only, which is a `surface` concern.
        let kind = if path_shape(&base_key.path) == path_shape(&head_key.path) {
            ChangeKind::PathParameterRenamed
        } else {
            ChangeKind::EndpointPathChanged
        };
        changes.push(Change {
            kind,
            endpoint: Some(head_key.clone()),
            pointer: head_endpoint.span.pointer.clone(),
            detail: format!(
                "operationId `{operation_id}` moved from `{} {}` to `{} {}`",
                base_key.method, base_key.path, head_key.method, head_key.path
            ),
            span: head_endpoint.span.clone(),
        });
    }

    // Precomputed rather than scanned per endpoint: a linear scan inside the
    // loop makes a large specification quadratic for no reason.
    let head_paths = head
        .endpoints
        .keys()
        .map(|key| key.path.as_str())
        .collect::<BTreeSet<_>>();

    for (base_key, base_endpoint) in &base.endpoints {
        if moved.contains(base_key) || head.endpoints.contains_key(base_key) {
            continue;
        }

        let kind = if head_paths.contains(base_key.path.as_str()) {
            ChangeKind::MethodRemoved
        } else {
            ChangeKind::EndpointRemoved
        };
        changes.push(Change {
            kind,
            endpoint: Some(base_key.clone()),
            pointer: base_endpoint.span.pointer.clone(),
            detail: String::new(),
            span: base_endpoint.span.clone(),
        });

        // The sanctioned path for any removal is deprecate, ship, wait,
        // remove. A team that follows it never needs a suppression.
        if !base_endpoint.deprecated {
            changes.push(Change {
                kind: ChangeKind::RemovedWithoutDeprecation,
                endpoint: Some(base_key.clone()),
                pointer: base_endpoint.span.pointer.clone(),
                detail: format!("endpoint `{} {}`", base_key.method, base_key.path),
                span: base_endpoint.span.clone(),
            });
        }
    }

    for (head_key, head_endpoint) in &head.endpoints {
        let known = base.endpoints.contains_key(head_key)
            || head_endpoint
                .operation_id
                .as_ref()
                .is_some_and(|id| base_operations.contains_key(id));
        if !known {
            changes.push(Change {
                kind: ChangeKind::EndpointAdded,
                endpoint: Some(head_key.clone()),
                pointer: head_endpoint.span.pointer.clone(),
                detail: String::new(),
                span: head_endpoint.span.clone(),
            });
        }
    }

    changes.sort();
    changes
}

fn compare_endpoint_details(base: &Contract, head: &Contract) -> Vec<Change> {
    let mut changes = Vec::new();
    let head_by_operation_id = operation_id_index(head);

    for (base_key, base_endpoint) in &base.endpoints {
        // Follow an endpoint that moved, so a path change does not mask every
        // field-level break underneath it.
        let (head_key, head_endpoint) = match head.endpoints.get(base_key) {
            Some(endpoint) => (base_key, endpoint),
            None => {
                let Some(moved_key) = base_endpoint
                    .operation_id
                    .as_ref()
                    .and_then(|id| head_by_operation_id.get(id))
                else {
                    continue;
                };
                (moved_key, &head.endpoints[moved_key])
            }
        };

        let mut push = |kind: ChangeKind, pointer: String, detail: String, span: Span| {
            changes.push(Change {
                kind,
                endpoint: Some(head_key.clone()),
                pointer,
                detail,
                span,
            });
        };

        compare_operation_id(base_endpoint, head_endpoint, &mut push);
        compare_deprecation(head_endpoint, &mut push);
        compare_parameters(base_endpoint, head_endpoint, &mut push);
        compare_request(base_endpoint, head_endpoint, &mut push);
        compare_responses(base_endpoint, head_endpoint, &mut push);
        compare_security(base_endpoint, head_endpoint, &mut push);
    }

    changes
}

type Push<'a> = dyn FnMut(ChangeKind, String, String, Span) + 'a;

fn compare_operation_id(base: &Endpoint, head: &Endpoint, push: &mut Push<'_>) {
    if let (Some(base_id), Some(head_id)) = (&base.operation_id, &head.operation_id)
        && base_id != head_id
    {
        push(
            ChangeKind::OperationIdChanged,
            head.span.pointer.clone(),
            format!("`{base_id}` became `{head_id}`"),
            head.span.clone(),
        );
    }
}

fn compare_deprecation(head: &Endpoint, push: &mut Push<'_>) {
    if head.deprecated && head.sunset.is_none() {
        push(
            ChangeKind::DeprecatedNoSunset,
            head.span.pointer.clone(),
            String::new(),
            head.span.clone(),
        );
    }
}

fn compare_parameters(base: &Endpoint, head: &Endpoint, push: &mut Push<'_>) {
    let key = |parameter: &Parameter| format!("{}:{}", parameter.location, parameter.name);
    let by_key = |endpoint: &Endpoint| {
        endpoint
            .parameters
            .iter()
            .map(|parameter| (key(parameter), parameter.clone()))
            .collect::<BTreeMap<_, _>>()
    };
    let base_params = by_key(base);
    let head_params = by_key(head);

    // A parameter that moved between `query`, `path`, `header` and `cookie`
    // keeps its name; without this it would read as one removal and one
    // unrelated addition, and an optional one would only warn.
    let base_by_name = index_by_name(&base.parameters);
    let head_by_name = index_by_name(&head.parameters);
    let mut relocated = BTreeSet::new();
    for (name, base_parameter) in &base_by_name {
        let Some(head_parameter) = head_by_name.get(name) else {
            continue;
        };
        if base_parameter.location != head_parameter.location {
            relocated.insert(key(base_parameter));
            relocated.insert(key(head_parameter));
            push(
                ChangeKind::ParamLocationChanged,
                head_parameter.span.pointer.clone(),
                format!(
                    "parameter `{name}` moved from `{}` to `{}`",
                    base_parameter.location, head_parameter.location
                ),
                head_parameter.span.clone(),
            );
        }
    }

    for (parameter_key, base_parameter) in &base_params {
        if relocated.contains(parameter_key) {
            continue;
        }
        let Some(head_parameter) = head_params.get(parameter_key) else {
            push(
                ChangeKind::ParamRemoved,
                base_parameter.span.pointer.clone(),
                parameter_key.clone(),
                base_parameter.span.clone(),
            );
            continue;
        };

        if !base_parameter.required && head_parameter.required {
            push(
                ChangeKind::ParamBecameRequired,
                head_parameter.span.pointer.clone(),
                parameter_key.clone(),
                head_parameter.span.clone(),
            );
        }
        for issue in types::compare(
            &base_parameter.ty,
            &head_parameter.ty,
            TypeDirection::Request,
        ) {
            emit_issue(
                issue,
                &base_parameter.span,
                &head_parameter.span,
                &format!("parameter `{parameter_key}`"),
                push,
            );
        }
    }

    for (parameter_key, head_parameter) in &head_params {
        if base_params.contains_key(parameter_key) || relocated.contains(parameter_key) {
            continue;
        }
        push(
            if head_parameter.required {
                ChangeKind::ParamAddedRequired
            } else {
                ChangeKind::ParamAddedOptional
            },
            head_parameter.span.pointer.clone(),
            parameter_key.clone(),
            head_parameter.span.clone(),
        );
    }
}

fn index_by_name(parameters: &[Parameter]) -> BTreeMap<String, Parameter> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for parameter in parameters {
        *counts.entry(parameter.name.as_str()).or_default() += 1;
    }
    parameters
        .iter()
        // A name that appears in two locations at once is genuinely ambiguous;
        // guessing which one moved would invent a finding.
        .filter(|parameter| counts[parameter.name.as_str()] == 1)
        .map(|parameter| (parameter.name.clone(), parameter.clone()))
        .collect()
}

fn compare_request(base: &Endpoint, head: &Endpoint, push: &mut Push<'_>) {
    let (Some(base_request), Some(head_request)) = (&base.request, &head.request) else {
        return;
    };

    for (media_type, base_ty) in &base_request.media_types {
        let Some(head_ty) = head_request.media_types.get(media_type) else {
            push(
                ChangeKind::RequestMediaTypeRemoved,
                base_request.span.pointer.clone(),
                media_type.clone(),
                base_request.span.clone(),
            );
            continue;
        };
        for issue in types::compare(base_ty, head_ty, TypeDirection::Request) {
            emit_issue(
                issue,
                &base_request.span,
                &head_request.span,
                &format!("request body (`{media_type}`)"),
                push,
            );
        }
    }
}

fn compare_responses(base: &Endpoint, head: &Endpoint, push: &mut Push<'_>) {
    for (status, base_response) in &base.responses {
        let Some(head_response) = head.responses.get(status) else {
            push(
                ChangeKind::ResponseStatusRemoved,
                base_response.span.pointer.clone(),
                status.clone(),
                base_response.span.clone(),
            );
            continue;
        };
        compare_payload_media_types(status, base_response, head_response, push);
    }

    for (status, head_response) in &head.responses {
        if !base.responses.contains_key(status) {
            push(
                ChangeKind::ResponseStatusAdded,
                head_response.span.pointer.clone(),
                status.clone(),
                head_response.span.clone(),
            );
        }
    }
}

fn compare_payload_media_types(
    status: &str,
    base_response: &Payload,
    head_response: &Payload,
    push: &mut Push<'_>,
) {
    for (media_type, base_ty) in &base_response.media_types {
        let Some(head_ty) = head_response.media_types.get(media_type) else {
            push(
                ChangeKind::ResponseMediaTypeRemoved,
                base_response.span.pointer.clone(),
                format!("`{media_type}` from response `{status}`"),
                base_response.span.clone(),
            );
            continue;
        };
        for issue in types::compare(base_ty, head_ty, TypeDirection::Response) {
            emit_issue(
                issue,
                &base_response.span,
                &head_response.span,
                &format!("response `{status}`"),
                push,
            );
        }
    }

    // A response that had a schema and now has none is not "no change": the
    // payload became unverifiable, which is exactly what partial reports.
    if !base_response.media_types.is_empty() && head_response.media_types.is_empty() {
        push(
            ChangeKind::ContractPartial,
            head_response.span.pointer.clone(),
            format!("response `{status}` no longer declares a schema"),
            head_response.span.clone(),
        );
    }
}

fn compare_security(base: &Endpoint, head: &Endpoint, push: &mut Push<'_>) {
    let names = |endpoint: &Endpoint| {
        endpoint
            .security
            .iter()
            .map(|requirement| requirement.name.clone())
            .collect::<BTreeSet<_>>()
    };
    let base_names = names(base);
    let head_names = names(head);

    for added in head_names.difference(&base_names) {
        push(
            ChangeKind::SecurityAdded,
            head.span.pointer.clone(),
            added.clone(),
            head.span.clone(),
        );
    }
    for removed in base_names.difference(&head_names) {
        push(
            ChangeKind::SecurityRemoved,
            head.span.pointer.clone(),
            removed.clone(),
            head.span.clone(),
        );
    }

    // Requiring more scopes than before is strengthening, and it locks out a
    // token that used to be sufficient.
    for base_requirement in &base.security {
        let Some(head_requirement) = head
            .security
            .iter()
            .find(|candidate| candidate.name == base_requirement.name)
        else {
            continue;
        };
        let added = head_requirement
            .scopes
            .difference(&base_requirement.scopes)
            .cloned()
            .collect::<Vec<_>>();
        if !added.is_empty() {
            push(
                ChangeKind::SecurityAdded,
                head.span.pointer.clone(),
                format!(
                    "scheme `{}` now requires scopes: {}",
                    head_requirement.name,
                    added.join(", ")
                ),
                head.span.clone(),
            );
        }
    }
}

fn compare_security_schemes(base: &Contract, head: &Contract) -> Vec<Change> {
    let mut changes = Vec::new();
    for (name, base_scheme) in &base.security_schemes {
        let Some(head_scheme) = head.security_schemes.get(name) else {
            continue;
        };
        if let Some(detail) = scheme_difference(base_scheme, head_scheme) {
            changes.push(Change {
                kind: ChangeKind::SecuritySchemeChanged,
                endpoint: None,
                pointer: head_scheme.span.pointer.clone(),
                detail: format!("`{name}`: {detail}"),
                span: head_scheme.span.clone(),
            });
        }
    }
    changes
}

fn scheme_difference(base: &SecurityScheme, head: &SecurityScheme) -> Option<String> {
    if base.ty != head.ty {
        return Some(format!("type changed from `{}` to `{}`", base.ty, head.ty));
    }
    if base.scheme != head.scheme {
        return Some(format!(
            "scheme changed from `{}` to `{}`",
            base.scheme.as_deref().unwrap_or("none"),
            head.scheme.as_deref().unwrap_or("none")
        ));
    }
    if base.location != head.location {
        return Some(format!(
            "location changed from `{}` to `{}`",
            base.location.as_deref().unwrap_or("none"),
            head.location.as_deref().unwrap_or("none")
        ));
    }
    if base.flows != head.flows {
        return Some("the declared OAuth flows changed".to_owned());
    }
    None
}

/// Lift a type-level issue into a `Change`, prefixing the location so a
/// message reads "response `200` field `id`" rather than a bare pointer.
///
/// Takes both spans and reports against whichever artifact contains the thing
/// being described: a removed field only exists in the baseline, so pointing at
/// the head would send a reader to a line that does not mention it.
fn emit_issue(
    issue: TypeIssue,
    base_span: &Span,
    head_span: &Span,
    context: &str,
    push: &mut Push<'_>,
) {
    let removal = matches!(
        issue,
        TypeIssue::ResponseFieldRemoved { .. } | TypeIssue::RequestVariantRemoved { .. }
    );
    let span = if removal { base_span } else { head_span };
    let full = |pointer: &str| format!("{}{pointer}", span.pointer);
    let at = |pointer: &str| {
        if pointer.is_empty() {
            context.to_owned()
        } else {
            format!("{context} at `{pointer}`")
        }
    };

    let (kind, pointer, detail) = match issue {
        TypeIssue::RequestTypeNarrowed { pointer, reason } => (
            ChangeKind::ParamTypeNarrowed,
            full(&pointer),
            format!("{}: {reason}", at(&pointer)),
        ),
        TypeIssue::RequestFieldAddedRequired { pointer, field } => (
            ChangeKind::ParamAddedRequired,
            full(&pointer),
            format!("{}: required field `{field}`", at(&pointer)),
        ),
        TypeIssue::RequestFieldAddedOptional { pointer, field } => (
            ChangeKind::ParamAddedOptional,
            full(&pointer),
            format!("{}: optional field `{field}`", at(&pointer)),
        ),
        TypeIssue::RequestVariantRemoved { pointer } => (
            ChangeKind::ParamTypeNarrowed,
            full(&pointer),
            format!("{}: a accepted union variant was removed", at(&pointer)),
        ),
        TypeIssue::ResponseTypeChanged { pointer, reason } => (
            ChangeKind::ResponseTypeChanged,
            full(&pointer),
            format!("{}: {reason}", at(&pointer)),
        ),
        TypeIssue::ResponseFieldRemoved { pointer, field } => (
            ChangeKind::ResponseFieldRemoved,
            full(&pointer),
            format!("{}: field `{field}`", at(&pointer)),
        ),
        TypeIssue::ResponseFieldOptional { pointer, field } => (
            ChangeKind::ResponseFieldOptional,
            full(&pointer),
            format!("{}: field `{field}`", at(&pointer)),
        ),
        TypeIssue::ResponseFieldAdded { pointer, field } => (
            ChangeKind::ResponseFieldAdded,
            full(&pointer),
            format!("{}: field `{field}`", at(&pointer)),
        ),
        TypeIssue::ResponseEnumExtended { pointer } => (
            ChangeKind::ResponseEnumExtended,
            full(&pointer),
            at(&pointer),
        ),
        TypeIssue::ResponseVariantAdded { pointer } => (
            ChangeKind::ResponseEnumExtended,
            full(&pointer),
            format!("{}: a new union variant may now be returned", at(&pointer)),
        ),
        TypeIssue::FieldRenamed { pointer, from, to } => (
            ChangeKind::FieldRenamed,
            full(&pointer),
            format!("{}: `{from}` became `{to}`", at(&pointer)),
        ),
        TypeIssue::FieldNumberChanged {
            pointer,
            field,
            from,
            to,
        } => (
            ChangeKind::FieldNumberChanged,
            full(&pointer),
            format!(
                "{}: field `{field}` moved from number {from} to {to}",
                at(&pointer)
            ),
        ),
        TypeIssue::Partial { pointer, detail } => (
            ChangeKind::ContractPartial,
            full(&pointer),
            format!("{}: {detail}", at(&pointer)),
        ),
    };

    push(kind, pointer, detail, span.clone());
}

/// The path template with every parameter name blanked, so `/p/{id}` and
/// `/p/{payment_id}` compare equal but `/p/{id}` and `/p/{id}/detail` do not.
fn path_shape(path: &str) -> String {
    let mut shape = String::new();
    let mut in_parameter = false;
    for character in path.chars() {
        match character {
            '{' => {
                in_parameter = true;
                shape.push('{');
            }
            '}' => {
                in_parameter = false;
                shape.push('}');
            }
            _ if in_parameter => {}
            _ => shape.push(character),
        }
    }
    shape
}

fn operation_id_index(contract: &Contract) -> BTreeMap<String, EndpointKey> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for endpoint in contract.endpoints.values() {
        if let Some(operation_id) = &endpoint.operation_id {
            *counts.entry(operation_id.as_str()).or_default() += 1;
        }
    }

    let mut index = BTreeMap::new();
    for (key, endpoint) in &contract.endpoints {
        let Some(operation_id) = &endpoint.operation_id else {
            continue;
        };
        // A duplicated operationId is not a usable identity. Silently keeping
        // whichever one sorted last would move findings onto the wrong path.
        if counts[operation_id.as_str()] == 1 {
            index.insert(operation_id.clone(), key.clone());
        }
    }
    index
}

/// Every unmodelled construct reachable from a compared endpoint.
///
/// The contract-level companion to `TypeIssue::Partial`: it catches a
/// construct that could not be modelled on an endpoint whose payloads are
/// otherwise identical on both sides.
#[must_use]
pub fn partial_changes(contract: &Contract) -> Vec<Change> {
    let mut changes = Vec::new();
    for (key, endpoint) in &contract.endpoints {
        let mut payloads: Vec<(&str, &Payload)> = Vec::new();
        if let Some(request) = &endpoint.request {
            payloads.push(("request body", request));
        }
        for (status, response) in &endpoint.responses {
            payloads.push((status, response));
        }

        for (label, payload) in payloads {
            for ty in payload.media_types.values() {
                for kind in types::unmodelled_kinds(ty) {
                    changes.push(Change {
                        kind: ChangeKind::ContractPartial,
                        endpoint: Some(key.clone()),
                        pointer: payload.span.pointer.clone(),
                        detail: format!("{label}: {}", kind.describe()),
                        span: payload.span.clone(),
                    });
                }
            }
        }

        for parameter in &endpoint.parameters {
            if matches!(parameter.ty, TypeRef::Unknown(_)) {
                continue;
            }
            for kind in types::unmodelled_kinds(&parameter.ty) {
                changes.push(Change {
                    kind: ChangeKind::ContractPartial,
                    endpoint: Some(key.clone()),
                    pointer: parameter.span.pointer.clone(),
                    detail: format!("parameter `{}`: {}", parameter.name, kind.describe()),
                    span: parameter.span.clone(),
                });
            }
        }
    }
    changes.sort();
    changes.dedup();
    changes
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::contract::{
        Constraints, Field, MEDIA_ANY, Parameter, SecurityRequirement, UnmodelledKind, openapi,
    };

    fn span() -> Span {
        Span::new("api/openapi.yaml", 1, 1, "/paths")
    }

    fn scalar(ty: &str) -> TypeRef {
        TypeRef::Scalar {
            ty: ty.to_owned(),
            format: None,
            nullable: false,
            constraints: Constraints::default(),
        }
    }

    fn parameter(name: &str, location: &str, required: bool) -> Parameter {
        Parameter {
            name: name.to_owned(),
            location: location.to_owned(),
            required,
            deprecated: false,
            ty: scalar("string"),
            span: span(),
        }
    }

    fn endpoint(operation_id: Option<&str>) -> Endpoint {
        Endpoint {
            operation_id: operation_id.map(ToOwned::to_owned),
            deprecated: false,
            sunset: None,
            parameters: Vec::new(),
            request: None,
            responses: BTreeMap::new(),
            security: Vec::new(),
            span: span(),
        }
    }

    fn contract(entries: Vec<(&str, &str, Endpoint)>) -> Contract {
        let mut contract = Contract::empty();
        for (method, path, endpoint) in entries {
            contract.endpoints.insert(
                EndpointKey {
                    method: method.to_owned(),
                    path: path.to_owned(),
                },
                endpoint,
            );
        }
        contract
    }

    fn kinds(changes: &[Change]) -> Vec<ChangeKind> {
        changes.iter().map(|change| change.kind).collect()
    }

    #[test]
    fn emits_endpoint_removed_for_missing_path() {
        let base = contract(vec![(
            "GET",
            "/payments/{id}",
            endpoint(Some("getPayment")),
        )]);
        let changes = compare_endpoint_sets(&base, &Contract::empty());
        assert!(kinds(&changes).contains(&ChangeKind::EndpointRemoved));
    }

    #[test]
    fn emits_method_removed_when_the_path_survives() {
        let base = contract(vec![
            ("GET", "/payments", endpoint(Some("listPayments"))),
            ("POST", "/payments", endpoint(Some("createPayment"))),
        ]);
        let head = contract(vec![("GET", "/payments", endpoint(Some("listPayments")))]);

        let changes = compare_endpoint_sets(&base, &head);
        assert!(kinds(&changes).contains(&ChangeKind::MethodRemoved));
        assert!(!kinds(&changes).contains(&ChangeKind::EndpointRemoved));
    }

    #[test]
    fn removal_of_a_deprecated_endpoint_skips_the_hygiene_rule() {
        let mut deprecated = endpoint(Some("getPayment"));
        deprecated.deprecated = true;
        let base = contract(vec![("GET", "/payments/{id}", deprecated)]);

        let changes = compare_endpoint_sets(&base, &Contract::empty());
        assert!(kinds(&changes).contains(&ChangeKind::EndpointRemoved));
        assert!(!kinds(&changes).contains(&ChangeKind::RemovedWithoutDeprecation));
    }

    #[test]
    fn removal_of_a_live_endpoint_reports_the_hygiene_rule() {
        let base = contract(vec![(
            "GET",
            "/payments/{id}",
            endpoint(Some("getPayment")),
        )]);
        let changes = compare_endpoint_sets(&base, &Contract::empty());
        assert!(kinds(&changes).contains(&ChangeKind::RemovedWithoutDeprecation));
    }

    #[test]
    fn distinguishes_a_moved_path_from_a_renamed_path_parameter() {
        let base = contract(vec![(
            "GET",
            "/payments/{id}",
            endpoint(Some("getPayment")),
        )]);
        let renamed = contract(vec![(
            "GET",
            "/payments/{payment_id}",
            endpoint(Some("getPayment")),
        )]);
        let moved = contract(vec![(
            "GET",
            "/v2/payments/{id}",
            endpoint(Some("getPayment")),
        )]);

        assert!(
            kinds(&compare_endpoint_sets(&base, &renamed))
                .contains(&ChangeKind::PathParameterRenamed)
        );
        assert!(
            kinds(&compare_endpoint_sets(&base, &moved)).contains(&ChangeKind::EndpointPathChanged)
        );
    }

    #[test]
    fn a_duplicated_operation_id_is_not_used_as_an_identity() {
        let base = contract(vec![
            ("GET", "/a", endpoint(Some("shared"))),
            ("GET", "/b", endpoint(Some("shared"))),
        ]);
        let head = contract(vec![("GET", "/c", endpoint(Some("shared")))]);

        let changes = compare_endpoint_sets(&base, &head);
        // Both are reported gone rather than one being guessed as "moved".
        assert_eq!(
            kinds(&changes)
                .iter()
                .filter(|kind| **kind == ChangeKind::EndpointRemoved)
                .count(),
            2
        );
    }

    #[test]
    fn detects_a_parameter_moving_between_locations() {
        let mut base = endpoint(Some("getPayment"));
        base.parameters = vec![parameter("token", "query", false)];
        let mut head = endpoint(Some("getPayment"));
        head.parameters = vec![parameter("token", "header", false)];

        let changes = compare_contracts(
            &contract(vec![("GET", "/p", base)]),
            &contract(vec![("GET", "/p", head)]),
        );
        assert!(kinds(&changes).contains(&ChangeKind::ParamLocationChanged));
        // and not as an unrelated removal plus addition
        assert!(!kinds(&changes).contains(&ChangeKind::ParamRemoved));
        assert!(!kinds(&changes).contains(&ChangeKind::ParamAddedOptional));
    }

    #[test]
    fn detects_media_type_removal_in_both_directions() {
        let payload = |media: Vec<(&str, TypeRef)>| Payload {
            media_types: media
                .into_iter()
                .map(|(name, ty)| (name.to_owned(), ty))
                .collect(),
            span: span(),
        };
        let mut base = endpoint(Some("createPayment"));
        base.request = Some(payload(vec![
            ("application/json", scalar("string")),
            ("application/xml", scalar("string")),
        ]));
        base.responses.insert(
            "200".to_owned(),
            payload(vec![
                ("application/json", scalar("string")),
                ("text/csv", scalar("string")),
            ]),
        );

        let mut head = endpoint(Some("createPayment"));
        head.request = Some(payload(vec![("application/json", scalar("string"))]));
        head.responses.insert(
            "200".to_owned(),
            payload(vec![("application/json", scalar("string"))]),
        );

        let changes = compare_contracts(
            &contract(vec![("POST", "/p", base)]),
            &contract(vec![("POST", "/p", head)]),
        );
        assert!(kinds(&changes).contains(&ChangeKind::RequestMediaTypeRemoved));
        assert!(kinds(&changes).contains(&ChangeKind::ResponseMediaTypeRemoved));
    }

    #[test]
    fn detects_security_strengthening_and_relaxation() {
        let requirement = |name: &str, scopes: &[&str]| SecurityRequirement {
            name: name.to_owned(),
            scopes: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
        };
        let mut base = endpoint(Some("getPayment"));
        base.security = vec![requirement("legacy", &[])];
        let mut head = endpoint(Some("getPayment"));
        head.security = vec![requirement("oauth", &["write"])];

        let changes = compare_contracts(
            &contract(vec![("GET", "/p", base)]),
            &contract(vec![("GET", "/p", head)]),
        );
        assert!(kinds(&changes).contains(&ChangeKind::SecurityAdded));
        assert!(kinds(&changes).contains(&ChangeKind::SecurityRemoved));
    }

    #[test]
    fn a_moved_endpoint_still_has_its_payload_compared() {
        let response = |fields: Vec<(&str, bool)>| {
            let mut responses = BTreeMap::new();
            responses.insert(
                "200".to_owned(),
                Payload {
                    media_types: BTreeMap::from([(
                        MEDIA_ANY.to_owned(),
                        TypeRef::Object {
                            fields: fields
                                .into_iter()
                                .map(|(name, required)| {
                                    (name.to_owned(), Field::new(scalar("string"), required))
                                })
                                .collect(),
                            additional: true,
                            nullable: false,
                        },
                    )]),
                    span: span(),
                },
            );
            responses
        };

        let mut base = endpoint(Some("getPayment"));
        base.responses = response(vec![("id", true), ("legacy", true)]);
        let mut head = endpoint(Some("getPayment"));
        head.responses = response(vec![("id", true)]);

        let changes = compare_contracts(
            &contract(vec![("GET", "/payments/{id}", base)]),
            &contract(vec![("GET", "/v2/payments/{id}", head)]),
        );
        assert!(
            kinds(&changes).contains(&ChangeKind::ResponseFieldRemoved),
            "a path change must not mask the field removal underneath it: {changes:?}"
        );
    }

    #[test]
    fn partial_changes_finds_unmodelled_constructs_on_untouched_endpoints() {
        let mut endpoint = endpoint(Some("getPayment"));
        endpoint.responses.insert(
            "200".to_owned(),
            Payload::single(
                TypeRef::Unknown(UnmodelledKind::ExternalRef("common.yaml#/X".to_owned())),
                span(),
            ),
        );
        let contract = contract(vec![("GET", "/p", endpoint)]);

        let changes = partial_changes(&contract);
        assert_eq!(kinds(&changes), vec![ChangeKind::ContractPartial]);
    }

    #[test]
    fn faithful_openapi_30_to_31_translation_has_no_changes() {
        let openapi_30 = r#"
openapi: 3.0.3
paths:
  /payments/{id}:
    get:
      operationId: getPayment
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                type: object
                required: [id, note]
                properties:
                  id:
                    type: string
                  note:
                    type: string
                    nullable: true
                  tags:
                    type: array
                    items:
                      type: string
"#;
        let openapi_31 = r#"
openapi: 3.1.0
paths:
  /payments/{id}:
    get:
      operationId: getPayment
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                type: object
                required: [id, note]
                properties:
                  id:
                    type: string
                  note:
                    type: [string, "null"]
                  tags:
                    type: array
                    items:
                      type: string
"#;

        let base = openapi::ingest("api/openapi-30.yaml", openapi_30.as_bytes()).expect("30");
        let head = openapi::ingest("api/openapi-31.yaml", openapi_31.as_bytes()).expect("31");
        let changes = compare_contracts(&base, &head);

        assert!(changes.is_empty(), "unexpected changes: {changes:?}");
    }

    #[test]
    fn media_type_order_does_not_change_the_verdict() {
        let one = r#"
openapi: 3.1.0
paths:
  /p:
    get:
      operationId: getP
      responses:
        "200":
          description: ok
          content:
            application/json: { schema: { type: object, properties: { id: { type: string } } } }
            application/xml: { schema: { type: string } }
"#;
        let two = r#"
openapi: 3.1.0
paths:
  /p:
    get:
      operationId: getP
      responses:
        "200":
          description: ok
          content:
            application/xml: { schema: { type: string } }
            application/json: { schema: { type: object, properties: { id: { type: string } } } }
"#;
        let base = openapi::ingest("api/openapi.yaml", one.as_bytes()).expect("one");
        let head = openapi::ingest("api/openapi.yaml", two.as_bytes()).expect("two");
        assert!(
            compare_contracts(&base, &head).is_empty(),
            "reordering media types must not change the verdict"
        );
    }

    #[test]
    fn yaml_key_order_does_not_change_the_verdict() {
        let one = r#"
openapi: 3.1.0
paths:
  /p:
    get:
      operationId: getP
      deprecated: false
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                type: object
                properties:
                  alpha: { type: string }
                  beta: { type: integer }
"#;
        let two = r#"
openapi: 3.1.0
paths:
  /p:
    get:
      deprecated: false
      responses:
        "200":
          content:
            application/json:
              schema:
                properties:
                  beta: { type: integer }
                  alpha: { type: string }
                type: object
          description: ok
      operationId: getP
"#;
        let base = openapi::ingest("api/openapi.yaml", one.as_bytes()).expect("one");
        let head = openapi::ingest("api/openapi.yaml", two.as_bytes()).expect("two");
        assert!(
            compare_contracts(&base, &head).is_empty(),
            "reordering mapping keys must not change the verdict"
        );
    }

    #[test]
    fn compares_request_and_response_types_end_to_end() {
        let mut base = endpoint(Some("createPayment"));
        base.parameters = vec![Parameter {
            ty: TypeRef::Enum {
                values: BTreeSet::from(["safe".to_owned(), "fast".to_owned()]),
                numbers: BTreeMap::new(),
            },
            ..parameter("mode", "query", false)
        }];
        base.responses.insert(
            "200".to_owned(),
            Payload::single(
                TypeRef::Enum {
                    values: BTreeSet::from(["pending".to_owned()]),
                    numbers: BTreeMap::new(),
                },
                span(),
            ),
        );

        let mut head = endpoint(Some("createPayment"));
        head.parameters = vec![Parameter {
            ty: TypeRef::Enum {
                values: BTreeSet::from(["safe".to_owned()]),
                numbers: BTreeMap::new(),
            },
            ..parameter("mode", "query", true)
        }];
        head.responses.insert(
            "200".to_owned(),
            Payload::single(
                TypeRef::Enum {
                    values: BTreeSet::from(["pending".to_owned(), "paid".to_owned()]),
                    numbers: BTreeMap::new(),
                },
                span(),
            ),
        );

        let changes = compare_contracts(
            &contract(vec![("POST", "/payments", base)]),
            &contract(vec![("POST", "/payments", head)]),
        );
        let kinds = kinds(&changes);
        assert!(kinds.contains(&ChangeKind::ParamBecameRequired));
        assert!(kinds.contains(&ChangeKind::ParamTypeNarrowed));
        assert!(kinds.contains(&ChangeKind::ResponseEnumExtended));
    }
}
