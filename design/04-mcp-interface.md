# 04 — MCP interface

**Design only; not implemented.** This document specifies an MCP server that
exposes the existing ruleset to a coding agent, so that a compatibility break
is caught while an API is being edited rather than when it is committed.

The thesis is [01-thesis.md](01-thesis.md); the ruleset this exposes is
[02-contract-gates.md](02-contract-gates.md). Nothing here adds a rule, changes
a verdict, or relaxes a guarantee.

---

## 1. Why

`brake check` runs at the moment of commit, which is the last moment the change
can be stopped and the worst moment to learn about it. The work is already
done; the finding arrives as an obstacle.

An agent editing an OpenAPI file is in a different position. It has the intent
in hand and has not written the change yet, and it is exactly the kind of
consumer that will confidently ship `customer_id` → `customerId` because
nothing told it not to. The ruleset that blocks that at commit time is more
useful one step earlier.

> **The MCP interface is the same gate, consulted earlier.** It is a delivery
> channel for the existing ruleset, not a new subject.

That framing is the constraint that keeps this from becoming a second product.
If a tool here would need a rule that `brake check` does not have, it does not
belong here either.

### The acid test, applied again

[01-thesis.md](01-thesis.md) rejected building contract gating inside forge by
asking whether it was useful with no architecture model. The same question:
*is this useful to an agent with no brake.toml and no repository?*

Mostly yes — `compare_contracts` and the rule catalogue work on two documents
and nothing else, which is the case an agent hits when reviewing a diff in a
repository it has not configured. That is the argument for the tool surface
below being usable without configuration, and for configuration being an
enrichment rather than a prerequisite.

---

## 2. What this does not do

Recorded here for the same reason [01-thesis.md](01-thesis.md) records its
exclusions: so a later session does not "complete" something cut on purpose.

- **It does not design APIs.** brake has no opinion on whether an endpoint
  should exist, be paginated, or be named well. `vacuum` and `spectral` lint
  style; brake reports what breaks a consumer. An advice interface makes the
  boundary *more* tempting to cross, not less.
- **It does not generate advice.** It returns the catalogued evolution
  strategies of [02-contract-gates.md](02-contract-gates.md) §5.7, bound to the
  specific field or endpoint that triggered the finding. Deterministic text
  keyed to a rule is something brake can stand behind; prose invented per-call
  is not.
- **It does not choose between them.** Which strategy fits depends on whether
  the team controls every consumer and whether they have a version scheme.
  brake cannot see either, and an agent handed a single confident
  recommendation will follow it. Naming the options with their costs and saying
  the choice is not brake's is the honest shape, and the agent is in a better
  position to weigh them than brake is — it can read the rest of the repository.
- **It never runs a subprocess.** See §5 — this is the load-bearing exclusion.
- **It is not a network service.** stdio transport only, no HTTP, no SSE.

---

## 3. Tool surface

Four tools. Each maps to something the CLI already does, and each returns the
same findings the CLI would.

### 3.1 `check_change`

The core tool, and the one that makes this worth building.

```jsonc
{
  "name": "check_change",
  "arguments": {
    "format": "openapi",                  // openapi | proto | graphql
    "proposed": "<the full document text>",
    "baseline": {                         // optional; defaults to brake.toml
      "contract": "payments"              // …or an inline baseline document
    },
    "compatibility": "wire-json"          // optional; defaults to configuration
  }
}
```

`proposed` is document text, not a path, and that is the whole point: an agent
holds an unsaved draft. The library API in
[03-implementation-plan.md](03-implementation-plan.md) §3 already takes bytes
for exactly this reason, so no new library surface is required.

Returns the findings, each with its rule ID, severity, JSON pointer, message,
and the rule's rationale. Structured content, not prose:

```jsonc
{
  "verdict": "findings",                  // clean | findings | unavailable
  "contracts_checked": 1,
  "findings": [{
    "rule": "response-field-removed",
    "severity": "error",
    "pointer": "/paths/~1payments~1{id}/get/responses/200/customer_id",
    "subject": "customer_id",
    "message": "response field removed: field `customer_id` in `GET /payments/{id}`",
    "rationale": "Any consumer reading that field now gets nothing…",
    // §5.7 of the specification, bound to this finding's subject. Ordered
    // most direct first; brake does not choose between them, and the
    // response says so rather than leaving it implied.
    "remediation": [{
      "strategy": "deprecate-then-remove",
      "summary": "mark `customer_id` deprecated now and remove it in a later release, once consumers have had a version to migrate",
      "cost": "the removal waits for a deprecation window you have to actually observe"
    }, {
      "strategy": "expand-then-contract",
      "summary": "add the replacement alongside `customer_id`, move readers across, and remove `customer_id` only when nothing reads it",
      "cost": "both shapes are live at once, and the second half is easy to forget"
    }, {
      "strategy": "version-the-endpoint",
      "summary": "serve the change at a new path, media type or version header, leaving `GET /payments/{id}` answering as it does today",
      "cost": "two implementations to maintain until the old one is retired"
    }],
    "choice_is_not_brakes": "which strategy fits depends on whether you control every consumer and whether you have a version scheme; brake can see neither",
    "help_uri": "https://…/docs/rules.md#response-field-removed"
  }],
  "unverified": [{                        // contract-partial, promoted
    "pointer": "/paths/~1payments/get/responses/200",
    "reason": "`$ref` into another file (`common.yaml#/Payment`), which ingest does not read"
  }]
}
```

`unverified` is a separate key rather than another finding, and §6 explains
why that distinction gets *more* important here, not less.

### 3.2 `compare_contracts`

Two documents, no configuration, no repository.

```jsonc
{ "format": "openapi", "base": "<text>", "head": "<text>",
  "compatibility": "strict" }
```

The tool for reviewing a diff in a repository the agent has not configured, and
the one that makes the acid test in §1 pass. It is `brake diff` without the
`brake.toml` prerequisite.

### 3.3 `explain_rule`

`brake explain`, verbatim. Takes a rule ID, returns severity, the level it
fires from, the summary, and the full rationale. With no ID, lists the
catalogue.

This is what turns a finding into something an agent can act on rather than
route around, and it is why the catalogue's explanations are written as
arguments rather than restatements.

### 3.4 `check_repository`

`brake analyze`, for "what is our compatibility posture?" Takes an optional
contract name filter and compatibility override. Reads `brake.toml`.

Bounded deliberately: it reports, and it does not fix.

---

## 4. Resources and prompts

**Resources** carry what is static, so an agent can read the ruleset without
spending a tool call per rule:

| URI | Content |
| --- | --- |
| `brake://rules` | The catalogue — every rule, severity, and level |
| `brake://rules/{id}` | One rule, with its full rationale and its strategies |
| `brake://strategies` | The evolution strategies of §5.7, with their costs |
| `brake://config` | The resolved `brake.toml`, or a note that there is none |

`brake://rules` is generated from `rules/catalogue.rs`, exactly as
`docs/rules.md` is. Three renderings of one source, and a test already fails if
the generated document drifts.

`brake://strategies` exists so an agent can read the techniques *before* it has
broken anything. An agent that knows `expand-then-contract` while drafting is
more useful than one told about it afterwards, and this is the cheapest place
the whole interface earns its keep.

**One prompt**, `review-api-change`: takes a format and two documents, and
returns a message pre-loaded with the findings, their rationale, and the
applicable strategies. The prompt exists so the *framing* is brake's rather
than the agent's — the difference between "here are some warnings" and "here is
what a consumer of this API experiences, and here are three ways to give them
what you want without that" is most of the value.

---

## 5. Trust posture

This is the section to read before implementing.

brake's pitch is that it is safe to run against a repository you do not trust.
An MCP server is driven by a model, which makes "what can this be talked into
doing?" a live question rather than a theoretical one.

### 5.1 `--drift` is not exposed. At all.

`[contract.generated]` runs a command from a config file.
[02-contract-gates.md](02-contract-gates.md) §8 already treats this as a
different trust posture from parsing one, which is why it is opt-in behind an
explicit flag.

Over MCP the calculus is worse. An agent that can write `brake.toml` — which
any agent editing a repository can — and then call a tool that honours
`[contract.generated]` has arbitrary command execution, obtained through a tool
whose stated purpose is reading files. **No tool in this interface accepts a
drift flag, and the server must refuse to honour `[contract.generated]` even
when the configuration declares it.** Drift checking stays a CLI concern, run
by a person or a CI job that chose to.

This is not a hardening measure to add later. A server that exposes it is a
different and much more dangerous product.

### 5.2 The other constraints

| Constraint | Why |
| --- | --- |
| **stdio transport only** | No HTTP, no SSE, no port. Guarantee G1 says no network under any flag, and a server listening on one is a server |
| **No filesystem writes** | The server reads; the agent writes. Anything else makes an editing agent's mistakes irreversible |
| **Reads bounded to the repository root** | The same tree bound as a local `$ref` (G2). A tool argument naming `../../../etc` is an error, not a read |
| **Configuration re-read per call** | A long-lived server holding a stale `brake.toml` gates against configuration nobody can see. Cheap to re-read; expensive to debug |

### 5.3 Caching

Tempting for a long-lived process, and constrained by G4: same inputs, same
verdict, same bytes. Any cache must be keyed on the content hash of both
documents plus the compatibility level, so a cache hit is indistinguishable
from a recomputation. A cache keyed on a file path is a correctness bug the
first time a file changes underneath it.

The honest default is no cache until something measures a reason for one.

---

## 6. Honest failure, which matters more here

The rule inherited from tropism is *report `unavailable` rather than a clean
result you cannot justify*. Over MCP the bar goes up, for a specific reason:

**A human skims a warning. An agent acts on the absence of one.**

A `contract-partial` in a terminal is a line a developer reads and weighs. The
same finding returned to an agent that asked "is this safe?" is either surfaced
loudly or effectively invisible — and if it is invisible, brake has told a
confident, automated consumer that an unverified change is fine. That is the
failure mode this whole tool exists to avoid, arriving through a new door.

Hence:

- **`verdict` is a required field**, and `unavailable` is one of its values. A
  caller cannot read the findings array and skip the caveat.
- **`unverified` is a separate key**, not a low-severity finding mixed in with
  the rest. An empty `findings` array with a non-empty `unverified` is *not* a
  pass, and the shape makes that hard to misread.
- **Tool errors are reserved for tool failure.** Findings are an answer, not an
  error: `isError` is false for a change that breaks compatibility, and true
  only when brake could not determine an answer. This is exit code `1` versus
  `2` in the CLI ([02-contract-gates.md](02-contract-gates.md) §7.1), and it
  matters for the same reason — conflating "your API broke" with "the gate is
  broken" trains the caller to ignore both, and an agent will do so faster and
  more consistently than a person.

---

## 7. Shape and cost

`brake mcp`, a subcommand, behind a non-default `mcp` feature.

```toml
[features]
default = ["cli"]
cli = ["dep:clap", "dep:annotate-snippets"]
mcp = ["cli", "dep:rmcp", "dep:tokio"]
```

| Decision | Choice | Why |
| --- | --- | --- |
| **M1 — SDK** | `rmcp` | The official `modelcontextprotocol/rust-sdk`, Apache-2.0. Hand-rolling a JSON-RPC framing layer is the wrong thing to own |
| **M2 — Transport** | stdio | §5.1. The SDK's HTTP transports are not compiled in |
| **M3 — Runtime** | `tokio`, feature-gated | The real cost, stated plainly below |
| **M4 — Surface** | A subcommand, not a second binary | One binary is one thing to install and one version to verify at release |

**The cost worth stating.** brake is synchronous and has no async runtime.
`rmcp`'s server feature requires `tokio`, which is a large dependency for a
tool whose pitch includes not needing much. Three mitigations, in order of how
much they matter:

1. The `mcp` feature is **not** in `default`. A consumer taking
   `default-features = false` — which forge does — is unaffected, and so is
   anyone building the CLI.
2. The async surface stops at the transport. Every tool handler calls the same
   synchronous library functions the CLI calls, so there is one implementation
   of every verdict and no second code path to keep honest.
3. `cargo-deny` already gates advisories and licences over the whole tree, so
   the addition is visible rather than assumed.

If the cost is judged too high, the fallback is a separate `brake-mcp` crate in
this repository depending on the library. That keeps `brake` itself untouched
at the price of a second manifest and a second version to release — which is
the trade [03-implementation-plan.md](03-implementation-plan.md) §1 declined
once already, and should be declined again unless the dependency weight
actually bites.

---

## 8. Milestone

### M10 — MCP interface (~4 days)

- `brake mcp` behind the `mcp` feature, stdio transport
- `check_change`, `compare_contracts`, `explain_rule`, `check_repository`
- `brake://rules`, `brake://rules/{id}`, `brake://config`
- The `review-api-change` prompt
- A test asserting `[contract.generated]` is **not** honoured over MCP, in the
  shape of §5.1 — the same test `tests/check.rs` already has for the CLI's
  `--drift` flag being unreachable without it

**Done when:** an agent with the server configured is handed
`response-field-removed` with its rationale for a draft that removes a field,
against a repository it has not configured; the drift test passes; and the
determinism tests in `tests/self_defence.rs` pass against the MCP path as well
as the CLI path, because the guarantees are the tool's and not the CLI's.

---

## 9. Open questions

1. **Should a `propose_migration` tool exist?** For a mechanical break there is
   often a mechanical fix — a removed field becomes a deprecated one, a new
   required parameter becomes optional with a default. brake could emit the
   safe form rather than describing it. Against: it is a source transformation,
   materially larger than "reads files and reports", and a new class of output
   that can be wrong. **Deferred**, and the bar for revisiting is now concrete:
   the `remediation` array names the strategy and its cost, so the question is
   whether an agent handed that can produce the edit. If it can, the tool is
   redundant; if it cannot, that is evidence rather than speculation.
2. **Should `check_change` accept a diff instead of a full document?** Cheaper
   for a large specification, and it would let the tool answer without the
   agent holding the whole file. But applying a patch to produce the head
   document is a second way to be wrong about what the head document is, and
   brake would be reporting on a document nobody has seen.
3. **How should multiple contracts over one artifact be presented?** §2.1's
   release-gating shape means one file can have both a merge-base contract and
   a `latest-tag` one, and an agent asking "is this safe?" arguably wants both
   answers with their questions attached, not a merged list.
4. **Does the prompt belong here or in the agent?** A prompt is brake asserting
   how its findings should be framed. That is either the most valuable thing in
   this document or an overreach into the agent's job, and one round of real
   use would settle it.
