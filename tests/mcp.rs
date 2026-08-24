//! The MCP server, driven over its real transport.
//!
//! `src/mcp/handlers.rs` tests the tools directly. This drives the binary the
//! way a client does — JSON-RPC over stdio — because the wiring between the
//! two is exactly the layer library tests cannot see, and it is where a
//! dropped argument hides.
//!
//! Skipped unless the `mcp` feature is on: `make check` builds
//! `--all-features`, so it runs there.

#![cfg(feature = "mcp")]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

const BRAKE: &str = env!("CARGO_BIN_EXE_brake");

const BASE: &str = r#"
openapi: 3.1.0
paths:
  /payments/{id}:
    get:
      operationId: getPayment
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                type: object
                required: [id, customer_id]
                properties:
                  id: { type: string }
                  customer_id: { type: string }
"#;

const HEAD_BREAKS: &str = r#"
openapi: 3.1.0
paths:
  /payments/{id}:
    get:
      operationId: getPayment
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                type: object
                required: [id]
                properties:
                  id: { type: string }
"#;

/// A live `brake mcp` process, spoken to in JSON-RPC.
struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl Client {
    fn start(root: &Path) -> Self {
        let mut child = Command::new(BRAKE)
            .arg("mcp")
            .arg(root)
            // Pinned so the expiry path is deterministic, exactly as the CLI
            // takes `--as-of`.
            .args(["--as-of", "2026-08-23"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("brake mcp should launch");

        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let mut client = Self {
            child,
            stdin,
            stdout,
            next_id: 0,
        };
        client.initialize();
        client
    }

    fn initialize(&mut self) {
        let result = self.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "brake-tests", "version": "0" },
            }),
        );
        assert_eq!(result["serverInfo"]["name"], "brake", "{result}");
        self.notify("notifications/initialized", json!({}));
    }

    fn send(&mut self, message: &Value) {
        writeln!(self.stdin, "{message}").expect("write");
        self.stdin.flush().expect("flush");
    }

    fn notify(&mut self, method: &str, params: Value) {
        let message = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.send(&message);
    }

    /// Send a request and read until its response arrives.
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let message = json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params,
        });
        self.send(&message);

        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).expect("read");
            assert!(
                read > 0,
                "the server closed the connection during `{method}`"
            );
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            // Skip anything that is not the answer to this request.
            if value["id"] != json!(id) {
                continue;
            }
            if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
                panic!("`{method}` failed: {error}");
            }
            return value["result"].clone();
        }
    }

    fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A shell command that creates `path` as a marker, in whichever shell
/// `--drift` uses.
///
/// `mkdir` rather than `touch`: it is a builtin in both `sh` and `cmd`, needs
/// no redirection, and depends on nothing being on PATH. `touch` is not a cmd
/// builtin, which made this guard half-vacuous on Windows — the "did not run"
/// assertion passed because the command failed rather than because brake
/// refused it.
///
/// Embedded in a TOML *literal* string (single quotes) by the caller, because
/// a Windows path is full of backslashes and `\U` in `C:\Users` is not a valid
/// escape in a TOML basic string. That parse failure is what made this test
/// fail on Windows — and made its first assertion pass for the wrong reason,
/// since a config that does not load runs no generator either.
fn create_file_command(path: &Path) -> String {
    format!("mkdir \"{}\"", path.display())
}

fn repo(files: &[(&str, &str)]) -> TempDir {
    let repo = tempdir().expect("tempdir");
    for (path, body) in files {
        let full = repo.path().join(path);
        fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
        fs::write(full, body).expect("write");
    }
    repo
}

fn configured() -> TempDir {
    repo(&[
        (
            "brake.toml",
            "[[contract]]\nname=\"payments\"\nformat=\"openapi\"\nsource=\"api/c.yaml\"\n\
             baseline={file=\"api/c.baseline.yaml\"}\n",
        ),
        ("api/c.baseline.yaml", BASE),
        ("api/c.yaml", BASE),
    ])
}

#[test]
fn the_server_initializes_and_advertises_its_surface() {
    let repo = configured();
    let mut client = Client::start(repo.path());

    let tools = client.request("tools/list", json!({}));
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(
        names,
        vec![
            "check_change",
            "compare_contracts",
            "explain_rule",
            "check_repository",
            "who_consumes"
        ]
    );

    for tool in tools["tools"].as_array().expect("tools") {
        assert!(
            tool["description"].as_str().is_some_and(|d| d.len() > 60),
            "{}: the description is the only documentation an agent reads first",
            tool["name"]
        );
        assert_eq!(tool["inputSchema"]["type"], "object", "{}", tool["name"]);
    }

    let resources = client.request("resources/list", json!({}));
    let uris: Vec<&str> = resources["resources"]
        .as_array()
        .expect("resources")
        .iter()
        .filter_map(|resource| resource["uri"].as_str())
        .collect();
    assert!(uris.contains(&"brake://rules"));
    assert!(uris.contains(&"brake://strategies"));
    assert!(uris.contains(&"brake://config"));
    assert!(uris.contains(&"brake://consumers"));

    let prompts = client.request("prompts/list", json!({}));
    assert_eq!(prompts["prompts"][0]["name"], "review-api-change");
}

#[test]
fn check_change_reports_a_break_in_an_unsaved_draft() {
    let repo = configured();
    let mut client = Client::start(repo.path());

    let result = client.call_tool(
        "check_change",
        json!({
            "format": "openapi",
            "proposed": HEAD_BREAKS,
            "contract": "payments",
        }),
    );

    assert_eq!(
        result["isError"], false,
        "a break is an answer, not a tool failure"
    );
    let structured = &result["structuredContent"];
    assert_eq!(structured["verdict"], "findings");
    assert_eq!(structured["findings"][0]["rule"], "response-field-removed");
    assert_eq!(structured["findings"][0]["subject"], "customer_id");

    // The draft was never written to disk.
    assert_eq!(
        fs::read_to_string(repo.path().join("api/c.yaml")).expect("read"),
        BASE,
        "the server must not write the proposal"
    );
}

#[test]
fn a_finding_arrives_with_the_ways_to_make_the_change_safely() {
    let repo = configured();
    let mut client = Client::start(repo.path());

    let result = client.call_tool(
        "check_change",
        json!({ "format": "openapi", "proposed": HEAD_BREAKS }),
    );
    let finding = &result["structuredContent"]["findings"][0];

    let strategies: Vec<&str> = finding["remediation"]
        .as_array()
        .expect("remediation")
        .iter()
        .filter_map(|item| item["strategy"].as_str())
        .collect();
    assert_eq!(
        strategies,
        vec![
            "deprecate-then-remove",
            "expand-then-contract",
            "version-the-endpoint"
        ]
    );

    assert!(
        finding["remediation"][0]["summary"]
            .as_str()
            .expect("summary")
            .contains("`customer_id`"),
        "the strategy must name the field it is about"
    );
    assert!(
        finding["remediation"][0]["cost"].as_str().is_some(),
        "options with no costs read as though they are all free"
    );
    assert!(
        finding["choice_is_not_brakes"].is_string(),
        "an agent handed one confident recommendation will follow it"
    );
}

#[test]
fn compare_contracts_works_with_no_configuration_at_all() {
    // The acid test from design/04-mcp-interface.md §1: useful to an agent
    // reviewing a diff in a repository it has never configured.
    let bare = tempdir().expect("tempdir");
    let mut client = Client::start(bare.path());

    let result = client.call_tool(
        "compare_contracts",
        json!({ "format": "openapi", "base": BASE, "head": HEAD_BREAKS }),
    );
    assert_eq!(result["isError"], false);
    assert_eq!(result["structuredContent"]["verdict"], "findings");
}

#[test]
fn an_unverifiable_payload_is_not_reported_as_clean() {
    // A human skims a warning; an agent acts on the absence of one.
    let unreadable = r#"
openapi: 3.1.0
paths:
  /payments:
    get:
      operationId: listPayments
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                $ref: 'common.yaml#/components/schemas/Payment'
"#;
    let bare = tempdir().expect("tempdir");
    let mut client = Client::start(bare.path());

    let result = client.call_tool(
        "compare_contracts",
        json!({ "format": "openapi", "base": unreadable, "head": unreadable }),
    );
    let structured = &result["structuredContent"];

    assert_eq!(structured["verdict"], "unverified", "{structured}");
    assert!(
        structured["findings"].as_array().expect("array").is_empty(),
        "and not mixed in with the real findings"
    );
    assert!(
        !structured["unverified"]
            .as_array()
            .expect("array")
            .is_empty()
    );
}

#[test]
fn a_tool_that_cannot_answer_is_an_error_not_a_verdict() {
    let bare = tempdir().expect("tempdir");
    let mut client = Client::start(bare.path());

    let result = client.call_tool(
        "compare_contracts",
        json!({ "format": "openapi", "base": BASE, "head": "@@ not a document @@" }),
    );
    assert_eq!(
        result["isError"], true,
        "exit code 2, not 1: brake could not determine an answer"
    );
}

#[test]
fn a_remote_ref_is_refused_over_mcp_as_well() {
    let bare = tempdir().expect("tempdir");
    let mut client = Client::start(bare.path());

    let result = client.call_tool(
        "compare_contracts",
        json!({
            "format": "openapi",
            "base": BASE,
            "head": "openapi: 3.1.0\npaths:\n  /p:\n    get:\n      responses:\n        \"200\":\n          content:\n            application/json:\n              schema:\n                $ref: 'https://example.invalid/s.yaml'\n",
        }),
    );
    assert_eq!(result["isError"], true);
    let text = result["content"][0]["text"].as_str().expect("text");
    assert!(text.contains("network"), "{text}");
}

#[test]
fn resources_serve_the_catalogue_and_the_strategies() {
    let repo = configured();
    let mut client = Client::start(repo.path());

    let rules = client.request("resources/read", json!({ "uri": "brake://rules" }));
    let body = rules["contents"][0]["text"].as_str().expect("text");
    assert!(body.contains("response-field-removed"), "{body}");

    let strategies = client.request("resources/read", json!({ "uri": "brake://strategies" }));
    let body = strategies["contents"][0]["text"].as_str().expect("text");
    assert!(body.contains("deprecate-then-remove"), "{body}");
    assert!(body.contains("cost"), "{body}");

    let config = client.request("resources/read", json!({ "uri": "brake://config" }));
    let body = config["contents"][0]["text"].as_str().expect("text");
    assert!(body.contains("payments"), "{body}");
}

#[test]
fn the_prompt_arrives_with_the_findings_already_gathered() {
    let bare = tempdir().expect("tempdir");
    let mut client = Client::start(bare.path());

    let result = client.request(
        "prompts/get",
        json!({
            "name": "review-api-change",
            "arguments": { "format": "openapi", "base": BASE, "head": HEAD_BREAKS },
        }),
    );

    let text = result["messages"][0]["content"]["text"]
        .as_str()
        .expect("prompt text");
    assert!(text.contains("response-field-removed"), "{text}");
    assert!(text.contains("deprecate-then-remove"), "{text}");
    assert!(
        text.contains("on behalf of the people who consume it"),
        "{text}"
    );
}

/// The load-bearing exclusion: `design/04-mcp-interface.md` §5.1.
///
/// Drift runs a command out of a config file. An agent that can write
/// `brake.toml` — which any agent editing a repository can — and then call a
/// tool honouring it would have arbitrary command execution, obtained through
/// a tool whose stated purpose is reading files.
#[test]
fn no_tool_call_can_execute_a_declared_generator() {
    let witness_dir = tempdir().expect("tempdir");
    let witness = witness_dir.path().join("the-generator-ran");

    let repo = repo(&[
        (
            "brake.toml",
            &format!(
                "[[contract]]\nname=\"payments\"\nformat=\"openapi\"\nsource=\"api/c.yaml\"\n\
                 baseline={{file=\"api/c.baseline.yaml\"}}\n\
                 [contract.generated]\ncommand = '{}'\n",
                create_file_command(&witness)
            ),
        ),
        ("api/c.baseline.yaml", BASE),
        ("api/c.yaml", BASE),
    ]);

    let mut client = Client::start(repo.path());

    // Every tool, including ones that read the configuration declaring it,
    // and with arguments an agent might try to smuggle a flag through.
    client.call_tool(
        "check_change",
        json!({ "format": "openapi", "proposed": HEAD_BREAKS, "contract": "payments" }),
    );
    client.call_tool("check_repository", json!({}));
    client.call_tool("check_repository", json!({ "drift": true }));
    client.call_tool(
        "check_change",
        json!({
            "format": "openapi",
            "proposed": HEAD_BREAKS,
            "contract": "payments",
            "drift": true,
            "generated": { "command": "brake-should-never-run" },
        }),
    );
    client.request("resources/read", json!({ "uri": "brake://config" }));
    // And the demand surface, with arguments an agent might try to smuggle a
    // generator through.
    client.call_tool(
        "who_consumes",
        json!({ "contract": "payments", "field": "customer_id", "drift": true }),
    );
    client.request("resources/read", json!({ "uri": "brake://consumers" }));

    assert!(
        !witness.exists(),
        "the MCP server executed a config-declared command; \
         an agent that can write brake.toml now has arbitrary code execution"
    );
}

#[test]
fn an_unknown_resource_is_refused_rather_than_read_as_a_path() {
    let repo = configured();
    let mut client = Client::start(repo.path());

    // Not a `request`: this one is expected to come back as a JSON-RPC error.
    client.next_id += 1;
    let id = client.next_id;
    client.send(&json!({
        "jsonrpc": "2.0", "id": id, "method": "resources/read",
        "params": { "uri": "file:///etc/passwd" },
    }));

    loop {
        let mut line = String::new();
        assert!(client.stdout.read_line(&mut line).expect("read") > 0);
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value["id"] != json!(id) {
            continue;
        }
        assert!(
            !value["error"].is_null(),
            "reading an arbitrary path must fail: {value}"
        );
        let message = value["error"]["message"].as_str().unwrap_or_default();
        assert!(
            !message.contains("root:"),
            "the server appears to have read the file"
        );
        break;
    }
}

#[test]
fn the_server_survives_a_tool_failure_and_answers_the_next_call() {
    // A long-lived server that dies on a bad argument is a server an agent
    // gives up on.
    let bare = tempdir().expect("tempdir");
    let mut client = Client::start(bare.path());

    let failed = client.call_tool("compare_contracts", json!({ "format": "openapi" }));
    assert_eq!(failed["isError"], true);

    let recovered = client.call_tool(
        "compare_contracts",
        json!({ "format": "openapi", "base": BASE, "head": BASE }),
    );
    assert_eq!(recovered["structuredContent"]["verdict"], "clean");
}

#[test]
fn explain_rule_reaches_the_whole_catalogue() {
    let bare = tempdir().expect("tempdir");
    let mut client = Client::start(bare.path());

    let all = client.call_tool("explain_rule", json!({}));
    let rules = all["structuredContent"]["rules"]
        .as_array()
        .expect("rules")
        .len();
    assert!(rules > 30, "only {rules} rules");

    let one = client.call_tool("explain_rule", json!({ "rule": "field-number-changed" }));
    let structured = &one["structuredContent"];
    assert_eq!(structured["id"], "field-number-changed");
    assert_eq!(
        structured["remediation"][0]["strategy"], "reserve-the-number",
        "renumbering has exactly one correct answer"
    );

    let unknown = client.call_tool("explain_rule", json!({ "rule": "no-such-rule" }));
    assert_eq!(unknown["isError"], true);
}

/// A repository whose contract has a declared consumer.
fn with_a_consumer() -> TempDir {
    repo(&[
        (
            "brake.toml",
            "[[contract]]\nname=\"payments\"\nformat=\"openapi\"\nsource=\"api/c.yaml\"\n\
             baseline={file=\"api/c.baseline.yaml\"}\n\
             \n[[consumer]]\nformat=\"pact\"\nsource=\"pacts/web-checkout.json\"\n",
        ),
        ("api/c.baseline.yaml", BASE),
        ("api/c.yaml", BASE),
        ("pacts/web-checkout.json", PACT),
    ])
}

const PACT: &str = r#"{
  "consumer": { "name": "web-checkout" },
  "provider": { "name": "payments" },
  "interactions": [
    {
      "description": "a request for payment 42",
      "request": { "method": "GET", "path": "/payments/42" },
      "response": {
        "status": 200,
        "headers": { "Content-Type": "application/json" },
        "body": { "id": "42", "customer_id": "c-1" }
      }
    }
  ]
}"#;

#[test]
fn who_consumes_answers_before_the_edit_is_written() {
    let repo = with_a_consumer();
    let mut client = Client::start(repo.path());

    let answer = client.call_tool(
        "who_consumes",
        json!({ "contract": "payments", "field": "customer_id" }),
    )["structuredContent"]
        .clone();

    assert_eq!(answer["consumers"][0]["consumer"], "web-checkout");
    assert_eq!(
        answer["consumers"][0]["uses"][0]["endpoint"],
        "GET /payments/{id}"
    );
    assert!(
        answer["consumers"][0]["uses"][0]["declared_at"]
            .as_str()
            .is_some_and(|at| at.starts_with("pacts/web-checkout.json:")),
        "the interaction that declares it is the whole point: {answer}"
    );
    // An empty answer must never read as "nobody uses this".
    assert!(
        answer["note"]
            .as_str()
            .is_some_and(|note| note.contains("no others")),
        "{answer}"
    );
}

#[test]
fn who_consumes_says_nothing_it_cannot_support() {
    let repo = with_a_consumer();
    let mut client = Client::start(repo.path());

    let answer = client.call_tool(
        "who_consumes",
        json!({ "contract": "payments", "field": "never_declared" }),
    )["structuredContent"]
        .clone();
    assert_eq!(
        answer["consumers"].as_array().map(Vec::len),
        Some(0),
        "{answer}"
    );
    assert_eq!(answer["declared_consumers"], 1, "{answer}");
}

#[test]
fn check_change_names_who_a_break_reaches() {
    let repo = with_a_consumer();
    let mut client = Client::start(repo.path());

    let verdict = client.call_tool(
        "check_change",
        json!({ "format": "openapi", "proposed": HEAD_BREAKS, "contract": "payments" }),
    )["structuredContent"]
        .clone();

    let removal = verdict["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .find(|finding| finding["rule"] == "response-field-removed")
        .unwrap_or_else(|| panic!("expected a removal: {verdict}"));
    assert_eq!(removal["affects"][0]["consumer"], "web-checkout");
    assert_eq!(removal["affects"][0]["source"], "pacts/web-checkout.json");
}

#[test]
fn the_consumers_resource_is_the_inventory_and_says_what_it_is_not() {
    let repo = with_a_consumer();
    let mut client = Client::start(repo.path());

    let resource = client.request("resources/read", json!({ "uri": "brake://consumers" }));
    let text = resource["contents"][0]["text"]
        .as_str()
        .expect("resource text");
    let value: Value = serde_json::from_str(text).expect("valid JSON");

    assert_eq!(value["contracts"][0]["name"], "payments");
    assert_eq!(
        value["contracts"][0]["consumers"][0]["consumer"],
        "web-checkout"
    );
    assert!(
        value["contracts"][0]["consumers"][0]["digest"]
            .as_str()
            .is_some_and(|digest| digest.starts_with("sha256:")),
        "{value}"
    );
    assert!(
        value["note"]
            .as_str()
            .is_some_and(|note| note.contains("no others")),
        "{value}"
    );
}
