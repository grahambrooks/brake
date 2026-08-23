//! The `rmcp` adapter.
//!
//! The only part of brake that knows about MCP or about async. It deserialises
//! arguments, calls a synchronous function in [`super::handlers`], and shapes
//! the result — nothing here decides whether something is a breaking change,
//! for the same reason `main.rs` does not.
//!
//! Transport is stdio and only stdio. Guarantee G1 says no network under any
//! flag, and a server listening on a port is a server.

use std::borrow::Cow;
use std::future::{Future, ready};
use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData as McpError,
    GetPromptRequestParams, GetPromptResponse, GetPromptResult, Implementation, ListPromptsResult,
    ListResourcesResult, ListToolsResult, PaginatedRequestParams, Prompt, PromptArgument,
    PromptMessage, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, Role, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ServerHandler, ServiceExt};
use serde_json::Value;

use super::handlers::{self, Context};

/// Instructions the client shows the model when the server connects.
///
/// This is the only place brake gets to say what it is for before being
/// called, so it is worth the words.
const INSTRUCTIONS: &str = "\
brake checks whether a change to an API contract would break the people who consume \
it, across OpenAPI, protobuf and GraphQL. It never makes a network request and never \
runs the service.

Call `check_change` *while drafting* a change, not after committing it — that is the \
point of this server. It takes the proposed document as text, so a draft that has not \
been written to disk can still be checked.

When something breaks, the response carries named strategies for making the same \
change safely, each with what it costs. brake does not choose between them: which one \
fits depends on whether the team controls every consumer and whether they have a \
version scheme, and it can see neither. You can read the repository, so you are in a \
better position to weigh them — but say which you picked and why.

Two things to read literally. `verdict` is authoritative: `unverified` means part of \
the payload could not be modelled and the change is NOT known to be safe, and \
`unavailable` means brake could not answer at all. An empty `findings` list is not a \
pass unless `verdict` says `clean`.";

/// The MCP server.
///
/// Holds only the repository root: configuration is read per call rather than
/// cached, because a long-lived server holding a stale `brake.toml` gates
/// against configuration nobody can see.
#[derive(Debug, Clone)]
pub struct BrakeServer {
    context: Context,
}

impl BrakeServer {
    #[must_use]
    pub fn new(context: Context) -> Self {
        Self { context }
    }

    /// Run a tool, mapping its outcome onto the MCP result shape.
    ///
    /// A finding is an answer, not an error: `isError` is false for a change
    /// that breaks compatibility and true only when brake could not determine
    /// an answer. That is exit code `1` versus `2`, and it matters for the same
    /// reason — see `design/04-mcp-interface.md` §6.
    fn dispatch(&self, request: CallToolRequestParams) -> CallToolResult {
        let arguments = Value::Object(request.arguments.unwrap_or_default());

        let outcome =
            match request.name.as_ref() {
                "check_change" => {
                    decode(arguments).and_then(|args| handlers::check_change(&self.context, args))
                }
                "compare_contracts" => decode(arguments)
                    .and_then(|args| handlers::compare_contracts(&self.context, args)),
                "explain_rule" => {
                    decode(arguments).and_then(|args| handlers::explain_rule(&self.context, args))
                }
                "check_repository" => decode(arguments)
                    .and_then(|args| handlers::check_repository(&self.context, args)),
                other => Err(handlers::ToolFailure {
                    message: format!(
                        "unknown tool `{other}`. This server offers: {}",
                        handlers::TOOL_NAMES.join(", ")
                    ),
                }),
            };

        match outcome {
            Ok(value) => CallToolResult::structured(value),
            Err(failure) => CallToolResult::error(vec![ContentBlock::text(failure.message)]),
        }
    }
}

/// Deserialise tool arguments, reporting the schema mismatch rather than a
/// bare serde message.
fn decode<T: serde::de::DeserializeOwned>(arguments: Value) -> Result<T, handlers::ToolFailure> {
    serde_json::from_value(arguments).map_err(|error| handlers::ToolFailure {
        message: format!("the arguments did not match the tool's schema: {error}"),
    })
}

impl ServerHandler for BrakeServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
        )
        .with_server_info(
            Implementation::new("brake", crate::VERSION)
                .with_title("brake — a brake on breaking API changes")
                .with_description("Checks API contracts for backward compatibility, hermetically."),
        )
        .with_instructions(INSTRUCTIONS)
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        let schemas = handlers::tool_schemas();
        let descriptions = handlers::tool_descriptions();

        let tools = handlers::TOOL_NAMES
            .iter()
            .filter_map(|name| {
                let schema = schemas.get(name)?.as_object()?.clone();
                Some(Tool::new(
                    Cow::Borrowed(*name),
                    Cow::Borrowed(*descriptions.get(name)?),
                    Arc::new(schema),
                ))
            })
            .collect();

        ready(Ok(ListToolsResult {
            tools,
            ..ListToolsResult::default()
        }))
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, McpError>> + Send + '_ {
        ready(Ok(CallToolResponse::Complete(self.dispatch(request))))
    }

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, McpError>> + Send + '_ {
        let resources = handlers::RESOURCES
            .iter()
            .map(|(uri, description)| {
                Resource::new(*uri, *uri)
                    .with_description(*description)
                    .with_mime_type("application/json")
            })
            .collect();

        ready(Ok(ListResourcesResult {
            resources,
            ..ListResourcesResult::default()
        }))
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ReadResourceResponse, McpError>> + Send + '_ {
        let outcome = handlers::read_resource(&self.context, &request.uri);
        ready(match outcome {
            Ok(body) => Ok(ReadResourceResponse::Complete(ReadResourceResult::new(
                vec![ResourceContents::text(body, request.uri)],
            ))),
            // A resource brake does not serve is a protocol-level error: there
            // is no answer to return, unlike a tool that found breakage.
            Err(failure) => Err(McpError::resource_not_found(failure.message, None)),
        })
    }

    fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListPromptsResult, McpError>> + Send + '_ {
        let prompt = Prompt::new(
            handlers::PROMPT_NAME,
            Some(
                "Review a change to an API contract on behalf of the people who consume \
                 it, with the breaks and the ways to avoid them already gathered.",
            ),
            Some(vec![
                required_argument("format", "openapi, proto or graphql."),
                required_argument("base", "The previous document, as text."),
                required_argument("head", "The proposed document, as text."),
                PromptArgument::new("compatibility")
                    .with_description("wire, wire-json, surface or strict. Defaults to wire-json.")
                    .with_required(false),
            ]),
        );

        ready(Ok(ListPromptsResult {
            prompts: vec![prompt],
            ..ListPromptsResult::default()
        }))
    }

    fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<GetPromptResponse, McpError>> + Send + '_ {
        if request.name != handlers::PROMPT_NAME {
            return ready(Err(McpError::invalid_params(
                format!(
                    "unknown prompt `{}`; this server offers `{}`",
                    request.name,
                    handlers::PROMPT_NAME
                ),
                None,
            )));
        }

        let arguments = Value::Object(request.arguments.unwrap_or_default());
        let outcome =
            decode(arguments).and_then(|args| handlers::review_api_change(&self.context, args));

        ready(match outcome {
            Ok(text) => Ok(GetPromptResponse::Complete(
                GetPromptResult::new(vec![PromptMessage::new_text(Role::User, text)])
                    .with_description(
                        "An API compatibility review, with the findings and their \
                         remediation strategies already gathered.",
                    ),
            )),
            Err(failure) => Err(McpError::invalid_params(failure.message, None)),
        })
    }
}

fn required_argument(name: &str, description: &str) -> PromptArgument {
    PromptArgument::new(name)
        .with_description(description)
        .with_required(true)
}

/// Serve over stdio until the client disconnects.
///
/// # Errors
///
/// Returns the transport error if the connection cannot be established or
/// fails while running.
pub async fn serve_stdio(context: Context) -> Result<(), Box<dyn std::error::Error>> {
    let service = BrakeServer::new(context)
        .serve(rmcp::transport::stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn server() -> BrakeServer {
        BrakeServer::new(Context::new(PathBuf::from("."), "2026-08-23".to_owned()))
    }

    fn call(name: &str, arguments: Value) -> CallToolResult {
        let mut request = CallToolRequestParams::new(name.to_owned());
        if let Some(object) = arguments.as_object() {
            request = request.with_arguments(object.clone());
        }
        server().dispatch(request)
    }

    #[test]
    fn the_advertised_instructions_tell_a_model_how_to_read_a_verdict() {
        // An agent that treats an empty `findings` list as a pass is the
        // failure mode of this whole interface.
        assert!(INSTRUCTIONS.contains("verdict"));
        assert!(INSTRUCTIONS.contains("not a pass"));
        assert!(INSTRUCTIONS.contains("does not choose"));
    }

    #[test]
    fn a_breaking_change_is_an_answer_not_an_error() {
        let result = call(
            "compare_contracts",
            serde_json::json!({
                "format": "openapi",
                "base": "openapi: 3.1.0\npaths:\n  /p:\n    get: {operationId: g, responses: {\"200\": {description: ok}}}\n",
                "head": "openapi: 3.1.0\npaths: {}\n",
            }),
        );

        assert_eq!(
            result.is_error,
            Some(false),
            "a finding is a result, not a tool failure"
        );
        let structured = result.structured_content.expect("structured content");
        assert_eq!(structured["verdict"], "findings");
    }

    #[test]
    fn a_tool_that_cannot_answer_is_an_error() {
        let result = call(
            "compare_contracts",
            serde_json::json!({
                "format": "openapi",
                "base": "openapi: 3.1.0\npaths: {}\n",
                "head": "@@ not a document @@",
            }),
        );
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn an_unknown_tool_names_the_ones_that_exist() {
        let result = call("delete_everything", serde_json::json!({}));
        assert_eq!(result.is_error, Some(true));
        let ContentBlock::Text(text) = &result.content[0] else {
            panic!("expected text content");
        };
        assert!(text.text.contains("check_change"), "{}", text.text);
    }

    #[test]
    fn bad_arguments_report_the_schema_rather_than_a_serde_dump() {
        let result = call(
            "compare_contracts",
            serde_json::json!({ "format": "openapi" }),
        );
        assert_eq!(result.is_error, Some(true));
        let ContentBlock::Text(text) = &result.content[0] else {
            panic!("expected text content");
        };
        assert!(text.text.contains("schema"), "{}", text.text);
    }

    #[test]
    fn the_server_advertises_every_capability_it_implements() {
        let info = server().get_info();
        assert!(info.capabilities.tools.is_some());
        assert!(info.capabilities.resources.is_some());
        assert!(info.capabilities.prompts.is_some());
        assert_eq!(info.server_info.name, "brake");
        assert_eq!(info.server_info.version, crate::VERSION);
    }
}
