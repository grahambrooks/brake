# The MCP server

```sh
brake mcp .          # stdio, rooted at this repository
```

`brake mcp` serves the same ruleset over the Model Context Protocol, so a
coding agent learns that renaming a response field breaks consumers **while it
is drafting the change** rather than when the hook rejects it. Every tool
handler calls the same synchronous library functions the CLI calls, so there is
exactly one implementation of every verdict.

## Enabling it

The released binaries include the MCP server. From source it is behind a
non-default feature:

```sh
cargo install brake --features mcp
```

The feature is not on by default because the server needs an async runtime, and
a crate that is otherwise synchronous should not pay for one unless it is asked
— `forge` takes the library with `default-features = false` and never sees
tokio. `brake --help` lists `mcp` on every build regardless, and a build without
the feature tells you how to get it rather than pretending the command does not
exist.

```jsonc
// claude_desktop_config.json, or any MCP client
{
  "mcpServers": {
    "brake": { "command": "brake", "args": ["mcp", "/path/to/your/repo"] }
  }
}
```

For Claude Code:

```sh
claude mcp add brake -- brake mcp /path/to/your/repo
```

The path argument is the repository root. It is where `brake.toml` is read
from, and it is the only directory the server will resolve a contract in.
`--as-of YYYY-MM-DD` evaluates suppression expiry at a fixed date.

## Tools

### `check_change`

> Check whether a proposed API contract would break its consumers, before
> writing it.

| Argument | Required | Meaning |
| --- | --- | --- |
| `format` | yes | `openapi`, `proto` or `graphql` |
| `proposed` | yes | The full proposed document, **as text** |
| `contract` | no | Which `[[contract]]` to compare against. Required only when more than one is configured |
| `baseline_document` | no | An inline baseline — supply it to check a change with no `brake.toml` and no repository |
| `compatibility` | no | `wire`, `wire-json`, `surface`, `strict`. Defaults to the configured level |

`proposed` is text rather than a path so a draft that has not been written to
disk can still be checked. That is the whole point of the tool: the cheapest
moment to reconsider a rename is before it exists.

With more than one contract configured and no `contract` given, it refuses to
guess rather than picking one.

### `compare_contracts`

> Compare two API contract documents and report what would break a consumer.

| Argument | Required | Meaning |
| --- | --- | --- |
| `format` | yes | `openapi`, `proto` or `graphql` |
| `base` | yes | The previous document, as text |
| `head` | yes | The new document, as text |
| `compatibility` | no | Defaults to `wire-json` |

Needs **no configuration and no repository**, so it works on a repository the
agent has never seen — reviewing a diff, or a document pasted into the
conversation.

### `who_consumes`

> Name the declared consumers of an endpoint or a field, with the interaction
> that declares it.

| Argument | Required | Meaning |
| --- | --- | --- |
| `contract` | no | Required only when more than one is configured |
| `endpoint` | no | `GET /payments/{id}`, or just the path. Omit for every endpoint |
| `field` | no | A response field, request field, status code, parameter or media type |

Call this **before** proposing a removal or a rename: it answers who breaks
while the edit can still be reconsidered. An empty answer means nobody
*declared* it — not that nobody uses it. See [Consumer demand](consumers.md).

### `explain_rule`

> Explain why a rule exists, what it catches, and the ways to make the change
> it flags without breaking a consumer.

| Argument | Required | Meaning |
| --- | --- | --- |
| `rule` | no | A rule id, e.g. `response-field-removed`. Omit to list the whole catalogue |

### `check_repository`

> Check every API contract configured in this repository against its baseline.

| Argument | Required | Meaning |
| --- | --- | --- |
| `contracts` | no | Restrict the run to these contracts by name |
| `compatibility` | no | Override the configured level |

Answers "what is our compatibility posture?" rather than "is this change safe?".

## Resources

| URI | Contents |
| --- | --- |
| `brake://rules` | Every rule, with severity, the level it fires from, and the ways out |
| `brake://strategies` | The evolution strategies, each with what it costs. Read this *before* drafting a change |
| `brake://config` | The resolved `brake.toml`, including what each contract gates — "this change against the trunk" or "the delta since the last release" — stated rather than left to be inferred from the baseline kind |
| `brake://consumers` | The declared consumers of each contract, with the file and content digest each came from |

A URI outside that set is refused rather than treated as a path. This server
reads contracts, not arbitrary files.

## Prompt

`review-api-change` takes `format`, `base`, `head` and an optional
`compatibility`, runs the comparison, and returns a review framed from the
consumer's side with the findings and their remediation strategies already
gathered. The framing is `brake`'s rather than the agent's, which is most of
the value: the difference between "here are some warnings" and "here is what a
consumer of this API experiences, and here are the ways to give them what you
want without that".

## How to read a verdict

A tool that finds breakage returns an **answer**, not an error — breakage is
the thing being asked about. A tool that could not answer returns an error: an
unparseable document, an unresolvable baseline, an ambiguous contract
reference. The distinction matters to an agent deciding whether to retry.

Nothing is ever reported clean that could not be verified. An unmodelled
construct comes back as `unavailable` with the construct named.

## Trust posture

The server exposes **no way to run a declared generator command**. `--drift`
executes a command out of a config file, and a tool that honoured it would hand
arbitrary command execution to anything that can write `brake.toml` — including
the repository the agent was just asked to review. Drift stays a CLI concern,
run by a person or a CI job that chose to.

That boundary is not a convention: `tests/mcp.rs::no_tool_call_can_execute_a_declared_generator`
fails if it is ever crossed.

Nothing in the server opens a socket other than the stdio transport, and no
input is dereferenced. A URL in a document is data.

## Structure

- `src/mcp/handlers.rs` is synchronous and transport-free. Every tool is a
  plain function over a `Context`, which is why the tools are tested without a
  protocol.
- `src/mcp/server.rs` is the `rmcp` adapter and the only file that knows about
  async.

Keep the split. It is what makes the tool surface testable.

See [design/04-mcp-interface.md](../design/04-mcp-interface.md) for the
specification and the reasoning behind the trust boundary.
