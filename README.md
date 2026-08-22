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

error[response-field-removed]: response field `customer_id` was removed
  --> api/payments-openapi.yaml:142:9
    |
142 |         customer_id:
    |         ^^^^^^^^^^^ present in the baseline, absent here
    |
note: GET /payments/{id}, response 200
note: baseline: origin/main:api/payments-openapi.yaml (merge-base 8743cba)
help: a consumer reading this field will break. Deprecate it for a release
      before removing it, or run `brake explain response-field-removed`
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

**Not implemented.** The design is complete and the scaffold is in place; see
[design/](design/) for the specification and
[design/03-implementation-plan.md](design/03-implementation-plan.md) for the
build order.

Read [design/01-thesis.md](design/01-thesis.md) first — it carries the product
claim and, more usefully, the list of things deliberately not being built.

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
