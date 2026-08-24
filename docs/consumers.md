# Consumer demand

By default a break is reported as a break: something a consumer *could* be
relying on has gone. Declare what your consumers actually use and `brake` stops
guessing — a finding names who it breaks, and where they said so.

```
error[response-field-removed]: response field `customer_id` was removed
  --> api/payments-openapi.yaml:142:9
   |
   = note: breaks web-checkout — pacts/web-checkout-payments.json:88
```

A declaration is a file, and `brake` already reads files. Nothing here adds a
network call, a subprocess or a server.

## What it is not

**A green `brake` run is not a passing pact verification, and is never reported
as one.** `brake` checks that the *specification* still satisfies what
consumers declared. Whether the implementation matches its own specification is
what `--drift` and your test suite are for.

`brake` is not a broker client and not a pact generator. There is no
`can-i-deploy`, no environments, no deployment state. It never fetches a pact:
a `source` that looks like a URL is refused when `brake.toml` is parsed. Have
CI write the directory and point `source` at the path — a failed pull then
leaves the declared file absent, which is `consumer-unreachable` and exit `1`,
loud rather than clean.

## Declaring one

```toml
[[consumer]]
name = "web-checkout"                       # optional — a pact names itself
format = "pact"                             # pact | graphql-operations | manifest
source = "pacts/web-checkout-*.json"        # globs allowed, expanded and sorted
provider = "payments"                       # optional — defaults to the declared provider
```

Globs are expanded and sorted byte-wise before use, so a run over a directory
listing is deterministic. A declaration whose `provider` names no `[[contract]]`
is `consumer-provider-unmatched`, at `error`: a declaration nobody verifies is
worse than none, because it reads as coverage.

## The three formats

They differ in **fidelity**, and `brake` treats them differently because of it.

### `pact` — Pact v2/v3/v4 HTTP interactions

```json
{
  "consumer": { "name": "web-checkout" },
  "provider": { "name": "payments" },
  "interactions": [
    {
      "description": "a request for one payment",
      "request": { "method": "GET", "path": "/payments/9f01e2b7-…" },
      "response": {
        "status": 200,
        "headers": { "Content-Type": "application/json" },
        "body": { "id": "…", "amount": 4200, "status": "settled" }
      }
    }
  ]
}
```

A pact records **one example value**, not a schema. So a field appearing in the
example body is evidence the consumer reads it — and nothing more. `brake`
copies formats, bounds, nullability and enum membership from the contract
before comparing, because a demand is silent about them. Without that, every
`format: uuid` in your spec would become a false `consumer-request-rejected`,
and the hook would be uninstalled by Friday.

A pact's paths are concrete (`/payments/42`); the contract's are templates
(`/payments/{id}`). Binding one to the other needs the contract, and a
concrete path that matches more than one template is
`consumer-path-ambiguous`, at `warning` — not silently resolved.

Matchers `brake` does not model — `arrayContains`, `values`, `contentType`,
`semver` — are named as `consumer-partial` rather than ignored.

### `graphql-operations` — the consumer's own query documents

```graphql
query Checkout($id: ID!) {
  payment(id: $id) {
    id
    amount
    status
  }
}
```

The strongest of the three. A selection set *is* the field list, with no
inference at all: there is no example to generalise from and nothing to guess.
The consumer's name comes from the file, because an operation document has no
field for it and taking it from the first operation would produce a different
consumer per query.

A GraphQL *schema* is not a demand: a document with no executable operation is
refused.

### `manifest` — `*.brake-uses.toml`

The fallback: for gRPC, for consumers with no pact tests, and for third parties
who will only tell you in prose.

```toml
consumer = "billing-batch"
provider = "payments"

[[uses]]
endpoint = "GET /payments/{id}"     # already templated — no binding needed
statuses = ["200", "404"]           # omitted means the success response
reads = ["id", "amount", "customer.name"]
sends = []
```

Fidelity is whatever the author wrote, and `brake` says so: a manifest lists
field *paths*, so it declares presence and nothing more.

## What it checks

Verification is [`compare/types.rs`](../src/compare/types.rs) run sideways, not
a second comparator: the consumer's expectation goes on the *base* side and the
head contract on the head side, and the ordinary rules run. That is why there
is one notion of "incompatible" in the tool rather than two that drift apart.

Ten rules come out of it — the full text is in the [rule
catalogue](rules.md#consumer-endpoint-unmet):

| Rule | Severity | Fires when |
| --- | --- | --- |
| `consumer-endpoint-unmet` | error | A consumer calls an endpoint the contract does not document |
| `consumer-status-unmet` | error | A consumer expects a status, or that status in the media type it reads, that the contract does not document |
| `consumer-field-unmet` | error | A consumer reads a response field the contract does not produce |
| `consumer-request-rejected` | error | The contract would reject a request the consumer sends |
| `consumer-unreachable` | error | A declared source does not resolve or fails to parse |
| `consumer-provider-unmatched` | error | A declaration names a provider no `[[contract]]` declares |
| `consumer-partial` | warning | An interaction contains a construct `brake` cannot model |
| `consumer-path-ambiguous` | warning | A concrete path matches more than one contract template |
| `consumer-undeclared` | info | A file parses as a consumer declaration but `brake.toml` does not declare it |
| `consumer-surface-unused` | info | No declared consumer uses this endpoint |

## Attribution is evidence, not a finding

There is no `consumer-break` rule, deliberately. A break is already a finding;
the consumers it affects are attached to it as `affects`. One broken field must
not produce one `response-field-removed` plus three `consumer-break`s, because
a developer then has to work out that four findings are one problem.

`affects` is on every finding, in every format — text, JSON and SARIF.

## Policies — `[consumers]`

```toml
[consumers]
policy = "annotate"          # annotate | escalate | triage
completeness = "open-world"  # open-world | closed-world
```

| Policy | Effect |
| --- | --- |
| `annotate` | **Default.** Severities unchanged; affected consumers are named on the finding |
| `escalate` | A `warning` with a declared consumer becomes an `error`. `param-removed` and `security-removed` are warnings *precisely because* `brake` could not tell whether anyone relied on them — now it can |
| `triage` | An `error` no declared consumer can observe is downgraded to a `warning`. The one policy that can lie, and therefore the constrained one |

`triage` is honest only under four constraints, all enforced:

1. It requires `completeness = "closed-world"` — an explicit, reviewable
   assertion by a human that the declared set is exhaustive. `brake` cannot
   verify that claim and does not pretend to.
2. It applies only to rules the catalogue marks observable by demand. A pact
   says nothing about `operation-id-changed` or `path-parameter-renamed` —
   those break generated client *code*, which no declaration models. A rule
   demand cannot see is never downgraded on the strength of demand's silence.
3. The floor is `warning`. Nothing is downgraded to nothing, and nothing is
   suppressed — a suppression still requires a written reason, as it should.
4. Every downgraded finding renders the assumption it rests on, including how
   many consumers were declared.

`completeness = "closed-world"` also enables `consumer-surface-unused`, the one
rule that reports a suspected *absence*. It is produced by `brake analyze` and
`brake consumers` only, never by the commit gate, and without the closed-world
declaration it is off entirely — "nobody uses this" is not something a file
reader can know.

## Taking inventory

```sh
brake consumers          # JSON when piped, text on a terminal
brake consumers -f text
```

```
payments — api/payments-openapi.yaml

  web-checkout   pacts/web-checkout-payments.json  sha256:d2f56af7
    GET  /payments              200   reads: items.amount, items.id, items.status
    GET  /payments/{id}         200,404 reads: amount, id, status

  2 of 2 endpoints have a declared consumer.
  brake knows about the consumers declared in brake.toml and no others.
```

`consumers` always exits `0` — it is inventory, not a gate. Each declaration is
listed with its path and a **content digest**, because `brake` does not measure
freshness: a pact from eighteen months ago and one from this morning are the
same bytes to a file reader. The digest is what lets a reviewer notice that the
verdict rested on something stale.

## From an agent

`who_consumes` answers the same question over MCP, one endpoint or field at a
time, *before* the edit is drafted. See [the MCP guide](mcp.md).

## Further reading

[design/05-consumer-demand.md](../design/05-consumer-demand.md) carries the
specification, including §7.2 — the argument for why `triage` is constrained
the way it is.
