# User guide test cases

These cases are the canonical examples for evaluating `brake` behavior across
all supported contract formats. They map directly to automated acceptance tests
in `tests/user_guide_cases.rs`.

## Shared setup

Use one contract per run with a file baseline:

```toml
[defaults]
compatibility = "wire-json"

[[contract]]
name = "example"
format = "openapi" # or: proto, graphql
source = "api/contract.ext"
baseline = { file = "api/contract.baseline.ext" }
```

Run:

```sh
brake check --config brake.toml
```

## Case matrix

| Case ID | Format | Expected exit | Expected rule(s) | Purpose |
| --- | --- | --- | --- | --- |
| `openapi-clean` | OpenAPI | `0` | none | No contract change; gate stays clean |
| `openapi-endpoint-removed` | OpenAPI | `1` | `endpoint-removed` | Removing an endpoint is breaking |
| `protobuf-clean` | Protobuf | `0` | none | No RPC surface change |
| `protobuf-rpc-removed` | Protobuf | `1` | `endpoint-removed` | Removing an RPC is breaking |
| `graphql-clean` | GraphQL | `0` | none | Query surface preserved |
| `graphql-query-removed` | GraphQL | `1` | `endpoint-removed` | Removing a query field is breaking |

## Minimal example contracts

### OpenAPI negative (`openapi-endpoint-removed`)

Baseline:

```yaml
openapi: 3.1.0
paths:
  /payments/{id}:
    get:
      operationId: getPayment
      responses:
        "200":
          description: ok
```

Head:

```yaml
openapi: 3.1.0
paths:
  /payments:
    get:
      operationId: listPayments
      responses:
        "200":
          description: ok
```

### Protobuf negative (`protobuf-rpc-removed`)

Baseline:

```proto
syntax = "proto3";
package payments;

message GetPaymentRequest {
  string id = 1;
}

message Payment {
  string id = 1;
}

service PaymentService {
  rpc GetPayment(GetPaymentRequest) returns (Payment);
}
```

Head:

```proto
syntax = "proto3";
package payments;

message GetPaymentRequest {
  string id = 1;
}

message Payment {
  string id = 1;
}

service PaymentService {}
```

### GraphQL negative (`graphql-query-removed`)

Baseline:

```graphql
type Query {
  payment(id: ID!): Payment!
}

type Payment {
  id: ID!
}
```

Head:

```graphql
type Query {
  health: String!
}
```
