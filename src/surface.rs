//! What each surface can do, and why a surface lacks what it lacks.
//!
//! The library owns every operation and the CLI and MCP server are adapters
//! (`design/09-one-core-three-surfaces.md`). This table is how they stay in
//! step: `tests/surface_parity.rs` reads the flags the binary actually accepts
//! and the arguments the MCP server actually advertises, and fails on any that
//! no entry mentions. `docs/capabilities.md` is generated from it.
//!
//! An entry absent from one surface carries the reason. An exemption is a
//! decision written down; an unmapped flag is a decision nobody made.

/// One capability, as each surface reaches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capability {
    /// What it does, in a reader's words.
    pub name: &'static str,
    /// The library entry point every surface goes through.
    pub library: &'static str,
    /// CLI spellings: `check`, `check --since`, `compare <base>`.
    pub cli: &'static [&'static str],
    /// MCP spellings: `check_repository`, `check_repository.since`,
    /// `resource:brake://rules`, `prompt:review-api-change`.
    pub mcp: &'static [&'static str],
    /// Why one surface does not have it. Required when either list is empty.
    pub absent: Option<&'static str>,
}

pub static CAPABILITIES: &[Capability] = &[
    Capability {
        name: "Check the contracts a change touches",
        library: "check::check with Scope::Paths",
        cli: &["check", "check <paths>"],
        mcp: &["check_repository.paths"],
        absent: None,
    },
    Capability {
        name: "Check every configured contract",
        library: "check::check with Scope::All",
        cli: &["analyze"],
        mcp: &["check_repository"],
        absent: None,
    },
    Capability {
        name: "Check what changed since a ref",
        library: "check::check with Scope::Since",
        cli: &["check --since"],
        mcp: &["check_repository.since"],
        absent: None,
    },
    Capability {
        name: "Check an unsaved draft against a configured contract",
        library: "check::check with Options::overrides",
        cli: &[],
        mcp: &["check_change", "check_change.proposed"],
        absent: Some(
            "The CLI reads files: save the draft and run `brake check`. The \
             library takes the draft in `Options::overrides`.",
        ),
    },
    Capability {
        name: "Compare two documents with no configuration",
        library: "check::check_documents",
        cli: &["compare", "compare <head>"],
        mcp: &["compare_contracts", "compare_contracts.head"],
        absent: None,
    },
    Capability {
        name: "Supply the baseline as a document",
        library: "Options::baseline_document, check::check_documents",
        cli: &["compare <base>"],
        mcp: &["check_change.baseline_document", "compare_contracts.base"],
        absent: None,
    },
    Capability {
        name: "Say which format a document is",
        library: "ContractFormat::parse",
        cli: &["compare --contract-format"],
        mcp: &["check_change.format", "compare_contracts.format"],
        absent: None,
    },
    Capability {
        name: "Restrict to named contracts",
        library: "Options::only",
        cli: &[
            "check --contract",
            "analyze --contract",
            "diff --contract",
            "consumers --contract",
        ],
        mcp: &[
            "check_repository.contracts",
            "check_change.contract",
            "who_consumes.contract",
        ],
        absent: None,
    },
    Capability {
        name: "Restrict to named consumers",
        library: "Options::consumers",
        cli: &[
            "check --consumer",
            "analyze --consumer",
            "consumers --consumer",
        ],
        mcp: &["check_repository.consumers"],
        absent: None,
    },
    Capability {
        name: "Override the configured baseline",
        library: "Options::baseline, Baseline::from_flag",
        cli: &["check --baseline", "diff --baseline"],
        mcp: &["check_repository.baseline"],
        absent: None,
    },
    Capability {
        name: "Override the compatibility level",
        library: "Options::compatibility, Compatibility::parse",
        cli: &[
            "check --compatibility",
            "analyze --compatibility",
            "compare --compatibility",
        ],
        mcp: &[
            "check_change.compatibility",
            "compare_contracts.compatibility",
            "check_repository.compatibility",
        ],
        absent: None,
    },
    Capability {
        name: "Choose the repository and its brake.toml",
        library: "check::check(repo_root, config, ..)",
        cli: &[
            "analyze <path>",
            "check --config",
            "analyze --config",
            "diff --config",
            "consumers --config",
        ],
        mcp: &[],
        absent: Some(
            "The MCP server is started in one repository (`brake mcp <path>`) and \
             reads its brake.toml. A tool call cannot point it at another tree.",
        ),
    },
    Capability {
        name: "Evaluate suppression expiry against a date",
        library: "Options::as_of",
        cli: &["check --as-of", "analyze --as-of"],
        mcp: &[],
        absent: Some("Set once, when the server starts: `brake mcp --as-of`."),
    },
    Capability {
        name: "Detect drift from a declared generator",
        library: "Options::drift, gated by Options::trust",
        cli: &["check --drift", "analyze --drift"],
        mcp: &[],
        absent: Some(
            "Withheld on purpose. A tool call is untrusted input, and the core \
             refuses to spawn the generator for `Trust::Untrusted` \
             (design/04-mcp-interface.md §5.1).",
        ),
    },
    Capability {
        name: "Choose the severity that fails the run",
        library: "Report::exit_code, Severity::parse",
        cli: &[
            "check --severity",
            "analyze --fail-on",
            "compare --severity",
        ],
        mcp: &[],
        absent: Some(
            "An exit code is a process concept. A tool returns `verdict` and every \
             finding's severity, and the caller decides.",
        ),
    },
    Capability {
        name: "Choose the output format",
        library: "render::{text, json, sarif, github, gitlab}",
        cli: &[
            "check --format",
            "analyze --format",
            "diff --format",
            "compare --format",
            "consumers --format",
        ],
        mcp: &[],
        absent: Some(
            "Presentation. A tool always answers in JSON, built from the same \
             `render::json::finding` the CLI's `--format json` uses.",
        ),
    },
    Capability {
        name: "Report every change without failing",
        library: "check::check at Compatibility::Strict",
        cli: &["diff"],
        mcp: &[],
        absent: Some(
            "A review view for a person. An agent asks `check_repository` with \
             `compatibility: strict`.",
        ),
    },
    Capability {
        name: "Report who consumes what",
        library: "demand::inventory::build",
        cli: &["consumers"],
        mcp: &["who_consumes", "resource:brake://consumers"],
        absent: None,
    },
    Capability {
        name: "Ask who consumes one endpoint or field",
        library: "demand::load::load, then bind",
        cli: &[],
        mcp: &["who_consumes.endpoint", "who_consumes.field"],
        absent: Some(
            "`brake consumers` prints the whole inventory, which a person reads. \
             An agent asks about the one field it is about to change.",
        ),
    },
    Capability {
        name: "Explain a rule",
        library: "rules::catalogue",
        cli: &["explain", "explain <rule>"],
        mcp: &[
            "explain_rule",
            "explain_rule.rule",
            "resource:brake://rules",
        ],
        absent: None,
    },
    Capability {
        name: "List the evolution strategies",
        library: "rules::strategies::STRATEGIES",
        cli: &[],
        mcp: &["resource:brake://strategies"],
        absent: Some(
            "`brake explain <rule>` prints each rule's strategies. The whole list is \
             for an agent to read before drafting a change.",
        ),
    },
    Capability {
        name: "Read the resolved configuration",
        library: "Config",
        cli: &[],
        mcp: &["resource:brake://config"],
        absent: Some("brake.toml is a file a person reads directly."),
    },
    Capability {
        name: "Review a change on consumers' behalf",
        library: "check::check_documents",
        cli: &[],
        mcp: &["prompt:review-api-change"],
        absent: Some(
            "A prompt frames findings for an agent. `brake compare` gives a person \
             the same findings.",
        ),
    },
    Capability {
        name: "Discover contracts and write brake.toml",
        library: "init::discover, init::render_config",
        cli: &["init", "init <path>", "init --dry-run", "init --force"],
        mcp: &[],
        absent: Some("It writes files, which no MCP tool does."),
    },
    Capability {
        name: "Serve the ruleset over MCP",
        library: "mcp::serve_stdio",
        cli: &["mcp", "mcp <path>", "mcp --as-of"],
        mcp: &[],
        absent: Some("It is the MCP server."),
    },
];

/// `docs/capabilities.md`, generated so it cannot drift from the table.
#[must_use]
pub fn markdown() -> String {
    let mut out = String::from(
        "# Capabilities\n\n\
         <!-- Generated from src/surface.rs by `make docs`. Do not edit. -->\n\n\
         What each surface can do. The library owns every operation; the CLI and \
         the MCP server are adapters over it, so a capability missing from one \
         of them is missing on purpose, and the reason is given. See \
         [design/09-one-core-three-surfaces.md](../design/09-one-core-three-surfaces.md).\n\n\
         | Capability | Library | CLI | MCP |\n\
         | --- | --- | --- | --- |\n",
    );
    let cell = |spellings: &[&str]| {
        if spellings.is_empty() {
            "—".to_owned()
        } else {
            spellings
                .iter()
                .map(|spelling| format!("`{spelling}`"))
                .collect::<Vec<_>>()
                .join("<br>")
        }
    };
    for capability in CAPABILITIES {
        out.push_str(&format!(
            "| {} | `{}` | {} | {} |\n",
            capability.name,
            capability.library,
            cell(capability.cli),
            cell(capability.mcp),
        ));
    }
    out.push_str("\n## Why a surface lacks something\n\n");
    for capability in CAPABILITIES {
        if let Some(reason) = capability.absent {
            out.push_str(&format!("- **{}.** {reason}\n", capability.name));
        }
    }
    out
}
