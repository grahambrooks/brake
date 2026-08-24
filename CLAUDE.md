# CLAUDE.md

Guidance for working in this repository.

## Overview

`brake` checks API contracts for backward compatibility and fails the commit
when a change would break a consumer. One ruleset, two scopes — `brake check`
on a change at commit time, `brake analyze` over the whole repository in CI.

> **A brake on breaking API changes: one compatibility ruleset, enforced at
> commit time and over the whole repository, with no network, no toolchain, and
> no running service.**

`brake` is an anagram of `break`. See [README.md](README.md).

**Read [design/](design/) before planning work.** It is the source of truth.
[design/01-thesis.md](design/01-thesis.md) carries the scope decisions,
including what is deliberately *not* being built — check it before
"completing" anything, because several omissions are on purpose.

## Status

M0–M6 are done, and so are M7 (protobuf) and M8 (GraphQL). All three ingesters
produce the same `Contract` and share one comparator; `check`, `analyze`,
`diff`, `explain` and `consumers` work; text, JSON and SARIF all render.

M12–M15, consumer demand
([design/05-consumer-demand.md](design/05-consumer-demand.md)), are done too:
pact, GraphQL-operation and manifest declarations, the join, the ten
`consumer-*` rules, `affects` on every finding, and `who_consumes` over MCP.

The rule catalogue lives in `src/rules/catalogue.rs` and is the single source
of truth — [docs/rules.md](docs/rules.md) is generated from it by `make docs`
and a test fails if the two drift.

What is worth knowing before changing anything:

- **The seven self-defence tests are in `tests/self_defence.rs`.** They defend
  the numbered guarantees in
  [design/02-contract-gates.md](design/02-contract-gates.md) §6.1, the last two
  over the demand axis
  ([design/05-consumer-demand.md](design/05-consumer-demand.md) §8). If one of
  them starts failing, a claim the README makes has stopped being true.
- **`tests/cli.rs` covers the binary's own surface.** A dropped argument in
  `main.rs` is invisible to library tests; that is how `brake check <path>`
  once ignored its paths and checked everything.
- **`brake` gates its own fixture contract** (`api/payments-openapi.yaml` via
  `brake.toml`). `make self-check` runs it.
- **Contract detection lives in `src/init.rs::identify` and works by parsing.**
  `brake init` and the `contract-unconfigured` notice share it, so they cannot
  disagree about what a contract is. Never reintroduce a filename heuristic:
  the previous one called `.github/workflows/api-tests.yaml` an API.
- **The MCP server is `src/mcp/`, behind the non-default `mcp` feature.**
  `handlers.rs` is synchronous and transport-free; `server.rs` is the `rmcp`
  adapter and the only file that knows about async. Keep the split: it is what
  makes the tools testable without a protocol.
- **Nothing in `src/mcp/` may reach the `--drift` subprocess path.**
  `design/04-mcp-interface.md` §5.1 is the reasoning, and
  `tests/mcp.rs::no_tool_call_can_execute_a_declared_generator` is the guard.
- **`src/demand/` never fetches anything, under any flag.** A URL in a pact —
  `_links`, `pb:publish`, a `$ref` in an example body — is data. A consumer
  `source` that is a URL is refused when `brake.toml` is parsed. `brake` reads
  the directory a prior CI step wrote; it does not talk to a broker.
- **Verification is `compare/types.rs` run sideways, not a second comparator.**
  The consumer's expectation goes on the *base* side, the head contract on the
  head side. If a demand-specific comparison appears in `src/demand/`, the
  projection is wrong — fix it in `contract/` or `compare/`.
- **A demand is silent about formats, bounds, nullability and enum membership.**
  A pact records one value, not a schema, so `verify::reconcile` copies those
  from the contract before comparing. Removing that turns every `format: uuid`
  into a false `consumer-request-rejected`, which is how a hook gets
  uninstalled.
- **Attribution is evidence on a finding, never a second finding.** There is no
  `consumer-break` rule, deliberately: one broken field must not produce four
  findings a developer has to reassemble.

## Core constraints

These are not negotiable and they drive the design.

1. **No network, ever.** Not behind a flag. A `$ref` resolving to a URL is
   refused, not fetched. Remote refs are the largest source of non-determinism
   in OpenAPI tooling.
2. **No toolchain, no build, no running service.** `brake` reads files. It does
   not invoke a package manager, compile anything, or start the API under test.
   This is what makes a pre-commit hook deployable.
3. **One subprocess, opt-in.** `brake check --drift` runs a declared generator
   command. It is the only place a subprocess is spawned, it is off by default,
   and it must stay unreachable without the flag — `brake check` has to be safe
   to run against an untrusted repository.
4. **Report `unavailable` rather than a false clean.** When a construct cannot
   be modelled, say so and name it. A tool that silently ignores what it cannot
   parse manufactures confidence.
5. **Deterministic, and provably so.** Same inputs, same verdict, same bytes.
   Guarantees are enumerated in
   [design/02-contract-gates.md](design/02-contract-gates.md) §6.1 and each is
   defended by a test.

## Layout

Single crate, two targets — not a workspace. The library is a product: forge
depends on it.

```
design/            The specification — read this first
src/lib.rs         Library: ingest, compare, rules, report
src/main.rs        CLI: arguments, rendering, exit codes
src/config.rs      brake.toml
src/contract/      Format ingesters → the normalised Contract model
src/compare/       Contract × Contract → Change. Format-agnostic
src/rules/         Change × Level → Finding. The rule catalogue
src/demand/        Consumer declarations → Demand → the join → attribution
src/baseline.rs    file / git / git-merge-base, via gix
src/render/        text, json, sarif
```

**Two structural rules:**

- `main.rs` may parse arguments, render output, and exit. It may **not** decide
  whether something is a breaking change. If a behaviour cannot be tested
  through the library, it is in the wrong file.
- `compare/` is format-agnostic. If a `match` on format appears there, the
  ingest normalisation is under-specified — fix it in `contract/`, not with a
  special case.

## Commands

```sh
make check          # fmt, clippy -D warnings, tests — the pre-commit gate
make build
make test
make docs           # regenerate docs/rules.md from the catalogue
make self-check     # run brake against its own fixture contract
cargo run -- init            # scaffold brake.toml by parsing what is there
cargo run -- check api/openapi.yaml
cargo run -- consumers               # who uses what, non-gating
cargo run --features mcp -- mcp .     # the MCP server, on stdio
```

`make check` builds `--all-features`, so it covers the MCP path too.

**`make check` is not the whole gate.** CI runs the tests on Linux, macOS *and*
Windows, and several jobs have no local equivalent — the MSRV pin, the
library-only build forge depends on, `cargo-deny`, and the generated-docs
check. A green `make check` on one machine is necessary, not sufficient: the
Windows `test` job stayed red for a while because `--drift` runs its command
through `cmd` there, which does not expand `$VAR`. Check the run after pushing.

`make check` must pass before every commit.

## Testing

- **Every rule needs a positive and a negative test.** It fires on the break,
  and stays quiet on the safe change. Silent false positives are how a hook gets
  uninstalled permanently.
- **Fixtures go under `tempfile::tempdir()`**, never the surrounding checkout. A
  test that inspects the ambient git repository passes on a laptop and fails in
  CI, which checks out a detached HEAD.
- **The five self-defence tests** in
  [design/03-implementation-plan.md](design/03-implementation-plan.md) §6 are not
  optional — they are what the determinism and hermeticity claims rest on.
- Snapshot diagnostics with `insta`.

## Conventions

- Trunk-based: commit directly to `main`.
- CalVer `YYYY.M.MICRO`, committed in `Cargo.toml`, bumped by `make release`.
  Never edit the version by hand.
- Conventional Commits (`feat(compare): ...`, `fix(openapi): ...`).
- MSRV tracks latest stable and is pinned in `rust-toolchain.toml`. Keep it in
  step with `rust-version` in `Cargo.toml`.
- `unsafe_code = "forbid"`.
