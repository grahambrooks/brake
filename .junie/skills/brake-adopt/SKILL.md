---
name: brake-adopt
description: Set up brake in a repository so breaking API changes are caught at commit time and in CI — run brake init, write brake.toml, choose baselines and a compatibility level, install the pre-commit hook, and add the CI jobs. Use when asked to add API compatibility checking, prevent breaking changes to an API, gate an OpenAPI/protobuf/GraphQL contract, set up brake, or stop clients being broken by a spec change.
argument-hint: "[path to the contracts]"
---

# Adopting brake in a repository

The goal is a gate people keep. A gate that fires on files nobody asked it to
look at, or that fails a first commit with two hundred pre-existing findings,
gets uninstalled — and then nothing is checked at all.

Work in this order.

## 1. Let brake find the contracts

```sh
brake init --dry-run     # prints what it would write
brake init               # writes brake.toml
```

`init` identifies contracts by **parsing** them, not by filename, so a workflow
called `api-tests.yaml` is not mistaken for an API. It recognises OpenAPI,
protobuf, GraphQL SDL and AsyncAPI. Read the output with the
user before writing: it is their inventory of API surface, and it is often the
first time anyone has seen it in one place.

Add `--force` only to overwrite an existing `brake.toml`, and say so first.

## 2. Choose the baseline — this is the decision that matters

```toml
[defaults]
baseline = { git-merge-base = "origin/main" }
```

`git-merge-base` is the right default for a commit gate, and the reason is not
stylistic: it does not fire on breaks another pull request already landed, and
the merge-base advances on every merge, so history is forgiven automatically.
That is the ratchet — no baseline file, no state, nothing to regenerate after a
refactor.

It is the *wrong* baseline for a release. A break merged three weeks ago is
still a break for anyone upgrading from the last tag. If the project publishes
releases, add a second block over the same artifact:

```toml
[[contract]]
name = "payments"
format = "openapi"
source = "api/payments-openapi.yaml"
baseline = { git-merge-base = "origin/main" }   # did *this change* break anything?

[[contract]]
name = "payments-released"
format = "openapi"
source = "api/payments-openapi.yaml"
compatibility = "surface"
baseline = { latest-tag = "v*" }                # is the delta since the last release safe?
```

Two contracts over one file is the intended shape: they ask different questions.

## 3. Choose a level you can actually turn on today

| Level | Catches | Choose when |
| --- | --- | --- |
| `wire` | Endpoint/method removal, newly-required input, type narrowing, protobuf renumbering | Internal services, tolerant readers, or a repository with a backlog |
| `wire-json` | …plus field rename, response field removal, status-code removal, security strengthening | **Default.** Most HTTP/JSON APIs |
| `surface` | …plus what breaks generated client code — `operationId`, path-parameter names | Consumers generate clients |
| `strict` | Any non-additive change at all | Frozen public APIs under contract |

Run `brake analyze . --format text` before committing to a level. If it prints
a wall of findings, start one level lower, or start at `--severity error`, and
tighten later. Each level is a strict superset of the one below, so tightening
costs no relearning.

## 4. Install the commit hook

```yaml
# .pre-commit-config.yaml
repos:
  - repo: https://github.com/grahambrooks/brake
    rev: v2026.8.4           # use the current release
    hooks:
      - id: brake
```

The hook passes the changed files and `brake check` checks only the contracts
among them. Scoping to the change is what makes it adoptable on a repository
that already has findings.

Check that the hook's `files` pattern actually matches where this project's
contracts live — it covers `api/`, `contracts/`, `schemas/`, `proto/`,
`openapi*`/`swagger*` filenames and any `.proto`/`.graphql`/`.gql`. Override it
if they live elsewhere. Widening costs time, not correctness.

## 4b. Pick the CI output format

`--format github` puts findings on the diff as inline annotations with no
upload step; `--format gitlab` feeds the merge-request Code Quality widget;
`--format sarif` feeds GitHub code scanning. Choose the one the team already
looks at — a finding nobody sees is a gate nobody has.

## 5. Add the CI jobs

Two, doing different things:

```yaml
- uses: actions/checkout@v7
  with:
    fetch-depth: 0        # every baseline but `file` is resolved out of git
```

- On a pull request: `brake check --since origin/${{ github.base_ref }}`.
- On main, or before a release: `brake analyze .` — every contract, including
  the advisory rules that are only correct on a run that covered everything.

Make sure CI distinguishes exit `1` (the API broke) from exit `2` (the gate
broke). They need different responses, and conflating them teaches a team to
ignore both.

## 6. Offer consumer declarations, if they exist

If the repository (or a sibling) has pact files, GraphQL operation documents, or
can write a `*.brake-uses.toml`, declaring them turns "this might break
somebody" into "this breaks web-checkout, at this line".

```toml
[[consumer]]
format = "pact"                              # pact | graphql-operations | manifest
source = "pacts/web-checkout-payments.json"  # globs allowed; never a URL
```

`brake` never fetches a pact from a broker. Have CI write the directory and
point `source` at the path.

Leave `[consumers] policy` at the default `annotate` to begin with. `escalate`
and especially `triage` change verdicts, and `triage` requires an explicit
`completeness = "closed-world"` assertion that nobody should make on the first
day.

## 7. Prove it works before declaring victory

Make a deliberately breaking change — remove a response field — and confirm the
hook rejects it and the message is comprehensible. Then revert. An installed
gate nobody has seen fire is not yet known to be a gate.

## 8. Offer the MCP server

```sh
cargo install brake --features mcp     # released binaries already include it
claude mcp add brake -- brake mcp .
```

It serves the same ruleset, so an agent editing a contract learns that a rename
breaks consumers while drafting rather than at commit time.

## Do not

- Turn rules off. If a break is acceptable it is a `[[contract.allow]]` entry
  with a written reason and, preferably, an expiry date.
- Point a baseline or a consumer `source` at a URL. `brake` refuses it — the
  hermeticity is the reason it can run in a hook at all.
- Enable `--drift` in the commit hook by default. It runs a declared command;
  keep it to CI or an explicit local run.
