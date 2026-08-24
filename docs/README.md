<img src="assets/brake-logo.svg" alt="" align="right" width="108" height="108">

# brake documentation

`brake` checks API contracts for backward compatibility and fails the commit
when a change would break a consumer. One ruleset, four formats, two scopes —
`brake check` on a change at commit time, `brake analyze` over the whole
repository in CI.

Start with **[Getting started](getting-started.md)** if you have never run it.

## Guides

| Guide | What it covers |
| --- | --- |
| [Getting started](getting-started.md) | Install, `brake init`, your first finding, and how to read one |
| [Configuration](configuration.md) | Every key in `brake.toml`: contracts, baselines, compatibility levels, suppressions, drift |
| [Contract formats](formats.md) | OpenAPI, protobuf, GraphQL and AsyncAPI — what each models, and contracts that span several files |
| [Consumer demand](consumers.md) | Declaring pacts, GraphQL operations and manifests so a finding names *who* it breaks |
| [CI and hooks](ci.md) | The pre-commit hook, GitHub Actions, SARIF, the commit gate and the release gate |
| [MCP server](mcp.md) | `brake mcp` — the same ruleset consulted by a coding agent while it drafts a change |
| [Agent skills](agent-skills.md) | The skills in `.claude/skills/`, what each is for, and how to install them elsewhere |

## Reference

| Reference | What it is |
| --- | --- |
| [Rule catalogue](rules.md) | Every rule, its severity, the level it fires from, and the ways out. Generated from `src/rules/catalogue.rs` |
| [User guide test cases](user-guide-test-cases.md) | The canonical example matrix, exercised by `tests/user_guide_cases.rs` |
| [Design specification](../design/) | Why the tool is shaped this way, and what is deliberately not being built |

## The three claims everything else rests on

1. **Hermetic.** No network, no toolchain, no running service. `brake` reads
   files. That is why it can live in a pre-commit hook, and why a `$ref`
   resolving to a URL is refused rather than fetched.
2. **Deterministic.** Same inputs, same verdict, same bytes — on your laptop,
   on CI, on a colleague's machine. Enumerated in
   [design/02-contract-gates.md](../design/02-contract-gates.md) §6.1 and
   defended by `tests/self_defence.rs`.
3. **Honest about what it cannot tell.** A construct `brake` cannot model is
   reported as `unavailable` and named. A clean result it cannot justify is
   worse than no result, because it manufactures confidence.
