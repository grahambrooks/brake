# 01 — Thesis

This document exists so implementation can start anywhere without re-deriving
the decisions, and so a decision that turns out to be wrong can be found and
changed in one place. It remains the source of truth for scope: the
"Deliberately not built" list below is still binding.

---

## One sentence

> **A brake on breaking API changes: one compatibility ruleset, enforced at
> commit time and over the whole repository, with no network, no toolchain, and
> no running service.**

## The name

**`brake` is an anagram of `break`.**

```diff
@@ the same five letters, one transposition @@

- b r e a k
+ b r a k e
```

Nothing added, nothing left over — the naming lineage runs through
[Hamcrest](https://hamcrest.org/) (*matchers*) and
[tropism](https://github.com/grahambrooks/tropism) (*imports*).

The meaning has to land as well as the letters, and it does. A brake is a device
that resists motion in the direction you do not want to travel. It does not stop
the vehicle; it stops the vehicle going somewhere bad. That is the whole
product: an API may grow forward, and may not roll backward onto its consumers.
A *breaking* change is precisely the thing a *brake* exists to arrest, and the
two words are spelled with the same letters.

The vocabulary extends, which is the test of a metaphor name that has to survive
in a CLI:

```sh
brake check api/openapi.yaml     # at the moment of commit, scoped to the change
brake analyze .                  # everything, in CI
brake release <finding>          # a suppression is releasing the brake — with a reason
```

"Who released the brake on this, and why?" is a better review question than "who
added an allowlist entry?"

Pronounced exactly like the thing it prevents. That is the joke.

---

## Why this is its own tool

This began as a specification for a feature inside
[forge](https://github.com/grahambrooks/forge), the architecture modelling DSL,
because forge already parses OpenAPI files and already models a `gate` element.
That spec was written, reviewed, and rejected. The reasoning is worth keeping,
because it is the argument for this repository existing.

### Why not forge

| Objection | Detail |
| --- | --- |
| **Adoption prerequisite is absurd** | To learn whether you broke your API you would first have to model your architecture in a DSL. That gates a near-universal need behind a niche commitment |
| **The hard part is not forge-shaped** | The structural type comparator is over half the work and has nothing to do with modelling, layout, or C4 |
| **It violates forge's own first principle** | "Every diagram is a projection of the shared semantic graph." A resolved OpenAPI schema is graph content that projects to no view and no layout |
| **It degrades forge's trust posture** | Contract gating wants git ref resolution and, for generated-code drift, subprocess execution. forge parses one file and draws pictures |

Forge keeps the part that genuinely needs a model, and consumes this crate to
get it — see §"What forge keeps" below.

### Why not tropism

Closer, and the fit is real: tropism is already the enforcement tool, with the
diagnostic renderer, the `check` / `analyze` split, exit codes, hermeticity, and
`unavailable`-not-false-clean. Two arguments defeated it anyway.

- **Tropism's unit of analysis is the import graph, and its extension axis is
  language providers.** Contract formats are an orthogonal axis. Adding OpenAPI
  as an eleventh "language" would distort the provider abstraction that every
  one of its checks is built on.
- **"Baseline" would mean two things in one tool.** Tropism's baseline is
  suppression state for findings. A contract baseline is a *previous version of
  a different artifact*. Same word, unrelated concept, in a tool where both
  would appear on the same command line.

### The acid test

*Is this useful with no architecture model and no import graph?*

Overwhelmingly yes — which is why it belongs inside neither. There is no
Rust-native OpenAPI breaking-change checker at all, despite a mature Rust
OpenAPI ecosystem (`progenitor`, `utoipa`, `typify`) that consumes those specs.
The audience for a standalone gate is everyone with an OpenAPI file. The
audience for a forge feature is forge users.

---

## What tropism taught this design

Two ideas are inherited outright, and one of them corrected an error in the
original spec.

**Scope is a better ratchet than a baseline.** A run scoped to changed files
passes on a repository with two hundred existing violations, as long as the
commit does not add a two-hundred-and-first — with no state file, no drift, and
nothing to regenerate after a refactor. The original spec had an
`allow` / `expires` / `stale-allow` apparatus; tropism's
`design/17-baselines.md` deliberately downgraded exactly that machinery in
favour of scoping. `brake check <files>` is therefore the primary ratchet and
the primary product, and the suppression list is a distant fallback.

**Never put an unreliable check in a hook.** A high-false-positive check is how
a hook gets disabled permanently. This sets the bar for which rules may run at
commit time: only those that report the *presence* of a breaking change, never
the suspected absence of something.

---

## Two scopes, one ruleset

| Surface | When | What runs |
| --- | --- | --- |
| `brake check <files>` | pre-commit hook | Rules against the baseline, scoped to the contract files in the change. Fast, no false positives, blocking |
| `brake check --since <ref>` | CI, on a pull request | The same, scoped to what the branch changed |
| `brake analyze .` | CI on main, release gate | Every contract in the repository, every rule, including advisory ones |

The two scopes share one ruleset and one diff engine. This is tropism's shape,
and it is the shape because a gate nobody can adopt is a gate nobody runs.

---

## Deliberately not built

Recorded explicitly so a later session does not "complete" something that was
cut on purpose.

- **Not a spec linter.** Style rules — operation IDs must be camelCase, every
  response needs an example — are `vacuum` and `spectral`'s job. If a rule
  cannot break a consumer, it does not belong here.
- **Not a runtime contract tester.** Pact-style consumer-driven verification
  requires running both sides. `brake` never issues a request.
- **Not a mock server or code generator.** `progenitor` and `utoipa` do this
  well. `brake` gates their output for drift; it does not replace them.
- **Not a registry.** No stored version history, no server, no `can-i-deploy`.
  The baseline is a file or a git ref, resolved locally.
- **No transitive compatibility modes.** Confluent's `BACKWARD_TRANSITIVE` needs
  the full history, which needs the registry that is not being built. `brake`
  compares exactly two versions.
- **No network access, ever.** Not behind a flag. `$ref`s resolving to a URL are
  refused, not fetched — remote refs are the single largest source of
  non-determinism in OpenAPI tooling.

---

## What forge keeps

Forge depends on this crate with `default-features = false` and implements only
the rules that need an architecture model, none of which need the diff engine:

| Rule | Needs |
| --- | --- |
| `contract-drift` | The `apis` block vs. the real artifact — set comparison, no baseline |
| `unenforced-contract-gate` | A modelled `gate` with no contract behind it |
| `ungated-public-api` | Trust boundary membership |
| Blast radius | The **catalog** (`.forge-catalog`), not a single model — cross-repo consumers are the ones that can actually break |

That is a two-day forge feature rather than a three-week one, and this
repository carries the part that is hard.

---

## See also

- [02-contract-gates.md](02-contract-gates.md) — the specification
- [03-implementation-plan.md](03-implementation-plan.md) — build order and milestones
