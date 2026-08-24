---
name: api-compatibility
description: Check whether a change to an API contract would break its consumers, using brake, before the change is written. Use when editing, generating or reviewing an OpenAPI/Swagger document, a .proto file, a GraphQL SDL schema, or an AsyncAPI document — removing or renaming a field, changing a type, adding a required parameter, changing a status code, tightening security — or when asked "is this change backward compatible?", "will this break clients?", or "can I remove this field?".
argument-hint: "<the contract file or the change you are about to make>"
---

# Checking an API change for backward compatibility

You are about to change an API contract. `brake` compares the change against
its baseline and reports what would break a consumer, with the ways to make the
same change safely. Consult it **before** writing the edit — the cheapest
moment to reconsider a rename is before it exists.

`brake` reads files. It makes no network request, runs no toolchain, and starts
no service, so it is always safe to run.

## 1. Find out whether this repository uses brake

```sh
test -f brake.toml && cat brake.toml
```

- **`brake.toml` exists** — use the repository's own configuration. It already
  says which artifacts are gated, at what level, against which baseline.
- **No `brake.toml`, but `brake` is installed** — you can still compare two
  documents directly (step 3), or run `/brake-adopt` to set it up.
- **`brake` is not installed** — say so and offer `brew install brake` /
  `cargo install brake`. Do not guess at the answer it would have given.

## 2. Prefer the MCP tools when they are available

If an MCP server named `brake` is connected, use it — it takes the proposed
document **as text**, so a draft that has not been written to disk can be
checked:

| Tool | Use for |
| --- | --- |
| `check_change` | A draft you are about to write, against the repository's configured baseline |
| `compare_contracts` | Two documents you already have. Needs no `brake.toml` and no repository |
| `who_consumes` | Who declared they use this endpoint or field — call it **before** proposing a removal |
| `explain_rule` | Why a rule exists and what the ways out cost |
| `check_repository` | The whole repository's posture, not one change |

Read the `brake://strategies` resource before drafting a change, not after.

## 3. Otherwise, use the CLI

```sh
brake check path/to/contract.yaml    # the contracts among these paths
brake check --since origin/main      # everything this branch changed
brake diff                           # describe the change, never fail
```

To compare a draft with no configuration, write it to a temporary file and use
a file baseline:

```sh
brake check --config /tmp/brake-draft.toml
```

```toml
[[contract]]
name = "draft"
format = "openapi"                  # openapi | proto | graphql | asyncapi
source = "/tmp/proposed.yaml"
baseline = { file = "api/current.yaml" }
```

Exit codes: `0` clean, `1` a finding at or above the threshold, `2` the *gate*
is broken (baseline unresolvable, source unreadable). Never treat `2` as clean.

## 4. Act on what it says

Every break comes with named, costed ways to make the same change safely.
`brake` does not choose between them, and neither should you without saying
why — which one fits depends on facts `brake` cannot see.

The catalogue names sixteen; these four cover most removals and renames. Each
finding carries only the ones that apply to it, already bound to the field.

| Strategy | Fits when |
| --- | --- |
| `keep-emitting` | The old shape is cheap to go on producing alongside the new one |
| `deprecate-then-remove` | You control the release cadence and can observe a deprecation window |
| `expand-then-contract` | Readers can be migrated one at a time — add the new, move readers, then remove |
| `version-the-endpoint` | You cannot reach every consumer, and can afford two implementations |

`reserve-the-number` (protobuf) is the exception: it is the only correct move,
and its cost is none. A reused field number silently misreads data.

Present the options with their costs and ask the user which fits, unless the
repository's history already answers it. Do not silently pick the one that
makes the finding disappear.

`brake explain <rule-id>` gives the full reasoning for any finding. If the
answer is genuinely "this break is acceptable", that is a suppression in
`brake.toml` with a written reason and an expiry — never a rule turned off.

## 5. Two things that change the answer

**A contract may span several files.** If the document `$ref`s a sibling, a
tool that takes a single document as text — `check_change`, `compare_contracts`,
or a `--config` pointing at one file — sees only part of the contract, and the
rest is reported as `contract-partial`. Use `check_repository`, or
`brake check` in the repository, when the contract is split across files.

**AsyncAPI direction decides the variance.** A `PUBLISH` / `send` payload is
checked like a response — your service produces it, so removing a field breaks
readers. A `SUBSCRIBE` / `receive` payload is checked like a request body — your
service consumes it, so requiring a new field breaks producers. Read the
operation keyword before reasoning about which way a change is safe.

## 6. Know the limits before you rely on the answer

- **A clean run is not a passing pact verification.** `brake` checks that the
  *specification* still satisfies what consumers declared. Whether the
  implementation matches its own specification is a different question.
- **An empty `who_consumes` answer means nobody *declared* it**, not that
  nobody uses it. Unless `completeness = "closed-world"` is set — an explicit
  human assertion — treat undeclared consumers as possible.
- **`unavailable` is not clean.** A construct `brake` cannot model is named
  rather than ignored. Report it; do not round it down to a pass.
- The compatibility level changes the answer. `wire-json` is the default;
  `surface` also catches what breaks generated client code; `strict` reports
  additive changes too.

## Related

- `/brake-consumer-impact` — who breaks, before you remove or rename.
- `/brake-triage` — a finding is already blocking a commit or a build.
- `/brake-adopt` — this repository has no gate yet.
