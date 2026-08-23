# brake

**A brake on breaking API changes.** It compares an API contract against its
previous version and fails the commit when the change would break a consumer —
across OpenAPI, protobuf, and GraphQL, from one CLI and one ruleset.

**It never makes a network request, never runs your service, and never needs a
toolchain.** `brake` works on a fresh checkout with nothing installed, by
reading the contract files and the git history already in the repository. That
constraint is the reason it can live in a pre-commit hook, and it is also the
reason some checks are unavailable for some specs — which `brake` says out loud
rather than reporting a clean result it cannot justify.

```
$ brake check api/payments-openapi.yaml

error[response-field-removed]: response field removed: field `customer_id` in `GET /payments/{id}`
 --> api/payments-openapi.yaml:142:9
    |
142 |         customer_id:
    |         ^^^^^^^^^^^ here
    |
    = note: contract: `payments`
help: Any consumer reading that field now gets nothing, and a consumer
      deserialising into a type with a non-optional field for it fails
      outright. Deprecate the field for a release before removing it — that is
      the sanctioned path, and a team that follows it never needs a suppression.

      run `brake explain response-field-removed` for the full rationale
```

## The name

**`brake` is an anagram of `break`.**

```diff
@@ the same five letters, one transposition @@

- b r e a k
+ b r a k e
```

Nothing added, nothing left over — the trick is borrowed from
[Hamcrest](https://hamcrest.org/), an anagram of *matchers*, and from
[tropism](https://github.com/grahambrooks/tropism), an anagram of *imports*.

A brake resists motion in the direction you do not want to travel. It does not
stop the vehicle; it stops the vehicle going somewhere bad. That is the entire
product: your API may grow forward, and may not roll backward onto the people
who depend on it. A *breaking* change is exactly the thing a *brake* exists to
arrest, and the two words are spelled with the same letters.

Pronounced like the thing it prevents.

## Status

**Working.** OpenAPI 3.0/3.1, protobuf 3 and GraphQL SDL are all ingested and
compared through one ruleset; `check`, `analyze`, `diff` and `explain` are
implemented, with text, JSON and SARIF output.

Read [design/01-thesis.md](design/01-thesis.md) first — it carries the product
claim and, more usefully, the list of things deliberately not being built.
[docs/rules.md](docs/rules.md) is the rule catalogue.

## Quick start

```toml
# brake.toml, at the repository root
[defaults]
compatibility = "wire-json"
baseline = { git-merge-base = "origin/main" }

[[contract]]
name = "payments"
format = "openapi"
source = "api/payments-openapi.yaml"
```

```sh
brake check api/payments-openapi.yaml   # at commit time, scoped to the change
brake check --since origin/main         # on a pull request
brake analyze .                         # everything, in CI
brake diff                              # describe the change, never fail
brake explain response-field-removed    # why a rule exists
```

As a pre-commit hook, in another repository:

```yaml
repos:
  - repo: https://github.com/grahambrooks/brake
    rev: v2026.8.0
    hooks:
      - id: brake
```

`brake check` takes paths, so the hook passes the changed files and brake checks
only the contracts among them. That scoping is the ratchet: a repository with
two hundred existing findings still passes a commit that does not add a
two-hundred-and-first, with no state file and nothing to regenerate.

## Gating a release, not just a commit

`git-merge-base` forgives anything already on the trunk, which is what makes
the commit gate adoptable — and wrong for a release. A break merged three weeks
ago is still a break for anyone upgrading from the last tag.

```toml
[[contract]]
name = "payments"
source = "api/payments-openapi.yaml"
baseline = { git-merge-base = "origin/main" }   # did *this change* break anything?

[[contract]]
name = "payments-released"
source = "api/payments-openapi.yaml"
compatibility = "surface"
baseline = { latest-tag = "v*" }                # is the delta since the last release safe?
```

Two contracts over one artifact is the intended shape: they ask different
questions and deserve different answers. `latest-tag` resolves the newest tag
matching the glob that HEAD descends from, so it needs no editing at release
time. `tag = "v1.2.0"` and `rev = "8743cba"` pin an exact version.

Tags live in git, so CI must fetch them — `actions/checkout` needs
`fetch-depth: 0`. A pattern that matches nothing is reported as a tool failure,
never as a clean result.

## Compatibility levels

Each level is a strict superset of the one below, so a project can start loose
and tighten without relearning the tool.

| Level | Catches | Use when |
| --- | --- | --- |
| `wire` | Endpoint or method removal, newly-required input, type narrowing, protobuf renumbering | Internal services, tolerant readers |
| `wire-json` | `wire` plus field rename, response field removal, status-code removal, security strengthening | **Default.** Most HTTP/JSON APIs |
| `surface` | `wire-json` plus anything that breaks generated client code — `operationId` change, path-parameter rename | Consumers generate clients |
| `strict` | Any non-additive change at all, including new optional fields | Frozen public APIs under contract |

## Using it from an agent

[design/04-mcp-interface.md](design/04-mcp-interface.md) specifies an MCP
server exposing the same ruleset at edit time, so a coding agent learns that
renaming a response field breaks consumers while it is drafting the change
rather than when the hook rejects it. Designed, not yet built.

It exposes no way to run a declared generator command: `--drift` executes a
command out of a config file, and a tool that honoured it would hand arbitrary
command execution to anything that can write `brake.toml`.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | No finding at or above the threshold |
| `1` | At least one finding at or above the threshold |
| `2` | Tool failure — baseline unresolvable, source unreadable |

The `1` / `2` split is the one that matters. CI must distinguish "your API
broke" from "the gate is broken", because the correct response differs and
conflating them trains a team to ignore both.

## User guide examples

Reference examples and expected outcomes for OpenAPI, Protobuf, and GraphQL are
documented in [docs/user-guide-test-cases.md](docs/user-guide-test-cases.md).
The same matrix is exercised in automated acceptance tests at
`tests/user_guide_cases.rs`.

## Two scopes, one ruleset

| Surface | When | What runs |
| --- | --- | --- |
| `brake check <files>` | pre-commit | The contracts among the changed files, against the baseline. Fast, blocking |
| `brake check --since <ref>` | CI, on a pull request | The same, scoped to the branch |
| `brake analyze .` | CI on main, release gate | Every contract, every rule, including advisory ones |

Scoping the run to the change is what makes this adoptable: a repository with
two hundred existing findings still passes every commit that does not add a
two-hundred-and-first — with no baseline file, no state, and nothing to
regenerate after a refactor.

## What it is not

- **Not a spec linter.** Style rules are `vacuum` and `spectral`'s job. If a
  rule cannot break a consumer, it is out of scope.
- **Not a runtime contract tester.** Pact-style verification needs both sides
  running. `brake` never issues a request.
- **Not a code generator.** It gates `progenitor` and `utoipa` output for drift;
  it does not replace them.
- **Not a registry.** No version history, no server, no `can-i-deploy`. The
  baseline is a file or a git ref, resolved locally.

## Library

`brake` is a library as much as a binary.
[forge](https://github.com/grahambrooks/forge) consumes it for the contract
rules that need an architecture model.

```toml
[dependencies]
brake = { version = "2026.8", default-features = false }
```

`default-features = false` drops `clap`, colour, and terminal handling, leaving
ingest, comparison, and the rule catalogue.

## Licence

MIT. See [LICENSE](LICENSE).
