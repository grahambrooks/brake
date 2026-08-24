# Getting started

Ten minutes from nothing to a gate that fails the commit when your API breaks.

## Install

```sh
brew tap grahambrooks/brake https://github.com/grahambrooks/brake
brew install brake
```

The tap needs the URL because the formula lives in this repository rather than
a separate `homebrew-brake` tap.

Prebuilt binaries for macOS (Apple Silicon and Intel), Linux (x86_64 and
arm64) and Windows are attached to every
[release](https://github.com/grahambrooks/brake/releases), with a `SHA256SUMS`
file beside them. Or, with a Rust toolchain:

```sh
cargo install brake                 # the CLI
cargo install brake --features mcp  # …and the MCP server
```

The released binaries already include the MCP server. A `cargo install`
without the feature does not, and `brake mcp` says so rather than pretending
the command does not exist.

## What brake needs

Two things, both already in your repository:

- **A contract** — an OpenAPI 3.0/3.1 document, a protobuf 3 file, a GraphQL
  SDL schema, or an AsyncAPI 2.x/3.x document. It may span several files: a
  `$ref` into a sibling document is resolved, within that document's directory
  and without a network request.
- **A baseline** — the previous version to compare against. A git ref, a tag,
  or a second file. Nothing is fetched and nothing is stored: the baseline is
  resolved from the repository on every run.

There is no server, no registry, no state file, and nothing to regenerate
after a refactor.

## Scaffold the configuration

```sh
brake init
```

`init` finds your contracts by **parsing** them, not by guessing from
filenames — so a CI workflow that happens to be called `api-tests.yaml` is not
mistaken for an API. `brake init --dry-run` prints what it would write without
writing it; `--force` overwrites an existing `brake.toml`.

```toml
# brake.toml, written for you — edit freely, brake never rewrites it
[defaults]
compatibility = "wire-json"
baseline = { git-merge-base = "origin/main" }

[[contract]]
name = "payments"
format = "openapi"
source = "api/payments-openapi.yaml"
```

Every key is documented in [Configuration](configuration.md).

## Your first check

```sh
brake check                 # every configured contract
brake check api/payments-openapi.yaml   # only the contracts among these paths
brake check --since origin/main         # only what this branch changed
```

Remove a response field and run it again:

```
$ brake check

error[response-field-removed]: response field removed: response `200` at `/customer_id`: field `customer_id` in `GET /payments/{id}`
  --> api/payments.baseline.yaml:19:19
   |
19 |                   customer_id: { type: string }
   |                   ^^^^^^^^^^^ here
   |
   = note: contract: `payments`
help: three ways to make this change safely
      1. deprecate-then-remove — mark `customer_id` deprecated now and remove it in a later release, once consumers have had a version to migrate
         costs: the removal waits for a deprecation window you have to actually observe
      2. expand-then-contract — add the replacement alongside `customer_id`, move readers across, and remove `customer_id` only when nothing reads it
         costs: both shapes are live at once, and the second half is easy to forget
      3. version-the-endpoint — serve the change at a new path, media type or version header, leaving `GET /payments/{id}` answering as it does today
         costs: two implementations to maintain until the old one is retired

      which one fits depends on whether you control every consumer — brake cannot see that.
      run `brake explain response-field-removed` for why this breaks

$ echo $?
1
```

## Reading a finding

```
error[response-field-removed]: <what changed> in `GET /payments/{id}`
└─┬─┘ └────────┬────────────┘
  │            └─ the rule id. `brake explain <id>` for the reasoning.
  └─ the severity, after any consumer policy has been applied.

  --> api/payments.baseline.yaml:19:19    the span: file, line, column.
```

The span points at the **evidence**, which is not always the line you will
edit. For a removal it is the baseline — the removed field no longer has a
line in the head document, and pointing at where it used to be is the only
honest location. For a consumer finding it is the interaction in the pact that
declares the expectation, because that is *why* you cannot make the change;
the contract is merely *where* you would make it.

`help:` is not decoration. Every rule that reports a break carries the ways to
make the same change safely, named and costed, bound to the field it is about.
`brake` does not choose between them — which one fits depends on whether you
control every consumer and whether you have a version scheme, and it can see
neither.

## Exit codes

| Code | Meaning | What CI should do |
| --- | --- | --- |
| `0` | No finding at or above the threshold | Proceed |
| `1` | At least one finding at or above the threshold | Fail the build — the API broke |
| `2` | Tool failure — baseline unresolvable, source unreadable | Fail the build — *the gate* broke |

The `1` / `2` split is the one that matters. Conflating "your API broke" with
"the gate is broken" trains a team to ignore both.

## The other commands

```sh
brake analyze .                       # every contract, every rule, for CI
brake diff                            # describe the change, never fail
brake consumers                       # who uses what, and what of it
brake explain response-field-removed  # why a rule exists
brake mcp .                           # serve the ruleset to a coding agent
```

`diff` and `consumers` always exit `0`. They are for pull-request descriptions,
changelog drafting and inventory — not for gating.

## Output formats

`--format` (`-f`) takes `auto`, `text`, `json`, `sarif`, `github` or `gitlab`.
`auto` — the default — renders text to a terminal and JSON to a pipe, so
`brake consumers | jq` works without a flag and a hook still prints something a
human can read.

`github` emits GitHub Actions workflow commands and `gitlab` emits a GitLab
Code Quality report; both are covered in [CI and hooks](ci.md#output-for-ci).
`consumers` renders text or JSON only — there is no finding to report, so the
annotation formats have nothing to say.

## Where next

- [Configuration](configuration.md) — baselines, compatibility levels,
  suppressions.
- [CI and hooks](ci.md) — install it as a pre-commit hook and as a CI job.
- [Consumer demand](consumers.md) — make a finding name *who* it breaks.
- [Rule catalogue](rules.md) — every rule, with the reasoning.
