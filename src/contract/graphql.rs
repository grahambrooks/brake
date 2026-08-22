use std::collections::{BTreeMap, BTreeSet};

use apollo_parser::{Parser, cst};
use thiserror::Error;

use super::{Contract, Endpoint, EndpointKey, Field, Parameter, Payload, Span, TypeRef};

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
    let document = tree.document();

    let mut registry = TypeRegistry {
        input_defs: BTreeMap::new(),
        object_defs: BTreeMap::new(),
        enum_defs: BTreeMap::new(),
    };
    let mut query_root = "Query".to_owned();
    let mut mutation_root = "Mutation".to_owned();
    let mut subscription_root = "Subscription".to_owned();

    for definition in document.definitions() {
        match definition {
            cst::Definition::SchemaDefinition(schema) => {
                for root in schema.root_operation_type_definitions() {
                    let Some(op_type) = root.operation_type() else {
                        continue;
                    };
                    let Some(named_type) = root.named_type() else {
                        continue;
                    };
                    let Some(type_name) = extract_named_type_name(&named_type) else {
                        continue;
                    };
                    if op_type.query_token().is_some() {
                        query_root = type_name;
                    } else if op_type.mutation_token().is_some() {
                        mutation_root = type_name;
                    } else if op_type.subscription_token().is_some() {
                        subscription_root = type_name;
                    }
                }
            }
            cst::Definition::EnumTypeDefinition(enum_def) => {
                if let Some(name) = extract_name(enum_def.name()) {
                    registry.enum_defs.entry(name).or_default().extend(
                        enum_def
                            .enum_values_definition()
                            .into_iter()
                            .flat_map(|values| values.enum_value_definitions())
                            .filter_map(|value| value.enum_value())
                            .filter_map(extract_enum_value),
                    );
                }
            }
            cst::Definition::EnumTypeExtension(enum_ext) => {
                if let Some(name) = extract_name(enum_ext.name()) {
                    registry.enum_defs.entry(name).or_default().extend(
                        enum_ext
                            .enum_values_definition()
                            .into_iter()
                            .flat_map(|values| values.enum_value_definitions())
                            .filter_map(|value| value.enum_value())
                            .filter_map(extract_enum_value),
                    );
                }
            }
            cst::Definition::InputObjectTypeDefinition(input_def) => {
                if let Some(name) = extract_name(input_def.name()) {
                    registry.input_defs.entry(name).or_default().extend(
                        input_fields_from_definition(input_def.input_fields_definition()),
                    );
                }
            }
            cst::Definition::InputObjectTypeExtension(input_ext) => {
                if let Some(name) = extract_name(input_ext.name()) {
                    registry.input_defs.entry(name).or_default().extend(
                        input_fields_from_definition(input_ext.input_fields_definition()),
                    );
                }
            }
            cst::Definition::ObjectTypeDefinition(object_def) => {
                if let Some(name) = extract_name(object_def.name()) {
                    registry.object_defs.entry(name).or_default().extend(
                        output_fields_from_definition(object_def.fields_definition()),
                    );
                }
            }
            cst::Definition::ObjectTypeExtension(object_ext) => {
                if let Some(name) = extract_name(object_ext.name()) {
                    registry.object_defs.entry(name).or_default().extend(
                        output_fields_from_definition(object_ext.fields_definition()),
                    );
                }
            }
            _ => {}
        }
    }

    let mut contract = Contract::empty();
    add_root_endpoints(
        &mut contract,
        source,
        "GET",
        "query",
        &query_root,
        &registry,
    );
    add_root_endpoints(
        &mut contract,
        source,
        "POST",
        "mutation",
        &mutation_root,
        &registry,
    );
    add_root_endpoints(
        &mut contract,
        source,
        "SUBSCRIBE",
        "subscription",
        &subscription_root,
        &registry,
    );

    Ok(contract)
}

#[derive(Debug, Clone)]
struct FieldShape {
    name: String,
    ty: GraphType,
    args: Vec<ArgumentShape>,
}

#[derive(Debug, Clone)]
struct ArgumentShape {
    name: String,
    ty: GraphType,
}

#[derive(Debug, Clone)]
enum GraphType {
    Named(String),
    List(Box<GraphType>),
    NonNull(Box<GraphType>),
}

struct TypeRegistry {
    input_defs: BTreeMap<String, Vec<FieldShape>>,
    object_defs: BTreeMap<String, Vec<FieldShape>>,
    enum_defs: BTreeMap<String, BTreeSet<String>>,
}

fn add_root_endpoints(
    contract: &mut Contract,
    source: &str,
    method: &str,
    root_segment: &str,
    root_name: &str,
    registry: &TypeRegistry,
) {
    let Some(fields) = registry.object_defs.get(root_name) else {
        return;
    };

    for (index, field) in fields.iter().enumerate() {
        let span = Span {
            file: source.to_owned(),
            line: 1,
            column: 1,
            pointer: format!("/{root_segment}/{index}"),
        };
        let operation_id = format!("{root_name}.{}", field.name);
        let path = format!("/{root_segment}/{}", field.name);
        let parameters = field
            .args
            .iter()
            .map(|arg| Parameter {
                name: arg.name.clone(),
                location: "arg".to_owned(),
                required: is_non_null(&arg.ty),
                ty: build_type(unwrap_non_null(&arg.ty), registry, &mut BTreeSet::new()),
            })
            .collect::<Vec<_>>();
        let response_ty = build_type(unwrap_non_null(&field.ty), registry, &mut BTreeSet::new());

        let mut responses = BTreeMap::new();
        responses.insert(
            "200".to_owned(),
            Payload {
                ty: response_ty,
                span: span.clone(),
            },
        );
        contract.endpoints.insert(
            EndpointKey {
                method: method.to_owned(),
                path,
            },
            Endpoint {
                operation_id: Some(operation_id),
                deprecated: false,
                parameters,
                request: None,
                responses,
                security: Vec::new(),
                span,
            },
        );
    }
}

fn build_type(ty: &GraphType, registry: &TypeRegistry, visiting: &mut BTreeSet<String>) -> TypeRef {
    match ty {
        GraphType::List(inner) => TypeRef::Array {
            items: Box::new(build_type(inner, registry, visiting)),
        },
        GraphType::NonNull(inner) => build_type(inner, registry, visiting),
        GraphType::Named(name) => {
            if let Some(values) = registry.enum_defs.get(name) {
                return TypeRef::Enum {
                    values: values.clone(),
                };
            }
            if let Some(fields) = registry.input_defs.get(name) {
                if !visiting.insert(name.clone()) {
                    return TypeRef::Cycle(name.clone());
                }
                let resolved = object_from_fields(fields, registry, visiting);
                visiting.remove(name);
                return resolved;
            }
            if let Some(fields) = registry.object_defs.get(name) {
                if !visiting.insert(name.clone()) {
                    return TypeRef::Cycle(name.clone());
                }
                let resolved = object_from_fields(fields, registry, visiting);
                visiting.remove(name);
                return resolved;
            }
            scalar_for_graphql_name(name, true)
        }
    }
}

fn object_from_fields(
    fields: &[FieldShape],
    registry: &TypeRegistry,
    visiting: &mut BTreeSet<String>,
) -> TypeRef {
    let mut output = BTreeMap::new();
    for field in fields {
        output.insert(
            field.name.clone(),
            Field {
                required: is_non_null(&field.ty),
                ty: build_type(unwrap_non_null(&field.ty), registry, visiting),
            },
        );
    }
    TypeRef::Object {
        fields: output,
        additional: false,
    }
}

fn scalar_for_graphql_name(name: &str, nullable: bool) -> TypeRef {
    match name {
        "Int" => TypeRef::Scalar {
            ty: "integer".to_owned(),
            format: None,
            nullable,
        },
        "Float" => TypeRef::Scalar {
            ty: "number".to_owned(),
            format: None,
            nullable,
        },
        "Boolean" => TypeRef::Scalar {
            ty: "boolean".to_owned(),
            format: None,
            nullable,
        },
        "ID" | "String" => TypeRef::Scalar {
            ty: "string".to_owned(),
            format: None,
            nullable,
        },
        other => TypeRef::Scalar {
            ty: "string".to_owned(),
            format: Some(format!("graphql:{other}")),
            nullable,
        },
    }
}

fn extract_name(name: Option<cst::Name>) -> Option<String> {
    name.and_then(|node| node.ident_token().map(|token| token.text().to_owned()))
}

fn extract_named_type_name(named: &cst::NamedType) -> Option<String> {
    extract_name(named.name())
}

fn extract_enum_value(value: cst::EnumValue) -> Option<String> {
    value
        .name()
        .and_then(|name| name.ident_token().map(|token| token.text().to_owned()))
}

fn input_fields_from_definition(definition: Option<cst::InputFieldsDefinition>) -> Vec<FieldShape> {
    definition
        .into_iter()
        .flat_map(|fields| fields.input_value_definitions())
        .filter_map(|value| {
            let name = extract_name(value.name())?;
            let ty = value.ty().map(graph_type_from_cst)?;
            Some(FieldShape {
                name,
                ty,
                args: Vec::new(),
            })
        })
        .collect()
}

fn output_fields_from_definition(definition: Option<cst::FieldsDefinition>) -> Vec<FieldShape> {
    definition
        .into_iter()
        .flat_map(|fields| fields.field_definitions())
        .filter_map(|field| {
            let name = extract_name(field.name())?;
            let ty = field.ty().map(graph_type_from_cst)?;
            let args = field
                .arguments_definition()
                .into_iter()
                .flat_map(|arg_defs| arg_defs.input_value_definitions())
                .filter_map(|arg| {
                    let arg_name = extract_name(arg.name())?;
                    let arg_ty = arg.ty().map(graph_type_from_cst)?;
                    Some(ArgumentShape {
                        name: arg_name,
                        ty: arg_ty,
                    })
                })
                .collect::<Vec<_>>();
            Some(FieldShape { name, ty, args })
        })
        .collect()
}

fn graph_type_from_cst(ty: cst::Type) -> GraphType {
    match ty {
        cst::Type::NamedType(named) => GraphType::Named(
            extract_named_type_name(&named).unwrap_or_else(|| "Unknown".to_owned()),
        ),
        cst::Type::ListType(list) => list
            .ty()
            .map(graph_type_from_cst)
            .map(Box::new)
            .map(GraphType::List)
            .unwrap_or_else(|| GraphType::Named("Unknown".to_owned())),
        cst::Type::NonNullType(non_null) => {
            if let Some(named) = non_null.named_type() {
                return GraphType::NonNull(Box::new(GraphType::Named(
                    extract_named_type_name(&named).unwrap_or_else(|| "Unknown".to_owned()),
                )));
            }
            if let Some(list) = non_null.list_type() {
                let inner = list
                    .ty()
                    .map(graph_type_from_cst)
                    .unwrap_or_else(|| GraphType::Named("Unknown".to_owned()));
                return GraphType::NonNull(Box::new(GraphType::List(Box::new(inner))));
            }
            GraphType::NonNull(Box::new(GraphType::Named("Unknown".to_owned())))
        }
    }
}

fn is_non_null(ty: &GraphType) -> bool {
    matches!(ty, GraphType::NonNull(_))
}

fn unwrap_non_null(ty: &GraphType) -> &GraphType {
    if let GraphType::NonNull(inner) = ty {
        inner
    } else {
        ty
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphql_ingest_extracts_query_fields() {
        let source = br#"
            type Query {
                payment(id: ID!): Payment!
            }

            type Payment {
                id: ID!
                status: String!
            }
        "#;

        let contract = ingest("api/schema.graphql", source).expect("ingest");
        assert_eq!(contract.endpoints.len(), 1);
        let endpoint = contract
            .endpoints
            .get(&EndpointKey {
                method: "GET".to_owned(),
                path: "/query/payment".to_owned(),
            })
            .expect("query endpoint");
        assert_eq!(endpoint.operation_id.as_deref(), Some("Query.payment"));
        assert_eq!(endpoint.parameters.len(), 1);
        assert!(endpoint.responses.contains_key("200"));
        assert!(contract.unmodelled.is_empty());
    }
}
