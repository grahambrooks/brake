//! GraphQL operation documents → [`Demand`].
//!
//! The strongest of the three sources, and the one that proves the model
//! generalised. A pact records an *example* response body, so a field
//! appearing in it is evidence the consumer reads it. A GraphQL selection set
//! is a statement: the selection *is* the field list, with no inference at
//! all.
//!
//! The routes produced here are the same ones `contract::graphql` produces
//! from a schema — `QUERY /query/<field>`, `MUTATION /mutation/<field>` — so
//! the join needs no GraphQL-specific branch. That is the whole test of §11's
//! M14: the same shape of finding, through the same join.

use std::collections::{BTreeMap, BTreeSet};

use apollo_parser::{Parser, cst, cst::CstNode};

use super::{Demand, Route, Usage, UsageKind, any_scalar};
use crate::contract::{Field, MEDIA_ANY, Span, TypeRef, Unmodelled, UnmodelledKind};

/// Ingest one operation document.
///
/// The consumer's name comes from the file: an operation document has no field
/// for it, and inventing one from the first operation's name would produce a
/// different consumer per query.
///
/// # Errors
///
/// Returns a message when the bytes are not UTF-8, are not GraphQL, or are
/// GraphQL containing no executable operation — a schema is not a demand.
pub fn ingest(source: &str, bytes: &[u8]) -> Result<Demand, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| format!("`{source}` is not valid UTF-8: {error}"))?;
    let tree = Parser::new(text).parse();
    let errors: Vec<String> = tree.errors().map(ToString::to_string).collect();
    if !errors.is_empty() {
        return Err(format!(
            "`{source}` is not a valid GraphQL document: {}",
            errors.join("; ")
        ));
    }

    let lines = LineIndex::new(text);
    let document = tree.document();

    // Fragments are resolved from this document only. One defined elsewhere is
    // reported, not guessed at.
    let mut fragments: BTreeMap<String, cst::SelectionSet> = BTreeMap::new();
    for definition in document.definitions() {
        if let cst::Definition::FragmentDefinition(fragment) = definition
            && let (Some(name), Some(selection)) =
                (fragment.fragment_name(), fragment.selection_set())
            && let Some(name) = name.name().map(|name| name.text().to_string())
        {
            fragments.insert(name, selection);
        }
    }

    let mut usages = BTreeSet::new();
    let mut unmodelled = Vec::new();
    let mut operations = 0usize;

    for definition in document.definitions() {
        let cst::Definition::OperationDefinition(operation) = definition else {
            continue;
        };
        operations += 1;

        let (method, segment) = match operation
            .operation_type()
            .map(|kind| kind.syntax().text().to_string())
            .as_deref()
            .map(str::trim)
        {
            Some("mutation") => ("MUTATION", "mutation"),
            Some("subscription") => {
                let offset = usize::from(operation.syntax().text_range().start());
                unmodelled.push(Unmodelled {
                    kind: UnmodelledKind::Unsupported(
                        "a `subscription` operation — brake models query and mutation roots only"
                            .to_owned(),
                    ),
                    pointer: "/subscription".to_owned(),
                    span: lines.span(source, offset, "/subscription"),
                });
                continue;
            }
            // An anonymous `{ … }` document is a query.
            _ => ("QUERY", "query"),
        };

        let Some(selection_set) = operation.selection_set() else {
            continue;
        };

        for selection in selection_set.selections() {
            let cst::Selection::Field(field) = selection else {
                let offset = usize::from(selection_set.syntax().text_range().start());
                unmodelled.push(Unmodelled {
                    kind: UnmodelledKind::Unsupported(
                        "a fragment at the operation root, which brake does not expand into \
                         root fields"
                            .to_owned(),
                    ),
                    pointer: format!("/{segment}"),
                    span: lines.span(source, offset, format!("/{segment}")),
                });
                continue;
            };
            let Some(name) = field.name().map(|name| name.text().to_string()) else {
                continue;
            };

            let pointer = format!("/{segment}/{name}");
            let offset = usize::from(field.syntax().text_range().start());
            let span = lines.span(source, offset, &pointer);
            let route = Route::new(method, &format!("/{segment}/{name}"));

            usages.insert(Usage {
                route: route.clone(),
                kind: UsageKind::Endpoint,
                span: span.clone(),
            });

            for argument in field
                .arguments()
                .into_iter()
                .flat_map(|arguments| arguments.arguments())
            {
                let Some(argument_name) = argument.name().map(|name| name.text().to_string())
                else {
                    continue;
                };
                usages.insert(Usage {
                    route: route.clone(),
                    kind: UsageKind::Parameter {
                        name: argument_name,
                        // The location `contract::graphql` gives a field
                        // argument, so the join lines them up without knowing
                        // either side is GraphQL.
                        location: "arg".to_owned(),
                        // A literal here is usually a variable reference, and
                        // a variable's value is not in this document.
                        value: None,
                    },
                    span: span.clone(),
                });
            }

            let ty = selection_type(
                field.selection_set().as_ref(),
                &fragments,
                &mut BTreeSet::new(),
                &pointer,
                &span,
                &mut unmodelled,
            );

            usages.insert(Usage {
                route,
                kind: UsageKind::Response {
                    status: "200".to_owned(),
                    media_type: MEDIA_ANY.to_owned(),
                    ty,
                },
                span,
            });
        }
    }

    if operations == 0 {
        return Err(format!(
            "`{source}` declares no GraphQL operation, so it is not a consumer declaration"
        ));
    }

    Ok(Demand {
        // The file names the consumer. `services/web-checkout/queries.graphql`
        // is `web-checkout`; a `[[consumer]]` entry overrides it.
        consumer: consumer_name(source),
        // An operation document does not name its provider, so the
        // `[[consumer]]` entry has to — and `consumer-provider-unmatched`
        // reports it when nothing does.
        provider: String::new(),
        source: source.to_owned(),
        usages,
        unmodelled,
    })
}

/// A selection set, as a type.
fn selection_type(
    selection_set: Option<&cst::SelectionSet>,
    fragments: &BTreeMap<String, cst::SelectionSet>,
    visiting: &mut BTreeSet<String>,
    pointer: &str,
    span: &Span,
    unmodelled: &mut Vec<Unmodelled>,
) -> TypeRef {
    let Some(selection_set) = selection_set else {
        // A leaf selection: the consumer reads the value and says nothing
        // about its shape.
        return any_scalar();
    };

    let mut fields: BTreeMap<String, Field> = BTreeMap::new();
    collect(
        selection_set,
        fragments,
        visiting,
        pointer,
        span,
        unmodelled,
        &mut fields,
    );

    TypeRef::Object {
        fields,
        // A selection set is what the consumer reads, never a claim that
        // nothing else exists.
        additional: true,
        nullable: false,
    }
}

#[allow(clippy::too_many_arguments)]
fn collect(
    selection_set: &cst::SelectionSet,
    fragments: &BTreeMap<String, cst::SelectionSet>,
    visiting: &mut BTreeSet<String>,
    pointer: &str,
    span: &Span,
    unmodelled: &mut Vec<Unmodelled>,
    fields: &mut BTreeMap<String, Field>,
) {
    for selection in selection_set.selections() {
        match selection {
            cst::Selection::Field(field) => {
                let Some(name) = field.name().map(|name| name.text().to_string()) else {
                    continue;
                };
                // `__typename` and friends are answered by the server, not by
                // the schema's field set.
                if name.starts_with("__") {
                    continue;
                }
                let nested = format!("{pointer}/{name}");
                let ty = selection_type(
                    field.selection_set().as_ref(),
                    fragments,
                    visiting,
                    &nested,
                    span,
                    unmodelled,
                );
                fields.insert(name, Field::new(ty, true));
            }
            cst::Selection::FragmentSpread(spread) => {
                let Some(name) = spread
                    .fragment_name()
                    .and_then(|name| name.name())
                    .map(|name| name.text().to_string())
                else {
                    continue;
                };
                let Some(target) = fragments.get(&name) else {
                    unmodelled.push(Unmodelled {
                        kind: UnmodelledKind::UnresolvableRef(format!("fragment `{name}`")),
                        pointer: pointer.to_owned(),
                        span: span.clone(),
                    });
                    continue;
                };
                // A fragment that spreads itself is not expandable, and
                // following it would not terminate.
                if !visiting.insert(name.clone()) {
                    unmodelled.push(Unmodelled {
                        kind: UnmodelledKind::Unsupported(format!("fragment `{name}` recurses")),
                        pointer: pointer.to_owned(),
                        span: span.clone(),
                    });
                    continue;
                }
                collect(
                    target, fragments, visiting, pointer, span, unmodelled, fields,
                );
                visiting.remove(&name);
            }
            cst::Selection::InlineFragment(inline) => {
                // An inline fragment narrows to one member of a union or
                // interface. Its fields are read *when the value is that
                // member*, so they are collected — a field that vanishes from
                // the member still breaks this consumer.
                if let Some(nested) = inline.selection_set() {
                    collect(
                        &nested, fragments, visiting, pointer, span, unmodelled, fields,
                    );
                }
            }
        }
    }
}

/// `services/web-checkout/queries.graphql` → `web-checkout`.
///
/// The directory before the file where there is one, because a file called
/// `queries.graphql` names nothing, and the directory almost always does.
fn consumer_name(source: &str) -> String {
    let segments: Vec<&str> = source.split('/').filter(|part| !part.is_empty()).collect();
    let stem = segments
        .last()
        .and_then(|file| file.split('.').next())
        .unwrap_or("");
    const UNINFORMATIVE: &[&str] = &[
        "queries",
        "query",
        "operations",
        "mutations",
        "index",
        "main",
        "graphql",
    ];
    if !stem.is_empty() && !UNINFORMATIVE.contains(&stem) {
        return stem.to_owned();
    }
    segments
        .iter()
        .rev()
        .nth(1)
        .map_or_else(|| stem.to_owned(), |directory| (*directory).to_owned())
}

struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(input: &str) -> Self {
        let mut starts = vec![0];
        for (offset, byte) in input.bytes().enumerate() {
            if byte == b'\n' {
                starts.push(offset + 1);
            }
        }
        Self { starts }
    }

    fn span(&self, file: &str, offset: usize, pointer: impl Into<String>) -> Span {
        let line = self.starts.partition_point(|start| *start <= offset);
        let column = offset - self.starts[line.saturating_sub(1)] + 1;
        Span::new(file, line.max(1), column, pointer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPERATIONS: &str = r#"
query PaymentById($id: ID!) {
  payment(id: $id) {
    id
    amount { currency value }
    ...settlement
  }
}

fragment settlement on Payment {
  settledAt
}

mutation Refund($id: ID!) {
  refundPayment(id: $id) { id }
}
"#;

    fn demand() -> Demand {
        ingest(
            "services/web-checkout/queries.graphql",
            OPERATIONS.as_bytes(),
        )
        .expect("an operation document")
    }

    #[test]
    fn routes_match_the_schema_ingester_s_own_shape() {
        let routes: BTreeSet<String> = demand()
            .usages
            .iter()
            .map(|usage| usage.route.to_string())
            .collect();
        assert!(routes.contains("QUERY /query/payment"), "{routes:?}");
        assert!(
            routes.contains("MUTATION /mutation/refundPayment"),
            "{routes:?}"
        );
    }

    #[test]
    fn a_selection_set_is_the_field_list_including_fragments() {
        let demand = demand();
        let ty = demand
            .usages
            .iter()
            .find_map(|usage| match (&usage.route.path, &usage.kind) {
                (path, UsageKind::Response { ty, .. }) if path == "/query/payment" => Some(ty),
                _ => None,
            })
            .expect("the payment response");
        let TypeRef::Object { fields, .. } = ty else {
            panic!("expected an object: {ty:?}");
        };
        assert!(fields.contains_key("id"));
        assert!(fields.contains_key("amount"));
        assert!(
            fields.contains_key("settledAt"),
            "a fragment spread is part of the selection: {:?}",
            fields.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn arguments_become_parameters_the_join_can_see() {
        assert!(demand().usages.iter().any(|usage| matches!(
            &usage.kind,
            UsageKind::Parameter { name, location, .. } if name == "id" && location == "arg"
        )));
    }

    #[test]
    fn the_consumer_is_named_from_the_directory_when_the_file_says_nothing() {
        assert_eq!(
            consumer_name("services/web-checkout/queries.graphql"),
            "web-checkout"
        );
        assert_eq!(consumer_name("consumers/reporting.graphql"), "reporting");
    }

    #[test]
    fn a_missing_fragment_is_reported_rather_than_silently_dropped() {
        let demand =
            ingest("c/q.graphql", b"query Q { payment { id ...elsewhere } }").expect("parses");
        assert!(
            demand
                .unmodelled
                .iter()
                .any(|item| item.kind.describe().contains("elsewhere")),
            "{:?}",
            demand.unmodelled
        );
    }

    #[test]
    fn a_schema_is_not_a_demand() {
        let error = ingest("api/schema.graphql", b"type Query { payment: String }")
            .expect_err("a schema declares no operation");
        assert!(error.contains("no GraphQL operation"), "{error}");
    }
}
