# 08 — MCP tool contracts

## Decision

`brake` gains a fifth ingester, `format = "mcp"`, which reads a checked-in
snapshot of an MCP server's `tools/list` result and produces the same
`Contract` as every other format. Each tool is an endpoint, its `inputSchema`
is the request body and its `outputSchema` is the response. The comparator,
the rule catalogue, suppressions, levels and renderers are unchanged.

No new rule is added in this slice. Every break described below is already a
rule.

---

## Why this belongs in brake

An MCP server's tool list is an API contract, and it breaks consumers in the
same ways an HTTP API does. A tool disappears, an argument becomes required, an
argument's type narrows, or a field of `structuredContent` vanishes. Nothing
gates those changes today: MCP servers are versioned by whatever the release
script does, and the first sign of a break is an agent failing at run time.

The acid test from [01-thesis.md](01-thesis.md) still holds. A tool list is
useful to gate with no architecture model and no import graph, and the hard
part — JSON Schema subtyping — is already the comparator.

### Who actually breaks

The argument depends on this, so it is set out honestly.

An LLM client reads `tools/list` at the start of each session and adapts to
whatever it finds. **A model reading the schema fresh is not the consumer this
protects**, and claiming otherwise would manufacture exactly the confidence
this tool exists to refuse. The consumers that do break are the ones that
stored something about the tool list:

| Consumer | What it stored | What breaks it |
| --- | --- | --- |
| Permission allowlists (`mcp__server__tool` in Claude Code settings, and similar elsewhere) | The tool name | A rename or removal. The tool silently drops back to prompting, or a hook stops matching |
| Skills, prompts and slash commands that name a tool and its arguments | Name and argument shape | Removal, a new required argument, a narrowed type |
| Programmatic clients (scripts, CI jobs, other agents calling over the SDK) | The full input shape, and often a parsed `structuredContent` | Every request rule, and every response rule on `outputSchema` |
| Agent frameworks that cache tool definitions between sessions | The schema | Any break, until the cache is refreshed |

A session that has already started also holds the old list until the server
sends `notifications/tools/list_changed`, and many clients do not handle that
notification.

## The artifact

**A JSON file holding a `tools/list` result.** Either the bare result:

```json
{ "tools": [ { "name": "search_issues", "inputSchema": { "type": "object" } } ] }
```

or the full JSON-RPC response that carried it:

```json
{ "jsonrpc": "2.0", "id": 1, "result": { "tools": [ … ] } }
```

Both are accepted because both are what people actually save. There is no
standard MCP contract file. An MCP Bundle `manifest.json` lists tool names and
descriptions but not schemas, and the registry's `server.json` lists no tools
at all, so neither can carry a compatibility check.

### Where the snapshot comes from

**brake never starts the server.** Starting it would break constraint 2 (no
running service) and constraint 3 (one opt-in subprocess). The snapshot is
produced by the server's own build — a `--dump-tools` flag, a test that
serialises the tool router, or a CI step that calls `tools/list` once — and
committed like any other generated contract.

That makes it a generated artifact, and `brake check --drift` already exists
for those. A server that declares its dump command as the contract's
`generator` gets drift detection for free, under the same opt-in flag, with no
MCP-specific code.

## Mapping

| MCP | `Contract` |
| --- | --- |
| Tool `name` | Endpoint `CALL /tools/<name>` |
| `inputSchema` | The request body, under `*/*` |
| `outputSchema` | The response, under status key `result` |
| No `outputSchema` | No response. Unstructured `content` is not a contract |
| `title`, `description`, `annotations`, `icons`, `_meta` | Not modelled |

`inputSchema` and `outputSchema` are JSON Schema, which MCP defaults to the
2020-12 dialect — the same dialect OpenAPI 3.1 uses. The ingester reuses the
OpenAPI schema parser rather than copying it, so every construct it models
(type arrays, `prefixItems`, `oneOf`, `allOf`, `additionalProperties` schemas)
is modelled here too, and every construct it defers is deferred here too.

**A `$ref` is resolved against the schema that contains it**, not against the
file. In JSON Schema `#/$defs/Filter` inside a tool's `inputSchema` means that
schema's own `$defs`, and resolving it against the snapshot's root would find
nothing, or the wrong thing. A remote `$ref` is refused, as everywhere else.

### What the existing rules then catch

| Change | Rule |
| --- | --- |
| Tool removed or renamed | `endpoint-removed` |
| Required argument added | `param-added-required` |
| Optional argument became required | `param-type-narrowed` |
| Argument removed | `param-type-narrowed` (no longer accepted) |
| Argument type narrowed, enum value dropped, bound tightened | `param-type-narrowed` |
| `additionalProperties` tightened to `false` | `param-type-narrowed` |
| `structuredContent` field removed, or made optional | `response-field-removed`, `response-field-optional` |
| `structuredContent` field type changed | `response-type-changed` |
| `outputSchema` removed | `response-status-removed` |
| Tool added, optional argument added, output field added | Nothing below `strict` |

An argument is a field of the request body, not a parameter, so a newly
required one routes through `param-type-narrowed` rather than
`param-became-required`. An OpenAPI request body gets the same routing.

### Partial snapshots and malformed tools

`contract-partial` is raised from a `TypeRef::Unknown` inside an endpoint's
types. An entry in `Contract::unmodelled` with no endpoint to attach to reaches
no finding at all. So a gap is either attached to a tool, or it is an error.

- **A paginated snapshot is refused.** A result carrying `nextCursor` is one
  page of the list. A tool absent from it may only be on the next page, so no
  removal can be judged from it. There is no tool to attach that gap to, and
  marking it partial would print nothing. Concatenate the pages before
  committing.
- **A tool with no `name` is refused**, because it cannot be compared with
  anything.
- **A duplicate tool name is refused**, because which of the two is "the" tool
  is undefined.
- **A tool with no `inputSchema` keeps its endpoint**, so its removal is still
  checked. Its request is `Unknown`, so `contract-partial` names it.
- Every schema construct the OpenAPI parser already defers is deferred here too.

## Deliberately not built

- **Descriptions are not compared.** A changed description can change how a
  model uses a tool, which is real. But brake cannot tell a clarification from
  a semantic change, and a rule that fires on every edited sentence is a rule
  that gets the hook uninstalled. If this becomes a rule it will be advisory,
  and only after evidence shows which description changes break callers.
- **Annotations are not compared.** `readOnlyHint` flipping to `false`, or
  `destructiveHint` flipping to `true`, changes what a client may auto-approve.
  That is a trust change rather than a wire break. It is the strongest
  candidate for a later rule, and it would be a new rule rather than a reuse.
- **Prompts and resources are out of scope** for this slice. `prompts/list`
  has arguments that could map the same way. Resource templates are URI
  templates, and their compatibility rules are not settled.
- **No deprecation signal.** MCP has no `deprecated` field on a tool, so
  `removed-without-deprecation` fires on every tool removal. The remedy for an
  MCP server is to keep the old name as an alias for a release. Reading a
  deprecation marker out of `_meta` would invent a convention, and brake does
  not invent conventions.
- **No live `tools/list` call, ever.** See above.

## Compatibility note

`ContractFormat` gains a variant, `Mcp`. It is public API (`brake::Format`),
so an exhaustive `match` downstream — forge's — needs an arm. Say so in the
release notes.
