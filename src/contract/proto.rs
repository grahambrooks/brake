//! Protobuf 3 `.proto` source → `Contract`.
//!
//! The governing fact of this ingester is that **protobuf compatibility is
//! defined by field number, not by field name**. A field renamed with a stable
//! number is wire-compatible; a field renumbered with a stable name is a hard
//! break that no name-based diff can see. Every field and enum value therefore
//! carries its number into the model, and `compare/` uses it as the identity.

use std::collections::{BTreeMap, BTreeSet};

use prost_types::{DescriptorProto, EnumDescriptorProto, FieldDescriptorProto, SourceCodeInfo};
use thiserror::Error;

use super::{
    Constraints, Contract, Endpoint, EndpointKey, Field, MEDIA_GRPC, MEDIA_GRPC_STREAM, Payload,
    Span, TypeRef, Unmodelled, UnmodelledKind,
};

/// Field descriptor label values, from `descriptor.proto`.
const LABEL_OPTIONAL: i32 = 1;
const LABEL_REQUIRED: i32 = 2;
const LABEL_REPEATED: i32 = 3;

/// `FileDescriptorProto` field numbers, for `source_code_info` paths.
const PATH_SERVICE: i32 = 6;
const PATH_SERVICE_METHOD: i32 = 2;

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

    let package = descriptor.package.clone().unwrap_or_default();
    let mut registry = Registry {
        source: source.to_owned(),
        package: package.clone(),
        messages: BTreeMap::new(),
        enums: BTreeMap::new(),
        unmodelled: Vec::new(),
    };
    registry.collect_messages(&package, &descriptor.message_type);
    registry.collect_enums(&package, &descriptor.enum_type);

    let spans = Spans::new(source, descriptor.source_code_info.as_ref());
    let mut contract = Contract::empty();

    for (service_index, service) in descriptor.service.iter().enumerate() {
        let Some(service_name) = service.name.as_ref() else {
            continue;
        };
        let scoped_service = qualify(&package, service_name);

        for (method_index, method) in service.method.iter().enumerate() {
            let Some(method_name) = method.name.as_ref() else {
                continue;
            };
            let pointer = format!("/service/{service_index}/method/{method_index}");
            let span = spans.for_path(
                &[
                    PATH_SERVICE,
                    service_index as i32,
                    PATH_SERVICE_METHOD,
                    method_index as i32,
                ],
                &pointer,
            );

            // A method that changes between unary and streaming changes the
            // call shape a generated client uses, in both directions.
            let request_media = if method.client_streaming.unwrap_or(false) {
                MEDIA_GRPC_STREAM
            } else {
                MEDIA_GRPC
            };
            let response_media = if method.server_streaming.unwrap_or(false) {
                MEDIA_GRPC_STREAM
            } else {
                MEDIA_GRPC
            };

            let request = method.input_type.as_deref().map(|name| Payload {
                media_types: BTreeMap::from([(
                    request_media.to_owned(),
                    registry.resolve(name, &mut BTreeSet::new()),
                )]),
                span: span.clone(),
            });
            let mut responses = BTreeMap::new();
            if let Some(output) = method.output_type.as_deref() {
                responses.insert(
                    "200".to_owned(),
                    Payload {
                        media_types: BTreeMap::from([(
                            response_media.to_owned(),
                            registry.resolve(output, &mut BTreeSet::new()),
                        )]),
                        span: span.clone(),
                    },
                );
            }

            contract.endpoints.insert(
                EndpointKey {
                    method: "RPC".to_owned(),
                    path: format!("/{scoped_service}/{method_name}"),
                },
                Endpoint {
                    operation_id: Some(format!("{scoped_service}.{method_name}")),
                    deprecated: method
                        .options
                        .as_ref()
                        .and_then(|options| options.deprecated)
                        .unwrap_or(false),
                    sunset: None,
                    parameters: Vec::new(),
                    request,
                    responses,
                    security: Vec::new(),
                    span,
                },
            );
        }
    }

    contract.unmodelled = registry.unmodelled;
    contract
        .unmodelled
        .sort_by(|a, b| a.pointer.cmp(&b.pointer).then_with(|| a.kind.cmp(&b.kind)));
    contract.unmodelled.dedup();
    Ok(contract)
}

struct Registry {
    source: String,
    /// The file's `package`, which is the outermost scope a relative type
    /// reference is resolved against.
    package: String,
    messages: BTreeMap<String, DescriptorProto>,
    enums: BTreeMap<String, EnumDescriptorProto>,
    unmodelled: Vec<Unmodelled>,
}

impl Registry {
    fn collect_messages(&mut self, parent: &str, descriptors: &[DescriptorProto]) {
        for descriptor in descriptors {
            let Some(name) = descriptor.name.as_ref() else {
                continue;
            };
            let fq = qualify(parent, name);
            self.messages.insert(fq.clone(), descriptor.clone());
            self.collect_messages(&fq, &descriptor.nested_type);
            self.collect_enums(&fq, &descriptor.enum_type);
        }
    }

    fn collect_enums(&mut self, parent: &str, descriptors: &[EnumDescriptorProto]) {
        for descriptor in descriptors {
            let Some(name) = descriptor.name.as_ref() else {
                continue;
            };
            self.enums.insert(qualify(parent, name), descriptor.clone());
        }
    }

    fn record(&mut self, kind: UnmodelledKind, pointer: &str) -> TypeRef {
        self.unmodelled.push(Unmodelled {
            kind: kind.clone(),
            pointer: pointer.to_owned(),
            span: Span::new(&self.source, 1, 1, pointer),
        });
        TypeRef::Unknown(kind)
    }

    /// Resolve a type reference the way protoc does.
    ///
    /// `protox_parse` is a parse-only step and leaves references as written,
    /// so `rpc Get(Req)` arrives as `Req` rather than `.payments.Req`. A
    /// leading dot means fully qualified; otherwise the innermost enclosing
    /// scope wins and the search widens outward to the package root.
    fn qualify_reference(&self, type_name: &str) -> String {
        if let Some(absolute) = type_name.strip_prefix('.') {
            return absolute.to_owned();
        }

        let mut scope = self.package.as_str();
        loop {
            let candidate = qualify(scope, type_name);
            if self.messages.contains_key(&candidate) || self.enums.contains_key(&candidate) {
                return candidate;
            }
            match scope.rfind('.') {
                Some(cut) => scope = &scope[..cut],
                None => break,
            }
        }
        type_name.to_owned()
    }

    fn resolve(&mut self, type_name: &str, visiting: &mut BTreeSet<String>) -> TypeRef {
        let normalized = self.qualify_reference(type_name);

        if let Some(message) = self.messages.get(&normalized).cloned() {
            if !visiting.insert(normalized.clone()) {
                return TypeRef::Cycle(normalized);
            }
            let object = self.object_for(&normalized, &message, visiting);
            visiting.remove(&normalized);
            return object;
        }

        if let Some(enum_descriptor) = self.enums.get(&normalized).cloned() {
            let mut values = BTreeSet::new();
            let mut numbers = BTreeMap::new();
            for value in &enum_descriptor.value {
                let Some(name) = value.name.clone() else {
                    continue;
                };
                if let Some(number) = value.number {
                    numbers.insert(name.clone(), number);
                }
                values.insert(name);
            }
            return TypeRef::Enum { values, numbers };
        }

        // An unresolved type is almost always an `import` this ingester was
        // not given. Reporting it keeps the run honest: bytes-only ingest
        // cannot read the imported file, and pretending the type matched
        // would be a false clean.
        self.record(
            UnmodelledKind::ExternalRef(normalized.clone()),
            &format!("/type/{normalized}"),
        )
    }

    fn object_for(
        &mut self,
        message_name: &str,
        message: &DescriptorProto,
        visiting: &mut BTreeSet<String>,
    ) -> TypeRef {
        // A `map<k, v>` is compiled to a synthetic repeated entry message.
        // Both sides normalise the same way, so it compares consistently.
        let mut fields = BTreeMap::new();
        for field in &message.field {
            let Some(name) = field.name.clone() else {
                continue;
            };
            let ty = self.field_type(message_name, field, visiting);
            fields.insert(
                name,
                Field {
                    ty,
                    // proto3 has no `required`; presence is what `optional`
                    // and message-typed fields express, and neither makes a
                    // field mandatory on the wire.
                    required: field.label == Some(LABEL_REQUIRED),
                    deprecated: field
                        .options
                        .as_ref()
                        .and_then(|options| options.deprecated)
                        .unwrap_or(false),
                    number: field.number,
                },
            );
        }

        // A number inside a `reserved` range must never come back: reusing it
        // makes new data decode as the old field in deployed clients.
        for range in &message.reserved_range {
            if let (Some(start), Some(end)) = (range.start, range.end) {
                let reused = fields
                    .values()
                    .filter_map(|field| field.number)
                    .any(|number| number >= start && number < end);
                if reused {
                    self.record(
                        UnmodelledKind::Unsupported(format!(
                            "a field reuses reserved number range {start}..{end}"
                        )),
                        &format!("/message/{message_name}/reserved"),
                    );
                }
            }
        }

        TypeRef::Object {
            fields,
            additional: false,
            nullable: false,
        }
    }

    fn field_type(
        &mut self,
        message_name: &str,
        field: &FieldDescriptorProto,
        visiting: &mut BTreeSet<String>,
    ) -> TypeRef {
        let pointer = format!(
            "/message/{message_name}/field/{}",
            field.name.clone().unwrap_or_default()
        );
        // `type_name` is checked first because `protox_parse` is parse-only:
        // it cannot know whether `Status` names a message or an enum without
        // resolution, so it leaves `type` unset and only fills `type_name`.
        let mut ty = if let Some(type_name) = field.type_name.as_deref() {
            self.resolve(type_name, visiting)
        } else if let Some((name, format)) = field.r#type.and_then(scalar_for_proto_type) {
            TypeRef::Scalar {
                ty: name.to_owned(),
                format: format.map(ToOwned::to_owned),
                nullable: false,
                constraints: Constraints::default(),
            }
        } else {
            self.record(UnmodelledKind::InvalidShape, &pointer)
        };

        if field.label == Some(LABEL_REPEATED) {
            ty = TypeRef::Array {
                items: Box::new(ty),
                nullable: false,
            };
        } else if field.label == Some(LABEL_OPTIONAL)
            && field.proto3_optional.unwrap_or(false)
            && let TypeRef::Scalar {
                ty: name,
                format,
                constraints,
                ..
            } = ty
        {
            // `optional` in proto3 restores explicit presence, which is the
            // closest thing the format has to nullability.
            ty = TypeRef::Scalar {
                ty: name,
                format,
                nullable: true,
                constraints,
            };
        }
        ty
    }
}

/// Proto scalar types, preserving the distinctions that matter on the wire.
///
/// `int32` and `int64` are wire-compatible with each other and are not
/// collapsed here anyway: keeping the declared name means a change between
/// signed and unsigned, or fixed and varint, is visible rather than lost to a
/// coarse "integer" bucket.
fn scalar_for_proto_type(value: i32) -> Option<(&'static str, Option<&'static str>)> {
    let mapped = match value {
        1 => ("number", Some("double")),
        2 => ("number", Some("float")),
        3 => ("integer", Some("int64")),
        4 => ("integer", Some("uint64")),
        5 => ("integer", Some("int32")),
        6 => ("integer", Some("fixed64")),
        7 => ("integer", Some("fixed32")),
        8 => ("boolean", None),
        9 => ("string", None),
        12 => ("string", Some("bytes")),
        13 => ("integer", Some("uint32")),
        15 => ("integer", Some("sfixed32")),
        16 => ("integer", Some("sfixed64")),
        17 => ("integer", Some("sint32")),
        18 => ("integer", Some("sint64")),
        // 10 group, 11 message, 14 enum — resolved through `type_name`.
        _ => return None,
    };
    Some(mapped)
}

/// Source locations from the descriptor's `source_code_info`.
///
/// Without this every protobuf finding pointed at line 1, which makes a SARIF
/// annotation useless and a text diagnostic misleading.
struct Spans<'a> {
    source: &'a str,
    info: Option<&'a SourceCodeInfo>,
}

impl<'a> Spans<'a> {
    fn new(source: &'a str, info: Option<&'a SourceCodeInfo>) -> Self {
        Self { source, info }
    }

    fn for_path(&self, path: &[i32], pointer: &str) -> Span {
        let located = self.info.and_then(|info| {
            info.location
                .iter()
                .find(|location| location.path == path)
                .and_then(|location| {
                    // span is [start_line, start_col, end_col] or
                    // [start_line, start_col, end_line, end_col], zero-based.
                    let line = *location.span.first()? as usize + 1;
                    let column = *location.span.get(1)? as usize + 1;
                    Some((line, column))
                })
        });
        let (line, column) = located.unwrap_or((1, 1));
        Span::new(self.source, line, column, pointer)
    }
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
    use crate::compare::{ChangeKind, compare_contracts};

    fn body(fields: &str, service: &str) -> String {
        format!(
            r#"
syntax = "proto3";
package payments;

message Payment {{
{fields}
}}

message Req {{
  string id = 1;
}}

service PaymentService {{
{service}
}}
"#
        )
    }

    fn kinds(base: &str, head: &str) -> Vec<ChangeKind> {
        let base = ingest("api/payments.proto", base.as_bytes()).expect("base");
        let head = ingest("api/payments.proto", head.as_bytes()).expect("head");
        compare_contracts(&base, &head)
            .iter()
            .map(|change| change.kind)
            .collect()
    }

    #[test]
    fn extracts_services_messages_and_field_numbers() {
        let source = body(
            "  string id = 1;\n  int32 amount = 2;",
            "  rpc Get(Req) returns (Payment);",
        );
        let contract = ingest("api/service.proto", source.as_bytes()).expect("ingest");

        assert_eq!(contract.endpoints.len(), 1);
        let endpoint = &contract.endpoints[&EndpointKey {
            method: "RPC".to_owned(),
            path: "/payments.PaymentService/Get".to_owned(),
        }];
        assert_eq!(
            endpoint.operation_id.as_deref(),
            Some("payments.PaymentService.Get")
        );

        let TypeRef::Object { fields, .. } = &endpoint.responses["200"].media_types[MEDIA_GRPC]
        else {
            panic!("response should be an object");
        };
        assert_eq!(fields["id"].number, Some(1));
        assert_eq!(fields["amount"].number, Some(2));
        assert!(contract.unmodelled.is_empty());
    }

    #[test]
    fn renumbering_a_field_is_a_wire_break() {
        let kinds = kinds(
            &body("  string id = 1;", "  rpc Get(Req) returns (Payment);"),
            &body("  string id = 7;", "  rpc Get(Req) returns (Payment);"),
        );
        assert!(
            kinds.contains(&ChangeKind::FieldNumberChanged),
            "renumbering is the canonical protobuf break: {kinds:?}"
        );
    }

    #[test]
    fn renaming_a_field_with_a_stable_number_is_a_rename_not_a_removal() {
        let kinds = kinds(
            &body("  string id = 1;", "  rpc Get(Req) returns (Payment);"),
            &body(
                "  string identifier = 1;",
                "  rpc Get(Req) returns (Payment);",
            ),
        );
        assert!(kinds.contains(&ChangeKind::FieldRenamed));
        assert!(
            !kinds.contains(&ChangeKind::ResponseFieldRemoved),
            "a stable wire number means the field survived: {kinds:?}"
        );
    }

    #[test]
    fn identical_files_produce_nothing() {
        let source = body("  string id = 1;", "  rpc Get(Req) returns (Payment);");
        assert!(kinds(&source, &source).is_empty());
    }

    #[test]
    fn removing_an_rpc_is_an_endpoint_removal() {
        let kinds = kinds(
            &body("  string id = 1;", "  rpc Get(Req) returns (Payment);"),
            &body("  string id = 1;", ""),
        );
        assert!(kinds.contains(&ChangeKind::EndpointRemoved));
    }

    #[test]
    fn switching_a_method_to_streaming_is_visible() {
        let kinds = kinds(
            &body("  string id = 1;", "  rpc Get(Req) returns (Payment);"),
            &body(
                "  string id = 1;",
                "  rpc Get(Req) returns (stream Payment);",
            ),
        );
        assert!(
            kinds.contains(&ChangeKind::ResponseMediaTypeRemoved),
            "a unary method becoming streaming changes every client: {kinds:?}"
        );
    }

    #[test]
    fn enum_value_renumbering_is_a_wire_break() {
        let base = r#"
syntax = "proto3";
package payments;
enum Status { UNKNOWN = 0; PAID = 1; }
message Payment { Status status = 1; }
message Req { string id = 1; }
service S { rpc Get(Req) returns (Payment); }
"#;
        let head = r#"
syntax = "proto3";
package payments;
enum Status { UNKNOWN = 0; PAID = 5; }
message Payment { Status status = 1; }
message Req { string id = 1; }
service S { rpc Get(Req) returns (Payment); }
"#;
        let kinds = kinds(base, head);
        assert!(
            kinds.contains(&ChangeKind::FieldNumberChanged),
            "renumbering an enum value changes what the bytes mean: {kinds:?}"
        );
    }

    #[test]
    fn an_unresolved_import_is_reported_not_assumed_compatible() {
        let source = r#"
syntax = "proto3";
package payments;
import "other/common.proto";
message Req { string id = 1; }
message Payment { common.Money amount = 1; }
service S { rpc Get(Req) returns (Payment); }
"#;
        let contract = ingest("api/payments.proto", source.as_bytes()).expect("ingest");
        assert!(
            !contract.unmodelled.is_empty(),
            "a type from an unread import must not compare as verified"
        );
    }

    #[test]
    fn narrowing_an_integer_format_is_visible() {
        let kinds = kinds(
            &body("  int64 amount = 1;", "  rpc Get(Req) returns (Payment);"),
            &body("  uint32 amount = 1;", "  rpc Get(Req) returns (Payment);"),
        );
        assert!(
            kinds.contains(&ChangeKind::ResponseTypeChanged),
            "int64 to uint32 changes what values can be represented: {kinds:?}"
        );
    }

    #[test]
    fn spans_point_at_the_method_not_line_one() {
        let source = body("  string id = 1;", "  rpc Get(Req) returns (Payment);");
        let contract = ingest("api/service.proto", source.as_bytes()).expect("ingest");
        let endpoint = contract.endpoints.values().next().expect("one endpoint");
        assert!(
            endpoint.span.line > 1,
            "protobuf spans must locate the method, got line {}",
            endpoint.span.line
        );
    }
}
