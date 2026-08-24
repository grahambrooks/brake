---
name: brake-triage
description: Diagnose and resolve a brake finding that is blocking a commit, a pre-commit hook or a CI job — an error like response-field-removed, endpoint-removed, param-added-required, field-number-changed, consumer-field-unmet, contract-unreachable, stale-allow or generated-drift. Use when a brake run failed, when the pre-commit hook rejected a change to an API contract, or when asked to fix, understand or suppress a brake error.
argument-hint: "<the rule id or the output brake printed>"
---

# A brake finding is blocking the change

Work out which of three different things happened before doing anything else.
They have different fixes and conflating them is how a gate gets uninstalled.

## 1. Read the exit code

| Code | What it means | Where to go |
| --- | --- | --- |
| `1` | A real finding at or above the threshold | Sections 2–4 |
| `2` | **The gate is broken** — baseline unresolvable, source unreadable, generator absent | Section 5 |

If you do not have the exit code, re-run the command and capture it. Treating a
`2` as an API problem sends you fixing the wrong file.

## 2. Understand the rule before touching the contract

```sh
brake explain <rule-id>        # or the explain_rule MCP tool
```

This prints why the rule exists, what a consumer experiences when it fires, and
the named ways to make the same change safely with what each costs. The
[rule catalogue](../../../docs/rules.md) has the same text.

The span in the output points at the **evidence**, which is not always the line
you will edit. For a removal it is the *baseline* — the removed field has no
line in the head document any more. For a `consumer-*` finding it is the
interaction in the pact that declares the expectation: that is *why* you cannot
make the change, while the contract is *where* you would make it.

## 3. Decide, with the user, which way out fits

Every break carries named, costed strategies bound to the field it is about —
`keep-emitting`, `deprecate-then-remove`, `expand-then-contract`,
`version-the-endpoint`, `reserve-the-number`, and others per rule.

**`brake` does not choose between them and neither should you silently.** Which
one fits depends on whether the team controls every consumer and whether there
is a version scheme, and `brake` can see neither. Present the applicable options
with their costs and recommend one, with the reason.

Before choosing, check who is actually affected: `/brake-consumer-impact`, or
the `who_consumes` MCP tool. A named consumer changes the answer.

For a `consumer-*` finding the contract may not be what is wrong at all — a
pact that expects a field the API never documented means the *consumer* is
relying on something undocumented, and `document-the-endpoint` may be the
correct move.

## 4. Suppress only as a deliberate, dated decision

If the break is genuinely acceptable, it is a `[[contract.allow]]` entry in
`brake.toml` — **never** a rule turned off, a level lowered to make it
disappear, or a `--severity` raised in CI.

```toml
[[contract.allow]]
rule = "response-field-removed"
endpoint = "GET /payments/{id}"        # optional; narrows to one endpoint
field = "legacy_reference"             # optional; narrows to one field
reason = "nothing has read it since the 2025 migration; tracked in PAY-4417"
expires = 2026-09-01                   # strongly preferred
```

Over MCP, every finding carries a `suggested_suppression` — the same block with
the rule, endpoint and field already filled in. It is a convenience, not a
recommendation: its `reason` is a placeholder you must replace with the real
one, and pasting it in to clear a build is precisely the misuse this section
exists to prevent.

- `reason` is required. Write the actual reason, with a ticket if there is one —
  a future reviewer reads this instead of re-deriving the decision.
- Prefer an `expires` date. When it passes the finding returns as
  `expired-allow` at `error`, which is the point: the deadline you wrote down
  arrived.
- Narrow it. A bare `rule = …` suppresses that rule across the whole contract,
  including breaks nobody has looked at yet.
- `contract-unreachable`, `stale-allow` and `expired-allow` cannot be
  suppressed at all. A suppression that could hide them would let the gate stop
  gating silently.

Always tell the user you are adding a suppression and why. Never add one to get
a build green without saying so.

## 5. When the gate itself is broken (exit `2`)

| Symptom | Cause | Fix |
| --- | --- | --- |
| `baseline` unresolvable, or a tag glob matched nothing | Shallow clone — the ref or the tags are not present | `fetch-depth: 0` on `actions/checkout` |
| Source unreadable | `source` path wrong, or the file moved | Correct `[[contract]] source` |
| Generator could not be started, or exited non-zero | `--drift` command wrong, or a missing tool | Fix `[contract.generated] command`; it runs via `sh -c` in a temp dir with `BRAKE_REPO_ROOT` set |
| Generator did not finish | Over the 120-second limit | Make the generator faster, or drop `--drift` from the hook and keep it in CI |
| A `$ref` was refused | It names a URL, or climbs above the contract's directory | Vendor the document into the contract's directory; `brake` never fetches one |

These are distinct from a *finding* on purpose. `baseline-unconfigured` (no
baseline set — you have not opted in) and `contract-new` (nothing existed to
break) are both `info` and exit `0`; an unresolvable baseline is exit `2`.

## 6. Housekeeping findings

| Rule | Means | Fix |
| --- | --- | --- |
| `stale-allow` | A suppression matched nothing — reported only by `brake analyze`, which covered everything | Delete the entry; the break it covered is gone |
| `expired-allow` | A suppression is past its `expires` date | Make the change properly, or renew the entry with a new reason and date |
| `generated-drift` | The committed contract differs from its generator's output | Re-run the generator and commit the result |
| `contract-partial` / `consumer-partial` | A construct `brake` cannot model — including a `$ref` to a document it could not read, and a protobuf field reusing a `reserved` number or name | Not a break. Do not report it as clean either — say what was not verified |
| `contract-unconfigured` | A file that parses as a contract is not declared | Add a `[[contract]]` block, or leave it and say why |

## Never do these

- Lower the compatibility level, raise `--severity`, or delete a `[[contract]]`
  block to make a finding go away.
- Edit the baseline — a tag, or the committed previous document — so the diff
  looks clean.
- Report `unavailable` or exit `2` as a passing run.
- Add a suppression without telling the user.

## Related

- `/api-compatibility` — check a change *before* writing it.
- `/brake-consumer-impact` — who actually breaks.
