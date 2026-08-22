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

Nothing is implemented. M0 (scaffold) is done; M1 is the walking skeleton in
[design/03-implementation-plan.md](design/03-implementation-plan.md) §5.

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
cargo run -- check api/openapi.yaml
```

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
