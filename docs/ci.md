# CI and hooks

`brake` is the same ruleset at two scopes. Which one you run depends on what
question you are asking, not on how much you want to check.

| Surface | When | What runs |
| --- | --- | --- |
| `brake check <files>` | pre-commit | The contracts among the changed files, against the baseline. Fast, blocking |
| `brake check --since <ref>` | CI, on a pull request | The same, scoped to the branch |
| `brake analyze .` | CI on main, release gate | Every contract, every rule, including the advisory ones |

Scoping the run to the change is what makes this adoptable. A repository with
two hundred existing findings still passes every commit that does not add a
two-hundred-and-first — with no baseline file, no state, and nothing to
regenerate after a refactor. That is the ratchet.

## As a pre-commit hook

```yaml
# .pre-commit-config.yaml, in your repository
repos:
  - repo: https://github.com/grahambrooks/brake
    rev: v2026.8.4
    hooks:
      - id: brake
```

Two hooks are published:

| id | Runs | Filenames |
| --- | --- | --- |
| `brake` | `brake check --format text` | Passed the changed files. **This is the one you want** |
| `brake-analyze` | `brake analyze .` | Always, ignoring filenames. For a CI-side hook run |

`pass_filenames: true` is the point of the `brake` hook. `brake check` takes
paths and checks only the contracts among them, so a commit that touches
nothing under `[[contract]]` costs nothing.

The hook's `files` pattern is deliberately narrow — `api/`, `contracts/`,
`schemas/`, `proto/`, an `openapi*`/`swagger*` filename, or any `.proto`,
`.graphql` or `.gql`. An earlier, wider pattern matched every YAML and JSON in
the repository, so the hook fired on CI config and lockfiles on most commits. A
hook that speaks up about files nobody asked it to look at is a hook people
mute. Widen it if your contracts live elsewhere:

```yaml
      - id: brake
        files: ^(contracts|idl)/.*\.(ya?ml|proto)$
```

Widening costs time, not correctness: `brake` only checks the files a
`[[contract]]` declares.

## GitHub Actions

### On a pull request

```yaml
name: api
on: [pull_request]

jobs:
  compatibility:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
        with:
          # A merge-base baseline needs history; a shallow clone has none, and
          # a tag baseline needs the tags.
          fetch-depth: 0
      - name: Install brake
        run: |
          set -euo pipefail
          version=v2026.8.4
          name="brake-${version}-x86_64-unknown-linux-gnu"
          curl -sSfL "https://github.com/grahambrooks/brake/releases/download/${version}/${name}.tar.gz" \
            | sudo tar -xz -C /usr/local/bin --strip-components=1 "${name}/brake"
      - run: brake check --since origin/${{ github.base_ref }} --format text
```

The release attaches a `SHA256SUMS` file beside every archive; verify against
it if your policy requires it. `cargo install brake --locked` works too, at the
cost of a toolchain and a compile.

`fetch-depth: 0` is not optional. Every baseline except `{ file = … }` is
resolved out of git, and an unresolvable baseline is exit `2` — a broken gate,
reported as one.

### On main, and as a release gate

```yaml
  posture:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
        with: { fetch-depth: 0 }
      - name: Install brake
        run: |
          set -euo pipefail
          version=v2026.8.4
          name="brake-${version}-x86_64-unknown-linux-gnu"
          curl -sSfL "https://github.com/grahambrooks/brake/releases/download/${version}/${name}.tar.gz" \
            | sudo tar -xz -C /usr/local/bin --strip-components=1 "${name}/brake"
      - run: brake analyze . --format text
```

`analyze` reports the advisory rules too — `stale-allow`, `consumer-undeclared`
and, under a closed-world declaration, `consumer-surface-unused`. Those are
correct only on a run that covered everything, which is why the commit gate
does not report them.

Pair it with a second `[[contract]]` block over the same file whose baseline is
`{ latest-tag = "v*" }`, so the release gate asks "is the delta since the last
release safe?" rather than "did *this change* break anything?". See
[Configuration](configuration.md#two-contracts-over-one-artifact).

### Uploading to code scanning

```yaml
      - run: brake analyze . --format sarif > brake.sarif
        continue-on-error: true
      - uses: github/codeql-action/upload-sarif@v4
        with:
          sarif_file: brake.sarif
```

`continue-on-error` on the `brake` step only — otherwise a finding stops the
job before the SARIF is ever uploaded, which is the opposite of what you want.
Each SARIF result carries a `helpUri` into [the rule
catalogue](rules.md), and every affected consumer as a related location.

## Exit codes, and why the split matters

| Code | Meaning | Correct response |
| --- | --- | --- |
| `0` | No finding at or above the threshold | Proceed |
| `1` | At least one finding at or above the threshold | Fix the API, or write a suppression with a reason |
| `2` | Tool failure — baseline unresolvable, source unreadable, generator absent | Fix the *gate* |

CI must distinguish "your API broke" from "the gate is broken", because the
correct response differs and conflating them trains a team to ignore both.

`brake diff` and `brake consumers` always exit `0`. They are for pull-request
descriptions, changelog drafting and inventory.

## Tuning the threshold

```sh
brake check --severity error      # only errors fail the commit
brake analyze . --fail-on error   # the same, for analyze
```

The default is `warning` for both. Dropping to `error` is the sensible first
step when adopting `brake` on a repository that already has warnings — it is
reversible, it is one flag, and it does not require suppressions with reasons
nobody has yet worked out.

Raising the *compatibility level* is the other axis, and the more useful one
over time: start at `wire`, move to `wire-json` once the obvious breaks are
gone, and to `surface` when consumers generate clients.

## Determinism, and what it buys you

Same inputs, same verdict, same bytes — on a laptop, on CI, on a colleague's
machine. Paths in output are repository-relative with `/` separators, and no
absolute path ever appears. That is what makes `diff`-ing two runs meaningful,
and it is enumerated as a guarantee in
[design/02-contract-gates.md](../design/02-contract-gates.md) §6.1 with a test
behind each one.

## Running brake in its own CI

This repository gates its own fixture contract — `api/payments-openapi.yaml`
via `brake.toml` — in the `self-check` job. `make self-check` runs it locally.
A tool whose pitch is a contract gate should be behind one.
