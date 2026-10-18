# Contract formats

Five ingesters, one `Contract` model, one comparator. A rule is written once
and fires on every format that can express the thing it is about — which is why
`endpoint-removed` catches a deleted OpenAPI path, a deleted protobuf RPC, a
deleted GraphQL query field, a deleted AsyncAPI channel operation and a deleted
MCP tool, without five implementations.

| `format` | Reads | Version support |
| --- | --- | --- |
| `openapi` | `.yaml` / `.json` OpenAPI documents | 3.0 and 3.1 (JSON Schema 2020-12) |
| `proto` | `.proto` files | proto3 |
| `graphql` | `.graphql` / `.graphqls` / `.gql` SDL | Schema documents |
| `asyncapi` | `.yaml` / `.json` AsyncAPI documents | 2.x and 3.x |
| `mcp` | `.json` snapshots of an MCP server's `tools/list` result | Tool schemas in JSON Schema 2020-12 |

`brake init` picks the format by **parsing**, never by filename. A file counts
as a contract only if an ingester can actually read it.

## OpenAPI

The most complete of the four. Endpoints are `METHOD /path/{template}`, and
request bodies, parameters, responses, status codes, media types and security
requirements are all modelled.

3.1 brings JSON Schema 2020-12, and these constructs are modelled rather than
deferred:

| Construct | Modelled as | What that catches |
| --- | --- | --- |
| `type: ["string", "null"]` | A union, or a nullable scalar when one arm is `null` | Dropping an accepted type from a union |
| `type: ["integer", "string"]` | A union of both | Narrowing a request that used to take either |
| `prefixItems: [A, B]` | An ordered tuple, with `items` as the tail | Changing the type at one index, or shortening the tuple |
| `discriminator` with `mapping` | The mapping, alongside the `oneOf` variants | Removing a mapping key, or repointing one at another schema |
| `additionalProperties: {…}` | An open object — a schema is not `false` | A response that stops admitting extra members |

Those feed the rules that already exist — `response-type-changed`,
`param-type-narrowed` — rather than adding new ones. A tuple whose element type
changed is a type change; it does not need a rule of its own.

## Protobuf

Messages and enums are modelled by **field number**, which is what is actually
on the wire. That is why `field-number-changed` and `field-renamed` are
different rules: renaming a field at the same number breaks generated code,
while renumbering a field at the same name silently misreads data.

`reserved` numbers and names are read. A field or enum value that reuses a
reserved number range or a reserved name is reported as `contract-partial` and
named, because `brake` will not claim to have verified a message whose numbering
contradicts its own reservations.

> Enforcing that every *removal* is accompanied by a `reserved` declaration is
> specified in
> [design/06-architectural-evolution.md](../design/06-architectural-evolution.md)
> §2.1 and is not built. The rules it names, `proto-field-unreserved` and
> `proto-enum-value-unreserved`, do not exist yet —
> [docs/rules.md](rules.md) is the list that does.

## GraphQL

A schema's query and mutation fields become endpoints — `QUERY /query/<field>`,
`MUTATION /mutation/<field>` — so the same comparator that handles HTTP handles
GraphQL with no format-specific branch. Consumer operation documents produce
routes of exactly the same shape, which is what lets a `.graphql` query be
verified against a schema through the ordinary join. See
[Consumer demand](consumers.md).

## AsyncAPI

Event-driven contracts break in the same ways HTTP ones do: a field vanishes
from a payload, a type narrows, a header becomes required. AsyncAPI 2.x and 3.x
map onto the same model.

| AsyncAPI | `Contract` |
| --- | --- |
| Channel or topic (`orders.v1`) | The endpoint path |
| 2.x `publish` / 3.x `send` | Method `PUBLISH` |
| 2.x `subscribe` / 3.x `receive` | Method `SUBSCRIBE` |
| Message payload | The payload schema, under `application/json` |

**Direction is what makes the rules correct**, and it is the one thing worth
understanding before turning this on:

- **`PUBLISH`** — your service produces the event. The payload is modelled the
  way an HTTP *response* is: consumers read it, so adding an optional field is
  safe and removing one is a break.
- **`SUBSCRIBE`** — your service consumes the event. The payload is modelled the
  way an HTTP *request body* is: producers send it, so requiring a new field is
  a break and accepting more is safe.

Get the direction wrong in the document and `brake` will faithfully check the
wrong variance, so it is worth reading the operation keywords once.

## MCP tool lists

An MCP server's tools are an API: a tool disappears, an argument becomes
required, a field of `structuredContent` vanishes. `format = "mcp"` reads a
committed snapshot of the server's `tools/list` result — the bare result, or
the JSON-RPC response that carried it:

```json
{
  "tools": [
    {
      "name": "search_issues",
      "inputSchema": {
        "type": "object",
        "properties": { "query": { "type": "string" } },
        "required": ["query"]
      }
    }
  ]
}
```

| MCP | `Contract` |
| --- | --- |
| Tool `name` | Endpoint `CALL /tools/<name>` |
| `inputSchema` | The request body |
| `outputSchema` | The response, under `result` |

**`brake` never starts the server.** The snapshot comes from the server's own
build: a dump flag, a test that serialises the tool list, or a CI step. Declare
that command as the contract's `generator` and `brake check --drift` will tell
you when the committed snapshot is stale.

What it catches, through the rules that already exist:

- A tool removed or renamed is `endpoint-removed`.
- A new required argument is `param-added-required`.
- An argument removed, narrowed, or made required is `param-type-narrowed`.
- A `structuredContent` field removed or retyped is `response-field-removed` or
  `response-type-changed`.
- Dropping `outputSchema` is `response-status-removed`.

Some things are not compared, on purpose:

- **Descriptions and annotations.** A changed description can change how a
  model uses a tool, but `brake` cannot tell a clarification from a semantic
  change.
- **Prompts and resources.**

Two limits are worth knowing:

- **A model reading the tool list fresh adapts to it.** The consumers this
  protects are the ones that stored something: permission allowlists naming
  `mcp__server__tool`, skills and prompts that name tools and arguments, and
  programmatic clients.
- **MCP has no deprecation marker on a tool**, so `removed-without-deprecation`
  fires on every removal. Keep the old name as an alias for a release.

A snapshot carrying `nextCursor` is one page of a paginated list and is
refused. Concatenate the pages before committing it. The reasoning is in
[design/08-mcp-tool-contracts.md](../design/08-mcp-tool-contracts.md).

## Contracts that span several files

Any of the document formats may `$ref` into a sibling file:

```yaml
        schema:
          $ref: "./common/models.yaml#/components/schemas/Payment"
```

The reference is resolved with no network request and no bundler, under three
rules — the directory boundary, the refusal of URLs, and reading the baseline's
siblings from the baseline's own revision. They are set out in
[Configuration](configuration.md#contracts-that-span-several-files).

From the library, supply your own source of documents:

```rust
use brake::{Format, InMemoryResolver, parse_with_resolver};

let resolver = InMemoryResolver::new()
    .with_document("common/models.yaml", shared_bytes);
let contract = parse_with_resolver(Format::Openapi, "api/openapi.yaml", bytes, &resolver)?;
```

Paths handed to a resolver are relative to the **contract document's own
directory**, never to the repository root — and never derived from the `source`
argument, which is a display label and for a git baseline reads `rev:HEAD`.
`FileSystemResolver` reads a directory, `InMemoryResolver` is for tests and
bundles, and `SingleDocumentResolver` refuses everything, which is what
`brake::parse` uses.

## When a construct cannot be modelled

Every ingester records what it could not read, and those become
`contract-partial` — named, at `warning`, never silence. A path containing a
construct `brake` does not model is an unverified path, and reporting it as
clean would manufacture exactly the confidence the tool exists to refuse.
