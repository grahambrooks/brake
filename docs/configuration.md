# Configuration

Everything `brake` does is declared in one file: `brake.toml`, at the root of
the repository. `brake init` writes a first version by parsing what is
actually there; after that the file is yours and `brake` never rewrites it.

Unknown keys are a **parse error**, not a warning. A typo in a gate's
configuration silently disabling the gate is the failure mode that matters
most here, so `brake` refuses the file instead.

## Where brake looks for it

| Command | Discovery |
| --- | --- |
| `check`, `diff`, `consumers` | Walks up from the working directory until it finds `brake.toml` — so it works from a subdirectory, which is where you are when a hook fires |
| `analyze <path>` | `<path>/brake.toml` |
| any of them, with `--config <file>` | That file. Its directory becomes the repository root for every relative `source` |

## A complete file

```toml
[defaults]
compatibility = "wire-json"
baseline = { git-merge-base = "origin/main" }

[[contract]]
name = "payments"
format = "openapi"                        # openapi | proto | graphql
source = "api/payments-openapi.yaml"
compatibility = "surface"                 # optional, overrides [defaults]
baseline = { latest-tag = "v*" }          # optional, overrides [defaults]

  [[contract.allow]]
  rule = "response-field-removed"
  endpoint = "GET /payments/{id}"
  field = "legacy_reference"
  reason = "nothing has read it since the 2025 migration; tracked in PAY-4417"
  expires = 2026-09-01

  [contract.generated]
  command = "cargo run -p payments-api --bin openapi"

[[consumer]]
name = "web-checkout"                     # optional; a pact names itself
format = "pact"                           # pact | graphql-operations | manifest
source = "pacts/web-checkout-*.json"      # globs allowed, sorted before use
provider = "payments"                     # optional; defaults to the declared provider

[consumers]
policy = "annotate"                       # annotate | escalate | triage
completeness = "open-world"               # open-world | closed-world
```

## `[defaults]`

| Key | Values | Default |
| --- | --- | --- |
| `compatibility` | `wire`, `wire-json`, `surface`, `strict` | `wire-json` |
| `baseline` | any baseline shape below | none — a contract with no baseline reports `baseline-unconfigured` (info) and nothing is compared |

## `[[contract]]`

One block per artifact you want gated. The same file may appear in several
blocks; that is the intended shape when you want to ask different questions of
it — see [Two contracts over one artifact](#two-contracts-over-one-artifact).

| Key | Required | Meaning |
| --- | --- | --- |
| `name` | yes | How the contract is referred to in findings and by `--contract` |
| `format` | yes | `openapi`, `proto` or `graphql` |
| `source` | yes | Repository-relative path to the head document |
| `compatibility` | no | Overrides `[defaults]` for this contract |
| `baseline` | no | Overrides `[defaults]` for this contract |
| `allow` | no | Suppressions — repeated `[[contract.allow]]` blocks |
| `generated` | no | A generator command, for `--drift` |

## Baselines

A baseline sets exactly one key. Setting none, or more than one, is a
configuration error — which is what stops `{ tag = "v1", rev = "abc" }` from
silently picking one.

| Shape | Resolves to | Use for |
| --- | --- | --- |
| `{ file = "api/previous.yaml" }` | A second file in the tree | Fixtures, tests, vendored snapshots |
| `{ git-merge-base = "origin/main" }` | `source` as of the merge-base with that ref | **The commit gate.** Forgives anything already on the trunk |
| `{ latest-tag = "v*" }` | `source` at the newest tag matching the glob that HEAD descends from | **The release gate.** Needs no editing at release time |
| `{ tag = "v1.2.0" }` | `source` at that tag | Pinning an exact published version |
| `{ rev = "8743cba" }` | `source` at any revision — commit, branch or tag | Ad-hoc comparisons |
| `{ git = "origin/main:api/old-path.yaml" }` | An explicit `ref:path` pair | Only when the path moved; prefer the shapes above |

Tags live in git, so CI must fetch them: `actions/checkout` needs
`fetch-depth: 0`. A tag glob that matches nothing is reported as a **tool
failure** (exit `2`), never as a clean result.

Three outcomes are deliberately kept apart, because conflating them is how a
gate stops gating without anyone noticing:

| Outcome | Reported as | Exit |
| --- | --- | --- |
| No baseline configured — you have not opted in | `baseline-unconfigured`, info | `0` |
| Baseline resolved, but this contract is not in it — the contract is new | `contract-new`, info | `0` |
| Baseline configured and *unresolvable* — bad ref, glob matching nothing | unavailable, named | `2` |

### Two contracts over one artifact

`git-merge-base` is what makes the commit gate adoptable — it does not fire on
breaks another pull request already landed, and the merge-base advances on
every merge, so history is forgiven automatically. That is exactly the wrong
behaviour for a release: a break merged three weeks ago is still a break for
anyone upgrading from the last tag.

```toml
[[contract]]
name = "payments"
source = "api/payments-openapi.yaml"
format = "openapi"
baseline = { git-merge-base = "origin/main" }   # did *this change* break anything?

[[contract]]
name = "payments-released"
source = "api/payments-openapi.yaml"
format = "openapi"
compatibility = "surface"
baseline = { latest-tag = "v*" }                # is the delta since the last release safe?
```

Two blocks over one file is the intended shape. They ask different questions
and deserve different answers, and `--contract payments` lets a hook run only
the fast one.

## Compatibility levels

Each level is a strict superset of the one below, so a project can start loose
and tighten without relearning the tool. A rule outside the selected level does
not fire at all — it is not downgraded to a warning, because a warning is a
thing a human has to read and dismiss.

| Level | Adds | Use when |
| --- | --- | --- |
| `wire` | Endpoint or method removal, newly-required input, type narrowing, protobuf renumbering | Internal services, tolerant readers |
| `wire-json` | Field rename, response field removal, status-code removal, security strengthening | **Default.** Most HTTP/JSON APIs |
| `surface` | Anything that breaks generated client code — `operationId` change, path-parameter rename | Consumers generate clients |
| `strict` | Any non-additive change at all, including new optional fields | Frozen public APIs under contract |

Which rules belong to which level is in the [rule catalogue](rules.md).

## Suppressions — `[[contract.allow]]`

A suppression is a claim, made in the repository, reviewable in a diff, that a
particular break is acceptable. It is not a way to turn a rule off.

```toml
[[contract.allow]]
rule = "response-field-removed"                  # required
endpoint = "GET /payments/{id}"                  # optional; narrows to one endpoint
field = "legacy_reference"                       # optional; narrows to one field
reason = "nothing has read it since PAY-4417"    # required
expires = 2026-09-01                             # optional; bare or quoted date
```

- `field` matches a **path segment** of the finding's pointer, not the rendered
  message — so `field = "id"` cannot accidentally suppress `customer_id`.
- `expires` accepts TOML's native date (`2026-09-01`) or a quoted string
  (`"2026-09-01"`). Both are normalised, because the tool knows what was meant.
  Expiry is evaluated against today, or against `--as-of YYYY-MM-DD`.
- Once a suppression expires the finding comes back as **`expired-allow`**, at
  `error`. It does not quietly revert to the original rule id: the point is
  that the deadline you wrote down passed.
- A suppression that matched nothing is reported as **`stale-allow`** — but
  only on a run that covered everything (`brake analyze`). A scoped
  `brake check` legitimately never looks at the endpoint a suppression names,
  and reporting it as dead would make suppressions unusable in a hook.
- `contract-unreachable`, `stale-allow` and `expired-allow` are **never
  suppressible**. A suppression that could hide them would let the gate stop
  gating silently.

## Generator drift — `[contract.generated]`

When the contract is generated from code, the committed file can fall behind
the generator, and a gate that checks the stale file gates nothing.

```toml
[contract.generated]
command = "cargo run -p payments-api --bin openapi"
```

The command runs **only** under `brake check --drift` / `brake analyze
--drift`. This is the one place `brake` spawns a subprocess, it is off by
default, and it must stay unreachable without the flag — `brake check` has to
be safe to run against a repository you have just cloned and not read.

What happens when you pass `--drift`:

- The command runs through `sh -c` (`cmd /C` on Windows), in a temporary
  directory, with `BRAKE_REPO_ROOT` set to the repository root. Write your
  command to use that variable rather than assuming a working directory.
- Its **stdout** is compared byte-for-byte with the committed `source`. A
  difference is `generated-drift`, at `error`.
- It is killed after 120 seconds. A hook that hangs is a hook that gets
  uninstalled.
- A generator that could not be started, or that exited non-zero, is
  **unavailable** — exit `2` — not a clean result. A generator that did not run
  tells you nothing about drift.

The MCP server exposes no way to reach this path, deliberately: a tool that
honoured `command` would hand arbitrary command execution to anything that can
write `brake.toml`. See [design/04-mcp-interface.md](../design/04-mcp-interface.md) §5.1.

## `[[consumer]]` and `[consumers]`

Declared consumer demand is a third input, so a finding can name *who* it
breaks. It has its own guide: [Consumer demand](consumers.md).

A consumer `source` that looks like a URL is refused when `brake.toml` is
parsed. `brake` never fetches a pact from a broker — have CI write the
directory and point `source` at the path.

## Overriding from the command line

Flags are a convenience over the file, for one run. Nothing writes back.

| Flag | On | Effect |
| --- | --- | --- |
| `--config <file>` | `check`, `analyze`, `diff`, `consumers` | Use this `brake.toml` |
| `--contract <name>` | `check`, `analyze`, `diff`, `consumers` | Restrict to these contracts. Repeatable |
| `--consumer <name>` | `check`, `analyze`, `consumers` | Restrict to these declared consumers. Repeatable |
| `--compatibility <level>` | `check`, `analyze` | Override every contract's level |
| `--baseline <spec>` | `check`, `diff` | Override every contract's baseline |
| `--since <ref>` | `check` | Check only contracts that differ from the merge-base with `<ref>` |
| `--severity <level>` | `check` | Minimum severity that fails. Default `warning` |
| `--fail-on <level>` | `analyze` | The same, for `analyze` |
| `--as-of <YYYY-MM-DD>` | `check`, `analyze`, `mcp` | Evaluate suppression expiry at this date |
| `--format <fmt>` | `check`, `analyze`, `diff`, `consumers` | `auto`, `text`, `json`, `sarif` (`consumers` has no SARIF form) |
| `--drift` | `check`, `analyze` | Also run declared generator commands |

`--baseline` accepts the same ideas as the file, in one string:

| Written as | Means |
| --- | --- |
| `v1.2.0`, `origin/main`, `8743cba` | `{ rev = … }` — a bare revision |
| `latest-tag:v*` | `{ latest-tag = "v*" }` |
| `merge-base:origin/main` | `{ git-merge-base = "origin/main" }` |
| `origin/main:api/old-path.yaml` | `{ git = "…" }` — the explicit `ref:path` shape |
