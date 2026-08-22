//! GraphQL SDL → `Contract`.
//!
//! Nullability is load-bearing in GraphQL and is carried all the way down: a
//! `String!` field relaxed to `String` breaks every consumer whose generated
//! type is non-optional, and that difference is invisible if `NonNull` is
//! unwrapped and discarded. Unions and interfaces are modelled too — removing
//! a union member is a canonical GraphQL break.

use std::collections::{BTreeMap, BTreeSet};

use apollo_parser::{Parser, cst, cst::CstNode};
use thiserror::Error;

use super::{
    Constraints, Contract, Endpoint, EndpointKey, Field, MEDIA_ANY, Parameter, Payload, Span,
    TypeRef, Unmodelled, UnmodelledKind,
};

#[derive(Debug, Error)]
pub enum GraphqlError {
    #[error("contract source `{contract_source}` is not valid UTF-8: {error}")]
    InvalidUtf8 {
        contract_source: String,
        error: std::str::Utf8Error,
    },
    #[error("failed to parse graphql schema `{contract_source}`: {details}")]
    Parse {
        contract_source: String,
        details: String,
    },
}

pub fn ingest(source: &str, bytes: &[u8]) -> Result<Contract, GraphqlError> {
    let input = std::str::from_utf8(bytes).map_err(|error| GraphqlError::InvalidUtf8 {
        contract_source: source.to_owned(),
        error,
    })?;
    let tree = Parser::new(input).parse();
    let errors = tree.errors().map(ToString::to_string).collect::<Vec<_>>();
    if !errors.is_empty() {
        return Err(GraphqlError::Parse {
            contract_source: source.to_owned(),
            details: errors.join("; "),
        });
    }

    let lines = LineIndex::new(input);
    let mut registry = Registry::default();
    let mut roots = Roots::default();

    for definition in tree.document().definitions() {
        match definition {
            cst::Definition::SchemaDefinition(schema) => roots.absorb(schema),
            cst::Definition::EnumTypeDefinition(node) => {
                if let Some(name) = extract_name(node.name()) {
                    registry
                        .enums
                        .entry(name)
                        .or_default()
                        .extend(enum_values(node.enum_values_definition()));
                }
            }
            cst::Definition::EnumTypeExtension(node) => {
                if let Some(name) = extract_name(node.name()) {
                    registry
                        .enums
                        .entry(name)
                        .or_default()
                        .extend(enum_values(node.enum_values_definition()));
                }
            }
            cst::Definition::InputObjectTypeDefinition(node) => {
                if let Some(name) = extract_name(node.name()) {
                    registry
                        .inputs
                        .entry(name)
                        .or_default()
                        .extend(input_fields(node.input_fields_definition()));
                }
            }
            cst::Definition::InputObjectTypeExtension(node) => {
                if let Some(name) = extract_name(node.name()) {
                    registry
                        .inputs
                        .entry(name)
                        .or_default()
                        .extend(input_fields(node.input_fields_definition()));
                }
            }
            cst::Definition::ObjectTypeDefinition(node) => {
                if let Some(name) = extract_name(node.name()) {
                    let span = lines.span(source, node.syntax().text_range().start().into(), &name);
                    registry
                        .objects
                        .entry(name.clone())
                        .or_default()
                        .extend(output_fields(
                            node.fields_definition(),
                            &lines,
                            source,
                            &name,
                        ));
                    registry.spans.entry(name).or_insert(span);
                }
            }
            cst::Definition::ObjectTypeExtension(node) => {
                if let Some(name) = extract_name(node.name()) {
                    registry
                        .objects
                        .entry(name.clone())
                        .or_default()
                        .extend(output_fields(
                            node.fields_definition(),
                            &lines,
                            source,
                            &name,
                        ));
                }
            }
            // An interface behaves as an object for compatibility purposes:
            // removing a field from it removes that field from every
            // implementor's selection set.
            cst::Definition::InterfaceTypeDefinition(node) => {
                if let Some(name) = extract_name(node.name()) {
                    registry
                        .objects
                        .entry(name.clone())
                        .or_default()
                        .extend(output_fields(
                            node.fields_definition(),
                            &lines,
                            source,
                            &name,
                        ));
                    registry.interfaces.insert(name);
                }
            }
            cst::Definition::InterfaceTypeExtension(node) => {
                if let Some(name) = extract_name(node.name()) {
                    registry
                        .objects
                        .entry(name.clone())
                        .or_default()
                        .extend(output_fields(
                            node.fields_definition(),
                            &lines,
                            source,
                            &name,
                        ));
                    registry.interfaces.insert(name);
                }
            }
            cst::Definition::UnionTypeDefinition(node) => {
                if let Some(name) = extract_name(node.name()) {
                    registry
                        .unions
                        .entry(name)
                        .or_default()
                        .extend(union_members(node.union_member_types()));
                }
            }
            cst::Definition::UnionTypeExtension(node) => {
                if let Some(name) = extract_name(node.name()) {
                    registry
                        .unions
                        .entry(name)
                        .or_default()
                        .extend(union_members(node.union_member_types()));
                }
            }
            cst::Definition::ScalarTypeDefinition(node) => {
                if let Some(name) = extract_name(node.name()) {
                    registry.custom_scalars.insert(name);
                }
            }
            _ => {}
        }
    }

    let mut contract = Contract::empty();
    for (method, segment, root) in [
        ("QUERY", "query", roots.query.clone()),
        ("MUTATION", "mutation", roots.mutation.clone()),
        ("SUBSCRIPTION", "subscription", roots.subscription.clone()),
    ] {
        add_root_endpoints(&mut contract, source, method, segment, &root, &mut registry);
    }

    contract.unmodelled = std::mem::take(&mut registry.unmodelled);
    contract
        .unmodelled
        .sort_by(|a, b| a.pointer.cmp(&b.pointer).then_with(|| a.kind.cmp(&b.kind)));
    contract.unmodelled.dedup();
    Ok(contract)
}

#[derive(Debug, Clone)]
struct Roots {
    query: String,
    mutation: String,
    subscription: String,
}

impl Default for Roots {
    fn default() -> Self {
        Self {
            query: "Query".to_owned(),
            mutation: "Mutation".to_owned(),
            subscription: "Subscription".to_owned(),
        }
    }
}

impl Roots {
    fn absorb(&mut self, schema: cst::SchemaDefinition) {
        for root in schema.root_operation_type_definitions() {
            let (Some(operation), Some(named)) = (root.operation_type(), root.named_type()) else {
                continue;
            };
            let Some(type_name) = extract_name(named.name()) else {
                continue;
            };
            if operation.query_token().is_some() {
                self.query = type_name;
            } else if operation.mutation_token().is_some() {
                self.mutation = type_name;
            } else if operation.subscription_token().is_some() {
                self.subscription = type_name;
            }
        }
    }
}

#[derive(Debug, Clone)]
struct FieldShape {
    name: String,
    ty: GraphType,
    args: Vec<ArgumentShape>,
    deprecated: bool,
    span: Span,
}

#[derive(Debug, Clone)]
struct ArgumentShape {
    name: String,
    ty: GraphType,
    has_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GraphType {
    Named(String),
    List(Box<GraphType>),
    NonNull(Box<GraphType>),
}

#[derive(Default)]
struct Registry {
    inputs: BTreeMap<String, Vec<FieldShape>>,
    objects: BTreeMap<String, Vec<FieldShape>>,
    enums: BTreeMap<String, BTreeSet<String>>,
    unions: BTreeMap<String, Vec<String>>,
    interfaces: BTreeSet<String>,
    custom_scalars: BTreeSet<String>,
    spans: BTreeMap<String, Span>,
    unmodelled: Vec<Unmodelled>,
}

impl Registry {
    fn record(&mut self, kind: UnmodelledKind, pointer: &str, span: &Span) -> TypeRef {
        self.unmodelled.push(Unmodelled {
            kind: kind.clone(),
            pointer: pointer.to_owned(),
            span: span.clone(),
        });
        TypeRef::Unknown(kind)
    }
}

fn add_root_endpoints(
    contract: &mut Contract,
    source: &str,
    method: &str,
    segment: &str,
    root_name: &str,
    registry: &mut Registry,
) {
    let Some(fields) = registry.objects.get(root_name).cloned() else {
        return;
    };

    for field in &fields {
        let pointer = format!("/{segment}/{}", field.name);
        let parameters = field
            .args
            .iter()
            .map(|argument| Parameter {
                name: argument.name.clone(),
                location: "arg".to_owned(),
                // An argument with a default is satisfiable without the caller
                // supplying it, so it is not required of them.
                required: is_non_null(&argument.ty) && !argument.has_default,
                deprecated: false,
                ty: build_type(
                    &argument.ty,
                    registry,
                    &mut BTreeSet::new(),
                    &pointer,
                    &field.span,
                ),
                span: field.span.clone(),
            })
            .collect::<Vec<_>>();

        let response_ty = build_type(
            &field.ty,
            registry,
            &mut BTreeSet::new(),
            &pointer,
            &field.span,
        );

        contract.endpoints.insert(
            EndpointKey {
                method: method.to_owned(),
                path: format!("/{segment}/{}", field.name),
            },
            Endpoint {
                operation_id: Some(format!("{root_name}.{}", field.name)),
                deprecated: field.deprecated,
                sunset: None,
                parameters,
                request: None,
                responses: BTreeMap::from([(
                    "200".to_owned(),
                    Payload {
                        media_types: BTreeMap::from([(MEDIA_ANY.to_owned(), response_ty)]),
                        span: Span::new(
                            source,
                            field.span.line,
                            field.span.column,
                            pointer.clone(),
                        ),
                    },
                )]),
                security: Vec::new(),
                span: Span::new(source, field.span.line, field.span.column, pointer),
            },
        );
    }
}

/// Build a `TypeRef`, keeping nullability at every level.
///
/// `NonNull` is not discarded: `[String!]!` and `[String]!` differ in a way a
/// consumer feels, and collapsing them hides a real break.
fn build_type(
    ty: &GraphType,
    registry: &mut Registry,
    visiting: &mut BTreeSet<String>,
    pointer: &str,
    span: &Span,
) -> TypeRef {
    match ty {
        GraphType::NonNull(inner) => {
            set_nullable(build_type(inner, registry, visiting, pointer, span), false)
        }
        GraphType::List(inner) => TypeRef::Array {
            items: Box::new(build_type(inner, registry, visiting, pointer, span)),
            nullable: true,
        },
        GraphType::Named(name) => {
            // Everything named is nullable unless a NonNull wraps it.
            let resolved = named_type(name, registry, visiting, pointer, span);
            set_nullable(resolved, true)
        }
    }
}

fn named_type(
    name: &str,
    registry: &mut Registry,
    visiting: &mut BTreeSet<String>,
    pointer: &str,
    span: &Span,
) -> TypeRef {
    if let Some(values) = registry.enums.get(name).cloned() {
        return TypeRef::Enum {
            values,
            numbers: BTreeMap::new(),
        };
    }

    // A union is a closed set of possible results. Removing a member is a
    // break for a consumer whose selection set names it, and modelling the
    // union as an opaque scalar made that invisible.
    if let Some(members) = registry.unions.get(name).cloned() {
        if !visiting.insert(name.to_owned()) {
            return TypeRef::Cycle(name.to_owned());
        }
        // A union member is discriminated by `__typename`, which is how a
        // consumer's `... on Refund` selection actually works. Modelling it
        // keeps two structurally identical members distinct — without it,
        // `Payment | Invoice` and `Payment | Invoice | Refund` compare equal
        // whenever the members happen to have the same fields.
        let variants = members
            .iter()
            .map(|member| {
                let resolved = named_type(member, registry, visiting, pointer, span);
                match resolved {
                    TypeRef::Object {
                        mut fields,
                        additional,
                        nullable,
                    } => {
                        fields.insert(
                            "__typename".to_owned(),
                            Field {
                                ty: TypeRef::Enum {
                                    values: BTreeSet::from([member.clone()]),
                                    numbers: BTreeMap::new(),
                                },
                                required: true,
                                deprecated: false,
                                number: None,
                            },
                        );
                        TypeRef::Object {
                            fields,
                            additional,
                            nullable,
                        }
                    }
                    other => other,
                }
            })
            .collect();
        visiting.remove(name);
        return TypeRef::OneOf { variants };
    }

    let defined = registry
        .inputs
        .get(name)
        .or_else(|| registry.objects.get(name))
        .cloned();
    {
        if let Some(fields) = defined {
            if !visiting.insert(name.to_owned()) {
                return TypeRef::Cycle(name.to_owned());
            }
            let mut modelled = BTreeMap::new();
            for field in &fields {
                modelled.insert(
                    field.name.clone(),
                    Field {
                        required: is_non_null(&field.ty),
                        ty: build_type(&field.ty, registry, visiting, pointer, span),
                        deprecated: field.deprecated,
                        number: None,
                    },
                );
            }
            visiting.remove(name);
            return TypeRef::Object {
                fields: modelled,
                additional: false,
                nullable: true,
            };
        }
    }

    match name {
        "Int" => scalar("integer", None),
        "Float" => scalar("number", None),
        "Boolean" => scalar("boolean", None),
        "String" => scalar("string", None),
        "ID" => scalar("string", Some("ID")),
        custom if registry.custom_scalars.contains(custom) => {
            scalar("string", Some(&format!("graphql:{custom}")))
        }
        // A type that is used but never defined is a schema this ingester was
        // not given all of. Reporting it beats inventing a scalar for it.
        unknown => registry.record(
            UnmodelledKind::ExternalRef(unknown.to_owned()),
            &format!("{pointer}/{unknown}"),
            span,
        ),
    }
}

fn scalar(ty: &str, format: Option<&str>) -> TypeRef {
    TypeRef::Scalar {
        ty: ty.to_owned(),
        format: format.map(ToOwned::to_owned),
        nullable: true,
        constraints: Constraints::default(),
    }
}

fn set_nullable(ty: TypeRef, nullable: bool) -> TypeRef {
    match ty {
        TypeRef::Scalar {
            ty,
            format,
            constraints,
            ..
        } => TypeRef::Scalar {
            ty,
            format,
            nullable,
            constraints,
        },
        TypeRef::Array { items, .. } => TypeRef::Array { items, nullable },
        TypeRef::Object {
            fields, additional, ..
        } => TypeRef::Object {
            fields,
            additional,
            nullable,
        },
        other => other,
    }
}

fn is_non_null(ty: &GraphType) -> bool {
    matches!(ty, GraphType::NonNull(_))
}

fn enum_values(definition: Option<cst::EnumValuesDefinition>) -> BTreeSet<String> {
    definition
        .into_iter()
        .flat_map(|values| values.enum_value_definitions())
        .filter_map(|value| value.enum_value())
        .filter_map(|value| extract_name(value.name()))
        .collect()
}

fn union_members(members: Option<cst::UnionMemberTypes>) -> Vec<String> {
    members
        .into_iter()
        .flat_map(|types| types.named_types())
        .filter_map(|named| extract_name(named.name()))
        .collect()
}

fn input_fields(definition: Option<cst::InputFieldsDefinition>) -> Vec<FieldShape> {
    definition
        .into_iter()
        .flat_map(|fields| fields.input_value_definitions())
        .filter_map(|value| {
            Some(FieldShape {
                name: extract_name(value.name())?,
                ty: convert_type(value.ty()?)?,
                args: Vec::new(),
                deprecated: has_deprecated_directive(value.directives()),
                span: Span::new("", 1, 1, ""),
            })
        })
        .collect()
}

fn output_fields(
    definition: Option<cst::FieldsDefinition>,
    lines: &LineIndex,
    source: &str,
    owner: &str,
) -> Vec<FieldShape> {
    definition
        .into_iter()
        .flat_map(|fields| fields.field_definitions())
        .filter_map(|field| {
            let name = extract_name(field.name())?;
            let offset: usize = field.syntax().text_range().start().into();
            Some(FieldShape {
                ty: convert_type(field.ty()?)?,
                args: field
                    .arguments_definition()
                    .into_iter()
                    .flat_map(|arguments| arguments.input_value_definitions())
                    .filter_map(|argument| {
                        Some(ArgumentShape {
                            name: extract_name(argument.name())?,
                            ty: convert_type(argument.ty()?)?,
                            has_default: argument.default_value().is_some(),
                        })
                    })
                    .collect(),
                deprecated: has_deprecated_directive(field.directives()),
                span: lines.span(source, offset, &format!("/{owner}/{name}")),
                name,
            })
        })
        .collect()
}

fn has_deprecated_directive(directives: Option<cst::Directives>) -> bool {
    directives
        .into_iter()
        .flat_map(|directives| directives.directives())
        .filter_map(|directive| extract_name(directive.name()))
        .any(|name| name == "deprecated")
}

fn convert_type(ty: cst::Type) -> Option<GraphType> {
    match ty {
        cst::Type::NamedType(named) => Some(GraphType::Named(extract_name(named.name())?)),
        cst::Type::ListType(list) => Some(GraphType::List(Box::new(convert_type(list.ty()?)?))),
        cst::Type::NonNullType(non_null) => {
            if let Some(named) = non_null.named_type() {
                return Some(GraphType::NonNull(Box::new(GraphType::Named(
                    extract_name(named.name())?,
                ))));
            }
            let list = non_null.list_type()?;
            Some(GraphType::NonNull(Box::new(GraphType::List(Box::new(
                convert_type(list.ty()?)?,
            )))))
        }
    }
}

fn extract_name(name: Option<cst::Name>) -> Option<String> {
    name.and_then(|node| node.ident_token().map(|token| token.text().to_owned()))
}

/// Byte offset → line and column, so a GraphQL finding points at the field
/// rather than at line 1.
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

    fn span(&self, file: &str, offset: usize, pointer: &str) -> Span {
        let line = self.starts.partition_point(|start| *start <= offset);
        let column = offset - self.starts[line.saturating_sub(1)] + 1;
        Span::new(file, line.max(1), column, pointer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compare::{ChangeKind, compare_contracts};

    fn kinds(base: &str, head: &str) -> Vec<ChangeKind> {
        let base = ingest("api/schema.graphql", base.as_bytes()).expect("base");
        let head = ingest("api/schema.graphql", head.as_bytes()).expect("head");
        compare_contracts(&base, &head)
            .iter()
            .map(|change| change.kind)
            .collect()
    }

    const BASE: &str = r#"
type Query {
  payment(id: ID!): Payment!
  search(q: String!): SearchResult!
}
union SearchResult = Payment | Invoice
type Payment { id: ID! amount: Int! tags: [String!]! }
type Invoice { id: ID! }
"#;

    #[test]
    fn identical_schemas_produce_nothing() {
        assert!(kinds(BASE, BASE).is_empty());
    }

    #[test]
    fn removing_a_union_member_is_a_break() {
        let head = BASE.replace(
            "union SearchResult = Payment | Invoice",
            "union SearchResult = Payment",
        );
        let kinds = kinds(BASE, &head);
        assert!(
            kinds.contains(&ChangeKind::ResponseTypeChanged),
            "a removed union member breaks every consumer selecting it: {kinds:?}"
        );
    }

    #[test]
    fn adding_a_union_member_is_dangerous_not_silent() {
        let head = BASE.replace(
            "union SearchResult = Payment | Invoice",
            "union SearchResult = Payment | Invoice | Refund",
        ) + "\ntype Refund { id: ID! }\n";
        assert!(kinds(BASE, &head).contains(&ChangeKind::ResponseEnumExtended));
    }

    #[test]
    fn relaxing_a_non_null_output_field_is_a_break() {
        let head = BASE.replace(
            "type Payment { id: ID! amount: Int! tags: [String!]! }",
            "type Payment { id: ID! amount: Int tags: [String!]! }",
        );
        let kinds = kinds(BASE, &head);
        assert!(
            kinds.contains(&ChangeKind::ResponseFieldOptional)
                || kinds.contains(&ChangeKind::ResponseTypeChanged),
            "Int! relaxed to Int breaks a non-optional consumer type: {kinds:?}"
        );
    }

    #[test]
    fn relaxing_nullability_inside_a_list_is_a_break() {
        let head = BASE.replace("tags: [String!]!", "tags: [String]!");
        let kinds = kinds(BASE, &head);
        assert!(
            kinds.contains(&ChangeKind::ResponseTypeChanged),
            "[String!]! to [String]! may now yield nulls in the list: {kinds:?}"
        );
    }

    #[test]
    fn requiring_a_previously_optional_argument_is_a_break() {
        let head = BASE.replace("payment(id: ID!)", "payment(id: ID!, mode: String!)");
        assert!(kinds(BASE, &head).contains(&ChangeKind::ParamAddedRequired));
    }

    #[test]
    fn an_argument_with_a_default_is_not_required_of_the_caller() {
        let head = BASE.replace(
            "payment(id: ID!)",
            "payment(id: ID!, mode: String! = \"fast\")",
        );
        let kinds = kinds(BASE, &head);
        assert!(
            !kinds.contains(&ChangeKind::ParamAddedRequired),
            "a defaulted argument does not have to be supplied: {kinds:?}"
        );
    }

    #[test]
    fn removing_a_query_field_is_an_endpoint_removal() {
        let head = "type Query { search(q: String!): String! }";
        assert!(kinds(BASE, head).contains(&ChangeKind::EndpointRemoved));
    }

    #[test]
    fn interface_fields_are_modelled() {
        let base = r#"
type Query { node(id: ID!): Node! }
interface Node { id: ID! label: String! }
"#;
        let head = r#"
type Query { node(id: ID!): Node! }
interface Node { id: ID! }
"#;
        assert!(kinds(base, head).contains(&ChangeKind::ResponseFieldRemoved));
    }

    #[test]
    fn a_deprecated_field_removal_skips_the_hygiene_rule() {
        let base = r#"
type Query {
  legacy: String! @deprecated(reason: "use modern")
  modern: String!
}
"#;
        let head = "type Query { modern: String! }";
        let kinds = kinds(base, head);
        assert!(kinds.contains(&ChangeKind::EndpointRemoved));
        assert!(!kinds.contains(&ChangeKind::RemovedWithoutDeprecation));
    }

    #[test]
    fn spans_point_at_the_field_not_line_one() {
        let contract = ingest("api/schema.graphql", BASE.as_bytes()).expect("ingest");
        let endpoint = &contract.endpoints[&EndpointKey {
            method: "QUERY".to_owned(),
            path: "/query/search".to_owned(),
        }];
        assert_eq!(
            endpoint.span.line, 4,
            "the `search` field is on line 4 of the fixture"
        );
    }

    #[test]
    fn a_custom_scalar_is_modelled_and_an_undefined_type_is_reported() {
        let schema = r#"
scalar DateTime
type Query { at: DateTime! missing: Mystery! }
"#;
        let contract = ingest("api/schema.graphql", schema.as_bytes()).expect("ingest");
        assert!(
            !contract.unmodelled.is_empty(),
            "an undefined type must not be assumed to be a string"
        );
    }

    #[test]
    fn mutations_and_subscriptions_become_endpoints() {
        let schema = r#"
type Query { a: String! }
type Mutation { pay(id: ID!): String! }
type Subscription { paid: String! }
"#;
        let contract = ingest("api/schema.graphql", schema.as_bytes()).expect("ingest");
        let methods = contract
            .endpoints
            .keys()
            .map(|key| key.method.as_str())
            .collect::<BTreeSet<_>>();
        assert!(methods.contains("QUERY"));
        assert!(methods.contains("MUTATION"));
        assert!(methods.contains("SUBSCRIPTION"));
    }

    #[test]
    fn a_custom_schema_block_redirects_the_roots() {
        let schema = r#"
schema { query: RootQuery }
type RootQuery { a: String! }
"#;
        let contract = ingest("api/schema.graphql", schema.as_bytes()).expect("ingest");
        assert!(contract.endpoints.contains_key(&EndpointKey {
            method: "QUERY".to_owned(),
            path: "/query/a".to_owned(),
        }));
    }
}
