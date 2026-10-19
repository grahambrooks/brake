//! MCP tool snapshots through the ordinary comparator and rule catalogue.
//!
//! `design/08-mcp-tool-contracts.md` claims every tool break is already a
//! rule. Each test here pins one row of its table: the break fires the rule it
//! names, and the safe change next to it stays quiet.

use std::fs;
use std::path::PathBuf;

use brake::check::{Options, Scope, check};
use brake::config::{Baseline, Compatibility, Config, ContractConfig, ContractFormat, Defaults};
use brake::render::text;
use brake::{Format, Level, Severity};
use serde_json::{Value, json};
use tempfile::tempdir;

fn base() -> Value {
    json!({
      "tools": [
        {
          "name": "search_issues",
          "description": "Search issues in a repository",
          "inputSchema": {
            "type": "object",
            "properties": {
              "repo": { "type": "string" },
              "query": { "type": "string" },
              "state": { "type": "string", "enum": ["open", "closed", "all"] },
              "limit": { "type": "integer", "minimum": 1, "maximum": 100 }
            },
            "required": ["repo", "query"]
          },
          "outputSchema": {
            "type": "object",
            "properties": {
              "total": { "type": "integer" },
              "items": {
                "type": "array",
                "items": {
                  "type": "object",
                  "properties": {
                    "number": { "type": "integer" },
                    "title": { "type": "string" }
                  },
                  "required": ["number", "title"]
                }
              }
            },
            "required": ["total", "items"]
          }
        },
        {
          "name": "close_issue",
          "inputSchema": {
            "type": "object",
            "properties": { "repo": { "type": "string" }, "number": { "type": "integer" } },
            "required": ["repo", "number"]
          }
        }
      ]
    })
}

/// The rule ids that fail a commit at `wire-json`, sorted and deduplicated.
fn errors(head: &Value) -> Vec<&'static str> {
    findings(head)
        .into_iter()
        .filter(|(_, severity)| *severity == Severity::Error)
        .map(|(rule, _)| rule)
        .collect()
}

fn findings(head: &Value) -> Vec<(&'static str, Severity)> {
    let parse = |value: &Value| {
        brake::parse(Format::Mcp, "mcp/tools.json", value.to_string().as_bytes())
            .expect("snapshot parses")
    };
    let changes = brake::compare(&parse(&base()), &parse(head));
    let mut found: Vec<_> = brake::evaluate(&changes, "tools", Level::WireJson)
        .into_iter()
        .map(|finding| (finding.rule_id, finding.severity))
        .collect();
    found.sort();
    found.dedup();
    found
}

fn edit(change: impl FnOnce(&mut Value)) -> Value {
    let mut head = base();
    change(&mut head);
    head
}

fn search(head: &mut Value) -> &mut Value {
    &mut head["tools"][0]
}

#[test]
fn an_unchanged_snapshot_is_clean() {
    assert_eq!(findings(&base()), vec![]);
}

#[test]
fn removing_or_renaming_a_tool_is_a_break() {
    let removed = edit(|head| {
        head["tools"].as_array_mut().unwrap().remove(1);
    });
    assert!(errors(&removed).contains(&"endpoint-removed"));

    let renamed = edit(|head| head["tools"][1]["name"] = json!("issue_close"));
    assert!(errors(&renamed).contains(&"endpoint-removed"));
}

#[test]
fn adding_a_tool_is_not() {
    let added = edit(|head| {
        head["tools"].as_array_mut().unwrap().push(json!({
            "name": "label_issue",
            "inputSchema": { "type": "object" }
        }));
    });
    assert_eq!(errors(&added), Vec::<&str>::new());
}

#[test]
fn a_new_required_argument_is_a_break_and_a_new_optional_one_is_not() {
    let required = edit(|head| {
        let input = &mut search(head)["inputSchema"];
        input["properties"]["org"] = json!({ "type": "string" });
        input["required"].as_array_mut().unwrap().push(json!("org"));
    });
    assert_eq!(errors(&required), vec!["param-added-required"]);

    let optional = edit(|head| {
        search(head)["inputSchema"]["properties"]["org"] = json!({ "type": "string" });
    });
    assert_eq!(errors(&optional), Vec::<&str>::new());
}

/// A body field is not a parameter, so this is `param-type-narrowed` rather
/// than `param-became-required`: the same routing an OpenAPI request body
/// gets.
#[test]
fn an_optional_argument_becoming_required_is_a_break() {
    let head = edit(|head| {
        search(head)["inputSchema"]["required"]
            .as_array_mut()
            .unwrap()
            .push(json!("limit"));
    });
    assert_eq!(errors(&head), vec!["param-type-narrowed"]);
}

#[test]
fn narrowing_an_argument_is_a_break_and_widening_it_is_not() {
    let dropped_enum_value = edit(|head| {
        search(head)["inputSchema"]["properties"]["state"]["enum"] = json!(["open", "closed"]);
    });
    assert_eq!(errors(&dropped_enum_value), vec!["param-type-narrowed"]);

    let tightened_bound = edit(|head| {
        search(head)["inputSchema"]["properties"]["limit"]["maximum"] = json!(50);
    });
    assert_eq!(errors(&tightened_bound), vec!["param-type-narrowed"]);

    let removed_argument = edit(|head| {
        let properties = search(head)["inputSchema"]["properties"]
            .as_object_mut()
            .unwrap();
        properties.remove("limit");
    });
    assert_eq!(errors(&removed_argument), vec!["param-type-narrowed"]);

    let closed = edit(|head| search(head)["inputSchema"]["additionalProperties"] = json!(false));
    assert_eq!(errors(&closed), vec!["param-type-narrowed"]);

    let widened = edit(|head| {
        let properties = &mut search(head)["inputSchema"]["properties"];
        properties["state"]["enum"] = json!(["open", "closed", "all", "draft"]);
        properties["limit"]["maximum"] = json!(500);
    });
    assert_eq!(errors(&widened), Vec::<&str>::new());
}

#[test]
fn structured_output_breaks_are_response_breaks() {
    let removed_field = edit(|head| {
        let output = &mut search(head)["outputSchema"];
        output["properties"]
            .as_object_mut()
            .unwrap()
            .remove("total");
        output["required"] = json!(["items"]);
    });
    assert_eq!(errors(&removed_field), vec!["response-field-removed"]);

    let changed_type = edit(|head| {
        search(head)["outputSchema"]["properties"]["total"] = json!({ "type": "string" });
    });
    assert_eq!(errors(&changed_type), vec!["response-type-changed"]);

    let dropped_schema = edit(|head| {
        search(head).as_object_mut().unwrap().remove("outputSchema");
    });
    assert_eq!(errors(&dropped_schema), vec!["response-status-removed"]);

    let added_field = edit(|head| {
        search(head)["outputSchema"]["properties"]["has_more"] = json!({ "type": "boolean" });
    });
    assert_eq!(errors(&added_field), Vec::<&str>::new());
}

#[test]
fn changing_an_argument_type_is_a_break() {
    let head = edit(|head| {
        search(head)["inputSchema"]["properties"]["limit"] = json!({ "type": "string" });
    });
    assert_eq!(errors(&head), vec!["param-type-narrowed"]);
}

#[test]
fn a_structured_output_field_made_optional_is_a_break() {
    let head = edit(|head| search(head)["outputSchema"]["required"] = json!(["items"]));
    assert_eq!(errors(&head), vec!["response-field-optional"]);
}

#[test]
fn adding_an_output_schema_where_there_was_none_is_safe() {
    let head = edit(|head| {
        head["tools"][1]["outputSchema"] = json!({
            "type": "object",
            "properties": { "closed": { "type": "boolean" } }
        });
    });
    assert_eq!(errors(&head), Vec::<&str>::new());
}

#[test]
fn descriptions_and_annotations_are_not_compared() {
    // `design/08-mcp-tool-contracts.md` "Deliberately not built".
    let head = edit(|head| {
        let tool = search(head);
        tool["description"] = json!("Find issues. Prefer narrow queries.");
        tool["title"] = json!("Search issues");
        tool["annotations"] = json!({ "readOnlyHint": true });
    });
    assert_eq!(findings(&head), vec![]);
}

#[test]
fn tool_order_does_not_change_the_verdict() {
    let reordered = edit(|head| head["tools"].as_array_mut().unwrap().reverse());
    assert_eq!(findings(&reordered), vec![]);
}

#[test]
fn a_tool_with_no_input_schema_is_reported_partial_not_clean() {
    let head = edit(|head| {
        head["tools"][1]
            .as_object_mut()
            .unwrap()
            .remove("inputSchema");
    });
    assert!(findings(&head).contains(&("contract-partial", Severity::Warning)));
}

#[test]
fn a_tool_removal_reads_as_a_tool_removal() {
    let checkout = tempdir().expect("tempdir");
    let removed = edit(|head| {
        head["tools"].as_array_mut().unwrap().remove(1);
    });
    for (path, value) in [
        ("mcp/tools.baseline.json", base()),
        ("mcp/tools.json", removed),
    ] {
        let full = checkout.path().join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, serde_json::to_string_pretty(&value).unwrap()).unwrap();
    }
    let config = Config {
        defaults: Defaults {
            compatibility: Compatibility::WireJson,
            baseline: None,
        },
        contracts: vec![ContractConfig {
            name: "github".to_owned(),
            format: ContractFormat::Mcp,
            source: PathBuf::from("mcp/tools.json"),
            compatibility: None,
            baseline: Some(Baseline::File(PathBuf::from("mcp/tools.baseline.json"))),
            allow: Vec::new(),
            generated: None,
        }],
        ..Config::default()
    };

    let report = check(checkout.path(), &config, &Scope::All, &Options::default());
    let rendered = text::render(&report);
    assert!(rendered.contains("CALL /tools/close_issue"), "{rendered}");
    assert!(rendered.contains("endpoint-removed"), "{rendered}");
}

/// `format = "mcp"` in `brake.toml`, and `--drift` against a declared dump
/// command with no MCP-specific code behind it.
#[test]
fn brake_toml_declares_an_mcp_contract_and_drift_checks_its_snapshot() {
    let checkout = tempdir().expect("tempdir");
    let snapshot = serde_json::to_string_pretty(&base()).unwrap();
    for (path, body) in [
        ("mcp/tools.baseline.json", snapshot.clone()),
        ("mcp/tools.json", snapshot.clone()),
        ("mcp/dumped.json", snapshot),
    ] {
        let full = checkout.path().join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, body).unwrap();
    }
    // `--drift` runs through the platform shell, and cmd neither has `cat`
    // nor expands `$VAR`.
    let dump = if cfg!(windows) {
        r#"type "%BRAKE_REPO_ROOT%\mcp\dumped.json""#
    } else {
        r#"cat "$BRAKE_REPO_ROOT/mcp/dumped.json""#
    };
    let config = Config::parse(&format!(
        "[[contract]]\nname = \"github\"\nformat = \"mcp\"\nsource = \"mcp/tools.json\"\n\
         baseline = {{ file = \"mcp/tools.baseline.json\" }}\n\
         generated = {{ command = '{dump}' }}\n"
    ))
    .expect("brake.toml parses");
    assert_eq!(config.contracts[0].format, ContractFormat::Mcp);

    let drift = Options {
        drift: true,
        ..Options::default()
    };
    let report = check(checkout.path(), &config, &Scope::All, &drift);
    assert!(report.findings.is_empty(), "{:?}", report.findings);

    // The server's build changed the tool list; the committed snapshot did not.
    let changed = edit(|head| head["tools"][1]["name"] = json!("issue_close"));
    fs::write(
        checkout.path().join("mcp/dumped.json"),
        serde_json::to_string_pretty(&changed).unwrap(),
    )
    .unwrap();
    let report = check(checkout.path(), &config, &Scope::All, &drift);
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.rule_id == "generated-drift"),
        "{:?}",
        report.findings
    );
}
