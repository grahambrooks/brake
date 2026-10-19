# Capabilities

<!-- Generated from src/surface.rs by `make docs`. Do not edit. -->

What each surface can do. The library owns every operation; the CLI and the MCP server are adapters over it, so a capability missing from one of them is missing on purpose, and the reason is given. See [design/09-one-core-three-surfaces.md](../design/09-one-core-three-surfaces.md).

| Capability | Library | CLI | MCP |
| --- | --- | --- | --- |
| Check the contracts a change touches | `check::check with Scope::Paths` | `check`<br>`check <paths>` | `check_repository.paths` |
| Check every configured contract | `check::check with Scope::All` | `analyze` | `check_repository` |
| Check what changed since a ref | `check::check with Scope::Since` | `check --since` | `check_repository.since` |
| Check an unsaved draft against a configured contract | `check::check with Options::overrides` | — | `check_change`<br>`check_change.proposed` |
| Compare two documents with no configuration | `check::check_documents` | `compare`<br>`compare <head>` | `compare_contracts`<br>`compare_contracts.head` |
| Supply the baseline as a document | `Options::baseline_document, check::check_documents` | `compare <base>` | `check_change.baseline_document`<br>`compare_contracts.base` |
| Say which format a document is | `ContractFormat::parse` | `compare --contract-format` | `check_change.format`<br>`compare_contracts.format` |
| Restrict to named contracts | `Options::only` | `check --contract`<br>`analyze --contract`<br>`diff --contract`<br>`consumers --contract` | `check_repository.contracts`<br>`check_change.contract`<br>`who_consumes.contract` |
| Restrict to named consumers | `Options::consumers` | `check --consumer`<br>`analyze --consumer`<br>`consumers --consumer` | `check_repository.consumers` |
| Override the configured baseline | `Options::baseline, Baseline::from_flag` | `check --baseline`<br>`diff --baseline` | `check_repository.baseline` |
| Override the compatibility level | `Options::compatibility, Compatibility::parse` | `check --compatibility`<br>`analyze --compatibility`<br>`compare --compatibility` | `check_change.compatibility`<br>`compare_contracts.compatibility`<br>`check_repository.compatibility` |
| Choose the repository and its brake.toml | `check::check(repo_root, config, ..)` | `analyze <path>`<br>`check --config`<br>`analyze --config`<br>`diff --config`<br>`consumers --config` | — |
| Evaluate suppression expiry against a date | `Options::as_of` | `check --as-of`<br>`analyze --as-of` | — |
| Detect drift from a declared generator | `Options::drift, gated by Options::trust` | `check --drift`<br>`analyze --drift` | — |
| Choose the severity that fails the run | `Report::exit_code, Severity::parse` | `check --severity`<br>`analyze --fail-on`<br>`compare --severity` | — |
| Choose the output format | `render::{text, json, sarif, github, gitlab}` | `check --format`<br>`analyze --format`<br>`diff --format`<br>`compare --format`<br>`consumers --format` | — |
| Report every change without failing | `check::check at Compatibility::Strict` | `diff` | — |
| Report who consumes what | `demand::inventory::build` | `consumers` | `who_consumes`<br>`resource:brake://consumers` |
| Ask who consumes one endpoint or field | `demand::load::load, then bind` | — | `who_consumes.endpoint`<br>`who_consumes.field` |
| Explain a rule | `rules::catalogue` | `explain`<br>`explain <rule>` | `explain_rule`<br>`explain_rule.rule`<br>`resource:brake://rules` |
| List the evolution strategies | `rules::strategies::STRATEGIES` | — | `resource:brake://strategies` |
| Read the resolved configuration | `Config` | — | `resource:brake://config` |
| Review a change on consumers' behalf | `check::check_documents` | — | `prompt:review-api-change` |
| Discover contracts and write brake.toml | `init::discover, init::render_config` | `init`<br>`init <path>`<br>`init --dry-run`<br>`init --force` | — |
| Serve the ruleset over MCP | `mcp::serve_stdio` | `mcp`<br>`mcp <path>`<br>`mcp --as-of` | — |

## Why a surface lacks something

- **Check an unsaved draft against a configured contract.** The CLI reads files: save the draft and run `brake check`. The library takes the draft in `Options::overrides`.
- **Choose the repository and its brake.toml.** The MCP server is started in one repository (`brake mcp <path>`) and reads its brake.toml. A tool call cannot point it at another tree.
- **Evaluate suppression expiry against a date.** Set once, when the server starts: `brake mcp --as-of`.
- **Detect drift from a declared generator.** Withheld on purpose. A tool call is untrusted input, and the core refuses to spawn the generator for `Trust::Untrusted` (design/04-mcp-interface.md §5.1).
- **Choose the severity that fails the run.** An exit code is a process concept. A tool returns `verdict` and every finding's severity, and the caller decides.
- **Choose the output format.** Presentation. A tool always answers in JSON, built from the same `render::json::finding` the CLI's `--format json` uses.
- **Report every change without failing.** A review view for a person. An agent asks `check_repository` with `compatibility: strict`.
- **Ask who consumes one endpoint or field.** `brake consumers` prints the whole inventory, which a person reads. An agent asks about the one field it is about to change.
- **List the evolution strategies.** `brake explain <rule>` prints each rule's strategies. The whole list is for an agent to read before drafting a change.
- **Read the resolved configuration.** brake.toml is a file a person reads directly.
- **Review a change on consumers' behalf.** A prompt frames findings for an agent. `brake compare` gives a person the same findings.
- **Discover contracts and write brake.toml.** It writes files, which no MCP tool does.
- **Serve the ruleset over MCP.** It is the MCP server.
