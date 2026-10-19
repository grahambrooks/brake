# 07 — Structural lint rules

## Decision

`brake` will enforce four structural validity rules against the head contract,
without a baseline comparison. These fire from the same rule catalogue, through
the same finding pipeline, and are suppressed by the same `[[contract.allow]]`
mechanism as every compatibility rule.

---

## Context

`design/01-thesis.md` lists *"not a spec linter"* as a deliberate non-scope and
names Spectral and vacuum as the right tools for style rules. That exclusion was
written for rules that cannot break a consumer: operation IDs in camelCase, every
response needs an example, descriptions must be non-empty. Those rules fire on
API quality, not on whether a consumer will crash.

The rules recorded here are a different category. They are **structural defects
that break consumers even when no version comparison is possible** — a single
draft contract that causes SDK generation to fail, or a deployed contract that
causes every consumer to crash on deserialization. They do not describe style
preferences; they describe facts about whether the contract is coherent.

The test: *if a consumer built against this contract as written would behave
incorrectly or fail to compile, brake should say so.* That test admits the rules
below and excludes everything Spectral covers by documentation quality.

---

## Rules added

### `enum-type-mismatch` — Severity: Error, Level: Wire

An enum value does not match the `type` declared alongside it.

```yaml
# triggers rule
schema:
  type: integer
  enum: [1, 2, "three"]
```

A Rust, Go, or TypeScript client generated from this schema will fail to
deserialize the third value at runtime. The spec parses without error; the
client crashes in production. This is not a style concern — it is a latent
runtime break waiting for the API to serve the mismatched value.

### `enum-duplicate-value` — Severity: Warning, Level: Wire

An enum array contains the same value more than once.

```yaml
# triggers rule
schema:
  type: string
  enum: [active, inactive, active]
```

Some language code generators, and Protobuf transpilers in particular,
deduplicate silently. A consumer's generated type has fewer members than the
contract implies; the API can serve a value the client's type does not model.
A `Warning` rather than `Error` because the immediate consumer impact is
generator-dependent.

### `operation-id-duplicate` — Severity: Error, Level: Surface

Two or more operations share the same `operationId`.

Code generators derive method names from `operationId`. A duplicate produces a
compile-time collision in every generated SDK — a guaranteed consumer breakage
for any team using code generation, which is the dominant consumption pattern
for OpenAPI-described APIs.

Level `Surface` rather than `Wire` because the wire protocol is unaffected; only
generated clients break.

### `path-param-undeclared` — Severity: Error, Level: Surface

A path template uses `{param}` but no parameter object with `in: path` and the
same name exists on the operation or its path item.

```yaml
# triggers rule — /payments/{id} has no `id` parameter object
paths:
  /payments/{id}:
    get:
      responses: { "200": { description: ok } }
```

SDK generators that follow the spec produce a method with a signature that is
missing the required argument. Clients compiled against the generated SDK call
the endpoint with a literal `{id}` in the URL, or fail to compile, depending on
the language.

Level `Surface` because URL routing still works; the break is in the generated
client surface.

---

## Why this fits the thesis

The thesis test is: *can this break a consumer?* All four rules satisfy it:

| Rule | Consumer break |
|---|---|
| `enum-type-mismatch` | Deserialization panic or type error at runtime |
| `enum-duplicate-value` | Missing variant in generated enum type |
| `operation-id-duplicate` | Compile-time collision in every generated SDK |
| `path-param-undeclared` | Generated method missing a required argument |

None of these are quality preferences. A team that wants to skip them for a
deliberately informal contract can suppress them with `[[contract.allow]]` and a
recorded reason, which is exactly the escape hatch every other breaking-change
rule uses.

---

## Why not Spectral's full rule set

Spectral's ~55 rules include a subset that matches the consumer-break test, and
those are the ones added here. The rest — documentation completeness, style
conventions, example validity, unused definitions — remain outside scope for the
same reason as before: a rule that cannot break a consumer does not belong in a
tool that gates breaking changes.

Concretely excluded:
- Documentation completeness (`info-description`, `operation-description`, etc.)
- Style conventions (`path-keys-no-trailing-slash`, `openapi-tags-alphabetical`)
- Security injection (`no-eval-in-markdown`)
- Unused definitions (`oas2-unused-definition`, `oas3-unused-component`)

---

## Implementation

These are **static lint rules**, not comparison rules. They run against the head
contract only, with no baseline involved. A new `rules::lint` module holds the
logic; `check::check_contract` calls it immediately after the comparison pass and
before suppressions are applied, so the full `[[contract.allow]]` mechanism covers
these findings like any other.

The two ingester-detected rules (`enum-type-mismatch`, `enum-duplicate-value`)
are recorded as `LintCandidate`s on the `Contract` during OpenAPI ingestion, then
converted to findings by the lint pass. The two model-level rules
(`operation-id-duplicate`, `path-param-undeclared`) are detected directly in the
lint pass by walking `contract.endpoints`.

Positive and negative tests exist for all four rules: the rule fires on the
defect, and stays silent on a valid contract.

---

## See also

- [01-thesis.md](01-thesis.md) — scope and the "not a spec linter" ruling
- [02-contract-gates.md](02-contract-gates.md) — the rule catalogue and levels
- [06-architectural-evolution.md](06-architectural-evolution.md) — roadmap
