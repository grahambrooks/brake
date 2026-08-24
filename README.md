<img src="docs/assets/brake-logo.svg" alt="" align="right" width="116" height="116">

# brake

[![build](https://github.com/grahambrooks/brake/actions/workflows/build.yml/badge.svg)](https://github.com/grahambrooks/brake/actions/workflows/build.yml)
[![release](https://github.com/grahambrooks/brake/actions/workflows/release.yml/badge.svg)](https://github.com/grahambrooks/brake/actions/workflows/release.yml)
[![crates.io](https://img.shields.io/crates/v/brake.svg?logo=rust)](https://crates.io/crates/brake)
[![docs.rs](https://img.shields.io/docsrs/brake?logo=docsdotrs&label=docs.rs)](https://docs.rs/brake)
[![MSRV](https://img.shields.io/crates/msrv/brake?logo=rust&label=MSRV)](rust-toolchain.toml)
[![MCP](https://img.shields.io/badge/MCP-server-6E56CF)](docs/mcp.md)
[![licence](https://img.shields.io/crates/l/brake.svg)](LICENSE)

**A brake on breaking API changes.** It compares an API contract against its
previous version and fails the commit when the change would break a consumer —
across OpenAPI, protobuf, GraphQL and AsyncAPI, from one CLI and one ruleset.

**It never makes a network request, never runs your service, and never needs a
toolchain.** `brake` works on a fresh checkout with nothing installed, by
reading the contract files and the git history already in the repository. That
constraint is the reason it can live in a pre-commit hook, and it is also the
reason some checks are unavailable for some specs — which `brake` says out loud
rather than reporting a clean result it cannot justify.

```
$ brake check api/payments-openapi.yaml

error[response-field-removed]: response field removed: response `200` at `/customer_id`: field `customer_id` in `GET /payments/{id}`
 --> api/payments-openapi.yaml:142:9
    |
142 |         customer_id:
    |         ^^^^^^^^^^^ here
    |
    = note: contract: `payments`
help: three ways to make this change safely
      1. deprecate-then-remove — mark `customer_id` deprecated now and remove it
         in a later release, once consumers have had a version to migrate
         costs: the removal waits for a deprecation window you have to actually
                observe
      2. expand-then-contract — add the replacement alongside `customer_id`, move
         readers across, and remove `customer_id` only when nothing reads it
         costs: both shapes are live at once, and the second half is easy to forget
      3. version-the-endpoint — serve the change at a new path, media type or
         version header, leaving `GET /payments/{id}` answering as it does today
         costs: two implementations to maintain until the old one is retired

      which one fits depends on whether you control every consumer — brake
      cannot see that.
      run `brake explain response-field-removed` for why this breaks
```

It does not stop at "no". Every rule that reports a break carries the ways to
make the same change safely, named and costed, bound to the field it is about.
brake does not choose between them — which one fits depends on whether you
control every consumer and whether you have a version scheme, and it can see
neither.

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

**Working.** OpenAPI 3.0/3.1, protobuf 3, GraphQL SDL and AsyncAPI 2.x/3.x are
all ingested and compared through one ruleset; `check`, `analyze`, `diff`,
`explain` and `consumers` are implemented, with text, JSON, SARIF, GitHub
workflow-command and GitLab Code Quality output.

A contract may span several files — a `$ref` into a sibling document is
resolved, within that document's directory, with no network request and no
bundler. The baseline reads its siblings from the baseline's own revision, so a
field deleted from a shared schema is still a removal.

Consumer declarations — pact files, GraphQL operation documents, native
manifests — are a third input, so a finding can name *who* it breaks rather
than reporting that somebody might be.

Read [design/01-thesis.md](design/01-thesis.md) first — it carries the product
claim and, more usefully, the list of things deliberately not being built.
[docs/rules.md](docs/rules.md) is the rule catalogue.

## Install

```sh
brew tap grahambrooks/brake https://github.com/grahambrooks/brake
brew install brake
```

The tap needs the URL because the formula lives in this repository rather than
a separate `homebrew-brake` tap. Prebuilt binaries for macOS (Apple Silicon and
Intel), Linux (x86_64 and arm64) and Windows are attached to each
[release](https://github.com/grahambrooks/brake/releases), with a `SHA256SUMS`
file beside them.

Or from source, which needs a Rust toolchain:

```sh
cargo install brake                 # the CLI
cargo install brake --features mcp  # …and the MCP server
```

The released binaries include the MCP server; a `cargo install` without the
feature does not, and `brake mcp` will say so.

## Quick start

```sh
brake init      # finds your contracts and writes brake.toml
```

It detects contracts by **parsing** them, not by guessing from filenames, so a
CI workflow that happens to be called `api-tests.yaml` is not mistaken for an
API. `--dry-run` shows what it would write.

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

```sh
brake check api/payments-openapi.yaml   # at commit time, scoped to the change
brake check --since origin/main         # on a pull request
brake analyze .                         # everything, in CI
brake diff                              # describe the change, never fail
brake consumers                         # who uses what, and what of it
brake explain response-field-removed    # why a rule exists
```

## Naming who a change breaks

Declare what your consumers actually use and `brake` stops reporting that a
change *might* break somebody:

```toml
[[consumer]]
format = "pact"                                  # or graphql-operations, or manifest
source = "pacts/web-checkout-payments.json"      # globs allowed, sorted before use
```

```
error[response-field-removed]: response field `customer_id` was removed
  --> api/payments-openapi.yaml:142:9
   |
   = note: breaks web-checkout — pacts/web-checkout-payments.json:88
```

A declaration is a file, and `brake` already reads files: nothing here adds a
network call, a subprocess or a server. It never fetches a pact from a broker —
have CI write the directory and point `source` at the path. A declared file that
is absent is `consumer-unreachable` and exit `1`, loud rather than clean.

**A green `brake` run is not a passing pact verification, and is never reported
as one.** `brake` checks that the *specification* still satisfies what consumers
declared; whether the implementation matches its own specification is what
`--drift` and your test suite are for. See
[design/05-consumer-demand.md](design/05-consumer-demand.md).

As a pre-commit hook, in another repository:

```yaml
repos:
  - repo: https://github.com/grahambrooks/brake
    rev: v2026.8.4
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

`brake mcp` serves the same ruleset over MCP, so a coding agent learns that
renaming a response field breaks consumers *while it is drafting the change*
rather than when the hook rejects it. `check_change` takes the proposed
document as text, so a draft that has not been written to disk can still be
checked.

```sh
cargo install brake --features mcp
```

```jsonc
// claude_desktop_config.json, or any MCP client
{
  "mcpServers": {
    "brake": { "command": "brake", "args": ["mcp", "/path/to/your/repo"] }
  }
}
```

Five tools — `check_change`, `compare_contracts`, `who_consumes`,
`explain_rule` and `check_repository` — plus the rule catalogue, the evolution
strategies, the resolved configuration and the consumer inventory as readable
resources. `compare_contracts` needs no `brake.toml` at all, so it works on a
repository the agent has never configured, and `who_consumes` answers *who
breaks* while the edit can still be reconsidered.

Four [agent skills](docs/agent-skills.md) ship alongside it, in
`.claude/skills/`: when to consult `brake`, and how to read the answer without
over-claiming.

It exposes **no way to run a declared generator command**. `--drift` executes a
command out of a config file, and a tool that honoured it would hand arbitrary
command execution to anything that can write `brake.toml`. Drift stays a CLI
concern, run by a person or a CI job that chose to.

The feature is not on by default: the MCP server needs an async runtime, and a
crate that is otherwise synchronous should not pay for one unless it is asked.
`brake --help` lists `mcp` on every build regardless, and a build without the
feature tells you how to get it rather than pretending the command does not
exist.

See [design/04-mcp-interface.md](design/04-mcp-interface.md).

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | No finding at or above the threshold |
| `1` | At least one finding at or above the threshold |
| `2` | Tool failure — baseline unresolvable, source unreadable |

The `1` / `2` split is the one that matters. CI must distinguish "your API
broke" from "the gate is broken", because the correct response differs and
conflating them trains a team to ignore both.

## Output for CI

`--format github` emits GitHub Actions workflow commands, so findings appear as
inline pull-request annotations with no upload step and no extra permissions.
`--format gitlab` emits a GitLab Code Quality report for the merge-request
widget. `sarif` still feeds code scanning, and `json` feeds your own tooling.
See [docs/ci.md](docs/ci.md#output-for-ci).

## Documentation

| Guide | What it covers |
| --- | --- |
| [Getting started](docs/getting-started.md) | Install, `brake init`, your first finding, and how to read one |
| [Configuration](docs/configuration.md) | Every key in `brake.toml`: contracts, baselines, levels, suppressions, drift |
| [Contract formats](docs/formats.md) | What each of the four ingesters models, and contracts that span several files |
| [Consumer demand](docs/consumers.md) | Pacts, GraphQL operations and manifests, so a finding names *who* it breaks |
| [CI and hooks](docs/ci.md) | The pre-commit hook, GitHub Actions, SARIF, and the exit-code split |
| [MCP server](docs/mcp.md) | The tool surface an agent sees, and the trust posture that constrains it |
| [Agent skills](docs/agent-skills.md) | The four skills in `.claude/skills/`, and how to install them elsewhere |
| [Rule catalogue](docs/rules.md) | Every rule, generated from `src/rules/catalogue.rs` |
| [Design specification](design/) | Why it is shaped this way, and what is deliberately not being built |

Reference examples and expected outcomes for OpenAPI, protobuf and GraphQL are
in [docs/user-guide-test-cases.md](docs/user-guide-test-cases.md); the same
matrix is exercised by `tests/user_guide_cases.rs`.

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
- **Not a runtime contract tester.** `brake` reads a pact; it never replays one.
  Provider verification needs both sides running, and `brake` never issues a
  request.
- **Not a broker client, and not a pact generator.** No `can-i-deploy`, no
  environments, no deployment state. `brake` reads consumer declarations and
  never writes one.
- **Not a code generator.** It gates `progenitor` and `utoipa` output for drift;
  it does not replace them.
- **Not a registry.** No version history, no server, no stored expectation
  timeline. The baseline is a file or a git ref, and a consumer declaration is a
  file in the tree — both resolved locally.

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
