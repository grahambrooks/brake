# 09 — One core, three surfaces

## Decision

The library owns every operation. The CLI and the MCP server are adapters:
each turns its input into a `check::Options`, calls one library function, and
renders the `Report` it gets back. No adapter calls `compare`, `evaluate`,
`lint` or the consumer policies itself.

Four changes make that possible:

1. **`Options` can supply documents from memory.** `overrides` replaces files
   in the working tree, and `baseline_document` replaces the baseline. An
   unsaved draft is checked by the same `check::check` the commit hook runs.
2. **`check::check_documents`** checks two in-memory documents with no
   configuration. MCP `compare_contracts` and CLI `brake compare` both call it.
3. **Trust is part of the request.** `Options::trust` decides whether a
   declared generator may run, and the core enforces it. The MCP server asks
   for `Trust::Untrusted`.
4. **One finding shape.** `render::json::finding` is the JSON for one finding.
   The CLI's `--format json` and every MCP tool use it. MCP keeps its own
   envelope (`verdict`, `unverified`) from
   [04-mcp-interface.md](04-mcp-interface.md) §3.1.

A capability table in the library (`brake::surface::CAPABILITIES`) records,
for every CLI flag and every MCP argument, what it maps to on the other side or
why it has no counterpart. A test fails when either surface gains something the
table does not mention, and `docs/capabilities.md` is generated from it.

---

## Why

The CLI and `check_repository` went through `check::check`, which runs
everything. `check_change` and `compare_contracts` rebuilt a subset from the
library's primitives: `parse`, then `compare`, then `evaluate`, then
`attribute`. Every feature added to `check` then had to be added to the MCP
tools by hand, and the ones that were not became disagreements between the
answer an agent got and the answer the commit hook gave:

| Missing from `check_change` | Effect |
| --- | --- |
| Structural lint | A draft reported clean failed the hook. Fixed by hand in `3b8cf85`, which is how this was noticed |
| `[[contract.allow]]` suppressions | A suppressed finding was reported to the agent as a break |
| The consumer rules (`consumer-field-unmet` and the rest) | Only `affects` was attached; the rules themselves never ran |
| Cross-file `$ref` | The draft was parsed with the single-document resolver, so a split contract read as partial |

Patching each gap by hand fixes that gap and leaves the cause. The cause is
that `check::check` could only read files from disk, so a tool holding a draft
in memory had no way to call it.

The MCP output had drifted too. Its findings carried no file, line, column or
`note`, so an agent could not point at the line CI would annotate, and could
not see why a `triage` policy had downgraded a finding.

## Overrides

`Options::overrides` maps a repository-relative path to bytes. Every place
`check` reads the head side consults it first:

- the contract document itself;
- sibling documents reached by a cross-file `$ref`, through an overlay
  resolver;
- the demand pass, which binds consumers against the head contract.

The baseline side never consults it. A baseline comes from git or from a named
file, and an override of the working tree must not change what "before"
means. `Options::baseline_document` is the explicit way to supply a baseline
from memory. It is resolved with the `Isolated` origin, because an inline
document has no siblings brake could locate.

`check_change` then reads:

```rust
options.overrides.insert(contract.source.clone(), proposed);
check::check(repo_root, &config, &Scope::Paths(vec![contract.source.clone()]), &options)
```

`Scope::Paths` is deliberate. It is the scope the commit hook runs with, so
the agent's draft is checked under the hook's rules: no undeclared-pact sweep
over the whole tree, and no `consumer-surface-unused`, which only `analyze`
reports.

## A tool that cannot answer

`check` reports an unparseable document as a `contract-unreachable` finding,
because in CI that is a broken gate. An MCP tool reports it as a tool error,
because `design/04` §6 separates "the API broke" from "brake could not
answer". The adapter maps one to the other. That mapping is presentation,
which is what an adapter is for.

## Trust

`Options::drift` asks for generator drift. `Options::trust` says whether this
caller may have it. `Trust::Trusted` is the default, so a library caller that
sets `drift` gets what it asked for. The MCP server sets `Trust::Untrusted`,
and `check` then never spawns the generator, whatever `drift` says.

Previously the only guard was that the MCP adapter never set `drift`. That is
one edit away from failing open. The core now refuses on its own, and
`no_tool_call_can_execute_a_declared_generator` still defends the outcome.

## The capability table

The model is a parity test from another project with the same shape, one core
behind several front ends: write the table before the next surface drifts, and
make every exemption carry a reason. Its first run found four operations
reachable from only one front end. The CLI flags are read from the binary's own
`--help`, and the MCP arguments from `mcp::handlers::tool_schemas()`, so the
test compares what the surfaces actually accept rather than what the table
claims.

Exemptions are expected and fine. `--format` is presentation. `--drift` is
withheld from MCP on purpose. `init` writes files, which no MCP tool does. An
exemption is a decision written down; an unmapped flag is a decision nobody
made.

## Not done here

- **`who_consumes` still reads the contract itself.** It is a query over the
  consumer declarations rather than a check, and its answer does not depend on
  any rule. It is listed in the table.
- **`brake check` cannot take a draft on stdin.** The library can
  (`Options::overrides`), and an agent uses `check_change`. A human saves the
  file.
