---
name: brake-consumer-impact
description: Find out which declared consumers use an API endpoint or field before removing, renaming or narrowing it, using brake's consumer demand inventory (pact files, GraphQL operation documents, brake-uses manifests). Use when asked "who uses this endpoint?", "is anyone reading this field?", "can we drop this response field?", "what breaks if I remove this?", or before deleting anything from an OpenAPI, protobuf or GraphQL contract.
argument-hint: "<endpoint or field, e.g. GET /payments/{id} customer_id>"
---

# Who breaks if this goes away

`brake` joins the contract against what consumers **declared** they use — pact
files, GraphQL operation documents, and `*.brake-uses.toml` manifests already in
the repository. Ask it before the edit, while the edit can still be
reconsidered.

## Ask it

**Over MCP**, if a `brake` server is connected:

```
who_consumes { "endpoint": "GET /payments/{id}", "field": "customer_id" }
```

`contract` is needed only when more than one is configured. Omit `field` for
the whole endpoint; omit `endpoint` for every endpoint the contract documents.

**From the CLI:**

```sh
brake consumers -f text        # the whole inventory
brake consumers                # JSON when piped — combine with jq
```

```
payments — api/payments-openapi.yaml

  web-checkout   pacts/web-checkout-payments.json  sha256:d2f56af7
    GET  /payments              200   reads: items.amount, items.id, items.status
    GET  /payments/{id}         200,404 reads: amount, id, status

  2 of 2 endpoints have a declared consumer.
```

`brake consumers` always exits `0`. It is inventory, not a gate.

## Read the answer correctly

**An empty answer means nobody *declared* it — not that nobody uses it.**

This is the single most important thing about this tool. `brake` knows about
the consumers declared in `brake.toml` and no others. Unless
`completeness = "closed-world"` is set in `[consumers]` — an explicit,
reviewable assertion by a human that the declared set is exhaustive — an
undeclared consumer is entirely possible, and `brake` cannot see it.

So:

- **Named consumers** → real evidence. Say who breaks, and cite the file and
  line the declaration came from. That is the finding's evidence, not decoration.
- **No consumers, open world** → "no *declared* consumer reads this". Do not
  report it as "safe to remove". Say what was checked and what was not, and let
  the human decide.
- **No consumers, closed world** → the repository asserts the set is complete.
  Still say that the verdict rests on that assertion.

Each declaration is listed with a **content digest**, because `brake` does not
measure freshness — a pact from eighteen months ago and one from this morning
are the same bytes to a file reader. If a decision turns on the answer, check
that the declarations are current.

## Fidelity differs by source

| Source | What a usage means |
| --- | --- |
| `graphql-operations` | The strongest. A selection set *is* the field list — no inference |
| `pact` | One recorded example. A field in the body is evidence it is read; it says nothing about types, formats, bounds or enum membership |
| `manifest` | Whatever the author wrote. Field paths only: presence, nothing more |

Never claim a pact proves a type constraint. It records a value, not a schema.

## Then what

If a consumer is named and the change still has to happen, that is
`/api-compatibility`: the finding will carry named, costed ways to make the
same change without breaking them.

If the declarations are missing entirely — no `[[consumer]]` blocks — the
honest answer is "this repository declares no consumers, so brake cannot name
anyone". Offer to add declarations rather than inferring an answer from
silence; `docs/consumers.md` covers the three formats.

## What this is not

`brake` never fetches a pact from a broker. There is no `can-i-deploy`, no
environments, no deployment state. It reads a directory a prior CI step wrote.
A declared file that is absent is `consumer-unreachable` and exit `1` — loud,
not clean — so a missing file is a broken gate, not an empty answer.
