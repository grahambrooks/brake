# 02 — API contract gates

This is the specification; build order is in
[03-implementation-plan.md](03-implementation-plan.md). It is implemented as
written — where the shipped behaviour extends it, the extension is noted
inline. The rule catalogue as built is generated into
[../docs/rules.md](../docs/rules.md).

This document specifies the check itself: what a contract is, what counts as a
breaking change, what the tool guarantees about its own determinism, and what it
says when it cannot answer. The thesis — why this is a standalone tool, and what
it deliberately does not do — is [01-thesis.md](01-thesis.md). The same ruleset
is exposed to a coding agent at edit time by
[04-mcp-interface.md](04-mcp-interface.md), which adds no rule and relaxes no
guarantee here.

---

## 1. Scope

| Phase | Format |
| --- | --- |
| 1 | OpenAPI 3.0 and 3.1, YAML and JSON |
| 2 | Protobuf 3 — `.proto` source, file-descriptor semantics |
| 3 | GraphQL SDL |

One ingester per format, one comparator shared by all of them. That split is the
architectural bet of the whole tool: if the comparator has to know which format
it is comparing, the design has failed.

---

## 2. Configuration

`brake.toml` at the repository root, in the shape of `tropism.toml`. There is no
DSL; a contract gate has perhaps six settings and none of them need a grammar.

```toml
# Applies to every contract unless overridden.
[defaults]
compatibility = "wire-json"
baseline = { git-merge-base = "origin/main" }

[[contract]]
name = "payments"
format = "openapi"
source = "api/payments-openapi.yaml"

[[contract]]
name = "ledger"
format = "openapi"
source = "api/ledger-openapi.yaml"
compatibility = "strict"           # public, frozen
baseline = { file = "api/ledger-openapi.baseline.yaml" }

# A suppression. Narrow by construction, and `reason` is mandatory — the parser
# rejects an entry without one. A suppression list without reasons becomes a
# garbage dump within two quarters, and that is cheap to prevent and expensive
# to retrofit.
[[contract.allow]]
rule = "response-field-removed"
endpoint = "GET /payments/{id}"
field = "legacy_reference"
reason = "Removed after a 90-day deprecation announced 2026-04-01"
expires = "2026-09-01"
```

### 2.1 Baseline resolution

Six shapes, in two families. The first family answers *"is this change safe to
merge?"*; the second answers *"has the API broken since we shipped?"*.

```toml
baseline = { file = "api/openapi.baseline.yaml" }        # a checked-in copy
baseline = { git-merge-base = "origin/main" }            # merge-base of HEAD and the ref
baseline = { tag = "v1.2.0" }                            # a named release tag
baseline = { latest-tag = "v*" }                         # the newest matching tag HEAD descends from
baseline = { rev = "8743cba" }                           # any revision — commit, branch, tag
baseline = { git = "origin/main:api/openapi.yaml" }      # a ref and an explicit path
```

`tag`, `latest-tag`, `rev` and `git-merge-base` all read `source` from the
resolved commit's tree. **`git` is the only one that takes a path**, and that is
the reason to prefer the others: a path written twice is a path that can drift.
A contract whose `source` moved while its `git` spec still names the old path
compares two different files and reports the difference as a breaking change,
which is a false positive that looks exactly like a true one. `git` remains for
the case the others cannot express — reading a contract from a path it no longer
occupies — and nothing else should use it.

**`git-merge-base` is the recommended default for `check`** and the reason is
not stylistic: it does not fire on changes another pull request already landed,
and the merge-base advances on every merge, so history is forgiven
automatically. It is the ratchet, without a state file.

#### Version-controlled baselines

`tag`, `latest-tag` and `rev` are the release-gating family, and they answer a
question the merge-base cannot: **has the published API broken since the last
version consumers actually have?** A merge-base baseline forgives everything
already on `main`, which is correct for a commit gate and wrong for a release
gate — a break merged three weeks ago is still a break for anyone upgrading from
the last tag.

```toml
[[contract]]
name = "payments"
source = "api/payments-openapi.yaml"
# Day to day: did this change break anything not already broken?
baseline = { git-merge-base = "origin/main" }

[[contract]]
name = "payments-released"
source = "api/payments-openapi.yaml"
compatibility = "surface"
# At release time: is the whole delta since v1.x safe for a consumer to upgrade
# into? Both entries gate the same artifact, and they are meant to.
baseline = { latest-tag = "v*" }
```

Two contracts over one artifact is the intended shape. They are different
questions with different answers, and a tool that only lets you ask one of them
forces a team to pick.

**`latest-tag` resolves without config churn.** Naming a tag literally means
editing `brake.toml` on every release, and a version somebody forgot to bump is
a gate quietly comparing against ancient history. The resolution is:

1. Take every tag matching the glob.
2. Order them by version, comparing numeric runs numerically so `v10.0.0`
   sorts above `v9.0.0` — byte order gets this backwards, and CalVer
   (`2026.8.1`) needs the same treatment.
3. Walk from the newest down, and take the first tag **HEAD descends from**.

Step 3 is what makes the answer meaningful on a branch. A tag cut on an
unrelated release branch is not a version this commit evolved from, and
comparing against it reports a "break" that is really a divergence. Restricting
to ancestors also makes the result stable under a fetch that brings in tags from
elsewhere.

**The determinism caveat, stated rather than hidden.** `latest-tag` reads the
tags present in the local repository, and a shallow or `--no-tags` clone has
none. That is the one place where identical file contents can produce a
different verdict on two machines, so it is a reported failure rather than a
silent one: no matching ancestor tag is `contract-unreachable`, never a clean
result. CI must fetch tags — `actions/checkout` needs `fetch-depth: 0`.

Resolution is via `gix`, in-process. `brake` never shells out to `git`.

---

## 3. The contract model

Ingestion normalises every format into one structure. `$ref`s are followed and
inlined, `allOf` is flattened, cycles terminate at a named marker.

```rust
pub struct Contract {
    pub endpoints: BTreeMap<EndpointKey, Endpoint>,
    /// Constructs the ingester met and could not model. Never empty silently —
    /// see §6.2.
    pub unmodelled: Vec<Unmodelled>,
}

pub struct EndpointKey {
    pub method: String,   // "GET", or "RPC" for proto, "Query" for GraphQL
    pub path: String,     // "/payments/{id}", or a fully-qualified method name
}

pub struct Endpoint {
    pub operation_id: Option<String>,
    pub deprecated: bool,
    pub parameters: Vec<Parameter>,
    pub request: Option<Payload>,
    pub responses: BTreeMap<String, Payload>,   // "200", "4XX", "default"
    pub security: Vec<SecurityRequirement>,
    pub span: Span,                              // file, line, JSON pointer
}

/// A resolved, normalised type.
pub enum TypeRef {
    Scalar { ty: String, format: Option<String>, nullable: bool },
    Enum { values: BTreeSet<String> },
    Array { items: Box<TypeRef> },
    Object { fields: BTreeMap<String, Field>, additional: bool },
    OneOf { variants: Vec<TypeRef> },
    Cycle(String),
    Unknown(UnmodelledKind),
}
```

`BTreeMap` and `BTreeSet` throughout, never `HashMap`. Output ordering is part
of the determinism contract (§6) and the type system should enforce it rather
than a sorting step somebody can forget.

`TypeRef::Unknown` is load-bearing. When the ingester meets a construct it does
not model — `not`, a `discriminator`, a `$ref` it cannot reach — it emits
`Unknown` rather than guessing, and §6.2 specifies how that reaches the verdict.
A tool that silently ignores what it cannot parse is worse than no tool, because
it manufactures confidence.

---

## 4. Compatibility levels

Modelled on `buf`'s four categories, which are the best prior art here. Each
level is a strict superset of the one below, so a project can start loose and
tighten without relearning the tool.

| Level | Catches | Use when |
| --- | --- | --- |
| `wire` | Endpoint or method removal, newly-required parameter, type narrowing, enum value removed from a response | Internal services, tolerant readers |
| `wire-json` | `wire` plus field rename, response field removal, status-code removal, security strengthening | **Default.** Most HTTP/JSON APIs |
| `surface` | `wire-json` plus anything that breaks *generated client code* — `operationId` change, schema rename, path-template parameter reorder | Consumers generate clients with `progenitor` or `openapi-generator` |
| `strict` | Any non-additive change at all, including new optional fields | Frozen public APIs under contract |

Protobuf maps `wire` → `WIRE`, `wire-json` → `WIRE_JSON`, `surface` → `PACKAGE`,
`strict` → `FILE`.

---

## 5. Rule catalogue

Every rule has a stable ID and a severity. A rule not applicable at the selected
compatibility level does not fire at all — it is not downgraded to a warning,
because a warning is a thing a human has to read and dismiss.

### 5.1 Endpoint surface

| ID | Severity | Fires when |
| --- | --- | --- |
| `endpoint-removed` | error | A `(method, path)` in the baseline is absent |
| `endpoint-path-changed` | error | An `operationId` survives but its path template changed |
| `method-removed` | error | A path survives but loses a method |

### 5.2 Request compatibility

Tightening the input breaks existing callers.

| ID | Severity | Fires when |
| --- | --- | --- |
| `param-added-required` | error | A new parameter or request field is `required` |
| `param-became-required` | error | An optional parameter or field became `required` |
| `param-removed` | warning | A parameter disappeared — callers still sending it fail under `additionalProperties: false` |
| `param-type-narrowed` | error | `string` → `integer`, wider enum → narrower, `nullable` true → false, tighter `maxLength` / `maximum` |
| `param-location-changed` | error | A parameter moved between `query`, `path`, `header`, `cookie` |
| `request-media-type-removed` | error | A request media type is no longer accepted |

### 5.3 Response compatibility

Loosening the output breaks existing readers.

| ID | Severity | Fires when |
| --- | --- | --- |
| `response-field-removed` | error | A field present in a baseline response is gone |
| `response-field-optional` | error | A field that was always present became optional |
| `response-type-changed` | error | A response field changed type incompatibly |
| `response-enum-extended` | warning | A response enum gained a value — breaks exhaustive matching, which is why `graphql-inspector` calls this `DANGEROUS` rather than breaking |
| `response-status-removed` | error | A documented status code is gone |
| `response-media-type-removed` | error | A response media type is gone |

### 5.4 Security

| ID | Severity | Fires when |
| --- | --- | --- |
| `security-added` | error | An endpoint gained a requirement it did not have |
| `security-scheme-changed` | error | A scheme's type or flow changed |
| `security-removed` | warning | An endpoint lost a requirement — not a compatibility break, but almost always a mistake |

### 5.5 Deprecation hygiene

| ID | Severity | Fires when |
| --- | --- | --- |
| `removed-without-deprecation` | error | Something was removed that was not `deprecated: true` in the baseline |
| `deprecated-no-sunset` | info | An endpoint is `deprecated` with no `x-sunset` date |

`removed-without-deprecation` is what makes the rest of the gate humane. The
sanctioned path for any removal is deprecate → ship → wait → remove, and a team
that follows it never needs a suppression.

### 5.6 Integrity

| ID | Severity | Fires when |
| --- | --- | --- |
| `contract-unreachable` | error | `source` does not resolve, or fails to parse |
| `contract-partial` | warning | An endpoint being compared contains an `Unknown` — reported as *not fully verified*, never as clean |
| `stale-allow` | error | A suppression matches nothing — dead suppressions hide live problems |
| `expired-allow` | error | A suppression is past its `expires` date |

---

## 6. Determinism

This is what makes the gate trustworthy. A deterministic tool must be *provably*
deterministic or it is a flaky test with a good reputation.

### 6.1 Guarantees

1. **Hermetic.** No network under any flag. A `$ref` resolving to a URL produces
   `contract-unreachable`; it is never fetched.
2. **Filesystem-bounded.** Local `$ref`s resolve only within the directory tree
   containing `source`. A `../../../etc` traversal is an error, not a read.
3. **Order-independent.** The verdict does not depend on YAML key order, file
   iteration order, or glob expansion order.
4. **Byte-stable.** Two runs on identical inputs produce identical bytes in every
   output format. No timestamps, no absolute paths, no run IDs, no durations.
5. **Clock-independent**, except for `expires`. That single dependency is
   documented and `--as-of <date>` overrides it, so the expiry path is testable.
6. **Locale- and platform-independent.** Byte-order sorting, never locale
   collation. Paths normalised to `/` in output.

Guarantees 3 and 4 are enforced by tests, not by intention — see
[03-implementation-plan.md](03-implementation-plan.md) §6.

### 6.2 Honest failure

The rule inherited from tropism: **report `unavailable` rather than a clean
result you cannot justify.**

| Condition | Behaviour |
| --- | --- |
| Baseline configured but unresolvable | Exit `2`, reported as unavailable. **Never** treated as "no changes" |
| Source missing or unparseable | `contract-unreachable`, exit `1` |
| `Unknown` on a compared path | `contract-partial`, naming the construct and its JSON pointer |
| No baseline configured at all | Exit `0` with an `info` explaining how to configure one |

The distinction in rows 1 and 4 is the important one. A *missing* baseline is a
tool failure; an *unconfigured* baseline is a user who has not opted in.
Conflating them is how a gate silently stops gating.

---

## 7. Interface

```
brake check [PATHS...] [--since REF] [--config FILE] [--contract NAME]...
            [--baseline REF] [--compatibility LEVEL] [--severity LEVEL]
            [--format FMT] [--as-of DATE]

brake analyze [PATH] [--config FILE] [--contract NAME]... [--fail-on LEVEL]
              [--format FMT]

brake diff [--config FILE] [--contract NAME]... [--baseline REF] [--format FMT]

brake explain <RULE-ID>
```

`brake check` takes paths, so a pre-commit hook can pass the changed files and
the tool checks only the contracts among them. This is the primary surface.

`brake diff` is the non-gating sibling: it reports every change with its
classification and always exits `0`. Its purpose is pull-request descriptions
and changelog drafting.

`brake explain` prints the rationale for a rule — why the rule exists, not just
what it caught. Tropism renders its ruleset's `reason` verbatim at the moment a
developer is blocked, on the grounds that this is when someone actually wants to
know why the constraint exists.

### 7.1 Exit codes

| Code | Meaning |
| --- | --- |
| `0` | No finding at or above the threshold |
| `1` | At least one finding at or above the threshold |
| `2` | Tool failure — baseline unresolvable, source unreadable, internal error |

The `1` / `2` split is the one that matters. CI must distinguish "your API
broke" from "the gate is broken", because the correct response differs and
conflating them trains a team to ignore both.

### 7.2 Output

`--format auto|text|json|sarif`, where `auto` is text on a tty and json when
piped.

**text** — rustc-style, via `annotate-snippets`, pointing into the contract file:

```
error[response-field-removed]: response field `customer_id` was removed
  --> api/payments-openapi.yaml:142:9
    |
142 |         customer_id:
    |         ^^^^^^^^^^^ present in the baseline, absent here
    |
note: GET /payments/{id}, response 200
note: baseline: origin/main:api/payments-openapi.yaml (merge-base 8743cba)
help: a consumer reading this field will break. Deprecate it for a release
      before removing it, or run `brake explain response-field-removed`
```

**json** — one object per finding: `rule`, `severity`, `contract`, `method`,
`path`, `pointer`, `file`, `line`, `message`, `compatibility_level`. Stable key
order.

**sarif** — SARIF 2.1.0. Each rule ID becomes a `reportingDescriptor` with a
`helpUri`. `physicalLocation` points at the **contract artifact and line**, so
GitHub annotates the pull request where the change is, which is the requirement
that forces the span-preserving YAML parser in §3.
`partialFingerprints` derive from `rule + contract + method + path + pointer`
so a finding is tracked across commits without re-alerting.

---

## 8. Generated-code drift

Rust consumers generate clients from these specs with `progenitor`, and servers
publish them with `utoipa`. Both directions drift from the committed artifact.

`brake` does not generate code. It gates the drift, using the universal pattern:

```toml
[[contract]]
name = "payments"
source = "api/payments-openapi.yaml"

[contract.generated]
command = "cargo run --bin export-openapi -- --out -"
```

`brake check --drift` runs the command in a temp directory, compares stdout to
the committed artifact byte-for-byte, and reports `generated-drift` (error) with
a unified diff.

This is the only place `brake` executes a subprocess. It is opt-in per contract,
behind an explicit flag, and skipped by default, because running commands out of
a config file is a different trust posture from parsing one. The flag exists so
that `brake check` stays safe to run against an untrusted repository — which a
pre-commit hook, by definition, sometimes is.

---

## 9. Open questions

1. **`response-enum-extended` severity.** `warning` here, following
   `graphql-inspector`'s `DANGEROUS`. Teams generating Rust clients with
   exhaustive matches will want `error`. Possibly a level distinction rather
   than a fixed severity.
2. **Multi-file protobuf** needs an import-root story that OpenAPI's single-root
   model does not. It may argue for a different `source` shape — better designed
   in now than bolted on at phase 2.
3. **Does `check` need `--since` at all**, given paths? A pull-request job wants
   the union of files changed on the branch, which a hook does not. Probably
   yes, but it is the one flag that could be a shell one-liner instead.
4. **Should `brake` read an OpenAPI `info.version`** and refuse to gate when the
   major version differs? A deliberate v1 → v2 break is not a regression, and
   every finding on it is noise.
