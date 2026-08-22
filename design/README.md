# brake design specification

These documents define what `brake` should be before it is built. They exist so
implementation can start anywhere without re-deriving the same decisions, and so
that a decision which turns out to be wrong can be found and changed in one
place.

**Read [01-thesis.md](01-thesis.md) first.** It carries the product claim, the
argument for this being a standalone tool rather than a feature of
[forge](https://github.com/grahambrooks/forge) or
[tropism](https://github.com/grahambrooks/tropism), and — most importantly — the
list of things deliberately not being built.

| Document | What it settles |
| --- | --- |
| [01-thesis.md](01-thesis.md) | Why the tool exists, the name, the two scopes, and what is out of scope on purpose |
| [02-contract-gates.md](02-contract-gates.md) | The specification: configuration, the contract model, compatibility levels, the rule catalogue, determinism guarantees, the interface |
| [03-implementation-plan.md](03-implementation-plan.md) | Crate shape, module layout, public API, dependency decisions, milestones with completion conditions, test strategy, risks |

## Status

Nothing is implemented. M0 — the scaffold — is complete; M1 is the walking
skeleton described in [03-implementation-plan.md](03-implementation-plan.md) §5.

## The shape of the argument

`brake` is a specification for a check, and a check is only worth as much as its
worst false positive. Three claims are load-bearing and each is defended by a
test rather than by assertion:

1. **It is hermetic.** No network, no toolchain, no running service — so it can
   live in a pre-commit hook.
2. **It is deterministic.** Same inputs, same verdict, same bytes.
3. **It says so when it cannot tell.** A clean result it cannot justify is worse
   than no result, because it manufactures confidence.

The third is the one inherited most directly from tropism, and the one most
often skipped by tools in this category.
