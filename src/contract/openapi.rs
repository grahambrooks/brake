use std::collections::{BTreeMap, BTreeSet};

use saphyr::{LoadableYamlNode, MarkedYamlOwned, ScalarOwned, YamlDataOwned};
use thiserror::Error;

use super::{
    Contract, Endpoint, EndpointKey, Field, Parameter, Payload, Span, TypeRef, UnmodelledKind,
};

const HTTP_METHODS: &[(&str, &str)] = &[
    ("get", "GET"),
    ("post", "POST"),
    ("put", "PUT"),
    ("patch", "PATCH"),
    ("delete", "DELETE"),
    ("options", "OPTIONS"),
    ("head", "HEAD"),
    ("trace", "TRACE"),
];

#[derive(Debug, Error)]
pub enum OpenApiError {
    #[error("contract source `{contract_source}` is not valid UTF-8: {error}")]
    InvalidUtf8 {
        contract_source: String,
        error: std::str::Utf8Error,
    },
    #[error("failed to parse OpenAPI document `{contract_source}`: {error}")]
    Parse {
        contract_source: String,
        error: saphyr::ScanError,
    },
    #[error("OpenAPI document `{contract_source}` does not contain a root mapping")]
    RootNotMapping { contract_source: String },
}

pub fn ingest(source: &str, bytes: &[u8]) -> Result<Contract, OpenApiError> {
    let input = std::str::from_utf8(bytes).map_err(|error| OpenApiError::InvalidUtf8 {
        contract_source: source.to_owned(),
        error,
    })?;

    let docs = MarkedYamlOwned::load_from_str(input).map_err(|error| OpenApiError::Parse {
        contract_source: source.to_owned(),
        error,
    })?;
    let root = docs.first().ok_or_else(|| OpenApiError::RootNotMapping {
        contract_source: source.to_owned(),
    })?;
    let paths = get_map_value(root, "paths").ok_or_else(|| OpenApiError::RootNotMapping {
        contract_source: source.to_owned(),
    })?;
    let path_map = paths
        .data
        .as_mapping()
        .ok_or_else(|| OpenApiError::RootNotMapping {
            contract_source: source.to_owned(),
        })?;

    let context = OpenApiContext { source, root };
    let mut contract = Contract::empty();
    for (path_key, path_item) in path_map {
        let Some(path) = string_node(path_key) else {
            continue;
        };
        let Some(path_item_map) = path_item.data.as_mapping() else {
            continue;
        };

        for (method_key, method_name) in HTTP_METHODS {
            let Some(operation) = path_item_map.get(&yaml_string(method_key)) else {
                continue;
            };
            let Some(_operation_map) = operation.data.as_mapping() else {
                continue;
            };

            let pointer = format!("/paths/{}/{method_key}", escape_json_pointer(path));
            let operation_span = span(source, operation, pointer.clone());
            let operation_id = get_map_value(operation, "operationId")
                .and_then(string_node)
                .map(ToOwned::to_owned);
            let deprecated = get_map_value(operation, "deprecated")
                .and_then(bool_node)
                .unwrap_or(false);

            let request = parse_request_payload(&context, operation, &pointer);
            let parameters = parse_parameters(&context, operation, &pointer);
            let responses = parse_responses(&context, operation, &pointer);

            contract.endpoints.insert(
                EndpointKey {
                    method: (*method_name).to_owned(),
                    path: path.to_owned(),
                },
                Endpoint {
                    operation_id,
                    deprecated,
                    parameters,
                    request,
                    responses,
                    security: Vec::new(),
                    span: operation_span,
                },
            );
        }
    }

    Ok(contract)
}

struct OpenApiContext<'a> {
    source: &'a str,
    root: &'a MarkedYamlOwned,
}

fn parse_request_payload(
    context: &OpenApiContext<'_>,
    operation: &MarkedYamlOwned,
    pointer: &str,
) -> Option<Payload> {
    let request_body = get_map_value(operation, "requestBody")?;
    let schema = payload_schema_node(request_body)?;
    let ty = parse_schema(context, schema, &mut Vec::new());
    Some(Payload {
        ty,
        span: span(
            context.source,
            request_body,
            format!("{pointer}/requestBody/content"),
        ),
    })
}

fn parse_parameters(
    context: &OpenApiContext<'_>,
    operation: &MarkedYamlOwned,
    pointer: &str,
) -> Vec<Parameter> {
    let mut parameters = Vec::new();
    let Some(parameter_nodes) =
        get_map_value(operation, "parameters").and_then(|node| node.data.as_vec())
    else {
        return parameters;
    };

    for (index, parameter_node) in parameter_nodes.iter().enumerate() {
        let Some(parameter_map) = parameter_node.data.as_mapping() else {
            continue;
        };
        let Some(name) = parameter_map
            .get(&yaml_string("name"))
            .and_then(string_node)
            .map(ToOwned::to_owned)
        else {
            continue;
        };
        let Some(location) = parameter_map
            .get(&yaml_string("in"))
            .and_then(string_node)
            .map(ToOwned::to_owned)
        else {
            continue;
        };
        let required = parameter_map
            .get(&yaml_string("required"))
            .and_then(bool_node)
            .unwrap_or(false);
        let ty = parameter_map
            .get(&yaml_string("schema"))
            .map(|schema| parse_schema(context, schema, &mut Vec::new()))
            .unwrap_or(TypeRef::Unknown(UnmodelledKind::SchemaDeferred));

        let _parameter_span = span(
            context.source,
            parameter_node,
            format!("{pointer}/parameters/{index}"),
        );
        parameters.push(Parameter {
            name,
            location,
            required,
            ty,
        });
    }

    parameters
}

fn parse_responses(
    context: &OpenApiContext<'_>,
    operation: &MarkedYamlOwned,
    pointer: &str,
) -> BTreeMap<String, Payload> {
    let mut responses = BTreeMap::new();
    let Some(response_map) =
        get_map_value(operation, "responses").and_then(|node| node.data.as_mapping())
    else {
        return responses;
    };

    for (status_key, status_node) in response_map {
        let Some(status) = string_node(status_key) else {
            continue;
        };
        let ty = payload_schema_node(status_node)
            .map(|schema| parse_schema(context, schema, &mut Vec::new()))
            .unwrap_or(TypeRef::Unknown(UnmodelledKind::SchemaDeferred));
        responses.insert(
            status.to_owned(),
            Payload {
                ty,
                span: span(
                    context.source,
                    status_node,
                    format!("{pointer}/responses/{}", escape_json_pointer(status)),
                ),
            },
        );
    }

    responses
}

fn parse_schema(
    context: &OpenApiContext<'_>,
    schema_node: &MarkedYamlOwned,
    ref_stack: &mut Vec<String>,
) -> TypeRef {
    if let Some(reference) = get_map_value(schema_node, "$ref").and_then(string_node) {
        return resolve_reference(context, reference, ref_stack);
    }

    if let Some(all_of) = get_map_value(schema_node, "allOf").and_then(|node| node.data.as_vec()) {
        return flatten_all_of(context, all_of, ref_stack);
    }

    if let Some(one_of) = get_map_value(schema_node, "oneOf").and_then(|node| node.data.as_vec()) {
        return TypeRef::OneOf {
            variants: one_of
                .iter()
                .map(|variant| parse_schema(context, variant, ref_stack))
                .collect(),
        };
    }
    if let Some(any_of) = get_map_value(schema_node, "anyOf").and_then(|node| node.data.as_vec()) {
        return TypeRef::OneOf {
            variants: any_of
                .iter()
                .map(|variant| parse_schema(context, variant, ref_stack))
                .collect(),
        };
    }

    if let Some(enum_values) =
        get_map_value(schema_node, "enum").and_then(|node| node.data.as_vec())
    {
        let values = enum_values
            .iter()
            .filter_map(enum_value_repr)
            .collect::<BTreeSet<_>>();
        if !values.is_empty() {
            return TypeRef::Enum { values };
        }
    }

    let nullable_30 = get_map_value(schema_node, "nullable")
        .and_then(bool_node)
        .unwrap_or(false);
    if let Some(ty_value) = get_map_value(schema_node, "type")
        && let Some((ty, nullable_31)) = parse_type_value(ty_value)
    {
        let nullable = nullable_30 || nullable_31;
        if ty == "array" {
            let items = get_map_value(schema_node, "items")
                .map(|items| parse_schema(context, items, ref_stack))
                .unwrap_or(TypeRef::Unknown(UnmodelledKind::SchemaDeferred));
            return TypeRef::Array {
                items: Box::new(items),
            };
        }
        if ty == "object" {
            return parse_object_type(context, schema_node, ref_stack);
        }
        return TypeRef::Scalar {
            ty,
            format: get_map_value(schema_node, "format")
                .and_then(string_node)
                .map(ToOwned::to_owned),
            nullable,
        };
    }

    if get_map_value(schema_node, "properties").is_some() {
        return parse_object_type(context, schema_node, ref_stack);
    }

    TypeRef::Unknown(UnmodelledKind::SchemaDeferred)
}

fn parse_object_type(
    context: &OpenApiContext<'_>,
    schema_node: &MarkedYamlOwned,
    ref_stack: &mut Vec<String>,
) -> TypeRef {
    let mut fields = BTreeMap::new();
    let required = get_map_value(schema_node, "required")
        .and_then(|node| node.data.as_vec())
        .map(|items| {
            items
                .iter()
                .filter_map(string_node)
                .map(ToOwned::to_owned)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();

    if let Some(properties) =
        get_map_value(schema_node, "properties").and_then(|node| node.data.as_mapping())
    {
        for (name_node, property_schema) in properties {
            let Some(name) = string_node(name_node) else {
                continue;
            };
            fields.insert(
                name.to_owned(),
                Field {
                    ty: parse_schema(context, property_schema, ref_stack),
                    required: required.contains(name),
                },
            );
        }
    }

    let additional = get_map_value(schema_node, "additionalProperties")
        .and_then(bool_node)
        .unwrap_or(true);
    TypeRef::Object { fields, additional }
}

fn resolve_reference(
    context: &OpenApiContext<'_>,
    reference: &str,
    ref_stack: &mut Vec<String>,
) -> TypeRef {
    if !reference.starts_with("#/") {
        return TypeRef::Unknown(UnmodelledKind::InvalidShape);
    }
    let name = reference
        .split('/')
        .next_back()
        .unwrap_or(reference)
        .replace("~1", "/")
        .replace("~0", "~");

    if ref_stack.contains(&reference.to_owned()) {
        return TypeRef::Cycle(name);
    }

    let Some(target) = find_pointer(context.root, reference) else {
        return TypeRef::Unknown(UnmodelledKind::InvalidShape);
    };
    ref_stack.push(reference.to_owned());
    let parsed = parse_schema(context, target, ref_stack);
    ref_stack.pop();
    parsed
}

fn flatten_all_of(
    context: &OpenApiContext<'_>,
    parts: &[MarkedYamlOwned],
    ref_stack: &mut Vec<String>,
) -> TypeRef {
    let mut fields = BTreeMap::new();
    let mut additional = true;
    let mut merged_any = false;

    for part in parts {
        let parsed = parse_schema(context, part, ref_stack);
        if let TypeRef::Object {
            fields: part_fields,
            additional: part_additional,
        } = parsed
        {
            merged_any = true;
            for (name, field) in part_fields {
                fields.insert(name, field);
            }
            additional &= part_additional;
        }
    }

    if merged_any {
        TypeRef::Object { fields, additional }
    } else {
        TypeRef::Unknown(UnmodelledKind::SchemaDeferred)
    }
}

fn parse_type_value(type_node: &MarkedYamlOwned) -> Option<(String, bool)> {
    if let Some(single) = string_node(type_node) {
        return Some((single.to_owned(), false));
    }

    let values = type_node.data.as_vec()?;
    let mut nullable = false;
    let mut non_null = None::<String>;
    for value in values {
        let ty = string_node(value)?;
        if ty == "null" {
            nullable = true;
        } else {
            non_null = Some(ty.to_owned());
        }
    }
    non_null.map(|ty| (ty, nullable))
}

fn payload_schema_node(payload_container: &MarkedYamlOwned) -> Option<&MarkedYamlOwned> {
    let content = get_map_value(payload_container, "content")?;
    let media = content.data.as_mapping()?.iter().next()?.1;
    get_map_value(media, "schema")
}

fn find_pointer<'a>(root: &'a MarkedYamlOwned, pointer: &str) -> Option<&'a MarkedYamlOwned> {
    let mut current = root;
    for token in pointer.trim_start_matches("#/").split('/') {
        let decoded = token.replace("~1", "/").replace("~0", "~");
        current = get_map_value(current, &decoded)?;
    }
    Some(current)
}

fn get_map_value<'a>(node: &'a MarkedYamlOwned, key: &str) -> Option<&'a MarkedYamlOwned> {
    node.data.as_mapping()?.get(&yaml_string(key))
}

fn enum_value_repr(node: &MarkedYamlOwned) -> Option<String> {
    if let Some(value) = node.data.as_str() {
        return Some(value.to_owned());
    }
    if let Some(value) = node.data.as_integer() {
        return Some(value.to_string());
    }
    if let Some(value) = node.data.as_bool() {
        return Some(value.to_string());
    }
    None
}

fn yaml_string(value: &str) -> MarkedYamlOwned {
    MarkedYamlOwned::from(YamlDataOwned::Value(ScalarOwned::String(value.to_owned())))
}

fn string_node(node: &MarkedYamlOwned) -> Option<&str> {
    node.data.as_str()
}

fn bool_node(node: &MarkedYamlOwned) -> Option<bool> {
    node.data.as_bool()
}

fn span(source: &str, node: &MarkedYamlOwned, pointer: String) -> Span {
    Span {
        file: source.to_owned(),
        line: node.span.start.line(),
        column: node.span.start.col(),
        pointer,
    }
}

fn escape_json_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingests_endpoint_set_and_spans() {
        let source = "api/payments-openapi.yaml";
        let spec = r#"
openapi: 3.1.0
paths:
  /payments/{id}:
    get:
      operationId: getPayment
      deprecated: true
      responses:
        "200":
          description: ok
"#;

        let contract = ingest(source, spec.as_bytes()).expect("ingest should succeed");

        assert_eq!(contract.endpoints.len(), 1);
        let endpoint = contract
            .endpoints
            .get(&EndpointKey {
                method: "GET".to_owned(),
                path: "/payments/{id}".to_owned(),
            })
            .expect("GET /payments/{id} should exist");
        assert_eq!(endpoint.operation_id.as_deref(), Some("getPayment"));
        assert!(endpoint.deprecated);
        assert_eq!(endpoint.span.file, source);
        assert_eq!(endpoint.span.line, 6);
        assert_eq!(endpoint.span.pointer, "/paths/~1payments~1{id}/get");
        assert!(matches!(
            endpoint
                .responses
                .get("200")
                .expect("response status exists")
                .ty,
            TypeRef::Unknown(UnmodelledKind::SchemaDeferred)
        ));
    }

    #[test]
    fn ingests_request_body_schema_and_parameters() {
        let spec = r#"
openapi: 3.0.3
paths:
  /payments:
    post:
      parameters:
        - name: dry_run
          in: query
          required: false
          schema:
            type: boolean
      requestBody:
        content:
          application/json:
            schema:
              type: object
              additionalProperties: false
              required: [id]
              properties:
                id:
                  type: string
                note:
                  type: string
                  nullable: true
      responses:
        "201":
          description: created
"#;

        let contract = ingest("api/payments-openapi.yaml", spec.as_bytes()).expect("ingest");
        let endpoint = contract
            .endpoints
            .get(&EndpointKey {
                method: "POST".to_owned(),
                path: "/payments".to_owned(),
            })
            .expect("POST /payments should exist");

        assert_eq!(endpoint.parameters.len(), 1);
        assert_eq!(endpoint.parameters[0].name, "dry_run");
        assert!(matches!(
            endpoint.parameters[0].ty,
            TypeRef::Scalar { ref ty, .. } if ty == "boolean"
        ));

        let TypeRef::Object { fields, additional } =
            &endpoint.request.as_ref().expect("request body exists").ty
        else {
            panic!("request should be parsed as object");
        };
        assert!(!additional);
        assert!(fields.get("id").expect("required field").required);
        assert!(matches!(
            fields.get("note").expect("nullable field").ty,
            TypeRef::Scalar { nullable: true, .. }
        ));
    }

    #[test]
    fn resolves_refs_flattens_allof_and_detects_cycles() {
        let spec = r#"
openapi: 3.1.0
components:
  schemas:
    Node:
      type: object
      properties:
        child:
          $ref: '#/components/schemas/Node'
    BasePayment:
      type: object
      required: [id]
      properties:
        id:
          type: string
    ExtendedPayment:
      allOf:
        - $ref: '#/components/schemas/BasePayment'
        - type: object
          properties:
            amount:
              type: integer
paths:
  /payments:
    get:
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ExtendedPayment'
  /tree:
    get:
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/Node'
"#;

        let contract = ingest("api/openapi.yaml", spec.as_bytes()).expect("ingest");
        let payments = contract
            .endpoints
            .get(&EndpointKey {
                method: "GET".to_owned(),
                path: "/payments".to_owned(),
            })
            .expect("payments endpoint");
        let TypeRef::Object { fields, .. } = &payments.responses.get("200").expect("200").ty else {
            panic!("allOf ref should flatten to object");
        };
        assert!(fields.get("id").expect("id").required);
        assert!(fields.contains_key("amount"));

        let tree = contract
            .endpoints
            .get(&EndpointKey {
                method: "GET".to_owned(),
                path: "/tree".to_owned(),
            })
            .expect("tree endpoint");
        let TypeRef::Object { fields, .. } = &tree.responses.get("200").expect("200").ty else {
            panic!("node schema should parse as object");
        };
        assert!(matches!(
            fields.get("child").expect("child field").ty,
            TypeRef::Cycle(ref name) if name == "Node"
        ));
    }

    #[test]
    fn parses_openapi_31_nullable_type_array() {
        let spec = r#"
openapi: 3.1.0
paths:
  /payments:
    get:
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                type: [string, "null"]
"#;

        let contract = ingest("api/openapi.yaml", spec.as_bytes()).expect("ingest");
        let endpoint = contract
            .endpoints
            .get(&EndpointKey {
                method: "GET".to_owned(),
                path: "/payments".to_owned(),
            })
            .expect("endpoint");
        assert!(matches!(
            endpoint.responses.get("200").expect("response").ty,
            TypeRef::Scalar {
                ref ty,
                nullable: true,
                ..
            } if ty == "string"
        ));
    }
}
