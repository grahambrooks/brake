use std::collections::{BTreeMap, BTreeSet};

use prost_types::{DescriptorProto, EnumDescriptorProto, FieldDescriptorProto};
use thiserror::Error;

use super::{Contract, Endpoint, EndpointKey, Field, Payload, Span, TypeRef, UnmodelledKind};

#[derive(Debug, Error)]
pub enum ProtoError {
    #[error("contract source `{contract_source}` is not valid UTF-8: {error}")]
    InvalidUtf8 {
        contract_source: String,
        error: std::str::Utf8Error,
    },
    #[error("failed to parse protobuf contract `{contract_source}`: {error}")]
    Parse {
        contract_source: String,
        error: Box<protox_parse::ParseError>,
    },
}

pub fn ingest(source: &str, bytes: &[u8]) -> Result<Contract, ProtoError> {
    let input = std::str::from_utf8(bytes).map_err(|error| ProtoError::InvalidUtf8 {
        contract_source: source.to_owned(),
        error,
    })?;
    let descriptor = protox_parse::parse(source, input).map_err(|error| ProtoError::Parse {
        contract_source: source.to_owned(),
        error: Box::new(error),
    })?;

    let package = descriptor.package.unwrap_or_default();
    let mut messages = BTreeMap::new();
    collect_messages(&package, &descriptor.message_type, &mut messages);
    let mut enums = BTreeMap::new();
    collect_enums(&package, &descriptor.enum_type, &mut enums);
    collect_nested_enums(&package, &descriptor.message_type, &mut enums);

    let mut contract = Contract::empty();
    for (service_index, service) in descriptor.service.iter().enumerate() {
        let Some(service_name) = service.name.as_ref() else {
            continue;
        };
        let scoped_service = if package.is_empty() {
            service_name.clone()
        } else {
            format!("{package}.{service_name}")
        };
        for (method_index, method) in service.method.iter().enumerate() {
            let Some(method_name) = method.name.as_ref() else {
                continue;
            };
            let pointer = format!("/service/{service_index}/method/{method_index}");
            let span = Span {
                file: source.to_owned(),
                line: 1,
                column: 1,
                pointer,
            };
            let operation_id = format!("{scoped_service}.{method_name}");
            let path = format!("/{scoped_service}/{method_name}");
            let request = method.input_type.as_deref().map(|name| Payload {
                ty: resolve_proto_type(&messages, &enums, name, &mut BTreeSet::new()),
                span: span.clone(),
            });
            let response = method.output_type.as_deref().map(|name| Payload {
                ty: resolve_proto_type(&messages, &enums, name, &mut BTreeSet::new()),
                span: span.clone(),
            });
            let mut responses = BTreeMap::new();
            if let Some(payload) = response {
                responses.insert("200".to_owned(), payload);
            }

            contract.endpoints.insert(
                EndpointKey {
                    method: "RPC".to_owned(),
                    path,
                },
                Endpoint {
                    operation_id: Some(operation_id),
                    deprecated: false,
                    parameters: Vec::new(),
                    request,
                    responses,
                    security: Vec::new(),
                    span,
                },
            );
        }
    }
    Ok(contract)
}

fn collect_messages(
    parent: &str,
    descriptors: &[DescriptorProto],
    output: &mut BTreeMap<String, DescriptorProto>,
) {
    for descriptor in descriptors {
        let Some(name) = descriptor.name.as_ref() else {
            continue;
        };
        let fq = qualify(parent, name);
        output.insert(fq.clone(), descriptor.clone());
        collect_messages(&fq, &descriptor.nested_type, output);
    }
}

fn collect_enums(
    parent: &str,
    descriptors: &[EnumDescriptorProto],
    output: &mut BTreeMap<String, EnumDescriptorProto>,
) {
    for descriptor in descriptors {
        let Some(name) = descriptor.name.as_ref() else {
            continue;
        };
        let fq = qualify(parent, name);
        output.insert(fq, descriptor.clone());
    }
}

fn collect_nested_enums(
    parent: &str,
    descriptors: &[DescriptorProto],
    output: &mut BTreeMap<String, EnumDescriptorProto>,
) {
    for descriptor in descriptors {
        let Some(name) = descriptor.name.as_ref() else {
            continue;
        };
        let fq = qualify(parent, name);
        collect_enums(&fq, &descriptor.enum_type, output);
        collect_nested_enums(&fq, &descriptor.nested_type, output);
    }
}

fn resolve_proto_type(
    messages: &BTreeMap<String, DescriptorProto>,
    enums: &BTreeMap<String, EnumDescriptorProto>,
    type_name: &str,
    visiting: &mut BTreeSet<String>,
) -> TypeRef {
    let normalized = normalize_type_name(type_name);
    if let Some(message) = messages.get(&normalized) {
        if !visiting.insert(normalized.clone()) {
            return TypeRef::Cycle(normalized);
        }
        let mut fields = BTreeMap::new();
        for field in &message.field {
            let field_name = field
                .name
                .clone()
                .unwrap_or_else(|| format!("field_{}", field.number.unwrap_or_default()));
            let required = matches!(field.label, Some(2));
            let field_ty = field_type(messages, enums, field, visiting);
            fields.insert(
                field_name,
                Field {
                    ty: field_ty,
                    required,
                },
            );
        }
        visiting.remove(&normalized);
        return TypeRef::Object {
            fields,
            additional: false,
        };
    }

    if let Some(enum_descriptor) = enums.get(&normalized) {
        let values = enum_descriptor
            .value
            .iter()
            .filter_map(|value| value.name.clone())
            .collect::<BTreeSet<_>>();
        return TypeRef::Enum { values };
    }

    TypeRef::Unknown(UnmodelledKind::InvalidShape)
}

fn field_type(
    messages: &BTreeMap<String, DescriptorProto>,
    enums: &BTreeMap<String, EnumDescriptorProto>,
    field: &FieldDescriptorProto,
    visiting: &mut BTreeSet<String>,
) -> TypeRef {
    let mut ty = match field.r#type {
        Some(1 | 2 | 6 | 7 | 15 | 16 | 17 | 18) => TypeRef::Scalar {
            ty: "number".to_owned(),
            format: None,
            nullable: true,
        },
        Some(3 | 4 | 5 | 13) => TypeRef::Scalar {
            ty: "integer".to_owned(),
            format: None,
            nullable: true,
        },
        Some(8) => TypeRef::Scalar {
            ty: "boolean".to_owned(),
            format: None,
            nullable: true,
        },
        Some(9) => TypeRef::Scalar {
            ty: "string".to_owned(),
            format: None,
            nullable: true,
        },
        Some(12) => TypeRef::Scalar {
            ty: "string".to_owned(),
            format: Some("bytes".to_owned()),
            nullable: true,
        },
        Some(10 | 11 | 14) => field
            .type_name
            .as_deref()
            .map(|name| resolve_proto_type(messages, enums, name, visiting))
            .unwrap_or(TypeRef::Unknown(UnmodelledKind::InvalidShape)),
        _ => TypeRef::Unknown(UnmodelledKind::InvalidShape),
    };

    if matches!(field.label, Some(3)) {
        ty = TypeRef::Array {
            items: Box::new(ty),
        };
    }
    ty
}

fn normalize_type_name(type_name: &str) -> String {
    type_name.trim_start_matches('.').to_owned()
}

fn qualify(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}.{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proto_ingest_extracts_services_and_messages() {
        let source = br#"
            syntax = "proto3";
            package payments;

            message PaymentRequest {
                string id = 1;
            }

            message PaymentResponse {
                string id = 1;
                string status = 2;
            }

            service PaymentService {
                rpc GetPayment (PaymentRequest) returns (PaymentResponse);
            }
        "#;

        let contract = ingest("api/service.proto", source).expect("ingest");
        assert_eq!(contract.endpoints.len(), 1);
        let endpoint = contract
            .endpoints
            .get(&EndpointKey {
                method: "RPC".to_owned(),
                path: "/payments.PaymentService/GetPayment".to_owned(),
            })
            .expect("endpoint");
        assert_eq!(
            endpoint.operation_id.as_deref(),
            Some("payments.PaymentService.GetPayment")
        );
        assert!(endpoint.request.is_some());
        assert!(endpoint.responses.contains_key("200"));
        assert!(contract.unmodelled.is_empty());
    }
}
