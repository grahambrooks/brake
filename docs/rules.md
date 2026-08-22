# Rule catalogue

<!-- Generated from `src/rules/catalogue.rs` by `make docs`. Do not edit by hand. -->

Every rule brake can report, why it exists, and the lowest compatibility level at which it fires. A rule outside the selected level does not fire at all — it is not downgraded to a warning, because a warning is a thing a human has to read and dismiss. The levels are specified in [design/02-contract-gates.md](../design/02-contract-gates.md).

Run `brake explain <rule-id>` to read any of this at the moment you are blocked by it, or `brake explain` with no argument to list the catalogue.

| Rule | Severity | Fires from | Summary |
| --- | --- | --- | --- |
| [`endpoint-removed`](#endpoint-removed) | error | `wire` | A baseline endpoint is absent in the head contract. |
| [`method-removed`](#method-removed) | error | `wire` | A method on a surviving path is absent in the head contract. |
| [`endpoint-path-changed`](#endpoint-path-changed) | error | `wire` | An operationId survived but its path template changed. |
| [`path-parameter-renamed`](#path-parameter-renamed) | error | `surface` | A path template kept its shape but renamed a parameter. |
| [`operation-id-changed`](#operation-id-changed) | error | `surface` | An endpoint kept its path and method but changed its operationId. |
| [`endpoint-added`](#endpoint-added) | warning | `strict` | A new endpoint appeared. |
| [`param-added-required`](#param-added-required) | error | `wire` | A new required request parameter or field was added. |
| [`param-became-required`](#param-became-required) | error | `wire` | An optional request parameter or field became required. |
| [`param-removed`](#param-removed) | warning | `wire-json` | A request parameter was removed. |
| [`param-type-narrowed`](#param-type-narrowed) | error | `wire` | A request parameter or body type stopped accepting values it used to accept. |
| [`param-location-changed`](#param-location-changed) | error | `wire` | A parameter moved between query, path, header or cookie. |
| [`param-added-optional`](#param-added-optional) | info | `strict` | A new optional request parameter or field was added. |
| [`request-media-type-removed`](#request-media-type-removed) | error | `wire` | A request media type is no longer accepted. |
| [`response-field-removed`](#response-field-removed) | error | `wire-json` | A field present in a baseline response is gone. |
| [`response-field-optional`](#response-field-optional) | error | `wire-json` | A response field that was always present became optional. |
| [`response-field-added`](#response-field-added) | info | `strict` | A response gained a field. |
| [`response-type-changed`](#response-type-changed) | error | `wire-json` | A response type changed incompatibly. |
| [`response-enum-extended`](#response-enum-extended) | warning | `wire-json` | A response enum gained a value. |
| [`response-status-removed`](#response-status-removed) | error | `wire-json` | A documented response status code is gone. |
| [`response-status-added`](#response-status-added) | info | `strict` | A response gained a documented status code. |
| [`response-media-type-removed`](#response-media-type-removed) | error | `wire-json` | A response media type is gone. |
| [`field-number-changed`](#field-number-changed) | error | `wire` | A field kept its name and changed its wire number. |
| [`field-renamed`](#field-renamed) | error | `wire-json` | A field kept its wire number and changed its name. |
| [`security-added`](#security-added) | error | `wire-json` | An endpoint gained a security requirement, or a stronger one. |
| [`security-scheme-changed`](#security-scheme-changed) | error | `wire-json` | A security scheme's type, transport or flow changed. |
| [`security-removed`](#security-removed) | warning | `wire-json` | An endpoint lost a security requirement. |
| [`removed-without-deprecation`](#removed-without-deprecation) | error | `wire-json` | Something was removed that was not marked deprecated in the baseline. |
| [`deprecated-no-sunset`](#deprecated-no-sunset) | info | `wire-json` | An endpoint is deprecated with no `x-sunset` date. |
| [`contract-unreachable`](#contract-unreachable) | error | `wire` | The configured contract source could not be read or parsed. |
| [`contract-partial`](#contract-partial) | warning | `wire` | A compared path contains a construct brake cannot model. |
| [`stale-allow`](#stale-allow) | error | `wire` | A suppression matches nothing. |
| [`expired-allow`](#expired-allow) | error | `wire` | A suppression is past its `expires` date. |
| [`baseline-unconfigured`](#baseline-unconfigured) | info | `wire` | A contract has no baseline, so nothing was compared. |
| [`contract-new`](#contract-new) | info | `wire` | A contract has no previous version in the baseline. |
| [`contract-unconfigured`](#contract-unconfigured) | info | `wire` | A file that looks like an API contract is not declared in brake.toml. |
| [`generated-drift`](#generated-drift) | error | `wire` | Generated contract output differs from the checked-in artifact. |

## endpoint-removed

**A baseline endpoint is absent in the head contract.**

Severity `error`, fires from `wire` upward.

Every consumer that still calls this endpoint now gets a 404. There is no version of this change that an existing caller survives, which is why it fires at every compatibility level. Deprecate the endpoint, ship that, wait for consumers to migrate, and remove it afterwards.

## method-removed

**A method on a surviving path is absent in the head contract.**

Severity `error`, fires from `wire` upward.

The path still answers, so a consumer gets a 405 rather than a 404 — which is harder to diagnose, because the endpoint looks alive. Treat it exactly like an endpoint removal: deprecate first.

## endpoint-path-changed

**An operationId survived but its path template changed.**

Severity `error`, fires from `wire` upward.

The operation still exists, so this reads as a rename rather than a removal — but a consumer is pinned to the old URL and does not know about the new one. If both paths must work, serve both for a release rather than moving the operation in one step.

## path-parameter-renamed

**A path template kept its shape but renamed a parameter.**

Severity `error`, fires from `surface` upward.

The URL a consumer builds is unchanged, so nothing breaks on the wire. Generated clients are a different matter: the parameter name becomes an argument name, so renaming it breaks every caller that passes arguments by name. That is why this is a `surface` rule and not a `wire` one.

## operation-id-changed

**An endpoint kept its path and method but changed its operationId.**

Severity `error`, fires from `surface` upward.

Nothing changes on the wire. Code generators derive method names from operationId, so this renames a function in every generated client — a compile error for consumers, and invisible to anyone testing over HTTP.

## endpoint-added

**A new endpoint appeared.**

Severity `warning`, fires from `strict` upward.

Purely additive, and safe for every consumer. It fires only under `strict`, where the contract is frozen and any change at all needs to be a deliberate, reviewed act.

## param-added-required

**A new required request parameter or field was added.**

Severity `error`, fires from `wire` upward.

Every existing caller omits it, because it did not exist when they were written. Add it as optional with a default, and require it in a later release once callers have migrated.

## param-became-required

**An optional request parameter or field became required.**

Severity `error`, fires from `wire` upward.

Callers that legitimately omitted it now fail validation. This is the same break as adding a required parameter, arriving by a different route.

## param-removed

**A request parameter was removed.**

Severity `warning`, fires from `wire-json` upward.

Whether this breaks a caller depends on the server: an API that ignores unknown parameters tolerates it, and one validating with `additionalProperties: false` rejects the request outright. It is a warning rather than an error because brake cannot see which one you are.

## param-type-narrowed

**A request parameter or body type stopped accepting values it used to accept.**

Severity `error`, fires from `wire` upward.

Narrowing an input — `string` to `integer`, a smaller `maxLength`, a removed enum member, nullable becoming non-nullable, `additionalProperties` closing — rejects requests that were valid yesterday. Widening an input is always safe; narrowing never is.

## param-location-changed

**A parameter moved between query, path, header or cookie.**

Severity `error`, fires from `wire` upward.

The parameter still exists under the same name, so this is easy to read as a harmless move. It is not: a caller sending it in the old location is now sending an unknown parameter and omitting a known one, at the same time.

## param-added-optional

**A new optional request parameter or field was added.**

Severity `info`, fires from `strict` upward.

Additive and safe: callers that ignore it are unaffected. It fires only under `strict`, where the contract is frozen.

## request-media-type-removed

**A request media type is no longer accepted.**

Severity `error`, fires from `wire` upward.

A caller sending that Content-Type now gets a 415. Dropping XML because 'everyone uses JSON' is the usual way this lands, and the callers who did not get the memo are exactly the ones who will not notice until production.

## response-field-removed

**A field present in a baseline response is gone.**

Severity `error`, fires from `wire-json` upward.

Any consumer reading that field now gets nothing, and a consumer deserialising into a type with a non-optional field for it fails outright. Deprecate the field for a release before removing it — that is the sanctioned path, and a team that follows it never needs a suppression.

## response-field-optional

**A response field that was always present became optional.**

Severity `error`, fires from `wire-json` upward.

Nothing was removed, so this looks safe in a diff. But a consumer whose type for this field is non-optional — which is what a generator produces from a `required` field — now fails to deserialise whenever the field is absent. It is a removal that only happens sometimes, which makes it harder to debug, not easier.

## response-field-added

**A response gained a field.**

Severity `info`, fires from `strict` upward.

Safe for any tolerant reader, and a break only for a consumer that rejects unknown fields. It fires only under `strict`, where the contract is frozen and additions are reviewed too.

## response-type-changed

**A response type changed incompatibly.**

Severity `error`, fires from `wire-json` upward.

The bytes a consumer receives no longer match the shape it was written against. Changing a type in place gives consumers no migration window; add a new field alongside the old one and remove the old one after a deprecation period.

## response-enum-extended

**A response enum gained a value.**

Severity `warning`, fires from `wire-json` upward.

A tolerant reader copes. A generated Rust or TypeScript client matching exhaustively on the enum does not — it panics, throws, or fails to compile on upgrade. `graphql-inspector` calls this DANGEROUS rather than breaking for the same reason, and it is a warning here so that teams generating exhaustive clients can raise it deliberately.

## response-status-removed

**A documented response status code is gone.**

Severity `error`, fires from `wire-json` upward.

A consumer with a branch for that status has dead code at best, and at worst is now falling through to an error path for a response it used to handle.

## response-status-added

**A response gained a documented status code.**

Severity `info`, fires from `strict` upward.

Additive documentation of an outcome that may already have been possible. It fires only under `strict`.

## response-media-type-removed

**A response media type is gone.**

Severity `error`, fires from `wire-json` upward.

A consumer sending an Accept header for that type now gets a 406, or silently receives a format it cannot parse.

## field-number-changed

**A field kept its name and changed its wire number.**

Severity `error`, fires from `wire` upward.

In protobuf the field number *is* the field: it is what appears on the wire, and the name exists only in the source. Renumbering a field means every already-deployed client decodes those bytes into a different field, or drops them as unknown. This is the canonical protobuf break and it is invisible in a name-based diff.

## field-renamed

**A field kept its wire number and changed its name.**

Severity `error`, fires from `wire-json` upward.

The binary encoding is unaffected, so this is safe at `wire`. Anything reading the JSON mapping of the message, or any generated struct field, breaks — which is why it fires from `wire-json` upward and not below.

## security-added

**An endpoint gained a security requirement, or a stronger one.**

Severity `error`, fires from `wire-json` upward.

Every existing caller is now unauthenticated or under-scoped, and gets a 401 or 403. Strengthening authentication is often correct and urgent — but it is a breaking change, and shipping it without telling consumers turns a security improvement into an outage.

## security-scheme-changed

**A security scheme's type, transport or flow changed.**

Severity `error`, fires from `wire-json` upward.

Consumers built credentials for the old scheme. Swapping an API key for OAuth, or moving a token from a header to a cookie, invalidates every one of them.

## security-removed

**An endpoint lost a security requirement.**

Severity `warning`, fires from `wire-json` upward.

Not a compatibility break — existing callers keep working, and that is precisely the problem. An endpoint that quietly stopped requiring authentication is almost always an accident, and nothing else in the pipeline will notice.

## removed-without-deprecation

**Something was removed that was not marked deprecated in the baseline.**

Severity `error`, fires from `wire-json` upward.

This is the rule that makes the rest of the gate humane. The sanctioned path for any removal is deprecate, ship, wait, remove — and a team that follows it never needs a suppression, because by the time the removal lands the baseline already says `deprecated: true`. If you are seeing this, the removal skipped a step rather than the gate being wrong.

## deprecated-no-sunset

**An endpoint is deprecated with no `x-sunset` date.**

Severity `info`, fires from `wire-json` upward.

A deprecation with no date is a deprecation that never ends, and consumers correctly read it as no reason to move. Give it a date so the eventual removal is something they were told about rather than something that happens to them.

## contract-unreachable

**The configured contract source could not be read or parsed.**

Severity `error`, fires from `wire` upward.

A contract that cannot be read cannot be verified, and reporting clean would be reporting a verification that did not happen. This also covers a `$ref` that resolves over the network or outside the source directory: brake refuses those rather than fetching them, because remote refs are the largest source of non-determinism in OpenAPI tooling.

## contract-partial

**A compared path contains a construct brake cannot model.**

Severity `warning`, fires from `wire` upward.

The comparison happened, but not over the whole payload — so the result is 'not fully verified', never 'clean'. A tool that silently ignores what it cannot parse is worse than no tool, because it manufactures confidence. The finding names the construct and its JSON pointer so you can decide whether the unverified part matters.

## stale-allow

**A suppression matches nothing.**

Severity `error`, fires from `wire` upward.

The break it was written for is gone, so the suppression is now a blanket permission for a finding nobody has reviewed. Dead suppressions accumulate into a list that hides live problems, which is the failure mode this rule exists to prevent.

## expired-allow

**A suppression is past its `expires` date.**

Severity `error`, fires from `wire` upward.

The exception was granted until a date, and that date has passed. Either the migration finished and the suppression should go, or it did not and that is worth someone knowing about.

## baseline-unconfigured

**A contract has no baseline, so nothing was compared.**

Severity `info`, fires from `wire` upward.

This is a user who has not opted in, not a broken gate, and the two must never share an exit code. A *missing* baseline — one configured but unresolvable — is a tool failure and exits 2. An *unconfigured* baseline exits 0 with this note, because failing a build over configuration nobody has written yet teaches a team to disable the tool.

## contract-new

**A contract has no previous version in the baseline.**

Severity `info`, fires from `wire` upward.

The contract is new: it does not exist in the baseline, so there is nothing it could have broken. This is deliberately not a tool failure — a `git-merge-base` baseline does not contain a file added by the change being checked, and failing there would make every new API file fail CI on the commit that introduces it. The next commit compares normally.

## contract-unconfigured

**A file that looks like an API contract is not declared in brake.toml.**

Severity `info`, fires from `wire` upward.

brake only checks what it is told about. A new OpenAPI, proto or GraphQL file that no `[[contract]]` declares is silently ungated, and the whole point of a gate is that its coverage is not a matter of luck. Declare it, or move it somewhere the hook does not watch.

## generated-drift

**Generated contract output differs from the checked-in artifact.**

Severity `error`, fires from `wire` upward.

The committed contract no longer matches what the code produces, so every check brake ran was against a stale document. Regenerate and commit the result.
