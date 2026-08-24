//! OpenAPI 3.0 / 3.1 → `Contract`.
//!
//! Ingest is bytes-only: no filesystem, no network, ever. A `$ref` that would
//! require either is refused and named, never fetched and never silently
//! dropped. See `design/02-contract-gates.md` §6.1.

use std::collections::{BTreeMap, BTreeSet};

use saphyr::{LoadableYamlNode, MarkedYamlOwned, ScalarOwned, YamlDataOwned};
use thiserror::Error;

use super::{
    Constraints, Contract, Discriminator, DocumentResolver, Endpoint, EndpointKey, Field,
    Parameter, Payload, SecurityRequirement, SecurityScheme, SingleDocumentResolver, Span, TypeRef,
    Unmodelled, UnmodelledKind,
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
    #[error(
        "OpenAPI document `{contract_source}` declares no `openapi` version; \
         Swagger 2.0 (`swagger: \"2.0\"`) is not supported"
    )]
    MissingVersion { contract_source: String },
    #[error(
        "OpenAPI document `{contract_source}` declares unsupported version `{version}`; \
         brake supports 3.0 and 3.1"
    )]
    UnsupportedVersion {
        contract_source: String,
        version: String,
    },
    #[error("OpenAPI document `{contract_source}` has no `paths` object")]
    MissingPaths { contract_source: String },
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

pub fn ingest(source: &str, bytes: &[u8]) -> Result<Contract, OpenApiError> {
    ingest_with_resolver(source, bytes, &SingleDocumentResolver)
}

pub fn ingest_with_resolver(
    source: &str,
    bytes: &[u8],
    resolver: &dyn DocumentResolver,
) -> Result<Contract, OpenApiError> {
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
    if root.data.as_mapping().is_none() {
        return Err(OpenApiError::RootNotMapping {
            contract_source: source.to_owned(),
        });
    }
    check_version(source, root)?;

    let paths = get_map_value(root, "paths").ok_or_else(|| OpenApiError::MissingPaths {
        contract_source: source.to_owned(),
    })?;
    let path_map = paths
        .data
        .as_mapping()
        .ok_or_else(|| OpenApiError::MissingPaths {
            contract_source: source.to_owned(),
        })?;

    let mut ctx = Ctx {
        source,
        root,
        resolver,
        external_docs: BTreeMap::new(),
        current_doc_file: None,
        ref_stack: Vec::new(),
        unmodelled: Vec::new(),
        fatal: None,
    };

    let default_security = parse_security(root);
    let mut contract = Contract::empty();
    contract.security_schemes = ctx.parse_security_schemes();

    for (path_key, path_item) in path_map {
        let Some(path) = string_node(path_key) else {
            continue;
        };
        let path_item = ctx.follow_ref(path_item);
        let Some(path_item_map) = path_item.data.as_mapping() else {
            continue;
        };
        let escaped_path = escape_json_pointer(path);

        // Parameters declared on the path item apply to every operation under
        // it. Dropping them made every shared `{id}` invisible to the gate.
        let shared_parameters =
            ctx.parse_parameters(path_item, &format!("/paths/{escaped_path}"), &[]);

        for (method_key, method_name) in HTTP_METHODS {
            let Some(operation) = path_item_map.get(&yaml_string(method_key)) else {
                continue;
            };
            if operation.data.as_mapping().is_none() {
                continue;
            }

            let pointer = format!("/paths/{escaped_path}/{method_key}");
            let operation_span = span(source, operation, pointer.clone());
            let operation_id = get_map_value(operation, "operationId")
                .and_then(string_node)
                .map(ToOwned::to_owned);
            let deprecated = get_map_value(operation, "deprecated")
                .and_then(bool_node)
                .unwrap_or(false);
            let sunset = get_map_value(operation, "x-sunset")
                .and_then(scalar_repr)
                .or_else(|| get_map_value(path_item, "x-sunset").and_then(scalar_repr));

            let parameters = ctx.parse_parameters(operation, &pointer, &shared_parameters);
            let request = ctx.parse_request_payload(operation, &pointer);
            let responses = ctx.parse_responses(operation, &pointer);
            let security = parse_security(operation)
                .unwrap_or_else(|| default_security.clone().unwrap_or_default());

            contract.endpoints.insert(
                EndpointKey {
                    method: (*method_name).to_owned(),
                    path: path.to_owned(),
                },
                Endpoint {
                    operation_id,
                    deprecated,
                    sunset,
                    parameters,
                    request,
                    responses,
                    security,
                    span: operation_span,
                },
            );
        }
    }

    if let Some(error) = ctx.fatal {
        return Err(error);
    }
    contract.unmodelled = ctx.unmodelled;
    contract
        .unmodelled
        .sort_by(|a, b| a.pointer.cmp(&b.pointer).then_with(|| a.kind.cmp(&b.kind)));
    contract.unmodelled.dedup();
    Ok(contract)
}

fn check_version(source: &str, root: &MarkedYamlOwned) -> Result<(), OpenApiError> {
    let Some(version) = get_map_value(root, "openapi").and_then(scalar_repr) else {
        return Err(OpenApiError::MissingVersion {
            contract_source: source.to_owned(),
        });
    };
    if version.starts_with("3.0") || version.starts_with("3.1") {
        return Ok(());
    }
    Err(OpenApiError::UnsupportedVersion {
        contract_source: source.to_owned(),
        version,
    })
}

struct Ctx<'a> {
    source: &'a str,
    root: &'a MarkedYamlOwned,
    resolver: &'a dyn DocumentResolver,
    external_docs: BTreeMap<String, MarkedYamlOwned>,
    current_doc_file: Option<String>,
    ref_stack: Vec<String>,
    unmodelled: Vec<Unmodelled>,
    fatal: Option<OpenApiError>,
}

impl<'a> Ctx<'a> {
    fn current_root(&self) -> &MarkedYamlOwned {
        if let Some(file) = &self.current_doc_file {
            self.external_docs.get(file).unwrap_or(self.root)
        } else {
            self.root
        }
    }

    fn current_source(&self) -> &str {
        if let Some(file) = &self.current_doc_file {
            file.as_str()
        } else {
            self.source
        }
    }

    fn record(&mut self, kind: UnmodelledKind, pointer: &str, node: &MarkedYamlOwned) -> TypeRef {
        self.unmodelled.push(Unmodelled {
            kind: kind.clone(),
            pointer: pointer.to_owned(),
            span: span(self.current_source(), node, pointer.to_owned()),
        });
        TypeRef::Unknown(kind)
    }

    /// Resolve a `$ref` on a non-schema node (path item, parameter, response).
    fn follow_ref(&mut self, node: &'a MarkedYamlOwned) -> &'a MarkedYamlOwned {
        let Some(reference) = get_map_value(node, "$ref").and_then(string_node) else {
            return node;
        };
        match self.classify_ref(reference) {
            RefKind::Local => find_pointer(self.root, reference).unwrap_or(node),
            RefKind::Fatal(error) => {
                self.fatal.get_or_insert(error);
                node
            }
            RefKind::External(_) => node,
        }
    }

    fn classify_ref(&self, reference: &str) -> RefKind {
        let lowered = reference.trim().to_ascii_lowercase();
        if lowered.starts_with("http://")
            || lowered.starts_with("https://")
            || lowered.starts_with("//")
        {
            return RefKind::Fatal(OpenApiError::RemoteRef {
                contract_source: self.source.to_owned(),
                reference: reference.to_owned(),
            });
        }
        if reference.starts_with("#/") || reference == "#" {
            return RefKind::Local;
        }

        // A file-relative ref. Ingest does not read it, but a ref that climbs
        // out of the source's directory is refused outright rather than
        // reported: guarantee G2 says that is an error, not a read.
        let file_part = reference.split('#').next().unwrap_or(reference);
        if escapes_source_tree(file_part) {
            return RefKind::Fatal(OpenApiError::EscapingRef {
                contract_source: self.source.to_owned(),
                reference: reference.to_owned(),
            });
        }
        RefKind::External(reference.to_owned())
    }

    fn parse_security_schemes(&mut self) -> BTreeMap<String, SecurityScheme> {
        let mut schemes = BTreeMap::new();
        let Some(components) = get_map_value(self.root, "components") else {
            return schemes;
        };
        let Some(entries) =
            get_map_value(components, "securitySchemes").and_then(|node| node.data.as_mapping())
        else {
            return schemes;
        };

        for (name_node, scheme_node) in entries {
            let Some(name) = string_node(name_node) else {
                continue;
            };
            let pointer = format!("/components/securitySchemes/{}", escape_json_pointer(name));
            let Some(ty) = get_map_value(scheme_node, "type").and_then(string_node) else {
                self.record(UnmodelledKind::InvalidShape, &pointer, scheme_node);
                continue;
            };
            let flows = get_map_value(scheme_node, "flows")
                .and_then(|node| node.data.as_mapping())
                .map(|mapping| {
                    mapping
                        .keys()
                        .filter_map(string_node)
                        .map(ToOwned::to_owned)
                        .collect::<BTreeSet<_>>()
                })
                .unwrap_or_default();
            schemes.insert(
                name.to_owned(),
                SecurityScheme {
                    ty: ty.to_owned(),
                    scheme: get_map_value(scheme_node, "scheme")
                        .and_then(string_node)
                        .map(ToOwned::to_owned),
                    flows,
                    location: get_map_value(scheme_node, "in")
                        .and_then(string_node)
                        .map(ToOwned::to_owned),
                    span: span(self.source, scheme_node, pointer),
                },
            );
        }
        schemes
    }

    fn parse_request_payload(
        &mut self,
        operation: &'a MarkedYamlOwned,
        pointer: &str,
    ) -> Option<Payload> {
        let request_body = get_map_value(operation, "requestBody")?;
        let request_body = self.follow_ref(request_body);
        let pointer = format!("{pointer}/requestBody");
        let media_types = self.parse_media_types(request_body, &pointer);
        Some(Payload {
            media_types,
            span: span(self.source, request_body, pointer),
        })
    }

    fn parse_media_types(
        &mut self,
        container: &'a MarkedYamlOwned,
        pointer: &str,
    ) -> BTreeMap<String, TypeRef> {
        let mut media_types = BTreeMap::new();
        let Some(content) =
            get_map_value(container, "content").and_then(|node| node.data.as_mapping())
        else {
            // A body with no content at all is a legitimate 204; a body whose
            // content cannot be read is not. Both are reported as deferred so
            // neither is mistaken for a verified-compatible payload.
            return media_types;
        };

        for (media_key, media_node) in content {
            let Some(media_type) = string_node(media_key) else {
                continue;
            };
            let media_pointer = format!("{pointer}/content/{}", escape_json_pointer(media_type));
            let ty = match get_map_value(media_node, "schema") {
                Some(schema) => self.parse_schema(schema, &media_pointer),
                None => self.record(UnmodelledKind::SchemaDeferred, &media_pointer, media_node),
            };
            media_types.insert(media_type.to_owned(), ty);
        }
        media_types
    }

    fn parse_parameters(
        &mut self,
        container: &'a MarkedYamlOwned,
        pointer: &str,
        inherited: &[Parameter],
    ) -> Vec<Parameter> {
        let mut parameters = inherited.to_vec();
        let Some(parameter_nodes) =
            get_map_value(container, "parameters").and_then(|node| node.data.as_vec())
        else {
            return parameters;
        };

        for (index, raw_node) in parameter_nodes.iter().enumerate() {
            let parameter_pointer = format!("{pointer}/parameters/{index}");
            let parameter_node = self.follow_ref(raw_node);
            if parameter_node.data.as_mapping().is_none() {
                self.record(UnmodelledKind::InvalidShape, &parameter_pointer, raw_node);
                continue;
            }

            let (Some(name), Some(location)) = (
                get_map_value(parameter_node, "name").and_then(string_node),
                get_map_value(parameter_node, "in").and_then(string_node),
            ) else {
                // A parameter we cannot identify used to be skipped silently,
                // which is a hole in the endpoint's request surface.
                self.record(
                    UnmodelledKind::InvalidShape,
                    &parameter_pointer,
                    parameter_node,
                );
                continue;
            };

            let required = get_map_value(parameter_node, "required")
                .and_then(bool_node)
                // A path parameter is required by definition; some documents
                // leave the flag off.
                .unwrap_or(location == "path");
            let ty = match get_map_value(parameter_node, "schema") {
                Some(schema) => self.parse_schema(schema, &parameter_pointer),
                None => self.record(
                    UnmodelledKind::SchemaDeferred,
                    &parameter_pointer,
                    parameter_node,
                ),
            };

            let parameter = Parameter {
                name: name.to_owned(),
                location: location.to_owned(),
                required,
                deprecated: get_map_value(parameter_node, "deprecated")
                    .and_then(bool_node)
                    .unwrap_or(false),
                ty,
                span: span(self.source, parameter_node, parameter_pointer),
            };
            // An operation-level parameter overrides an inherited path-level
            // one with the same identity, per the OpenAPI specification.
            if let Some(existing) = parameters
                .iter_mut()
                .find(|held| held.name == parameter.name && held.location == parameter.location)
            {
                *existing = parameter;
            } else {
                parameters.push(parameter);
            }
        }

        parameters.sort_by(|a, b| {
            a.location
                .cmp(&b.location)
                .then_with(|| a.name.cmp(&b.name))
        });
        parameters
    }

    fn parse_responses(
        &mut self,
        operation: &'a MarkedYamlOwned,
        pointer: &str,
    ) -> BTreeMap<String, Payload> {
        let mut responses = BTreeMap::new();
        let Some(response_map) =
            get_map_value(operation, "responses").and_then(|node| node.data.as_mapping())
        else {
            return responses;
        };

        for (status_key, raw_node) in response_map {
            let Some(status) = scalar_repr(status_key) else {
                continue;
            };
            let status_pointer = format!("{pointer}/responses/{}", escape_json_pointer(&status));
            let status_node = self.follow_ref(raw_node);
            let media_types = self.parse_media_types(status_node, &status_pointer);
            responses.insert(
                status,
                Payload {
                    media_types,
                    span: span(self.source, status_node, status_pointer),
                },
            );
        }

        responses
    }

    fn parse_schema(&mut self, schema_node: &MarkedYamlOwned, pointer: &str) -> TypeRef {
        if schema_node.data.as_mapping().is_none() {
            // 3.1 allows `true`/`false` as a schema. `true` accepts anything;
            // treat it as an open object rather than guessing a shape.
            if bool_node(schema_node) == Some(true) {
                return TypeRef::Object {
                    fields: BTreeMap::new(),
                    additional: true,
                    nullable: true,
                };
            }
            return self.record(UnmodelledKind::InvalidShape, pointer, schema_node);
        }

        if let Some(reference) = get_map_value(schema_node, "$ref").and_then(string_node) {
            return self.resolve_schema_reference(reference, pointer, schema_node);
        }

        // `not` cannot be modelled structurally and silently ignoring it would
        // claim a verification that did not happen.
        if get_map_value(schema_node, "not").is_some() {
            return self.record(
                UnmodelledKind::Unsupported("not".to_owned()),
                pointer,
                schema_node,
            );
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

        if let Some(enum_node) = get_map_value(schema_node, "enum") {
            if let Some(enum_values) = enum_node.data.as_vec() {
                let mut values = BTreeSet::new();
                for (index, value) in enum_values.iter().enumerate() {
                    match scalar_repr(value) {
                        Some(repr) => {
                            values.insert(repr);
                        }
                        // A dropped enum value silently widens the modelled
                        // type, which would hide a narrowing on the next run.
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
            } else {
                return self.record(
                    UnmodelledKind::Unsupported("enum".to_owned()),
                    pointer,
                    enum_node,
                );
            }
        }

        let nullable_30 = get_map_value(schema_node, "nullable")
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
            let nullable = nullable_30 || parsed.nullable;

            // A union of concrete types is a real construct; modelling it as
            // whichever member happened to be last is how a narrowing hides.
            if parsed.names.len() > 1 {
                return TypeRef::OneOf {
                    variants: parsed
                        .names
                        .iter()
                        .map(|name| self.type_for_name(name, schema_node, pointer, nullable))
                        .collect(),
                    discriminator: None,
                };
            }
            let Some(name) = parsed.names.first() else {
                // `type: [null]` alone.
                return TypeRef::Scalar {
                    ty: "null".to_owned(),
                    format: None,
                    nullable: true,
                    constraints: Constraints::default(),
                };
            };
            return self.type_for_name(&name.clone(), schema_node, pointer, nullable);
        }

        if get_map_value(schema_node, "properties").is_some()
            || get_map_value(schema_node, "additionalProperties").is_some()
        {
            return self.parse_object_type(schema_node, pointer, nullable_30);
        }
        if get_map_value(schema_node, "items").is_some()
            || get_map_value(schema_node, "prefixItems").is_some()
        {
            return self.parse_array_type(schema_node, pointer, nullable_30);
        }
        if schema_node
            .data
            .as_mapping()
            .is_some_and(|mapping| mapping.is_empty())
        {
            // An empty schema accepts anything, which is a modelled fact.
            return TypeRef::Object {
                fields: BTreeMap::new(),
                additional: true,
                nullable: true,
            };
        }

        self.record(UnmodelledKind::SchemaDeferred, pointer, schema_node)
    }

    fn type_for_name(
        &mut self,
        name: &str,
        schema_node: &MarkedYamlOwned,
        pointer: &str,
        nullable: bool,
    ) -> TypeRef {
        match name {
            "array" => self.parse_array_type(schema_node, pointer, nullable),
            "object" => self.parse_object_type(schema_node, pointer, nullable),
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
                // The *key* node, not the schema: `customer_id:` is the line a
                // reader is looking for, not the `type: string` beneath it.
                let field_span = span(self.current_source(), name_node, field_pointer.clone());
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

        // `additionalProperties` is a boolean *or* a schema. Reading only the
        // boolean made `additionalProperties: {type: string}` look wide open.
        let additional = match get_map_value(schema_node, "additionalProperties") {
            None => true,
            Some(node) => match bool_node(node) {
                Some(value) => value,
                None => {
                    self.parse_schema(node, &format!("{pointer}/additionalProperties"));
                    true
                }
            },
        };

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
        match self.classify_ref(reference) {
            RefKind::Fatal(error) => {
                self.fatal.get_or_insert(error);
                TypeRef::Unknown(UnmodelledKind::InvalidShape)
            }
            RefKind::External(ref_str) => {
                let (file_part, pointer_part) = ref_str.split_once('#').unwrap_or((&ref_str, ""));
                // Relative to the *document*, never to `source`: for a baseline read
                // out of git, `source` is a descriptor like `rev:HEAD`, and
                // deriving a directory from that resolved every sibling `$ref`
                // against the wrong place. The top-level document sits at the
                // resolver's root, so it contributes no directory of its own.
                let resolved_path = resolve_relative_file_path(
                    self.current_doc_file.as_deref().unwrap_or(""),
                    file_part,
                );

                if let Some(parsed) =
                    self.resolve_external_schema(&resolved_path, pointer_part, pointer)
                {
                    parsed
                } else {
                    self.record(UnmodelledKind::ExternalRef(ref_str), pointer, node)
                }
            }
            RefKind::Local => {
                let current_root = self.current_root();
                let name = reference
                    .rsplit('/')
                    .next()
                    .unwrap_or(reference)
                    .replace("~1", "/")
                    .replace("~0", "~");

                let ref_key = if let Some(file) = &self.current_doc_file {
                    format!("{file}{reference}")
                } else {
                    reference.to_owned()
                };

                if self.ref_stack.iter().any(|held| held == &ref_key) {
                    return TypeRef::Cycle(name);
                }
                let Some(target) = find_pointer(current_root, reference) else {
                    return self.record(
                        UnmodelledKind::UnresolvableRef(reference.to_owned()),
                        pointer,
                        node,
                    );
                };
                let target_clone = target.clone();
                self.ref_stack.push(ref_key);
                let parsed = self.parse_schema(&target_clone, pointer);
                self.ref_stack.pop();
                parsed
            }
        }
    }

    fn resolve_external_schema(
        &mut self,
        file_path: &str,
        pointer_part: &str,
        pointer: &str,
    ) -> Option<TypeRef> {
        let full_ref_id = format!("{file_path}#{pointer_part}");
        let name = pointer_part
            .rsplit('/')
            .next()
            .unwrap_or(pointer_part)
            .replace("~1", "/")
            .replace("~0", "~");
        let cycle_name = if name.is_empty() {
            file_path.rsplit('/').next().unwrap_or(file_path).to_owned()
        } else {
            name
        };

        if self.ref_stack.iter().any(|held| held == &full_ref_id) {
            return Some(TypeRef::Cycle(cycle_name));
        }

        if !self.external_docs.contains_key(file_path) {
            let bytes = self.resolver.resolve(file_path)?;
            let input = std::str::from_utf8(&bytes).ok()?;
            let docs = MarkedYamlOwned::load_from_str(input).ok()?;
            let root = docs.into_iter().next()?;
            self.external_docs.insert(file_path.to_owned(), root);
        }

        let doc = self.external_docs.get(file_path)?;
        let target = if pointer_part.is_empty() || pointer_part == "/" {
            doc
        } else {
            find_pointer(doc, pointer_part)?
        };

        let target_clone = target.clone();
        let prev_doc_file = self.current_doc_file.take();
        self.current_doc_file = Some(file_path.to_owned());
        self.ref_stack.push(full_ref_id);

        let parsed = self.parse_schema(&target_clone, pointer);

        self.ref_stack.pop();
        self.current_doc_file = prev_doc_file;
        Some(parsed)
    }

    fn flatten_all_of(&mut self, parts: &[MarkedYamlOwned], pointer: &str) -> TypeRef {
        let mut fields = BTreeMap::new();
        let mut additional = true;
        let mut nullable = false;
        let mut merged_any = false;

        for (index, part) in parts.iter().enumerate() {
            let parsed = self.parse_schema(part, &format!("{pointer}/allOf/{index}"));
            match parsed {
                TypeRef::Object {
                    fields: part_fields,
                    additional: part_additional,
                    nullable: part_nullable,
                } => {
                    merged_any = true;
                    fields.extend(part_fields);
                    additional &= part_additional;
                    nullable |= part_nullable;
                }
                // A branch that is not an object cannot be merged; saying so
                // is better than dropping it and reporting the rest as whole.
                TypeRef::Unknown(kind) => {
                    return TypeRef::Unknown(kind);
                }
                other => {
                    if !merged_any && parts.len() == 1 {
                        return other;
                    }
                    return self.record(
                        UnmodelledKind::Unsupported("allOf of non-object".to_owned()),
                        &format!("{pointer}/allOf/{index}"),
                        &parts[index],
                    );
                }
            }
        }

        if merged_any {
            TypeRef::Object {
                fields,
                additional,
                nullable,
            }
        } else {
            TypeRef::Unknown(UnmodelledKind::SchemaDeferred)
        }
    }
}

enum RefKind {
    Local,
    External(String),
    Fatal(OpenApiError),
}

/// Does this relative path climb above the directory holding the source?
///
/// Decided lexically, because ingest never touches the filesystem — the point
/// is to refuse the traversal, not to discover where it would have landed.
fn escapes_source_tree(file_part: &str) -> bool {
    if file_part.starts_with('/') {
        return true;
    }
    let mut depth = 0i32;
    for segment in file_part.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            _ => depth += 1,
        }
    }
    false
}

fn resolve_relative_file_path(source_file: &str, ref_file: &str) -> String {
    let source_dir = match source_file.rfind('/') {
        Some(pos) => &source_file[..pos],
        None => "",
    };
    let mut segments = Vec::new();
    if !source_dir.is_empty() {
        for seg in source_dir.split('/') {
            if !seg.is_empty() && seg != "." {
                segments.push(seg);
            }
        }
    }
    for seg in ref_file.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            s => segments.push(s),
        }
    }
    segments.join("/")
}

struct ParsedType {
    names: Vec<String>,
    nullable: bool,
}

fn parse_type_value(type_node: &MarkedYamlOwned) -> Option<ParsedType> {
    if let Some(single) = string_node(type_node) {
        return Some(ParsedType {
            names: vec![single.to_owned()],
            nullable: false,
        });
    }

    let values = type_node.data.as_vec()?;
    let mut nullable = false;
    let mut names = Vec::new();
    for value in values {
        let ty = string_node(value)?;
        if ty == "null" {
            nullable = true;
        } else {
            names.push(ty.to_owned());
        }
    }
    names.sort();
    names.dedup();
    Some(ParsedType { names, nullable })
}

fn parse_constraints(schema_node: &MarkedYamlOwned) -> Constraints {
    Constraints {
        minimum: get_map_value(schema_node, "minimum").and_then(scalar_repr),
        maximum: get_map_value(schema_node, "maximum").and_then(scalar_repr),
        min_length: get_map_value(schema_node, "minLength")
            .and_then(|node| node.data.as_integer())
            .and_then(|value| u64::try_from(value).ok()),
        max_length: get_map_value(schema_node, "maxLength")
            .and_then(|node| node.data.as_integer())
            .and_then(|value| u64::try_from(value).ok()),
        pattern: get_map_value(schema_node, "pattern")
            .and_then(string_node)
            .map(ToOwned::to_owned),
    }
}

fn parse_security(container: &MarkedYamlOwned) -> Option<Vec<SecurityRequirement>> {
    let entries = get_map_value(container, "security")?.data.as_vec()?;
    let mut requirements = Vec::new();
    for entry in entries {
        let Some(mapping) = entry.data.as_mapping() else {
            continue;
        };
        for (name_node, scopes_node) in mapping {
            let Some(name) = string_node(name_node) else {
                continue;
            };
            let scopes = scopes_node
                .data
                .as_vec()
                .map(|values| {
                    values
                        .iter()
                        .filter_map(string_node)
                        .map(ToOwned::to_owned)
                        .collect::<BTreeSet<_>>()
                })
                .unwrap_or_default();
            requirements.push(SecurityRequirement {
                name: name.to_owned(),
                scopes,
            });
        }
    }
    requirements.sort();
    requirements.dedup();
    Some(requirements)
}

fn find_pointer<'a>(root: &'a MarkedYamlOwned, pointer: &str) -> Option<&'a MarkedYamlOwned> {
    let trimmed = pointer.trim_start_matches('#');
    if trimmed.is_empty() || trimmed == "/" {
        return Some(root);
    }
    let mut current = root;
    for token in trimmed.trim_start_matches('/').split('/') {
        let decoded = token.replace("~1", "/").replace("~0", "~");
        current = get_map_value(current, &decoded)?;
    }
    Some(current)
}

fn get_map_value<'a>(node: &'a MarkedYamlOwned, key: &str) -> Option<&'a MarkedYamlOwned> {
    node.data.as_mapping()?.get(&yaml_string(key))
}

/// A scalar's canonical text, for enum members, status codes and bounds.
///
/// Every YAML scalar has one, so nothing is dropped for being a float or a
/// null the way `as_str`/`as_integer`/`as_bool` alone would drop it.
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

fn span(source: &str, node: &MarkedYamlOwned, pointer: String) -> Span {
    Span {
        file: source.to_owned(),
        line: node.span.start.line(),
        column: node.span.start.col() + 1,
        pointer,
    }
}

fn escape_json_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::InMemoryResolver;

    fn response_type<'a>(
        contract: &'a Contract,
        method: &str,
        path: &str,
        status: &str,
    ) -> &'a TypeRef {
        contract
            .endpoints
            .get(&EndpointKey {
                method: method.to_owned(),
                path: path.to_owned(),
            })
            .expect("endpoint exists")
            .responses
            .get(status)
            .expect("status exists")
            .media_types
            .values()
            .next()
            .expect("a media type")
    }

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
      x-sunset: "2026-12-01"
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
        assert_eq!(endpoint.sunset.as_deref(), Some("2026-12-01"));
        assert_eq!(endpoint.span.file, source);
        assert_eq!(endpoint.span.line, 6);
        assert_eq!(endpoint.span.pointer, "/paths/~1payments~1{id}/get");
        // A response with no content declares no schema, and says so.
        assert!(
            endpoint
                .responses
                .get("200")
                .expect("response status exists")
                .media_types
                .is_empty()
        );
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

        let TypeRef::Object {
            fields, additional, ..
        } = endpoint
            .request
            .as_ref()
            .expect("request body exists")
            .media_types
            .get("application/json")
            .expect("json media type")
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
        let TypeRef::Object { fields, .. } = response_type(&contract, "GET", "/payments", "200")
        else {
            panic!("allOf ref should flatten to object");
        };
        assert!(fields.get("id").expect("id").required);
        assert!(fields.contains_key("amount"));

        let TypeRef::Object { fields, .. } = response_type(&contract, "GET", "/tree", "200") else {
            panic!("node schema should parse as object");
        };
        assert!(matches!(
            fields.get("child").expect("child field").ty,
            TypeRef::Cycle(ref name) if name == "Node"
        ));
        assert!(contract.unmodelled.is_empty());
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
        assert!(matches!(
            response_type(&contract, "GET", "/payments", "200"),
            TypeRef::Scalar {
                ty,
                nullable: true,
                ..
            } if ty == "string"
        ));
    }

    #[test]
    fn keeps_every_media_type_not_just_the_first() {
        let spec = r#"
openapi: 3.1.0
paths:
  /payments:
    get:
      responses:
        "200":
          description: ok
          content:
            application/xml:
              schema:
                type: string
            application/json:
              schema:
                type: object
"#;
        let contract = ingest("api/openapi.yaml", spec.as_bytes()).expect("ingest");
        let payload = &contract
            .endpoints
            .get(&EndpointKey {
                method: "GET".to_owned(),
                path: "/payments".to_owned(),
            })
            .expect("endpoint")
            .responses["200"];
        assert_eq!(
            payload.media_types.keys().collect::<Vec<_>>(),
            vec!["application/json", "application/xml"]
        );
    }

    #[test]
    fn remote_ref_is_refused_never_fetched() {
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
                $ref: 'https://example.com/schema.yaml#/Payment'
"#;
        let error = ingest("api/openapi.yaml", spec.as_bytes())
            .expect_err("a remote ref must be an error, not a fetch");
        assert!(matches!(error, OpenApiError::RemoteRef { .. }));
    }

    #[test]
    fn ref_escaping_the_source_tree_is_an_error() {
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
                $ref: '../../../../etc/passwd#/x'
"#;
        let error = ingest("api/openapi.yaml", spec.as_bytes())
            .expect_err("an escaping ref must be an error, not a read");
        assert!(matches!(error, OpenApiError::EscapingRef { .. }));
    }

    #[test]
    fn sibling_file_ref_is_reported_as_unmodelled_not_ignored() {
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
                $ref: 'common.yaml#/components/schemas/Payment'
"#;
        let contract = ingest("api/openapi.yaml", spec.as_bytes()).expect("ingest");
        assert!(matches!(
            response_type(&contract, "GET", "/payments", "200"),
            TypeRef::Unknown(UnmodelledKind::ExternalRef(_))
        ));
        assert_eq!(contract.unmodelled.len(), 1);
    }

    #[test]
    fn unresolvable_local_ref_is_named() {
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
                $ref: '#/components/schemas/Missing'
"#;
        let contract = ingest("api/openapi.yaml", spec.as_bytes()).expect("ingest");
        assert!(matches!(
            response_type(&contract, "GET", "/payments", "200"),
            TypeRef::Unknown(UnmodelledKind::UnresolvableRef(reference))
                if reference == "#/components/schemas/Missing"
        ));
    }

    #[test]
    fn swagger_two_is_refused_rather_than_misread() {
        let spec = r#"
swagger: "2.0"
paths:
  /payments:
    get:
      responses:
        "200":
          description: ok
"#;
        let error = ingest("api/swagger.yaml", spec.as_bytes())
            .expect_err("swagger 2.0 must not be read as OpenAPI 3");
        assert!(matches!(error, OpenApiError::MissingVersion { .. }));
    }

    #[test]
    fn path_level_parameters_apply_to_every_operation() {
        let spec = r#"
openapi: 3.1.0
paths:
  /payments/{id}:
    parameters:
      - name: id
        in: path
        required: true
        schema:
          type: string
    get:
      responses:
        "200":
          description: ok
    delete:
      parameters:
        - name: force
          in: query
          schema:
            type: boolean
      responses:
        "204":
          description: gone
"#;
        let contract = ingest("api/openapi.yaml", spec.as_bytes()).expect("ingest");
        let get = &contract.endpoints[&EndpointKey {
            method: "GET".to_owned(),
            path: "/payments/{id}".to_owned(),
        }];
        assert_eq!(get.parameters.len(), 1);
        assert_eq!(get.parameters[0].name, "id");

        let delete = &contract.endpoints[&EndpointKey {
            method: "DELETE".to_owned(),
            path: "/payments/{id}".to_owned(),
        }];
        assert_eq!(delete.parameters.len(), 2);
    }

    #[test]
    fn resolves_referenced_parameters_and_responses() {
        let spec = r#"
openapi: 3.1.0
components:
  parameters:
    PaymentId:
      name: id
      in: path
      required: true
      schema:
        type: string
  responses:
    NotFound:
      description: missing
      content:
        application/json:
          schema:
            type: object
            properties:
              code:
                type: string
paths:
  /payments/{id}:
    get:
      parameters:
        - $ref: '#/components/parameters/PaymentId'
      responses:
        "404":
          $ref: '#/components/responses/NotFound'
"#;
        let contract = ingest("api/openapi.yaml", spec.as_bytes()).expect("ingest");
        let endpoint = &contract.endpoints[&EndpointKey {
            method: "GET".to_owned(),
            path: "/payments/{id}".to_owned(),
        }];
        assert_eq!(endpoint.parameters.len(), 1);
        assert_eq!(endpoint.parameters[0].name, "id");
        assert!(endpoint.parameters[0].required);
        assert!(matches!(
            response_type(&contract, "GET", "/payments/{id}", "404"),
            TypeRef::Object { .. }
        ));
        assert!(contract.unmodelled.is_empty());
    }

    #[test]
    fn parses_security_requirements_and_schemes() {
        let spec = r#"
openapi: 3.1.0
security:
  - apiKey: []
components:
  securitySchemes:
    apiKey:
      type: apiKey
      in: header
    oauth:
      type: oauth2
      flows:
        authorizationCode: {}
paths:
  /payments:
    get:
      responses:
        "200":
          description: ok
    post:
      security:
        - oauth: [write]
      responses:
        "201":
          description: ok
"#;
        let contract = ingest("api/openapi.yaml", spec.as_bytes()).expect("ingest");
        let get = &contract.endpoints[&EndpointKey {
            method: "GET".to_owned(),
            path: "/payments".to_owned(),
        }];
        assert_eq!(get.security.len(), 1);
        assert_eq!(get.security[0].name, "apiKey");

        let post = &contract.endpoints[&EndpointKey {
            method: "POST".to_owned(),
            path: "/payments".to_owned(),
        }];
        assert_eq!(post.security[0].name, "oauth");
        assert!(post.security[0].scopes.contains("write"));

        assert_eq!(contract.security_schemes["apiKey"].ty, "apiKey");
        assert!(
            contract.security_schemes["oauth"]
                .flows
                .contains("authorizationCode")
        );
    }

    #[test]
    fn captures_scalar_constraints() {
        let spec = r#"
openapi: 3.1.0
paths:
  /payments:
    post:
      parameters:
        - name: note
          in: query
          schema:
            type: string
            maxLength: 100
            pattern: '^[a-z]+$'
      responses:
        "200":
          description: ok
"#;
        let contract = ingest("api/openapi.yaml", spec.as_bytes()).expect("ingest");
        let endpoint = &contract.endpoints[&EndpointKey {
            method: "POST".to_owned(),
            path: "/payments".to_owned(),
        }];
        let TypeRef::Scalar { constraints, .. } = &endpoint.parameters[0].ty else {
            panic!("expected a scalar");
        };
        assert_eq!(constraints.max_length, Some(100));
        assert_eq!(constraints.pattern.as_deref(), Some("^[a-z]+$"));
    }

    #[test]
    fn unmodelled_constructs_are_recorded_not_dropped() {
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
                not:
                  type: string
"#;
        let contract = ingest("api/openapi.yaml", spec.as_bytes()).expect("ingest");
        assert_eq!(contract.unmodelled.len(), 1);
        assert!(matches!(
            contract.unmodelled[0].kind,
            UnmodelledKind::Unsupported(ref what) if what == "not"
        ));
    }

    #[test]
    fn media_type_key_order_does_not_change_the_model() {
        let one = r#"
openapi: 3.1.0
paths:
  /p:
    get:
      responses:
        "200":
          description: ok
          content:
            application/json: { schema: { type: object } }
            application/xml: { schema: { type: string } }
"#;
        let two = r#"
openapi: 3.1.0
paths:
  /p:
    get:
      responses:
        "200":
          description: ok
          content:
            application/xml: { schema: { type: string } }
            application/json: { schema: { type: object } }
"#;
        let first = ingest("api/openapi.yaml", one.as_bytes()).expect("ingest");
        let second = ingest("api/openapi.yaml", two.as_bytes()).expect("ingest");
        let media = |c: &Contract| {
            c.endpoints[&EndpointKey {
                method: "GET".to_owned(),
                path: "/p".to_owned(),
            }]
                .responses["200"]
                .media_types
                .clone()
        };
        assert_eq!(media(&first), media(&second));
    }

    #[test]
    fn json_documents_ingest_as_readily_as_yaml() {
        let spec = r#"{"openapi":"3.1.0","paths":{"/p":{"get":{"operationId":"getP",
          "responses":{"200":{"description":"ok"}}}}}}"#;
        let contract = ingest("api/openapi.json", spec.as_bytes()).expect("ingest");
        assert_eq!(contract.endpoints.len(), 1);
    }

    /// `additionalProperties` is a boolean *or* a schema, and reading only the
    /// boolean made `additionalProperties: {type: string}` look wide open.
    /// The guard was dropped when the multi-document tests landed in its place.
    #[test]
    fn additional_properties_schema_is_not_read_as_wide_open() {
        let spec = r#"
openapi: 3.1.0
paths:
  /p:
    get:
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                type: object
                additionalProperties:
                  type: string
"#;
        let contract = ingest("api/openapi.yaml", spec.as_bytes()).expect("ingest");
        let ty = response_type(&contract, "GET", "/p", "200");
        assert!(
            matches!(ty, TypeRef::Object { additional, .. } if *additional),
            "a schema-valued additionalProperties still admits extra members: {ty:?}"
        );
    }

    #[test]
    fn resolves_local_sibling_file_references_via_resolver() {
        let root_spec = r#"
openapi: 3.1.0
paths:
  /users:
    get:
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                $ref: "./common/models.yaml#/components/schemas/User"
"#;
        let models_spec = r#"
openapi: 3.1.0
components:
  schemas:
    User:
      type: object
      required: [id, name]
      properties:
        id:
          type: string
        name:
          type: string
"#;
        let resolver = InMemoryResolver::new()
            .with_document("common/models.yaml", models_spec.as_bytes().to_vec());

        let contract = ingest_with_resolver("api/openapi.yaml", root_spec.as_bytes(), &resolver)
            .expect("ingest with resolver");

        assert!(contract.unmodelled.is_empty());
        let ty = response_type(&contract, "GET", "/users", "200");
        let TypeRef::Object { fields, .. } = ty else {
            panic!("expected object type, got {ty:?}");
        };
        assert!(fields.contains_key("id"));
        assert!(fields.contains_key("name"));
        assert!(fields["id"].required);
    }

    #[test]
    fn resolves_nested_external_file_references_and_cycles() {
        let root_spec = r#"
openapi: 3.1.0
paths:
  /orders:
    get:
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                $ref: "./models/order.yaml#/Order"
"#;
        let order_spec = r#"
Order:
  type: object
  required: [id, customer]
  properties:
    id:
      type: string
    customer:
      $ref: "./customer.yaml#/Customer"
"#;
        let customer_spec = r#"
Customer:
  type: object
  required: [name]
  properties:
    name:
      type: string
"#;
        let resolver = InMemoryResolver::new()
            .with_document("models/order.yaml", order_spec.as_bytes().to_vec())
            .with_document("models/customer.yaml", customer_spec.as_bytes().to_vec());

        let contract = ingest_with_resolver("api/openapi.yaml", root_spec.as_bytes(), &resolver)
            .expect("ingest nested with resolver");

        assert!(contract.unmodelled.is_empty());
        let ty = response_type(&contract, "GET", "/orders", "200");
        let TypeRef::Object { fields, .. } = ty else {
            panic!("expected object type, got {ty:?}");
        };
        assert!(fields.contains_key("id"));
        assert!(fields.contains_key("customer"));

        let TypeRef::Object {
            fields: customer_fields,
            ..
        } = &fields["customer"].ty
        else {
            panic!("expected nested customer object");
        };
        assert!(customer_fields.contains_key("name"));
    }

    #[test]
    fn handles_circular_external_references_without_infinite_loop() {
        let root_spec = r#"
openapi: 3.1.0
paths:
  /node:
    get:
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                $ref: "./node.yaml#/Node"
"#;
        let node_spec = r#"
Node:
  type: object
  properties:
    next:
      $ref: "./node.yaml#/Node"
"#;
        let resolver =
            InMemoryResolver::new().with_document("node.yaml", node_spec.as_bytes().to_vec());

        let contract = ingest_with_resolver("api/openapi.yaml", root_spec.as_bytes(), &resolver)
            .expect("ingest circular with resolver");

        let ty = response_type(&contract, "GET", "/node", "200");
        let TypeRef::Object { fields, .. } = ty else {
            panic!("expected object");
        };
        assert!(matches!(fields["next"].ty, TypeRef::Cycle(_)));
    }

    #[test]
    fn single_document_resolver_records_external_ref_as_unmodelled() {
        let root_spec = r#"
openapi: 3.1.0
paths:
  /users:
    get:
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                $ref: "./common/models.yaml#/components/schemas/User"
"#;
        let contract = ingest("api/openapi.yaml", root_spec.as_bytes()).expect("ingest");
        assert_eq!(contract.unmodelled.len(), 1);
        assert!(matches!(
            &contract.unmodelled[0].kind,
            UnmodelledKind::ExternalRef(r) if r == "./common/models.yaml#/components/schemas/User"
        ));
    }
}
