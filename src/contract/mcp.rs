//! An MCP `tools/list` snapshot → `Contract`.
//!
//! Each tool is an endpoint, `CALL /tools/<name>`; its `inputSchema` is the
//! request body and its `outputSchema` the response. The server is never
//! started: the snapshot is a file the server's own build wrote. See
//! `design/08-mcp-tool-contracts.md`.

use std::collections::{BTreeMap, BTreeSet};

use saphyr::{LoadableYamlNode, MarkedYamlOwned, ScalarOwned, YamlDataOwned};
use thiserror::Error;

use super::openapi::{OpenApiError, parse_json_schema};
use super::{
    Contract, DocumentResolver, Endpoint, EndpointKey, Payload, Span, TypeRef, Unmodelled,
    UnmodelledKind,
};

/// The method every tool endpoint carries.
pub const METHOD_CALL: &str = "CALL";

/// The response key a tool's `outputSchema` is filed under.
///
/// A tool has one result shape, not a status table, so the key is a fixed
/// name rather than a code.
pub const RESULT: &str = "result";

#[derive(Debug, Error)]
pub enum McpError {
    #[error("contract source `{contract_source}` is not valid UTF-8: {error}")]
    InvalidUtf8 {
        contract_source: String,
        error: std::str::Utf8Error,
    },
    #[error("failed to parse MCP tool snapshot `{contract_source}`: {error}")]
    Parse {
        contract_source: String,
        error: saphyr::ScanError,
    },
    #[error(
        "`{contract_source}` is not an MCP `tools/list` result: expected a `tools` array, \
         at the root or under `result`"
    )]
    NotToolsList { contract_source: String },
    #[error(
        "MCP tool snapshot `{contract_source}` lists tool `{name}` more than once; \
         which definition is the tool is undefined"
    )]
    DuplicateTool {
        contract_source: String,
        name: String,
    },
    #[error(
        "MCP tool snapshot `{contract_source}` is one page of a paginated `tools/list` \
         (it carries `nextCursor`). A tool missing from it may be on the next page, so no \
         removal can be judged from it: concatenate the pages before committing"
    )]
    Paginated { contract_source: String },
    #[error(
        "MCP tool snapshot `{contract_source}` has a tool with no `name` at `{pointer}`; \
         a tool without a name cannot be compared with anything"
    )]
    UnnamedTool {
        contract_source: String,
        pointer: String,
    },
    #[error(transparent)]
    Schema(#[from] OpenApiError),
}

pub fn ingest(source: &str, bytes: &[u8]) -> Result<Contract, McpError> {
    let input = std::str::from_utf8(bytes).map_err(|error| McpError::InvalidUtf8 {
        contract_source: source.to_owned(),
        error,
    })?;
    let docs = MarkedYamlOwned::load_from_str(input).map_err(|error| McpError::Parse {
        contract_source: source.to_owned(),
        error,
    })?;
    let not_tools_list = || McpError::NotToolsList {
        contract_source: source.to_owned(),
    };
    let root = docs.first().ok_or_else(not_tools_list)?;

    // The bare result, or the JSON-RPC response that carried it. Both are what
    // people actually save.
    let (result, prefix) = match get_map_value(root, "result") {
        Some(result) if get_map_value(root, "jsonrpc").is_some() => (result, "/result"),
        _ => (root, ""),
    };
    let tools = get_map_value(result, "tools")
        .and_then(|node| node.data.as_vec())
        .ok_or_else(not_tools_list)?;

    // A gap outside every tool would reach no finding: `contract-partial` is
    // raised from an `Unknown` inside an endpoint's types, and there is no
    // endpoint to hang half a list on. So a partial list is refused outright.
    if get_map_value(result, "nextCursor").is_some() {
        return Err(McpError::Paginated {
            contract_source: source.to_owned(),
        });
    }

    let mut contract = Contract::empty();
    let mut seen = BTreeSet::new();

    for (index, tool) in tools.iter().enumerate() {
        let pointer = format!("{prefix}/tools/{index}");
        let Some(name) = get_map_value(tool, "name").and_then(|node| node.data.as_str()) else {
            return Err(McpError::UnnamedTool {
                contract_source: source.to_owned(),
                pointer,
            });
        };
        if !seen.insert(name.to_owned()) {
            return Err(McpError::DuplicateTool {
                contract_source: source.to_owned(),
                name: name.to_owned(),
            });
        }

        let request_pointer = format!("{pointer}/inputSchema");
        let request = match get_map_value(tool, "inputSchema") {
            Some(input_schema) => {
                let (ty, unmodelled) = parse_json_schema(source, input_schema, &request_pointer)?;
                contract.unmodelled.extend(unmodelled);
                Payload::single(ty, span(source, input_schema, request_pointer))
            }
            // The tool still exists, so its removal is still checked; only its
            // arguments go unverified, and `contract-partial` says so.
            None => {
                let at = span(source, tool, request_pointer.clone());
                contract.unmodelled.push(Unmodelled {
                    kind: UnmodelledKind::SchemaDeferred,
                    pointer: request_pointer,
                    span: at.clone(),
                });
                Payload::single(TypeRef::Unknown(UnmodelledKind::SchemaDeferred), at)
            }
        };

        let mut responses = BTreeMap::new();
        if let Some(output_schema) = get_map_value(tool, "outputSchema") {
            let response_pointer = format!("{pointer}/outputSchema");
            let (response_ty, unmodelled) =
                parse_json_schema(source, output_schema, &response_pointer)?;
            contract.unmodelled.extend(unmodelled);
            responses.insert(
                RESULT.to_owned(),
                Payload::single(response_ty, span(source, output_schema, response_pointer)),
            );
        }

        contract.endpoints.insert(
            EndpointKey {
                method: METHOD_CALL.to_owned(),
                path: format!("/tools/{name}"),
            },
            Endpoint {
                // The name is already the endpoint's identity. Repeating it
                // here would let `operation-id-changed` re-report a rename
                // that `endpoint-removed` has already reported.
                operation_id: None,
                // MCP has no deprecation marker on a tool.
                deprecated: false,
                sunset: None,
                parameters: Vec::new(),
                request: Some(request),
                responses,
                security: Vec::new(),
                span: span(source, tool, pointer),
            },
        );
    }

    contract
        .unmodelled
        .sort_by(|a, b| a.pointer.cmp(&b.pointer).then_with(|| a.kind.cmp(&b.kind)));
    contract.unmodelled.dedup();
    Ok(contract)
}

/// Every other ingester takes a resolver, and `check` hands one to each.
///
/// A tool snapshot is one self-contained document: its `$ref`s point into
/// the schema that holds them, never at a sibling file, so the resolver is
/// accepted and not consulted.
pub fn ingest_with_resolver(
    source: &str,
    bytes: &[u8],
    _resolver: &dyn DocumentResolver,
) -> Result<Contract, McpError> {
    ingest(source, bytes)
}

fn get_map_value<'a>(node: &'a MarkedYamlOwned, key: &str) -> Option<&'a MarkedYamlOwned> {
    node.data
        .as_mapping()?
        .get(&MarkedYamlOwned::from(YamlDataOwned::Value(
            ScalarOwned::String(key.to_owned()),
        )))
}

fn span(source: &str, node: &MarkedYamlOwned, pointer: String) -> Span {
    Span {
        file: source.to_owned(),
        line: node.span.start.line(),
        column: node.span.start.col() + 1,
        pointer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str) -> EndpointKey {
        EndpointKey {
            method: METHOD_CALL.to_owned(),
            path: format!("/tools/{name}"),
        }
    }

    fn request_fields(contract: &Contract, name: &str) -> BTreeMap<String, bool> {
        let request = contract.endpoints[&key(name)].request.as_ref().unwrap();
        match &request.media_types[crate::contract::MEDIA_ANY] {
            TypeRef::Object { fields, .. } => fields
                .iter()
                .map(|(field, held)| (field.clone(), held.required))
                .collect(),
            other => panic!("expected an object, got {other:?}"),
        }
    }

    const SNAPSHOT: &str = r#"{
  "tools": [
    {
      "name": "search_issues",
      "description": "Search issues",
      "inputSchema": {
        "type": "object",
        "properties": {
          "query": { "type": "string" },
          "limit": { "type": "integer", "minimum": 1 }
        },
        "required": ["query"]
      },
      "outputSchema": {
        "type": "object",
        "properties": { "total": { "type": "integer" } },
        "required": ["total"]
      }
    },
    {
      "name": "ping",
      "inputSchema": { "type": "object" }
    }
  ]
}"#;

    #[test]
    fn each_tool_is_an_endpoint_with_its_input_as_the_request() {
        let contract = ingest("mcp/tools.json", SNAPSHOT.as_bytes()).unwrap();

        assert_eq!(
            contract.endpoints.keys().cloned().collect::<Vec<_>>(),
            vec![key("ping"), key("search_issues")]
        );
        assert_eq!(
            request_fields(&contract, "search_issues"),
            BTreeMap::from([("limit".to_owned(), false), ("query".to_owned(), true)])
        );
        let search = &contract.endpoints[&key("search_issues")];
        assert_eq!(search.span.pointer, "/tools/0");
        assert_eq!(search.span.line, 3);
        assert!(contract.unmodelled.is_empty(), "{:?}", contract.unmodelled);
    }

    #[test]
    fn output_schema_is_the_result_and_its_absence_is_no_response() {
        let contract = ingest("mcp/tools.json", SNAPSHOT.as_bytes()).unwrap();

        let search = &contract.endpoints[&key("search_issues")];
        assert_eq!(
            search.responses.keys().collect::<Vec<_>>(),
            vec![RESULT],
            "outputSchema is filed under the result key"
        );
        assert!(contract.endpoints[&key("ping")].responses.is_empty());
    }

    #[test]
    fn a_json_rpc_response_envelope_ingests_like_the_bare_result() {
        let envelope = format!(r#"{{"jsonrpc": "2.0", "id": 7, "result": {SNAPSHOT}}}"#);
        let wrapped = ingest("mcp/tools.json", envelope.as_bytes()).unwrap();
        let bare = ingest("mcp/tools.json", SNAPSHOT.as_bytes()).unwrap();

        assert_eq!(
            wrapped.endpoints.keys().collect::<Vec<_>>(),
            bare.endpoints.keys().collect::<Vec<_>>()
        );
        assert_eq!(
            wrapped.endpoints[&key("search_issues")].span.pointer,
            "/result/tools/0"
        );
    }

    #[test]
    fn a_ref_resolves_against_the_schema_that_holds_it() {
        let snapshot = r##"{"tools": [{
          "name": "filter",
          "inputSchema": {
            "type": "object",
            "properties": { "by": { "$ref": "#/$defs/Filter" } },
            "$defs": {
              "Filter": { "type": "object", "properties": { "state": { "type": "string" } } }
            }
          }
        }]}"##;
        let contract = ingest("mcp/tools.json", snapshot.as_bytes()).unwrap();

        assert!(contract.unmodelled.is_empty(), "{:?}", contract.unmodelled);
        let request = contract.endpoints[&key("filter")].request.as_ref().unwrap();
        let TypeRef::Object { fields, .. } = &request.media_types[crate::contract::MEDIA_ANY]
        else {
            panic!("expected an object");
        };
        assert!(matches!(fields["by"].ty, TypeRef::Object { .. }));
    }

    #[test]
    fn a_remote_ref_is_refused_never_fetched() {
        let snapshot = r#"{"tools": [{
          "name": "fetch",
          "inputSchema": { "$ref": "https://example.com/schema.json" }
        }]}"#;
        let error = ingest("mcp/tools.json", snapshot.as_bytes()).unwrap_err();

        assert!(
            error.to_string().contains("never makes a network request"),
            "{error}"
        );
    }

    #[test]
    fn a_paginated_snapshot_is_refused() {
        let snapshot = r#"{"tools": [{"name": "a", "inputSchema": {"type": "object"}}],
                           "nextCursor": "page-2"}"#;
        let error = ingest("mcp/tools.json", snapshot.as_bytes()).unwrap_err();

        assert!(matches!(error, McpError::Paginated { .. }), "{error}");
        assert!(
            error.to_string().contains("concatenate the pages"),
            "{error}"
        );
    }

    #[test]
    fn a_tool_without_an_input_schema_keeps_its_endpoint_but_is_unverified() {
        let snapshot = r#"{"tools": [{"name": "no_schema"}]}"#;
        let contract = ingest("mcp/tools.json", snapshot.as_bytes()).unwrap();

        let request = contract.endpoints[&key("no_schema")]
            .request
            .as_ref()
            .unwrap();
        assert!(matches!(
            request.media_types[crate::contract::MEDIA_ANY],
            TypeRef::Unknown(UnmodelledKind::SchemaDeferred)
        ));
        assert_eq!(contract.unmodelled[0].pointer, "/tools/0/inputSchema");
    }

    #[test]
    fn a_tool_without_a_name_is_an_error() {
        let snapshot = r#"{"tools": [{"inputSchema": {}}]}"#;
        let error = ingest("mcp/tools.json", snapshot.as_bytes()).unwrap_err();

        assert!(matches!(error, McpError::UnnamedTool { .. }), "{error}");
    }

    #[test]
    fn a_duplicate_tool_name_is_an_error() {
        let snapshot = r#"{"tools": [
          {"name": "a", "inputSchema": {"type": "object"}},
          {"name": "a", "inputSchema": {"type": "object"}}
        ]}"#;
        let error = ingest("mcp/tools.json", snapshot.as_bytes()).unwrap_err();

        assert!(matches!(error, McpError::DuplicateTool { .. }), "{error}");
    }

    #[test]
    fn documents_that_are_not_a_tools_list_are_refused() {
        for document in [
            r#"{"openapi": "3.1.0", "paths": {}}"#,
            r#"{"tools": {"name": "a"}}"#,
            r#"{"jsonrpc": "2.0", "id": 1, "result": {"resources": []}}"#,
            "[]",
        ] {
            assert!(
                matches!(
                    ingest("x.json", document.as_bytes()),
                    Err(McpError::NotToolsList { .. })
                ),
                "{document}"
            );
        }
    }
}
