# 06 — Architectural evolution and extended capabilities

**Specification & Roadmap.** Proposed architectural and functional enhancements
following post-M15 codebase review.

This document formalises the architectural and functional evolution of `brake`
beyond its initial milestones (M0–M15). It defines the design for expanding
schema ingestion depth, event-driven contract support, hermetic multi-document
resolution, formalised type variance, and enhanced CI integration — while strictly
preserving the non-negotiable core invariants of zero network, zero toolchain, and
hermetic execution.

The thesis is [01-thesis.md](01-thesis.md); the core gate specification is
[02-contract-gates.md](02-contract-gates.md); the implementation plan is
[03-implementation-plan.md](03-implementation-plan.md); the MCP interface is
[04-mcp-interface.md](04-mcp-interface.md); and the consumer demand system is
[05-consumer-demand.md](05-consumer-demand.md).

---

## 1. Governing invariants

Every proposal in this document must strictly adhere to the load-bearing
principles set in `01-thesis.md` and `02-contract-gates.md`:

1. **No network, ever.** Not behind a flag, not for schema registries, not for
   remote `$ref`s, and not for broker queries.
2. **No toolchain, no build, no running service.** `brake` reads files and git
   trees. It never compiles code, starts a server, or runs test suites.
3. **Format-agnostic comparison.** The diff engine in `src/compare/` never knows
   the source format (OpenAPI, Protobuf, GraphQL, AsyncAPI). Formats normalise
   to `Contract` and `TypeRef`.
4. **Honest diagnostics.** A construct that cannot be modelled is reported as
   `unavailable` / `contract-partial`, never as a false clean result.
5. **Deterministic.** Same inputs, same verdict, same bytes.

---

## 2. Schema evolution and format depth

### 2.1 Protobuf field and number reservation enforcement

#### Context
In Protocol Buffers (proto2/proto3), removing a field or enum value without
explicitly declaring its number and name in a `reserved` statement invites future
field re-use. When a developer reuses that number for a different purpose later,
deployed clients parsing old serialized data will experience silent wire
corruption.

#### Specification
When comparing base and head Protobuf contracts:
- If a field number present in `base` message `M` is absent in `head` message `M`,
  it must appear in `head`'s `reserved` ranges or numbers.
- If a field name present in `base` message `M` is absent in `head` message `M`,
  it must appear in `head`'s `reserved` names.
- If an enum value number or name is removed, it must similarly be reserved in the
  head enum.

A failure to reserve is reported under a new wire-level rule:
- `proto-field-unreserved`: A removed protobuf field number was not marked `reserved`.
- `proto-enum-value-unreserved`: A removed protobuf enum value number was not marked `reserved`.

#### Remedies
1. `reserve-removed-field`: Add `reserved <number>, "<name>";` to the head message.
2. `deprecate-field`: Keep the field definition with `[deprecated = true]`.

### 2.2 OpenAPI 3.1 & JSON Schema 2020-12 depth

#### Context
OpenAPI 3.1 aligns fully with JSON Schema 2020-12. Key features include:
- Multi-type arrays (e.g. `type: ["string", "null"]` or `type: ["integer", "string"]`).
- Schema keyword `$defs` alongside OpenAPI `components/schemas`.
- Tuple validation via `prefixItems`.
- Discriminator-mapped polymorphism (`discriminator.mapping`).

#### Specification
1. **Type arrays**: Normalise `type: [T1, T2, ...]` directly into `TypeRef::OneOf`
   (or `nullable: true` when one variant is `"null"`).
2. **Polymorphic Discriminator**: When `discriminator` is declared on `oneOf`/`anyOf`,
   map the explicit discriminant keys to their resolved schema types. Removing a
   mapping key or changing its target type is flagged as a polymorphic narrowing break.
3. **Prefix items**: Normalise `prefixItems: [S1, S2]` as ordered tuple constraints
   so narrowing the allowed tuple length or altering element types at index `i` is
   checked contravariantly in requests and covariantly in responses.

### 2.3 GraphQL interface inheritance & directive arguments

#### Context
In GraphQL schemas, interface hierarchies can expand (`interface Node`, `interface Entity implements Node`),
and directive definitions or default arguments can shift.

#### Specification
1. **Interface implementations**: An object type removing an `implements Interface`
   declaration is a breaking change for queries selecting interface fields on that type.
2. **Input argument defaults**: Changing or removing a default value for an input field
   can alter execution semantics for clients that omit the argument.

---

## 3. Event-driven contract support (AsyncAPI & CloudEvents)

### 3.1 Architectural fit

Event-driven architectures (Kafka, RabbitMQ, SQS, NATS) suffer from the same
contract breaking changes as HTTP APIs: field removals, type mutations, payload
re-structuring, and required header introductions.

AsyncAPI 2.x and 3.0 map directly onto `brake`'s existing contract model:

| AsyncAPI Concept | `Contract` Model | Mapping |
| --- | --- | --- |
| Channel / Topic (`orders.v1`) | `EndpointKey.path` | `"orders.v1"` |
| Operation (`publish` / `subscribe` / `send` / `receive`) | `EndpointKey.method` | `"PUBLISH"`, `"SUBSCRIBE"` |
| Message Payload | `Payload.media_types` | `{"application/json": TypeRef}` |
| Message Headers | `Endpoint.parameters` | Parameter location `"header"` |

### 3.2 Direction semantics

- **Publish operations (Producer)**: The broker/producer produces events. This behaves
  like an HTTP response (covariant subtyping). Adding optional fields is safe;
  removing fields or narrowing types breaks downstream event consumers.
- **Subscribe operations (Consumer)**: The service consumes events from the topic.
  This behaves like an HTTP request (contravariant subtyping). Relaxing constraints
  or making fields optional is safe; adding required fields breaks upstream producers.

This requires **zero changes to `src/compare/`** or the rule catalogue — the
existing diff engine and rules evaluate AsyncAPI channels identically to HTTP endpoints.

---

## 4. Hermetic multi-document bundling for local `$ref`s

### 4.1 The problem

Currently, `$ref` resolution across local sibling files (e.g.
`$ref: "./common/types.yaml#/Customer"`) reports `UnmodelledKind::ExternalRef`
because the ingester takes a raw byte slice `&[u8]` of a single file. In multi-file
repositories, this leads to `contract-partial` warnings.

### 4.2 The abstraction

Introduce a hermetic document resolver:

```rust
pub trait DocumentResolver {
    /// Read a relative document path strictly within repository boundaries.
    /// Never hits the network.
    fn resolve(&self, relative_path: &str) -> Result<Vec<u8>, IngestError>;
}
```

Implementations:
1. `SingleDocumentResolver`: Refuses sibling file lookups (preserves pure in-memory
   string parsing for MCP `check_change` and unit tests).
2. `FileSystemResolver`: Resolves sibling files relative to the root contract path
   on disk for local CLI execution.
3. `GitTreeResolver`: Resolves sibling files from historical git trees using `gix`
   blob lookups for baseline comparisons.

This allows multi-document OpenAPI, Protobuf imports, and GraphQL schema extensions
to resolve fully while strictly guaranteeing zero network access and complete
hermeticity.

---

## 5. Formalized subtyping & variance lattice in `compare/types.rs`

### 5.1 The problem

Currently, `compare/types.rs` handles `TypeDirection::Request` (contravariant) vs
`TypeDirection::Response` (covariant) through pairwise combinatorial matching across
`Scalar`, `Enum`, `Object`, `OneOf`, and `Array`. As more complex schema features
(constraints, nullable unions, open/closed objects) are added, pairwise matching
risks subtle combinatorial omissions.

### 5.2 The subtyping relation

Formalize the type comparison engine around a structural subtyping relation `<:`:

- **Response position (Covariance)**: $\text{Head} <: \text{Base}$
  The head contract response must be a subtype of base (it may narrow types, add
  fields, or restrict enum sets).
- **Request position (Contravariance)**: $\text{Base} <: \text{Head}$
  The head contract request must be a supertype of base (it must accept everything
  base accepted: widening types, making fields optional, or expanding enum sets).

```rust
pub enum SubtypeResult {
    Valid,
    Incompatible(Vec<TypeIssue>),
}

pub fn check_subtype(
    sub: &TypeRef,
    sup: &TypeRef,
    direction: TypeDirection,
    pointer: &str,
) -> Vec<TypeIssue>;
```

This single traversal handles request and response comparisons symmetrically,
reducing code duplication and guaranteeing mathematical consistency across
nullability, constraints (min/max, regex), and structural object shapes.

---

## 6. Ingestion pipeline decoupling

### 6.1 Two-phase ingesters

Refactor large ingesters (`openapi.rs`, `graphql.rs`, `proto.rs`) into two distinct
phases:

```
Raw Bytes ──▶ [Phase 1: AST & Span Indexer] ──▶ Indexed AST ──▶ [Phase 2: Semantic Normalizer] ──▶ Contract
```

1. **Phase 1: Syntactic AST & Span Indexing**:
   Parses raw bytes into document AST with exact line/column byte spans and JSON
   pointers.
2. **Phase 2: Semantic Normalization**:
   Transforms the indexed AST into `Contract`, `Endpoint`, and `TypeRef` structures
   without needing to handle low-level tokenization or pointer formatting.

Benefits:
- Parser errors and span tracking are cleanly separated from contract semantics.
- Unit testing complex schema combinations (`allOf` merging, `oneOf` flattening)
  can be done on synthetic AST nodes without constructing YAML strings.

---

## 7. Developer experience & CI integration

### 7.1 Native CI workflow annotations (`--format github`)

Provide first-class support for CI platforms that parse stdout workflow commands:

```
::error file=api/payments.yaml,line=142,col=9,title=response-field-removed::response field `customer_id` was removed
```

- Adds `--format github` and `--format gitlab`.
- Emits immediate inline annotations on pull request diffs without requiring
  separate SARIF upload actions.

### 7.2 Actionable suppression suggestions (`--suggest-suppression`)

When a developer intentionally makes a breaking change and needs to release the
brake with review:

- CLI flag `brake check --suggest-suppression` or MCP tool `suggest_suppression`
  outputs the exact TOML snippet ready for `brake.toml`:

```toml
[[suppression]]
rule = "response-field-removed"
contract = "payments"
endpoint = "GET /payments/{id}"
subject = "/customer_id"
reason = "Migrated customer_id to customer object in v2"
expires = "2026-11-24"
```

---

## 8. Memory and allocation optimizations for large monorepos

For enterprise monorepos containing hundreds of microservice contracts and pacts:
- Use string interning (`Arc<str>` or `lasso`) for JSON pointers, media types,
  and field names across `TypeRef` and `Finding`.
- Avoid cloning schema trees during recursive reference resolution by using
  shared reference-counted nodes.

---

## 9. Implementation roadmap & milestones

| Phase | Milestone | Focus Areas | Key Deliverables | Status |
| :--- | :--- | :--- | :--- | :--- |
| **Phase 1** | **M16** | **Protobuf & Ingestion Integrity** | Protobuf reserved range/name tracking and collision prevention on wire numbers/fields. | **Complete** |
| **Phase 1** | **M17** | **CI Workflow & Tool Formatting** | `--format github` (`--format github-actions`), `--format gitlab` (Code Quality JSON), and actionable `Finding::suggest_suppression`. | **Complete** |
| **Phase 2** | **M18** | **Hermetic Local `$ref` Resolver** | `DocumentResolver` abstraction for multi-file OpenAPI and Protobuf imports. | **Complete** |
| **Phase 2** | **M19** | **Subtyping & Variance Lattice** | Structural `<:` subtyping engine refactoring in `compare/types.rs`. | **Complete** |
| **Phase 3** | **M20** | **OpenAPI 3.1 & Polymorphism Depth** | Discriminator mapping and tuple `prefixItems` evaluation. | **Complete** |
| **Phase 3** | **M21** | **AsyncAPI / Event Contracts** | AsyncAPI 2.x/3.0 ingester mapping to `Contract`. | **Complete** |
