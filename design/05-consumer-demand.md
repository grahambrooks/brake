# 05 — Consumer demand

**Implemented.** `src/demand/`, the `[[consumer]]` and `[consumers]` blocks of
`brake.toml`, the ten `consumer-*` rules, `affects` on every finding, `brake
consumers`, and `who_consumes` over MCP. The contracts this document sets are
tested in `tests/consumer_demand.rs`; the two guarantees §8 adds joined the
self-defence set in `tests/self_defence.rs`.

This document specifies the third input. Today `brake` compares a contract
against its own past and reports what a *hypothetical* consumer would notice.
Given a declaration of what real consumers actually use, it can name them —
which turns "this might break somebody" into "this breaks `web-checkout`, at
`pacts/web-checkout-payments.json:88`".

It adds no network, no subprocess and no server. A consumer declaration is a
file, and `brake` already reads files.

The thesis is [01-thesis.md](01-thesis.md); the contract specification is
[02-contract-gates.md](02-contract-gates.md), whose §6 determinism guarantees
this document extends rather than relaxes.

---

## 1. The line this does and does not cross

[01-thesis.md](01-thesis.md) rules out "Pact-style consumer-driven
verification", on the grounds that it requires running both sides. That
reasoning is correct about *provider verification* and wrong about the artifact.
A pact is a JSON document sitting in a directory. Reading it is the same act as
reading an OpenAPI file.

The distinction is worth stating precisely, because everything here depends on
it:

| | Needs a running service | `brake` does it |
| --- | --- | --- |
| Replay a consumer's recorded requests against the provider, compare real responses | yes | **no** |
| Check the provider's *published contract* against the consumer's recorded expectations | no | **yes** |
| Ask a broker what is deployed where (`can-i-deploy`) | needs a server | **no** |
| Name which declared consumers a diff would break | no | **yes** |

**A green `brake` run is not a passing pact verification, and must never be
reported as one.** `brake` checks that the *specification* still satisfies what
consumers declared. Whether the implementation matches its own specification is
what `--drift` and the provider's test suite are for. Every renderer says this
where it could be misread — see §9.

`brake` also never writes a pact, never generates one from an OpenAPI file, and
never dereferences a URL found inside one. Pact documents published by a broker
carry `_links`; those are data, not instructions, and §8 makes that a test.

---

## 2. What a consumer declaration is

A **demand**: a partial, one-sided description of an API surface — the
endpoints, fields, statuses, parameters and media types one named consumer
relies on. Three sources, one model, in the shape the contract ingesters
already use.

| Format | Source | Fidelity |
| --- | --- | --- |
| `pact` | Pact v2/v3/v4 HTTP interactions, JSON | High for what the consumer's tests assert; a pact only knows what its tests exercised |
| `graphql-operations` | The consumer's `.graphql` query documents | Exact. A selection set *is* the field list, with no inference at all |
| `manifest` | A hand- or codegen-written `*.brake-uses.toml` | Whatever the author wrote. The fallback for gRPC, for consumers without pact tests, and for third parties who will only tell you in prose |

One ingester per format, one join, one attribution pass. That is the §2 bet of
[03-implementation-plan.md](03-implementation-plan.md) reproduced on the demand
axis: if the join has to know which format a demand came from, the ingest
normalisation is under-specified.

`graphql-operations` is listed second and is the strongest of the three. A pact
records an *example* response body, so a field appearing in it is evidence the
consumer reads it — good evidence, since pact's own verification fails when the
field is absent, but evidence. A GraphQL selection set is a statement.

### 2.1 The model

```rust
pub struct Demand {
    pub consumer: String,          // "web-checkout"
    pub provider: String,          // as the artifact declares it
    pub source: String,            // repository-relative, for spans
    pub usages: BTreeSet<Usage>,
    /// Interactions the ingester met and could not model. Never empty
    /// silently — §7.
    pub unmodelled: Vec<Unmodelled>,
}

pub struct Usage {
    /// As written in the artifact: concrete for pact (`/payments/42`),
    /// already templated for a manifest.
    pub route: Route,
    pub kind: UsageKind,
    pub span: Span,
}

pub enum UsageKind {
    /// The consumer calls it at all.
    Endpoint,
    /// The consumer sends this request shape.
    Request { media_type: String, ty: TypeRef },
    /// The consumer reads this response.
    Response { status: String, media_type: String, ty: TypeRef },
    Parameter { name: String, location: String, value: Option<String> },
}
```

`TypeRef` is the contract model's own type, and reusing it is the load-bearing
decision of this design — see §4.

`BTreeSet` and `BTreeMap` throughout, for the same reason as §3 of
[02-contract-gates.md](02-contract-gates.md): output ordering is part of the
determinism contract and the type system should enforce it.

### 2.2 Ingest stays bytes-only

`demand::pact::ingest(source, bytes) -> Result<Demand, ParseError>` takes bytes,
like every contract ingester, and knows nothing about the provider it
constrains. That matters because a pact's paths are concrete and a contract's
are templates: resolving `/payments/42` to `GET /payments/{id}` needs the
contract, and doing it inside the ingester would make ingest untestable without
a second document and would put a `match` on nothing useful in the wrong place.

Binding is therefore a separate phase, §3, with its own failure modes and its
own tests.

---

## 3. The join

`demand::bind(&Demand, &Contract) -> (Expectation, Vec<BindIssue>)`.

An `Expectation` is a `Contract` — the same struct, populated only where the
consumer declared something. That is what makes §4 possible.

**Path binding.** A concrete path binds to a template when the segment counts
match, every literal segment is equal, and every `{param}` segment matches a
non-empty segment.

1. Exactly one template matches → bind, and record each `{param}` value as a
   `Parameter` usage. A consumer calling `/payments/abc` has declared that it
   sends `abc` for `id`, which is what lets a later narrowing of `id` to
   `integer` be reported as breaking *that consumer* rather than in the
   abstract.
2. Several match → prefer the one with the fewest template segments, which is
   the most literal. Still tied → `consumer-path-ambiguous`. **Never guess:** a
   guessed binding attributes a break to the wrong endpoint, which is worse than
   declining to attribute it.
3. None match → `consumer-endpoint-unmet`. The consumer calls something the
   contract does not document.

**Requests.** `request.query` (a string in v2, a map in v3+) and headers become
`Parameter` usages. `Content-Type` and `Accept` become media types. The body
example, plus whatever `matchingRules` constrain, becomes a `TypeRef`: an
`Object` whose present fields are `required` — the consumer does send them —
with scalar types taken from the matcher where there is one and inferred from
the JSON value where there is not.

**Responses.** The expected status becomes a key, matched against the contract's
`responses` including its `4XX` and `default` classes. The expected body becomes
a `TypeRef` the same way, with every present field `required`: pact's own
verification fails if the provider omits a field the consumer expected, so
presence in the example is a declaration that the field must keep being
produced.

**Matchers `brake` cannot model** — `arrayContains`, plugin-backed content
types, anything a v4 pact defers to a plugin — become `TypeRef::Unknown`,
exactly as an unmodellable OpenAPI construct does, and reach the verdict as
`consumer-partial` rather than as silence.

**Not modelled, deliberately:** provider states (they describe the provider's
database, not its API), `Asynchronous/Messages` interactions (a message pact
constrains a broker topic, and `brake` has no topic model — reported as
unmodelled, never ignored), and authentication headers (a pact carrying a bearer
token says nothing about which scheme the contract should require, and inferring
security from a fixture is how a tool starts being wrong confidently).

---

## 4. Verification is the comparator, run sideways

`compare/types.rs` already answers "does head still satisfy what base
promised?", in both directions:

- `TypeDirection::Response` — a field in base and absent in head is
  `ResponseFieldRemoved`.
- `TypeDirection::Request` — a field required in head and absent in base is
  `RequestFieldAddedRequired`; a type narrower in head than in base is
  `RequestTypeNarrowed`.

Put the consumer's expectation on the *base* side and the head contract on the
head side, and those are precisely the questions to ask of a consumer:

| Comparison | Reads as |
| --- | --- |
| `compare(expectation, head, Response)` | Does the contract still produce everything the consumer reads? |
| `compare(expectation, head, Request)` | Would the contract still accept what the consumer sends? |

`ResponseFieldRemoved` on that comparison means the contract does not document a
field the consumer expects. `RequestFieldAddedRequired` means the contract now
demands a field the consumer does not send. The whole verification is a
projection of the existing engine, which is why it is a handful of days rather
than a second comparator — and why it will stay consistent with the baseline
diff as the type comparison improves.

The mapping from `TypeIssue` to a consumer rule is a table in `rules/`, the same
shape as the existing `ChangeKind` table.

**No baseline is involved.** Demand is compared against `head` only. `brake`
does not version consumer declarations, does not diff a pact against its
previous self, and does not store which consumer had which expectation when.
That is the registry from the thesis's exclusion list, and this design stays on
the right side of it by never needing history: a consumer's expectation is a
statement about the contract as it is now.

---

## 5. Configuration

```toml
[[contract]]
name = "payments"
format = "openapi"
source = "api/payments-openapi.yaml"

[[consumer]]
name = "web-checkout"                 # optional; the pact names itself
format = "pact"
source = "pacts/web-checkout-payments.json"
provider = "payments"                 # which [[contract]]; default: match the pact's provider.name

[[consumer]]
format = "pact"
source = "services/*/pacts/*-payments.json"   # a monorepo, globbed and sorted

[[consumer]]
name = "reporting"
format = "manifest"
source = "consumers/reporting.brake-uses.toml"

[consumers]
policy = "annotate"                   # annotate | escalate | triage
completeness = "open-world"           # open-world | closed-world
```

`[consumers]` is one block, not a per-contract setting, because both knobs are
statements about *this repository's knowledge of the world* rather than about an
artifact.

A native manifest, for the consumer who has no pact tests:

```toml
consumer = "reporting"
provider = "payments"

[[uses]]
endpoint = "GET /payments/{id}"       # already templated: no binding needed
statuses = ["200", "404"]
reads = ["id", "amount.currency", "status"]
sends = []

[[uses]]
endpoint = "POST /payments"
statuses = ["201"]
sends = ["amount.value", "amount.currency", "idempotency_key"]
```

Glob expansion is sorted byte-wise before use, and consumers are ordered by
`(name, source)`, so guarantee G3 holds over a directory listing.

### 5.1 Where the files come from, with no network

This is the honest part of the design, and the workflows differ in how much
they can be trusted.

| Workflow | How the pact arrives | Failure mode |
| --- | --- | --- |
| **Monorepo** | The consumer's own tests write it into `services/<name>/pacts/`; `brake` globs it | Strongest. The pact is as fresh as the consumer's last test run |
| **Vendored** | Consumer CI opens a pull request against the provider repository updating `pacts/<consumer>.json` | The declaration is reviewed like code, and its staleness is visible in `git log` |
| **Pulled in CI** | A prior CI step (`pact-broker pull`, `curl`) writes the directory; `brake` reads it | The network stays in the pipeline, outside the tool. A failed pull leaves the declared file absent, which is `consumer-unreachable` and exit `1` — loud, not clean |

`brake` will not pull the files itself, under any flag, and this is not a
capability waiting for a use case. The moment a contract gate can be pointed at
a URL it stops being reproducible on a laptop, in an air-gapped build, or three
years from now when the broker has been decommissioned.

**`brake` does not measure freshness.** A pact from eighteen months ago and one
from this morning are the same bytes to a file reader, and any heuristic over
mtime would break guarantees G4 and G5. Instead, `brake consumers` reports every
declaration it used with its path and a short content digest, so a human — or a
CI step diffing that output — can see exactly what the verdict rested on.
Freshness is a pipeline property, and a tool that pretended to measure it would
be manufacturing the confidence §6.2 exists to prevent.

---

## 6. Rules

### 6.1 Expectation — head contract × demand, no baseline required

These fire on `check` for contracts in scope and on `analyze` everywhere. They
work on a brand-new contract with no history at all, which is a genuinely new
capability: today a first-commit contract can only be `contract-new`.

| ID | Severity | Level | Fires when |
| --- | --- | --- | --- |
| `consumer-endpoint-unmet` | error | `wire` | A consumer calls an endpoint the contract does not document |
| `consumer-status-unmet` | error | `wire-json` | A consumer expects a status the contract does not document |
| `consumer-field-unmet` | error | `wire-json` | A consumer reads a response field the contract does not produce |
| `consumer-request-rejected` | error | `wire` | The contract would reject a request the consumer sends — a required field or parameter it omits, a value outside a narrowed type, a media type no longer accepted |

### 6.2 Integrity — the same posture as §5.6 of the contract spec

| ID | Severity | Level | Fires when |
| --- | --- | --- | --- |
| `consumer-unreachable` | error | `wire` | A declared consumer source does not resolve or fails to parse |
| `consumer-partial` | warning | `wire` | An interaction contains a construct `brake` cannot model — named, with its pointer |
| `consumer-path-ambiguous` | warning | `wire` | A concrete path matches more than one template, so the expectation was not verified |
| `consumer-provider-unmatched` | error | `wire` | A pact names a provider with no matching `[[contract]]` — a configuration error, not a compatibility one |
| `consumer-undeclared` | info | `wire` | A file in the tree parses as a demand for a declared provider but is not declared in `brake.toml` |

`consumer-undeclared` is identified **by parsing**, sharing the mechanism of
`src/init.rs::identify` and `contract-unconfigured`. No filename heuristic: the
previous one called `.github/workflows/api-tests.yaml` an API, and a heuristic
that calls a fixture a pact would be the same mistake with a new file
extension.

### 6.3 Advisory — `analyze` and `brake consumers` only

| ID | Severity | Fires when |
| --- | --- | --- |
| `consumer-surface-unused` | info | An endpoint no declared consumer uses — **only** emitted under `completeness = "closed-world"` |

This is the one rule here that reports a suspected *absence*, which
[01-thesis.md](01-thesis.md) forbids at commit time: "only rules that report the
presence of a breaking change, never the suspected absence of something." It is
excluded from `check` for exactly that reason, and gated behind an explicit
closed-world declaration because otherwise it is a confident statement about
consumers `brake` has never heard of.

### 6.4 There is no `consumer-break` rule

A break is already a finding. Attribution is *evidence attached to it*, not a
second finding — one broken field must not produce one `response-field-removed`
plus three `consumer-break`s, because a developer then has to work out that four
findings are one problem. Every existing rule gains an `affects` list, and that
is the whole change.

---

## 7. Attribution and policy

A `Finding` gains:

```rust
/// Declared consumers this finding is evidence against, with the interaction
/// that says so. Empty when no consumer is declared, and — importantly —
/// also when none is affected. The two are not the same and §7.2 keeps them
/// distinguishable.
pub affects: Vec<ConsumerRef>,

pub struct ConsumerRef {
    pub consumer: String,
    pub source: String,
    pub span: Span,       // the interaction, not the contract
}
```

A change is attributed to a consumer when the consumer's usage set contains the
change's `(endpoint, subject)` — which is why §5.7.3 of the contract spec
insisted the subject be carried explicitly rather than recovered from a JSON
pointer. The attribution join is that field, used a second time.

### 7.1 The three policies

| `policy` | Effect |
| --- | --- |
| `annotate` | **Default.** Severities unchanged; affected consumers are named on the finding |
| `escalate` | A `warning` becomes an `error` when a declared consumer is affected. `param-removed` and `security-removed` are warnings precisely because `brake` could not tell whether anyone relied on them. Now it can |
| `triage` | An `error` becomes a `warning` when no declared consumer is affected — the narrow, opt-in case in §7.2 |

### 7.2 `triage` is constrained, because it is the one that can lie

Downgrading a break because no consumer declared it is the exact shape of the
false clean [02-contract-gates.md](02-contract-gates.md) §6.2 exists to forbid.
It is offered anyway, because a team that genuinely has every consumer in one
repository is being asked to treat a break nobody can observe as a blocker —
and that is how a gate gets uninstalled. Four constraints make it honest:

1. It requires `completeness = "closed-world"`: an explicit, reviewable
   assertion by a human that the declared set is exhaustive. `brake` cannot
   verify that claim and does not pretend to.
2. It only applies to rules the catalogue marks **observable by demand**. A
   pact says nothing about `operation-id-changed`, `security-scheme-changed` or
   `path-parameter-renamed` — those break generated client *code*, which no
   consumer declaration models. A rule that demand cannot see is never
   downgraded on the strength of demand's silence.
3. The floor is `warning`. Nothing is ever downgraded to nothing, and nothing is
   ever suppressed. A suppression still requires a `reason`, as it should.
4. Every downgraded finding renders the assumption it rests on:
   `note: no declared consumer uses this — 3 consumers declared, and brake
   cannot know that is all of them`.

---

## 8. Determinism and hermeticity

Guarantees G1–G6 of [02-contract-gates.md](02-contract-gates.md) §6.1 apply
unchanged. Two are extended:

- **G1 (hermetic)** now also covers demand: a URL anywhere in a pact —
  `_links`, `pb:publish`, a `$ref` inside an example body — is data. It is never
  dereferenced, under any flag. A demand source that is itself a URL is a
  configuration error, refused at parse time.
- **G3 (order-independent)** now also covers glob expansion for demand sources
  and the order interactions appear in a pact.

Two tests join the five self-defence tests of
[03-implementation-plan.md](03-implementation-plan.md) §6, making seven:

| Test | Defends |
| --- | --- |
| A pact carrying broker `_links` and an `http://` example ref produces findings and opens no socket | G1, over the demand axis |
| A declared consumer whose file is absent exits `1` with `consumer-unreachable`, never clean | §6.2 honest failure — the CI-pull workflow of §5.1 rests entirely on this |

And per the standing convention, every rule in §6 gets a positive and a negative
test: it fires on the mismatch, and stays quiet on a contract that satisfies the
pact.

---

## 9. Interface

```
brake check    [PATHS...] [--consumer NAME]...
brake analyze  [PATH]     [--consumer NAME]...
brake consumers [--contract NAME]... [--consumer NAME]... [--format FMT]
```

`--consumer` mirrors `--contract`. A path scope that names a pact file selects
the contracts that pact constrains, so a hook run on a pact-updating commit
verifies the right thing.

`brake consumers` is non-gating and always exits `0`, joining `diff` in that
family. It answers the question everybody actually has:

```
payments — api/payments-openapi.yaml

  web-checkout   pacts/web-checkout-payments.json  sha256:ab12cd34
    GET  /payments/{id}     200  reads: id, amount.currency, amount.value, status
    POST /payments          201  sends: amount.currency, amount.value

  reporting      consumers/reporting.brake-uses.toml  sha256:9f01e2b7
    GET  /payments/{id}     200  reads: id, status

  2 of 7 endpoints have a declared consumer.
  brake knows about the consumers declared in brake.toml and no others.
```

That last line is not decoration. Without it the inventory reads as a complete
census, and it is a list of files somebody remembered to declare.

A finding, once a consumer is declared:

```
error[response-field-removed]: response field `customer_id` was removed
  --> api/payments-openapi.yaml:142:9
    |
142 |         customer_id:
    |         ^^^^^^^^^^^ present in the baseline, absent here
    |
note: GET /payments/{id}, response 200
note: breaks web-checkout — pacts/web-checkout-payments.json:88
note: baseline: origin/main:api/payments-openapi.yaml (merge-base 8743cba)
help: a consumer reading this field will break. Deprecate it for a release
      before removing it, or run `brake explain response-field-removed`
```

**json** — findings gain `affects: [{consumer, source, line}]`. `sarif` — the
interaction becomes a `relatedLocation`, which is what SARIF's related locations
are for. `partialFingerprints` are unchanged, so attribution appearing on an
existing finding does not re-alert.

`brake explain` covers the new rules for free, from the catalogue.

### 9.1 The MCP surface

One tool, and it is the most valuable thing in this document for an agent:

**`who_consumes { contract, endpoint?, field? }`** → the declared consumers of
that endpoint or field, with the interaction that declares it.

An agent about to delete a response field can ask who reads it *before* writing
the edit, rather than being told afterwards by a hook. `check_change` gains the
same `affects` list.

The constraints of [04-mcp-interface.md](04-mcp-interface.md) §5 hold without
exception: `handlers.rs` stays synchronous and transport-free, nothing here goes
near the `--drift` subprocess path, and the server reads declared demand
sources, not arbitrary paths. `brake://consumers` joins the resource list.

---

## 10. What this is still not

Recorded in the same spirit as the thesis's exclusion list, because these are
the four things a reader of this document will suggest next.

- **Not a broker client.** No `can-i-deploy`, no environments, no deployment
  state, no versions. Those questions need a server that knows what is running
  where, and that server is not being built. Demand is a file.
- **Not provider verification.** `brake` never issues a request. It checks the
  specification against the expectation; the implementation against the
  specification is `--drift` and the provider's own tests.
- **Not a pact generator or publisher.** `brake` reads demand and never writes
  it. Generating a pact from an OpenAPI file would invert the direction that
  makes consumer-driven contracts worth anything.
- **Not a consumer registry.** No history, no stored expectation timeline, no
  transitive compatibility across consumer versions. Demand is evaluated
  against `head`, once, from the working tree.

---

## 11. Milestones

Effort is one person. Numbering continues from
[03-implementation-plan.md](03-implementation-plan.md) §5, where M11 is the
last built milestone.

### M12 — The demand model and pact ingest (~5 days)

`src/demand/{mod,pact}.rs`, the `[[consumer]]` configuration of §5, the binding
of §3, the integrity rules of §6.2, the expectation rules of §6.1, and
`brake consumers`.

**Done when:** a pact expecting a field the contract does not document is
`consumer-field-unmet` with a span pointing at the interaction; a pact the
contract fully satisfies is clean; a declared pact file that is absent exits `1`
as `consumer-unreachable` rather than passing; a pact whose path matches two
templates is `consumer-path-ambiguous` and not a guess; and a v4 message
interaction is `consumer-partial`, named.

### M13 — Attribution and policy (~3 days)

`affects` on `Finding`, rendered in text, JSON and SARIF; the three policies of
§7.1; the observable-by-demand flag in the catalogue; the closed-world
declaration and its four constraints.

**Done when:** removing a field a pact reads names the consumer and the
interaction; removing a field no pact reads is unchanged under `annotate`,
warned under `triage` with its assumption printed, and unchanged under `triage`
for a rule demand cannot observe.

**This changes a public type, and `brake` is the tool that gates exactly that.**
Adding a field to `Finding` breaks every downstream struct literal, forge's
included. It ships as a deliberate, announced break with `Finding` marked
`#[non_exhaustive]` in the same change so it is the last one of its kind —
taking the medicine this crate prescribes.

### M14 — Beyond pact (~4 days)

`graphql-operations` and `manifest` ingesters, and `consumer-surface-unused`.

**Done when:** a GraphQL query document produces the same shape of finding as a
pact does, through the same join and with no format-specific branch in it — the
proof that the demand model generalised rather than encoding pact's shape under
another name.

### M15 — `who_consumes` over MCP (~1 day)

**Done when:** an agent can ask who reads a field before proposing its removal,
and `tests/mcp.rs::no_tool_call_can_execute_a_declared_generator` still passes
with demand in the arguments.

**Total: ~13 days**, of which M12 alone is useful on its own.

---

## 12. Open questions

1. **Does a pact example body over-declare?** Consumers are told to include only
   what they assert on, and routinely paste whole payloads. Over-attribution
   errs toward blocking, which is the safe direction for a brake — but it makes
   `escalate` noisier than it looks. Worth measuring against a real pact
   directory before `escalate` is recommended anywhere.
2. **Several pacts for one consumer/provider pair**, from different consumer
   versions. Union is the obvious answer and is what M12 does. The alternative —
   refusing, on the grounds that the newest should win — needs a version
   ordering `brake` does not have without the registry it is not building.
3. **Should `param-removed` escalate by default** when a consumer sends it? It
   is a warning today only because `brake` could not tell. This is the strongest
   case for making `escalate` the default once §12.1 is measured.
4. **`consumer-request-rejected` and path parameter values.** Binding records
   that a consumer sends `id=abc`; narrowing `{id}` to `integer` should be
   authoritative for that consumer. Whether the same inference is safe for
   header and query values, where pact fixtures are often placeholders, is
   less clear.
