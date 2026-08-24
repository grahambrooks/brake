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
| [04-mcp-interface.md](04-mcp-interface.md) | The MCP server: the same ruleset consulted at edit time by a coding agent, its tool surface, and the trust posture that constrains it |
| [05-consumer-demand.md](05-consumer-demand.md) | Consumer declarations — pact files, GraphQL operations, native manifests — as a third input, so a finding can name who it breaks |
| [06-architectural-evolution.md](06-architectural-evolution.md) | Architectural evolution: schema evolution depth, event contracts, hermetic multi-document bundling, subtyping lattice, CI integration |

## Status

M0–M9 are built: all three ingesters, one comparator, the four compatibility
levels, suppressions, drift, and version-controlled baselines. See
[03-implementation-plan.md](03-implementation-plan.md) §5.

M10, the MCP interface in [04-mcp-interface.md](04-mcp-interface.md), is built
behind the non-default `mcp` feature.

M12–M15, consumer demand in [05-consumer-demand.md](05-consumer-demand.md), are
built: pact, GraphQL-operation and native-manifest declarations, the join, the
consumer rules, `affects` on every finding, the three policies, `brake
consumers` and `who_consumes` over MCP.

[06-architectural-evolution.md](06-architectural-evolution.md) is a **roadmap**,
and only partly built. Landed from it: the AsyncAPI ingester, hermetic
multi-document `$ref` resolution, OpenAPI 3.1 type arrays, `prefixItems` tuples
and `discriminator` mappings, protobuf `reserved` awareness, and the GitHub and
GitLab renderers. Not built: the `proto-field-unreserved` and
`proto-enum-value-unreserved` rules it proposes. `src/rules/catalogue.rs` is the
only list of rules that exist — a proposal in a design document is not one.

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
