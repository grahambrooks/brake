//! The CLI, the MCP server and the capability table agree.
//!
//! `design/09-one-core-three-surfaces.md`. The CLI side is read from the
//! binary's own `--help`, and the MCP side from the schemas the server
//! advertises, so this compares what the surfaces accept rather than what the
//! table claims. Regenerate `docs/capabilities.md` with `make docs`.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use brake::surface::CAPABILITIES;

const BRAKE: &str = env!("CARGO_BIN_EXE_brake");

fn help(args: &[&str]) -> String {
    let output = Command::new(BRAKE)
        .args(args)
        .arg("--help")
        .output()
        .expect("brake should launch");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Lines of one `--help` section, `Commands:`, `Arguments:` or `Options:`.
///
/// Long help separates entries with blank lines, so a section runs to the next
/// heading — an unindented line — rather than to the first blank one.
fn section<'a>(help: &'a str, title: &str) -> Vec<&'a str> {
    help.lines()
        .skip_while(|line| line.trim() != title)
        .skip(1)
        .take_while(|line| line.is_empty() || line.starts_with(' '))
        // An entry is indented a little and its description a lot; only the
        // entries name anything.
        .filter(|line| {
            let indent = line.len() - line.trim_start().len();
            !line.trim().is_empty() && indent < 8
        })
        .collect()
}

/// Every spelling the binary accepts: `check`, `check --since`, `check <paths>`.
fn cli_spellings() -> BTreeSet<String> {
    let mut spellings = BTreeSet::new();
    for line in section(&help(&[]), "Commands:") {
        let Some(command) = line.split_whitespace().next() else {
            continue;
        };
        if command == "help" {
            continue;
        }
        spellings.insert(command.to_owned());
        let sub = help(&[command]);
        for argument in section(&sub, "Arguments:") {
            // A wrapped description line is indented too; an argument is
            // the one that starts with `<` or `[`.
            let Some(name) = argument
                .split_whitespace()
                .next()
                .filter(|word| word.starts_with('<') || word.starts_with('['))
            else {
                continue;
            };
            let name = name
                .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .to_ascii_lowercase();
            spellings.insert(format!("{command} <{name}>"));
        }
        for option in section(&sub, "Options:") {
            if !option.trim_start().starts_with('-') {
                continue;
            }
            let Some(flag) = option
                .split_whitespace()
                .find(|word| word.starts_with("--"))
            else {
                continue;
            };
            let flag = flag.trim_end_matches(',');
            if flag == "--help" || flag == "--version" {
                continue;
            }
            spellings.insert(format!("{command} {flag}"));
        }
    }
    spellings
}

/// Every tool, argument, resource and prompt the server advertises.
fn mcp_spellings() -> BTreeSet<String> {
    let mut spellings = BTreeSet::new();
    for (tool, schema) in brake::mcp::handlers::tool_schemas() {
        spellings.insert(tool.to_owned());
        for argument in schema["properties"]
            .as_object()
            .expect("a tool schema has properties")
            .keys()
        {
            spellings.insert(format!("{tool}.{argument}"));
        }
    }
    for (uri, _) in brake::mcp::handlers::RESOURCES {
        spellings.insert(format!("resource:{uri}"));
    }
    spellings.insert(format!("prompt:{}", brake::mcp::handlers::PROMPT_NAME));
    spellings
}

fn table(
    side: impl Fn(&brake::surface::Capability) -> &'static [&'static str],
) -> BTreeSet<String> {
    CAPABILITIES
        .iter()
        .flat_map(|capability| side(capability).iter().map(|s| (*s).to_owned()))
        .collect()
}

#[test]
fn every_cli_flag_is_in_the_capability_table() {
    let accepted = cli_spellings();
    assert!(
        accepted.contains("check --since"),
        "help parsing broke: {accepted:?}"
    );
    let listed = table(|capability| capability.cli);

    let unlisted: Vec<_> = accepted.difference(&listed).collect();
    assert!(
        unlisted.is_empty(),
        "the CLI accepts these, and src/surface.rs does not say what they map to \
         on MCP or why they have no counterpart: {unlisted:?}"
    );
    let stale: Vec<_> = listed.difference(&accepted).collect();
    assert!(
        stale.is_empty(),
        "src/surface.rs lists CLI spellings that no longer exist: {stale:?}"
    );
}

#[test]
fn every_mcp_argument_is_in_the_capability_table() {
    let advertised = mcp_spellings();
    let listed = table(|capability| capability.mcp);

    let unlisted: Vec<_> = advertised.difference(&listed).collect();
    assert!(
        unlisted.is_empty(),
        "the MCP server advertises these, and src/surface.rs does not say what they \
         map to on the CLI or why they have no counterpart: {unlisted:?}"
    );
    let stale: Vec<_> = listed.difference(&advertised).collect();
    assert!(
        stale.is_empty(),
        "src/surface.rs lists MCP spellings that no longer exist: {stale:?}"
    );
}

#[test]
fn a_capability_missing_from_a_surface_says_why() {
    for capability in CAPABILITIES {
        let one_sided = capability.cli.is_empty() || capability.mcp.is_empty();
        assert_eq!(
            one_sided,
            capability.absent.is_some(),
            "`{}`: a reason is required exactly when one surface lacks it",
            capability.name
        );
    }
}

#[test]
fn the_committed_capabilities_document_matches_the_table() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/capabilities.md");
    let generated = brake::surface::markdown();
    if std::env::var_os("BRAKE_BLESS").is_some() {
        fs::write(&path, &generated).expect("write docs/capabilities.md");
        return;
    }
    let committed = fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        committed.replace("\r\n", "\n"),
        generated.replace("\r\n", "\n"),
        "docs/capabilities.md is out of date — run `make docs`"
    );
}

/// The structural rule of design/09: an adapter turns input into a request and
/// renders a report. Deciding what is a break, and which rules run, is the
/// core's. An adapter that calls these assembles its own subset of the check,
/// which is how `check_change` fell behind the commit hook.
#[test]
fn no_adapter_assembles_its_own_check() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut adapters = vec![root.join("src/main.rs")];
    for entry in fs::read_dir(root.join("src/mcp")).expect("src/mcp") {
        adapters.push(entry.expect("entry").path());
    }
    let forbidden = [
        "crate::compare(",
        "brake::compare(",
        "crate::evaluate(",
        "brake::evaluate(",
        "crate::lint(",
        "brake::lint(",
        "compare::compare_contracts",
        "rules::evaluate",
        "rules::lint",
        "policy::attribute",
        "policy::apply",
        "apply_suppressions",
    ];
    for path in adapters {
        let source = fs::read_to_string(&path).expect("read adapter");
        for call in forbidden {
            assert!(
                !source.contains(call),
                "{} calls `{call}`. Route it through check::check or \
                 check::check_documents instead",
                path.display()
            );
        }
    }
}
