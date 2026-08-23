# 03 — Implementation plan

Build order for the library and the CLI. Every milestone has a verifiable
completion condition; none is "the code is written".

The governing decision is that **`brake check` is the product**
([01-thesis.md](01-thesis.md)), so the build order is the one that gets a
working `check` in front of a developer soonest and then deepens it. The
temptation is to build the comparator first because it is the interesting part.
That would produce three weeks of untested type algebra with no way to run it.

---

## 1. Crate shape

One crate, two targets. Not a workspace.

```
brake
├── src/lib.rs      the library — ingest, compare, rules, report
└── src/main.rs     the CLI — argument parsing, output rendering, exit codes
```

The library is a product in its own right, not an implementation detail:
[forge](https://github.com/grahambrooks/forge) depends on it for the rules that
need an architecture model. That consumer takes `default-features = false` and
gets the core with no `clap`, no terminal handling, and no colour.

A workspace would buy inter-crate version pinning and cost a second manifest.
There is nothing here to pin.

**The rule that keeps this honest:** `main.rs` may contain argument parsing,
output rendering, and process exit. It may not contain a decision about whether
something is a breaking change. If a behaviour cannot be tested through the
library, it is in the wrong file.

## 2. Module layout

```
src/
├── lib.rs               public API, re-exports, the `Report` type
├── config.rs            brake.toml — parse, validate, defaults, suppressions
├── contract/
│   ├── mod.rs           the normalised model of §3 in 02-contract-gates.md
│   ├── openapi.rs       OpenAPI 3.0/3.1 → Contract
│   ├── proto.rs         phase 2
│   └── graphql.rs       phase 3
├── compare/
│   ├── mod.rs           Contract × Contract → Vec<Change>
│   ├── types.rs         TypeRef comparison — the hard part
│   └── change.rs        the Change vocabulary
├── rules/
│   ├── mod.rs           Change × Level → Vec<Finding>
│   └── catalogue.rs     rule IDs, severities, level gating, explain text
├── baseline.rs          file / git / git-merge-base resolution via gix
├── report.rs            Finding, Severity, Span, verdict, exit code mapping
└── render/
    ├── text.rs          annotate-snippets
    ├── json.rs
    └── sarif.rs
```

The dependency direction is strictly downward: `contract` knows nothing about
`compare`, `compare` knows nothing about `rules`, and `rules` knows nothing
about `render`. Enforced by tropism on this repository once it is checkable.

**The bet in this layout** is that `compare` is format-agnostic. `openapi.rs`
and `proto.rs` both produce a `Contract`, and `compare` never learns which one
it is holding. If a `match` on format appears in `compare/`, the ingest
normalisation is under-specified and the fix belongs in `contract/`, not a
special case.

## 3. Public API

The surface forge consumes. Small on purpose — every item here is a
compatibility obligation on a tool whose entire subject is compatibility
obligations.

```rust
// Ingest one contract artifact from bytes. No filesystem, no network.
pub fn parse(format: Format, source: &str, bytes: &[u8]) -> Result<Contract, ParseError>;

// The whole check, for callers that have their own file access.
pub fn compare(base: &Contract, head: &Contract, level: Level) -> Vec<Change>;
pub fn evaluate(changes: &[Change], cfg: &ContractConfig) -> Vec<Finding>;

// The convenience path the CLI uses.
pub fn check(cfg: &Config, scope: Scope) -> Result<Report, BrakeError>;

pub struct Report {
    pub findings: Vec<Finding>,
    pub unavailable: Vec<Unavailable>,
    pub contracts_checked: usize,
}

impl Report {
    /// The §7.1 contract: 0 clean, 1 findings, 2 tool failure.
    pub fn exit_code(&self, threshold: Severity) -> i32;
}
```

`Report::exit_code` lives in the library, not the CLI, because it *is* the CI
contract and it needs a test that does not spawn a process.

`parse` taking bytes rather than a path is what lets forge feed it a file it
already read, and what makes every ingest test a string literal rather than a
tempdir.

## 4. Dependency decisions

| Decision | Choice | Why |
| --- | --- | --- |
| **O1 — YAML** | `saphyr` | The blocking constraint is source spans. SARIF must annotate the line of the OpenAPI file that changed (§7.2), and the text renderer needs a span to underline. `serde_yaml` is unmaintained *and* discards spans. This is the one choice that is expensive to revisit after `openapi.rs` is written |
| **O2 — git** | `gix` | Pure Rust, no `git` on PATH, proven in tropism. A gate that requires a git binary is not hermetic |
| **O3 — diagnostics** | `annotate-snippets` | Same renderer as tropism, so the two tools' output is indistinguishable in a hook. Behind the `cli` feature |
| **O4 — JSON Schema** | none | No `jsonschema` crate. `brake` compares two schemas structurally; it never validates an instance against one. Pulling in a validator would import a resolution model that fights §6.1's filesystem bound |
| **O5 — snapshots** | `insta` | Diagnostic rendering is exactly what snapshot tests are for, and the determinism tests (§6) are byte-comparison anyway |
| **O6 — MCP** | `rmcp`, feature-gated | The official `modelcontextprotocol/rust-sdk`. Costs `tokio` on a synchronous crate, which is why it is not in `default` — see [04-mcp-interface.md](04-mcp-interface.md) §7 |

O1 is the only one worth revisiting, and only before M1 lands.

## 5. Milestones

Effort is one person, and assumes the design decisions above hold.

### M0 — Scaffold ✅

Repository, manifest, toolchain pin, licence, design documents, `make check`
green on an empty crate. Trunk-based from the first commit.

**Done when:** `make check` passes and `brake --version` prints the CalVer.

### M1 — Walking skeleton (~4 days) ✅

The narrowest end-to-end path: `brake.toml` → OpenAPI ingest → endpoint-set
comparison → text diagnostic → exit code.

- `config.rs` complete, including suppression parsing and the mandatory `reason`
- `contract/openapi.rs` producing endpoints and spans, but `TypeRef::Unknown`
  for every schema — deliberately. Types are M2
- `compare` implementing set comparison on `EndpointKey` only
- Rules: `endpoint-removed`, `method-removed`, `endpoint-path-changed`,
  `contract-unreachable`
- `baseline.rs` with `file` only
- `render/text.rs`
- Exit codes `0` / `1` / `2` wired and tested

**Done when:** a fixture repository with a removed endpoint fails
`brake check api/openapi.yaml` with exit `1` and a diagnostic pointing at the
right line, and the same repository with the endpoint restored exits `0`.

This is the milestone that proves the pipeline. It ships four rules, which is
already enough to catch the most common real breakage.

### M2 — The comparator (~8 days) ✅

`TypeRef` and the request/response rules. Half the total effort of phase 1, and
the reason the estimate for this tool is three weeks rather than one.

The traps, each of which needs a test before the code that handles it:

- **`$ref` cycles** — terminate at `TypeRef::Cycle(name)`, compare by name
- **`allOf`** — flatten at ingest, so a composition change producing an
  identical effective schema registers as no change
- **`oneOf` / `anyOf`** — variant *addition* breaks an exhaustive consumer,
  variant *removal* breaks a producer. Both need a rule; the level decides which
  fires
- **`additionalProperties` true → false** — a request-side break that is easy to
  miss because nothing was removed
- **3.0 `nullable: true` vs 3.1 `type: ["string","null"]`** — must normalise
  identically, or every 3.0 → 3.1 migration reports a wall of false breaks
- **`default` and `example`** — never breaking, must be excluded, or the gate
  becomes noise and gets switched off

**Done when:** every rule in §5.2 and §5.3 of the spec has a passing positive
and negative test, and a 3.0 spec compared against its faithful 3.1 translation
produces zero findings.

### M3 — Git baselines and the hook (~3 days) ✅

- `baseline.rs` gains `git` and `git-merge-base` via `gix`
- `brake check --since <ref>`
- `.pre-commit-hooks.yaml` so other repositories can consume the hook
- `prek.toml` running `brake` on itself — this repository has an OpenAPI fixture
  set, so it can gate its own fixtures

**Done when:** a commit that removes an endpoint is blocked by the hook on a
real clone, and a repository with pre-existing findings still passes a commit
that does not add a new one. That second half is the ratchet claim from
[01-thesis.md](01-thesis.md) and it is the one that must be demonstrated rather
than asserted.

### M4 — Output and explanation (~3 days) ✅

- `render/json.rs`, `render/sarif.rs`
- `brake explain <rule-id>`, with the rationale text living in
  `rules/catalogue.rs` beside the rule it explains
- `brake diff`
- `--format auto` tty detection

**Done when:** the SARIF validates against the 2.1.0 schema, a GitHub Code
Scanning upload annotates the correct line of the OpenAPI file in a real
repository, and `brake explain` covers every rule ID with no placeholder text.

### M5 — Levels and suppressions (~3 days) ✅

- `surface` and `strict`
- Suppression matching, `stale-allow`, `expired-allow`, `--as-of`
- `--severity` / `--fail-on`

**Done when:** the same fixture produces different verdicts at all four levels,
and an expired suppression fails with exit `1` while `--as-of` before the expiry
date passes.

### M6 — Drift (~2 days) ✅

`[contract.generated]` and `brake check --drift`.

**Done when:** a fixture whose generator output diverges from the committed
artifact fails, and the subprocess is provably not reachable without the flag.

### M9 — Version-controlled baselines ✅

`tag`, `latest-tag` and `rev`, which answer "has the published API broken since
we shipped?" rather than "is this change safe to merge?". None of them repeats
the contract path — that comes from `source` — which closes the one way the
existing `git = "ref:path"` shape could silently compare two unrelated files.

The traps, each with a test:

- **Annotated tags are tag objects, not commits.** Both kinds must peel.
- **Byte ordering ranks `v9.0.0` above `v10.0.0`**, which would gate a 10.x
  release against a 9.x baseline. Numeric runs compare numerically, and CalVer
  needs the same treatment.
- **A prerelease is not the release.** `v1.0.0-rc1` sorts below `v1.0.0`, while
  `v1.0.0.1` sorts above it — the separator `.` is itself a text run, so "text
  or number" is not the distinction that matters.
- **A tag on an unrelated branch is not a version HEAD evolved from.**
  `latest-tag` walks newest-first and takes the first ancestor.
- **A shallow clone has no tags**, which is the one place identical file
  contents can produce different verdicts on two machines. Reported as
  unavailable, naming `fetch-depth`, never as clean.

**Done when:** a repository tagged `v9.0.0` and `v10.0.0` with a break after the
newer tag fails against `latest-tag = "v*"` and names `v10.0.0` as the baseline.

### M7 — Protobuf ✅, M8 — GraphQL ✅

Each is a new ingester against an unchanged comparator. The §2 bet held: no
`match` on format appears in `compare/`. Two normalisations were needed to keep
it that way, and both belong in `contract/` exactly as the bet predicted.

- **Wire numbers.** Protobuf compatibility is defined by field number, not
  name. `Field::number` and `TypeRef::Enum::numbers` carry it into the model,
  and `compare/` uses the number as the field's identity wherever both sides
  have one. Formats without wire numbers leave it `None` and compare by name.
  Without this, renumbering — the canonical protobuf break — is invisible.
- **Streaming as a media type.** A unary method becoming streaming is modelled
  as `application/grpc` becoming `application/grpc+stream`, so it surfaces
  through `request-media-type-removed` rather than needing a protobuf-shaped
  flag on `Endpoint`.

GraphQL needed no comparator change at all. Union members carry a
`__typename` field, which is both how a consumer actually discriminates them
and what keeps two structurally identical members distinct.

### M11 — Remediation ✅

Every rule that reports a break carries an ordered list of evolution
strategies, bound to the finding's subject and rendered in text, JSON, SARIF
and `brake explain`. Specified in
[02-contract-gates.md](02-contract-gates.md) §5.7.

The trap, and it is the same one the tool exists to prevent: the subject has to
be carried explicitly on the `Change`. Deriving it from the JSON pointer works
for a field and fails for a parameter, whose pointer ends in its *index* — the
first cut confidently advised "keep `0` optional". A wrong instruction in the
part meant to help is worse than no help.

**Done when:** a removed response field is reported with three named strategies
naming that field, a newly required parameter with two naming that parameter,
and a purely additive change with none.

### M10 — MCP interface ✅

Specified and built: [04-mcp-interface.md](04-mcp-interface.md). The same
ruleset consulted at edit time by a coding agent rather than at commit time by
a hook: four tools, three resources, one prompt, stdio transport, behind a
non-default `mcp` feature so a consumer taking `default-features = false` is
unaffected.

`src/mcp/handlers.rs` is synchronous and transport-free — every tool is a plain
function from arguments to JSON, calling the same library functions the CLI
calls. `src/mcp/server.rs` is the `rmcp` adapter and the only file that knows
about async. That split is what keeps the §1 rule honest across a second
front-end: the adapter decides no more about breaking changes than `main.rs`
does.

The two decisions worth knowing without opening that document:

- **`[contract.generated]` is not honoured over MCP, at all.** An agent that
  can write `brake.toml` and call a tool honouring it has arbitrary command
  execution through a tool whose stated purpose is reading files. Drift stays a
  CLI concern.
- **`rmcp` requires `tokio`**, which is a real cost for a crate that is
  otherwise synchronous. The mitigation is the feature flag and keeping the
  async surface at the transport, so every handler calls the same synchronous
  library functions the CLI calls.

### Total

Phase 1, M1–M6, is roughly **23 working days**. M1 alone is four days and
delivers a genuinely useful tool, which is the argument for this order.

## 6. Test strategy

Per the standing convention: every rule needs a fixture-backed test proving it
fires on a positive case *and* stays quiet on a negative one. Silent false
positives are the failure mode that gets a hook uninstalled.

Beyond the per-rule pairs, five tests exist to defend claims the tool makes
about itself, and each maps to a numbered guarantee in
[02-contract-gates.md](02-contract-gates.md) §6.1:

| Test | Defends |
| --- | --- |
| Run twice, compare bytes, all formats | G4 byte-stability |
| Shuffle YAML mapping key order, assert identical output | G3 order-independence |
| Spec with an `http://` `$ref` produces `contract-unreachable` and opens no socket | G1 hermeticity |
| `$ref` escaping the source directory is an error, not a read | G2 filesystem bound |
| Configured-but-missing baseline exits `2`, not `0` | §6.2 honest failure |

The last one is worth stating as a test rather than a convention because it is
the failure that would make every other test meaningless — a gate that returns
clean when it cannot see the baseline is worse than no gate.

**Fixtures are built under `tempfile::tempdir()`**, never read from the
surrounding checkout. A test that inspects the ambient repository passes on
every laptop and fails in CI, which checks out a detached HEAD.

## 7. Release and distribution

Set up at M0, not later.

- **Trunk-based**, committing directly to `main`
- **CalVer** `YYYY.M.MICRO`, committed in `Cargo.toml`, bumped by `make release`
- **Binaries from a build matrix in `release.yml`**, not `dist`. Five targets:
  macOS on both architectures, Linux on both, and Windows x86_64. Hand-rolled
  because the whole thing is about 130 lines of YAML that can be read in one
  sitting, against a tool that generates and owns the workflow file and has to
  be kept current. The four Mac/Linux targets exist because the Homebrew
  formula needs all four to be installable.
- **Homebrew formula in this repository**, rendered by
  `.github/render-formula.py` and committed by the release. In this repository
  rather than a `homebrew-brake` tap, so no second repository and no
  cross-repository token — at the cost of an explicit URL on `brew tap`. The
  script refuses to emit a formula referencing an archive that was not built,
  because that failure would otherwise land on someone else's machine at
  `brew install`.
- **Released binaries carry `--features mcp`.** The feature is off by default
  to spare a library consumer the async runtime; someone downloading a binary
  has already accepted its size and expects `brake mcp` to work.
- **crates.io publishing stays manual** — it is the irreversible step, and this
  crate has a library consumer, so a bad publish is someone else's build failure
- **Verify the published artifact, not the local build.** Download it, check the
  SHA, run `brake --version`, and run the check the release was cut for

Because forge will depend on the library, the first `0.x` publish sets a
compatibility obligation. §3 is deliberately small for that reason, and the
public API should not grow beyond it without a specific consumer asking.

## 8. Risks

| Risk | Mitigation |
| --- | --- |
| **`saphyr` span quality is worse than assumed** and SARIF locations degrade to file-level | Prove spans in M1 on a real OpenAPI file before M2 depends on them. This is the single decision to de-risk first |
| **The comparator over-reports on real specs**, and the hook gets uninstalled | Run against real public OpenAPI documents during M2, not only fixtures. A measured false-positive rate is the acceptance bar, exactly as it was for tropism's hygiene checks |
| **Level semantics are wrong** — `wire-json` too strict or too loose in practice | The levels are configuration, so this is recoverable. Do not add a fifth level to paper over a mis-tuned one |
| **Scope creep into spec linting** | [01-thesis.md](01-thesis.md) records the exclusion. A rule that cannot break a consumer is out, and the test is mechanical |
| **forge integration pulls model concepts into this crate** | The library API takes bytes and returns findings. If forge needs a model concept, it belongs in forge |

## 9. First commits

1. `M0`: this scaffold — manifest, toolchain, licence, design, `make check`
2. `config.rs` plus its tests — no ingest yet, the smallest thing with a real test
3. `contract/openapi.rs` endpoints and spans, against a checked-in fixture
4. `compare` set comparison, `render/text.rs`, exit codes — M1 closes here
