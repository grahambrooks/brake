//! AsyncAPI 2.x / 3.x → `Contract`.
//!
//! Ingest is bytes-only and hermetic: no filesystem or network access without
//! explicit resolver. Channels and operations are mapped directly to `Contract`
//! `EndpointKey` and `Payload` models.

use std::collections::{BTreeMap, BTreeSet};

use saphyr::{LoadableYamlNode, MarkedYamlOwned, ScalarOwned, YamlDataOwned};
use thiserror::Error;

use super::{
    Constraints, Contract, Discriminator, DocumentResolver, Endpoint, EndpointKey, Field,
    Parameter, Payload, SingleDocumentResolver, Span, TypeRef, Unmodelled, UnmodelledKind,
};

#[derive(Debug, Error)]
pub enum AsyncApiError {
    #[error("contract source `{contract_source}` is not valid UTF-8: {error}")]
    InvalidUtf8 {
        contract_source: String,
        error: std::str::Utf8Error,
    },
    #[error("failed to parse AsyncAPI document `{contract_source}`: {error}")]
    Parse {
        contract_source: String,
        error: saphyr::ScanError,
    },
    #[error("AsyncAPI document `{contract_source}` does not contain a root mapping")]
    RootNotMapping { contract_source: String },
    #[error("AsyncAPI document `{contract_source}` declares no `asyncapi` version")]
    MissingVersion { contract_source: String },
    #[error(
        "AsyncAPI document `{contract_source}` declares unsupported version `{version}`; \
         brake supports AsyncAPI 2.x and 3.x"
    )]
    UnsupportedVersion {
        contract_source: String,
        version: String,
    },
    #[error("AsyncAPI document `{contract_source}` has no `channels` object")]
    MissingChannels { contract_source: String },
    #[error(
        "`$ref` `{reference}` in `{contract_source}` resolves over the network. \
         brake never makes a network request: remote refs are refused, not fetched"
    )]
    RemoteRef {
        contract_source: String,
        reference: String,
    },
    #[error(
        "`$ref` `{reference}` in `{contract_source}` escapes the directory containing the \
         contract source. Local refs resolve only within that tree"
    )]
    EscapingRef {
        contract_source: String,
        reference: String,
    },
}

pub fn ingest(source: &str, bytes: &[u8]) -> Result<Contract, AsyncApiError> {
    ingest_with_resolver(source, bytes, &SingleDocumentResolver)
}

pub fn ingest_with_resolver(
    source: &str,
    bytes: &[u8],
    resolver: &dyn DocumentResolver,
) -> Result<Contract, AsyncApiError> {
    let input = std::str::from_utf8(bytes).map_err(|error| AsyncApiError::InvalidUtf8 {
        contract_source: source.to_owned(),
        error,
    })?;

    let docs = MarkedYamlOwned::load_from_str(input).map_err(|error| AsyncApiError::Parse {
        contract_source: source.to_owned(),
        error,
    })?;
    let root = docs.first().ok_or_else(|| AsyncApiError::RootNotMapping {
        contract_source: source.to_owned(),
    })?;
    if root.data.as_mapping().is_none() {
        return Err(AsyncApiError::RootNotMapping {
            contract_source: source.to_owned(),
        });
    }
    check_version(source, root)?;

    let mut ingester = Ingester {
        root,
        source,
        resolver,
        endpoints: BTreeMap::new(),
        unmodelled: Vec::new(),
        external_docs: BTreeMap::new(),
        resolving_files: Vec::new(),
        visited_pointers: BTreeSet::new(),
        fatal: None,
    };

    ingester.ingest()?;
    if let Some(err) = ingester.fatal {
        return Err(err);
    }

    Ok(Contract {
        endpoints: ingester.endpoints,
        security_schemes: BTreeMap::new(),
        unmodelled: ingester.unmodelled,
    })
}

fn check_version(source: &str, root: &MarkedYamlOwned) -> Result<(), AsyncApiError> {
    let version_node =
        get_map_value(root, "asyncapi").ok_or_else(|| AsyncApiError::MissingVersion {
            contract_source: source.to_owned(),
        })?;
    let version = string_node(version_node).ok_or_else(|| AsyncApiError::MissingVersion {
        contract_source: source.to_owned(),
    })?;
    if !version.starts_with("2.") && !version.starts_with("3.") {
        return Err(AsyncApiError::UnsupportedVersion {
            contract_source: source.to_owned(),
            version: version.to_owned(),
        });
    }
    Ok(())
}

struct Ingester<'a> {
    root: &'a MarkedYamlOwned,
    source: &'a str,
    resolver: &'a dyn DocumentResolver,
    endpoints: BTreeMap<EndpointKey, Endpoint>,
    unmodelled: Vec<Unmodelled>,
    external_docs: BTreeMap<String, MarkedYamlOwned>,
    resolving_files: Vec<String>,
    visited_pointers: BTreeSet<String>,
    fatal: Option<AsyncApiError>,
}

impl<'a> Ingester<'a> {
    fn current_source(&self) -> &str {
        self.resolving_files
            .last()
            .map(|s| s.as_str())
            .unwrap_or(self.source)
    }

    fn current_root(&self) -> &MarkedYamlOwned {
        if let Some(doc) = self
            .resolving_files
            .last()
            .and_then(|current_file| self.external_docs.get(current_file))
        {
            return doc;
        }
        self.root
    }

    fn record(&mut self, kind: UnmodelledKind, pointer: &str, node: &MarkedYamlOwned) -> TypeRef {
        self.unmodelled.push(Unmodelled {
            kind: kind.clone(),
            pointer: pointer.to_owned(),
            span: span(self.current_source(), node, pointer),
        });
        TypeRef::Unknown(kind)
    }

    fn ingest(&mut self) -> Result<(), AsyncApiError> {
        let channels_node = get_map_value(self.root, "channels");
        let operations_node = get_map_value(self.root, "operations");

        if channels_node.is_none() && operations_node.is_none() {
            return Err(AsyncApiError::MissingChannels {
                contract_source: self.source.to_owned(),
            });
        }

        // Handle AsyncAPI 2.x and 3.x channels
        if let Some(channels) = channels_node.and_then(|n| n.data.as_mapping()) {
            for (channel_name_node, channel_item) in channels {
                let Some(channel_path) = string_node(channel_name_node) else {
                    continue;
                };

                // AsyncAPI 2.x publish / subscribe on channel
                for (op_key, method) in [("publish", "PUBLISH"), ("subscribe", "SUBSCRIBE")] {
                    if let Some(operation) = get_map_value(channel_item, op_key) {
                        self.ingest_operation_2x(
                            channel_path,
                            method,
                            operation,
                            channel_item,
                            channel_name_node,
                        );
                    }
                }
            }
        }

        // Handle AsyncAPI 3.x operations
        if let Some(operations) = operations_node.and_then(|n| n.data.as_mapping()) {
            for (op_name_node, op_item) in operations {
                let action = get_map_value(op_item, "action")
                    .and_then(string_node)
                    .unwrap_or("send");
                let method = match action {
                    "receive" => "SUBSCRIBE",
                    _ => "PUBLISH",
                };

                let channel_ref = get_map_value(op_item, "channel")
                    .and_then(|c| get_map_value(c, "$ref"))
                    .and_then(string_node);

                let mut channel_address = None;
                if let Some(r) = channel_ref {
                    if let Some(addr) = self
                        .lookup_json_pointer(self.current_root(), r.trim_start_matches('#'))
                        .and_then(|target| get_map_value(target, "address"))
                        .and_then(string_node)
                    {
                        channel_address = Some(addr.to_owned());
                    }
                    if channel_address.is_none() {
                        let parts: Vec<&str> = r.split('/').collect();
                        channel_address = parts.last().map(|s| (*s).to_owned());
                    }
                } else if let Some(addr) = get_map_value(op_item, "channel")
                    .and_then(|direct_chan| get_map_value(direct_chan, "address"))
                    .and_then(string_node)
                {
                    channel_address = Some(addr.to_owned());
                }

                let addr = channel_address
                    .unwrap_or_else(|| string_node(op_name_node).unwrap_or("default").to_owned());

                self.ingest_operation_3x(&addr, method, op_item, op_name_node);
            }
        }

        Ok(())
    }

    fn ingest_operation_2x(
        &mut self,
        channel_path: &str,
        method: &str,
        operation: &MarkedYamlOwned,
        channel_item: &MarkedYamlOwned,
        name_node: &MarkedYamlOwned,
    ) {
        let op_pointer = format!("/channels/{channel_path}/{method}");
        let op_span = span(self.source, name_node, op_pointer.as_str());

        let mut parameters = Vec::new();
        // Channel parameters
        if let Some(params) =
            get_map_value(channel_item, "parameters").and_then(|n| n.data.as_mapping())
        {
            for (param_name_node, param_item) in params {
                if let Some(name) = string_node(param_name_node) {
                    let schema = get_map_value(param_item, "schema").unwrap_or(param_item);
                    let param_pointer = format!("{op_pointer}/parameters/{name}");
                    let param_span = span(self.source, param_name_node, param_pointer.as_str());
                    let ty = self.parse_schema(schema, &param_pointer);
                    parameters.push(Parameter {
                        name: name.to_owned(),
                        location: "path".to_owned(),
                        required: true,
                        deprecated: false,
                        ty,
                        span: param_span,
                    });
                }
            }
        }

        // Message headers and payload
        let message_node = get_map_value(operation, "message");
        let (request_body, responses) =
            self.extract_message_payloads(message_node, &op_pointer, method);

        let endpoint_key = EndpointKey {
            method: method.to_owned(),
            path: if channel_path.starts_with('/') {
                channel_path.to_owned()
            } else {
                format!("/{channel_path}")
            },
        };

        self.endpoints.insert(
            endpoint_key,
            Endpoint {
                operation_id: None,
                deprecated: false,
                sunset: None,
                parameters,
                request: request_body,
                responses,
                security: Vec::new(),
                span: op_span,
            },
        );
    }

    fn ingest_operation_3x(
        &mut self,
        channel_path: &str,
        method: &str,
        operation: &MarkedYamlOwned,
        name_node: &MarkedYamlOwned,
    ) {
        let op_pointer = format!("/operations/{channel_path}/{method}");
        let op_span = span(self.source, name_node, op_pointer.as_str());

        let message_node =
            get_map_value(operation, "messages").or_else(|| get_map_value(operation, "message"));
        let (request_body, responses) =
            self.extract_message_payloads(message_node, &op_pointer, method);

        let endpoint_key = EndpointKey {
            method: method.to_owned(),
            path: if channel_path.starts_with('/') {
                channel_path.to_owned()
            } else {
                format!("/{channel_path}")
            },
        };

        self.endpoints.insert(
            endpoint_key,
            Endpoint {
                operation_id: None,
                deprecated: false,
                sunset: None,
                parameters: Vec::new(),
                request: request_body,
                responses,
                security: Vec::new(),
                span: op_span,
            },
        );
    }

    fn extract_message_payloads(
        &mut self,
        message_node: Option<&MarkedYamlOwned>,
        op_pointer: &str,
        method: &str,
    ) -> (Option<Payload>, BTreeMap<String, Payload>) {
        let mut request_body = None;
        let mut responses = BTreeMap::new();

        let Some(msg) = message_node else {
            return (request_body, responses);
        };

        let schema_node = if let Some(payload) = get_map_value(msg, "payload") {
            payload
        } else if let Some(one_of) = get_map_value(msg, "oneOf") {
            one_of
        } else {
            msg
        };

        let payload_pointer = format!("{op_pointer}/payload");
        let ty = self.parse_schema(schema_node, &payload_pointer);
        let mut media_types = BTreeMap::new();
        media_types.insert("application/json".to_owned(), ty);

        let payload = Payload {
            span: span(self.source, schema_node, payload_pointer.as_str()),
            media_types,
        };

        if method == "SUBSCRIBE" {
            request_body = Some(payload);
        } else {
            responses.insert("200".to_owned(), payload);
        }

        (request_body, responses)
    }

    fn parse_schema(&mut self, schema_node: &MarkedYamlOwned, pointer: &str) -> TypeRef {
        if let Some(reference) = get_map_value(schema_node, "$ref").and_then(string_node) {
            return self.resolve_schema_reference(reference, pointer, schema_node);
        }

        if let Some(all_of) =
            get_map_value(schema_node, "allOf").and_then(|node| node.data.as_vec())
        {
            return self.flatten_all_of(all_of, pointer);
        }

        for keyword in ["oneOf", "anyOf"] {
            if let Some(variants) =
                get_map_value(schema_node, keyword).and_then(|node| node.data.as_vec())
            {
                let parsed = variants
                    .iter()
                    .enumerate()
                    .map(|(index, variant)| {
                        self.parse_schema(variant, &format!("{pointer}/{keyword}/{index}"))
                    })
                    .collect();
                let discriminator = self.parse_discriminator(schema_node);
                return TypeRef::OneOf {
                    variants: parsed,
                    discriminator,
                };
            }
        }

        if let Some(enum_values) =
            get_map_value(schema_node, "enum").and_then(|node| node.data.as_vec())
        {
            let mut values = BTreeSet::new();
            for (index, value) in enum_values.iter().enumerate() {
                match scalar_repr(value) {
                    Some(repr) => {
                        values.insert(repr);
                    }
                    None => {
                        return self.record(
                            UnmodelledKind::Unsupported(format!("enum value {index}")),
                            pointer,
                            value,
                        );
                    }
                }
            }
            if !values.is_empty() {
                return TypeRef::Enum {
                    values,
                    numbers: BTreeMap::new(),
                };
            }
        }

        let nullable = get_map_value(schema_node, "nullable")
            .and_then(bool_node)
            .unwrap_or(false);

        if let Some(ty_value) = get_map_value(schema_node, "type") {
            let Some(parsed) = parse_type_value(ty_value) else {
                return self.record(
                    UnmodelledKind::Unsupported("type".to_owned()),
                    pointer,
                    ty_value,
                );
            };
            let is_null = nullable || parsed.nullable;

            if parsed.names.len() > 1 {
                return TypeRef::OneOf {
                    variants: parsed
                        .names
                        .iter()
                        .map(|name| self.type_for_name(name, schema_node, pointer, is_null))
                        .collect(),
                    discriminator: None,
                };
            }
            let Some(name) = parsed.names.first() else {
                return TypeRef::Scalar {
                    ty: "null".to_owned(),
                    format: None,
                    nullable: true,
                    constraints: Constraints::default(),
                };
            };
            return self.type_for_name(name, schema_node, pointer, is_null);
        }

        if get_map_value(schema_node, "properties").is_some()
            || get_map_value(schema_node, "additionalProperties").is_some()
        {
            return self.parse_object_type(schema_node, pointer, nullable);
        }
        if get_map_value(schema_node, "items").is_some()
            || get_map_value(schema_node, "prefixItems").is_some()
        {
            return self.parse_array_type(schema_node, pointer, nullable);
        }

        TypeRef::Object {
            fields: BTreeMap::new(),
            additional: true,
            nullable: true,
        }
    }

    fn parse_discriminator(&self, schema_node: &MarkedYamlOwned) -> Option<Discriminator> {
        let disc_node = get_map_value(schema_node, "discriminator")?;
        let prop_name = get_map_value(disc_node, "propertyName")
            .and_then(string_node)?
            .to_owned();
        let mut mapping = BTreeMap::new();
        if let Some(map_node) =
            get_map_value(disc_node, "mapping").and_then(|n| n.data.as_mapping())
        {
            for (k, v) in map_node {
                if let (Some(ks), Some(vs)) = (string_node(k), string_node(v)) {
                    mapping.insert(ks.to_owned(), vs.to_owned());
                }
            }
        }
        Some(Discriminator {
            property_name: prop_name,
            mapping,
        })
    }

    fn parse_array_type(
        &mut self,
        schema_node: &MarkedYamlOwned,
        pointer: &str,
        nullable: bool,
    ) -> TypeRef {
        if let Some(prefix_items_node) =
            get_map_value(schema_node, "prefixItems").and_then(|node| node.data.as_vec())
        {
            let prefix_items = prefix_items_node
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    self.parse_schema(item, &format!("{pointer}/prefixItems/{index}"))
                })
                .collect();
            let additional_items = get_map_value(schema_node, "items")
                .map(|items| Box::new(self.parse_schema(items, &format!("{pointer}/items"))));
            return TypeRef::Tuple {
                prefix_items,
                additional_items,
                nullable,
            };
        }

        let items = match get_map_value(schema_node, "items") {
            Some(items) => self.parse_schema(items, &format!("{pointer}/items")),
            None => self.record(
                UnmodelledKind::SchemaDeferred,
                &format!("{pointer}/items"),
                schema_node,
            ),
        };
        TypeRef::Array {
            items: Box::new(items),
            nullable,
        }
    }

    fn parse_object_type(
        &mut self,
        schema_node: &MarkedYamlOwned,
        pointer: &str,
        nullable: bool,
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
                let field_pointer = format!("{pointer}/properties/{}", escape_json_pointer(name));
                let field_span = span(self.current_source(), name_node, field_pointer.as_str());
                fields.insert(
                    name.to_owned(),
                    Field {
                        ty: self.parse_schema(property_schema, &field_pointer),
                        required: required.contains(name),
                        deprecated: get_map_value(property_schema, "deprecated")
                            .and_then(bool_node)
                            .unwrap_or(false),
                        number: None,
                        span: Some(field_span),
                    },
                );
            }
        }

        let additional = match get_map_value(schema_node, "additionalProperties") {
            None => true,
            Some(node) => bool_node(node).unwrap_or(true),
        };

        TypeRef::Object {
            fields,
            additional,
            nullable,
        }
    }

    fn type_for_name(
        &mut self,
        name: &str,
        schema_node: &MarkedYamlOwned,
        pointer: &str,
        nullable: bool,
    ) -> TypeRef {
        match name {
            "object" => self.parse_object_type(schema_node, pointer, nullable),
            "array" => self.parse_array_type(schema_node, pointer, nullable),
            _ => TypeRef::Scalar {
                ty: name.to_owned(),
                format: get_map_value(schema_node, "format")
                    .and_then(string_node)
                    .map(ToOwned::to_owned),
                nullable,
                constraints: parse_constraints(schema_node),
            },
        }
    }

    fn flatten_all_of(&mut self, members: &[MarkedYamlOwned], pointer: &str) -> TypeRef {
        let mut fields = BTreeMap::new();
        let mut additional = true;
        let mut nullable = false;

        for (index, member) in members.iter().enumerate() {
            let member_ty = self.parse_schema(member, &format!("{pointer}/allOf/{index}"));
            match member_ty {
                TypeRef::Object {
                    fields: member_fields,
                    additional: member_additional,
                    nullable: member_nullable,
                } => {
                    for (name, field) in member_fields {
                        fields.insert(name, field);
                    }
                    if !member_additional {
                        additional = false;
                    }
                    if member_nullable {
                        nullable = true;
                    }
                }
                TypeRef::Unknown(_) => return member_ty,
                _ => {}
            }
        }

        TypeRef::Object {
            fields,
            additional,
            nullable,
        }
    }

    fn resolve_schema_reference(
        &mut self,
        reference: &str,
        pointer: &str,
        node: &MarkedYamlOwned,
    ) -> TypeRef {
        if reference.starts_with("http://") || reference.starts_with("https://") {
            self.fatal.get_or_insert(AsyncApiError::RemoteRef {
                contract_source: self.source.to_owned(),
                reference: reference.to_owned(),
            });
            return TypeRef::Unknown(UnmodelledKind::InvalidShape);
        }

        if reference.starts_with('/') || reference.contains("../") {
            self.fatal.get_or_insert(AsyncApiError::EscapingRef {
                contract_source: self.source.to_owned(),
                reference: reference.to_owned(),
            });
            return TypeRef::Unknown(UnmodelledKind::InvalidShape);
        }

        let (file_part, pointer_part) = reference.split_once('#').unwrap_or((reference, ""));
        if !file_part.is_empty() {
            // Relative to the *document*, never to `source`: for a baseline read
            // out of git, `source` is a descriptor like `rev:HEAD`, and
            // deriving a directory from that resolved every sibling `$ref`
            // against the wrong place. The top-level document sits at the
            // resolver's root, so it contributes no directory of its own.
            let resolved_path = resolve_relative_file_path(
                self.resolving_files.last().map_or("", String::as_str),
                file_part,
            );
            if let Some(parsed) =
                self.resolve_external_schema(&resolved_path, pointer_part, pointer)
            {
                return parsed;
            }
            return self.record(
                UnmodelledKind::ExternalRef(reference.to_owned()),
                pointer,
                node,
            );
        }

        let name = pointer_part.rsplit('/').next().unwrap_or(pointer_part);
        if !self.visited_pointers.insert(pointer_part.to_owned()) {
            return TypeRef::Cycle(name.to_owned());
        }

        let target = self
            .lookup_json_pointer(self.current_root(), pointer_part)
            .cloned();
        let resolved = match target {
            Some(target_node) => self.parse_schema(&target_node, pointer),
            None => self.record(
                UnmodelledKind::ExternalRef(reference.to_owned()),
                pointer,
                node,
            ),
        };

        self.visited_pointers.remove(pointer_part);
        resolved
    }

    fn resolve_external_schema(
        &mut self,
        file_path: &str,
        pointer_part: &str,
        original_pointer: &str,
    ) -> Option<TypeRef> {
        let key = file_path.to_owned();
        if !self.external_docs.contains_key(&key) {
            let bytes = self.resolver.resolve(file_path)?;
            let input = std::str::from_utf8(&bytes).ok()?;
            let docs = MarkedYamlOwned::load_from_str(input).ok()?;
            let doc_root = docs.into_iter().next()?;
            self.external_docs.insert(key.clone(), doc_root);
        }

        self.resolving_files.push(key);
        let doc = self.current_root().clone();
        let target = if pointer_part.is_empty() {
            Some(doc)
        } else {
            self.lookup_json_pointer(&doc, pointer_part).cloned()
        };

        let result = target.map(|target_node| self.parse_schema(&target_node, original_pointer));
        self.resolving_files.pop();
        result
    }

    fn lookup_json_pointer<'n>(
        &self,
        node: &'n MarkedYamlOwned,
        pointer: &str,
    ) -> Option<&'n MarkedYamlOwned> {
        let mut current = node;
        let segments = pointer.trim_start_matches('/').split('/');
        for segment in segments {
            if segment.is_empty() {
                continue;
            }
            let unescaped = segment.replace("~1", "/").replace("~0", "~");
            current = get_map_value(current, &unescaped)?;
        }
        Some(current)
    }
}

fn resolve_relative_file_path(base_file: &str, relative: &str) -> String {
    let parent = match base_file.rfind('/') {
        Some(idx) => &base_file[..idx],
        None => "",
    };
    if parent.is_empty() {
        relative.trim_start_matches("./").to_owned()
    } else {
        format!("{parent}/{}", relative.trim_start_matches("./"))
    }
}

fn parse_type_value(node: &MarkedYamlOwned) -> Option<ParsedTypeValue> {
    if let Some(name) = string_node(node) {
        return Some(ParsedTypeValue {
            names: vec![name.to_owned()],
            nullable: false,
        });
    }
    if let Some(list) = node.data.as_vec() {
        let mut names = Vec::new();
        let mut nullable = false;
        for item in list {
            let name = string_node(item)?;
            if name == "null" {
                nullable = true;
            } else {
                names.push(name.to_owned());
            }
        }
        return Some(ParsedTypeValue { names, nullable });
    }
    None
}

struct ParsedTypeValue {
    names: Vec<String>,
    nullable: bool,
}

fn parse_constraints(schema_node: &MarkedYamlOwned) -> Constraints {
    let minimum = get_map_value(schema_node, "minimum")
        .and_then(scalar_repr)
        .or_else(|| get_map_value(schema_node, "exclusiveMinimum").and_then(scalar_repr));
    let maximum = get_map_value(schema_node, "maximum")
        .and_then(scalar_repr)
        .or_else(|| get_map_value(schema_node, "exclusiveMaximum").and_then(scalar_repr));
    let min_length = get_map_value(schema_node, "minLength").and_then(u64_node);
    let max_length = get_map_value(schema_node, "maxLength").and_then(u64_node);
    let pattern = get_map_value(schema_node, "pattern")
        .and_then(string_node)
        .map(ToOwned::to_owned);

    Constraints {
        minimum,
        maximum,
        min_length,
        max_length,
        pattern,
    }
}

fn scalar_repr(node: &MarkedYamlOwned) -> Option<String> {
    match &node.data {
        YamlDataOwned::Value(ScalarOwned::String(value)) => Some(value.clone()),
        YamlDataOwned::Value(ScalarOwned::Integer(value)) => Some(value.to_string()),
        YamlDataOwned::Value(ScalarOwned::FloatingPoint(value)) => Some(value.to_string()),
        YamlDataOwned::Value(ScalarOwned::Boolean(value)) => Some(value.to_string()),
        YamlDataOwned::Value(ScalarOwned::Null) => Some("null".to_owned()),
        _ => None,
    }
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

fn u64_node(node: &MarkedYamlOwned) -> Option<u64> {
    node.data
        .as_integer()
        .and_then(|value| u64::try_from(value).ok())
}

fn get_map_value<'a>(node: &'a MarkedYamlOwned, key: &str) -> Option<&'a MarkedYamlOwned> {
    node.data.as_mapping()?.get(&yaml_string(key))
}

fn escape_json_pointer(fragment: &str) -> String {
    fragment.replace('~', "~0").replace('/', "~1")
}

fn span(file: &str, node: &MarkedYamlOwned, pointer: &str) -> Span {
    let marker = node.span.start;
    Span {
        file: file.to_owned(),
        line: marker.line(),
        column: marker.col() + 1,
        pointer: pointer.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_asyncapi_2x_publish_and_subscribe() {
        let spec = r#"
asyncapi: '2.6.0'
info:
  title: User Service
  version: 1.0.0
channels:
  user/signup:
    publish:
      message:
        payload:
          type: object
          required: [userId, email]
          properties:
            userId:
              type: string
            email:
              type: string
    subscribe:
      message:
        payload:
          type: object
          properties:
            status:
              type: string
"#;
        let contract = ingest("api/asyncapi.yaml", spec.as_bytes()).expect("ingest");
        assert_eq!(contract.endpoints.len(), 2);

        let pub_key = EndpointKey {
            method: "PUBLISH".to_owned(),
            path: "/user/signup".to_owned(),
        };
        let sub_key = EndpointKey {
            method: "SUBSCRIBE".to_owned(),
            path: "/user/signup".to_owned(),
        };

        assert!(contract.endpoints.contains_key(&pub_key));
        assert!(contract.endpoints.contains_key(&sub_key));

        let pub_ep = &contract.endpoints[&pub_key];
        assert!(pub_ep.responses.contains_key("200"));
        let TypeRef::Object { fields, .. } =
            &pub_ep.responses["200"].media_types["application/json"]
        else {
            panic!("expected object");
        };
        assert!(fields.contains_key("userId"));
        assert!(fields.contains_key("email"));
        assert!(fields["userId"].required);
    }

    #[test]
    fn parses_asyncapi_3x_operations() {
        let spec = r#"
asyncapi: '3.0.0'
info:
  title: Order Service
  version: 1.0.0
channels:
  ordersChannel:
    address: orders/v1
operations:
  sendOrders:
    action: send
    channel:
      $ref: '#/channels/ordersChannel'
    message:
      payload:
        type: object
        required: [orderId, total]
        properties:
          orderId:
            type: string
          total:
            type: number
"#;
        let contract = ingest("api/asyncapi.yaml", spec.as_bytes()).expect("ingest");
        assert_eq!(contract.endpoints.len(), 1);

        let key = EndpointKey {
            method: "PUBLISH".to_owned(),
            path: "/orders/v1".to_owned(),
        };
        assert!(contract.endpoints.contains_key(&key));
    }
}
