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
| [`consumer-endpoint-unmet`](#consumer-endpoint-unmet) | error | `wire` | A declared consumer calls an endpoint the contract does not document. |
| [`consumer-status-unmet`](#consumer-status-unmet) | error | `wire-json` | A declared consumer expects a response the contract does not document — a status code, or that status in the media type the consumer reads. |
| [`consumer-field-unmet`](#consumer-field-unmet) | error | `wire-json` | A declared consumer reads a response field the contract does not produce. |
| [`consumer-request-rejected`](#consumer-request-rejected) | error | `wire` | The contract would reject a request a declared consumer sends. |
| [`consumer-unreachable`](#consumer-unreachable) | error | `wire` | A declared consumer source does not resolve or fails to parse. |
| [`consumer-partial`](#consumer-partial) | warning | `wire` | An interaction contains a construct brake cannot model. |
| [`consumer-path-ambiguous`](#consumer-path-ambiguous) | warning | `wire` | A consumer's concrete path matches more than one contract template. |
| [`consumer-provider-unmatched`](#consumer-provider-unmatched) | error | `wire` | A consumer declaration names a provider no `[[contract]]` declares. |
| [`consumer-undeclared`](#consumer-undeclared) | info | `wire` | A file parses as a consumer declaration but brake.toml does not declare it. |
| [`consumer-surface-unused`](#consumer-surface-unused) | info | `wire` | No declared consumer uses this endpoint. |

## endpoint-removed

**A baseline endpoint is absent in the head contract.**

Severity `error`, fires from `wire` upward.

Every consumer that still calls this endpoint now gets a 404. There is no version of this change that an existing caller survives, which is why it fires at every compatibility level. Deprecate the endpoint, ship that, wait for consumers to migrate, and remove it afterwards.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`deprecate-then-remove`** — mark the field deprecated now and remove it in a later release, once consumers have had a version to migrate
  *Costs:* the removal waits for a deprecation window you have to actually observe
- **`version-the-endpoint`** — serve the change at a new path, media type or version header, leaving the endpoint answering as it does today
  *Costs:* two implementations to maintain until the old one is retired
- **`major-version`** — if the break is genuinely intended, ship it as a new major version and tell consumers, rather than moving the endpoint underneath them
  *Costs:* a major version is a migration you are asking every consumer to do

## method-removed

**A method on a surviving path is absent in the head contract.**

Severity `error`, fires from `wire` upward.

The path still answers, so a consumer gets a 405 rather than a 404 — which is harder to diagnose, because the endpoint looks alive. Treat it exactly like an endpoint removal: deprecate first.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`deprecate-then-remove`** — mark the field deprecated now and remove it in a later release, once consumers have had a version to migrate
  *Costs:* the removal waits for a deprecation window you have to actually observe
- **`version-the-endpoint`** — serve the change at a new path, media type or version header, leaving the endpoint answering as it does today
  *Costs:* two implementations to maintain until the old one is retired
- **`major-version`** — if the break is genuinely intended, ship it as a new major version and tell consumers, rather than moving the endpoint underneath them
  *Costs:* a major version is a migration you are asking every consumer to do

## endpoint-path-changed

**An operationId survived but its path template changed.**

Severity `error`, fires from `wire` upward.

The operation still exists, so this reads as a rename rather than a removal — but a consumer is pinned to the old URL and does not know about the new one. If both paths must work, serve both for a release rather than moving the operation in one step.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`expand-then-contract`** — add the replacement alongside the field, move readers across, and remove the field only when nothing reads it
  *Costs:* both shapes are live at once, and the second half is easy to forget
- **`version-the-endpoint`** — serve the change at a new path, media type or version header, leaving the endpoint answering as it does today
  *Costs:* two implementations to maintain until the old one is retired

## path-parameter-renamed

**A path template kept its shape but renamed a parameter.**

Severity `error`, fires from `surface` upward.

The URL a consumer builds is unchanged, so nothing breaks on the wire. Generated clients are a different matter: the parameter name becomes an argument name, so renaming it breaks every caller that passes arguments by name. That is why this is a `surface` rule and not a `wire` one.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`keep-the-name`** — leave the field named as it is — the rename buys nothing on the wire and costs every generated client a code change
  *Costs:* you live with a name you no longer like
- **`major-version`** — if the break is genuinely intended, ship it as a new major version and tell consumers, rather than moving the endpoint underneath them
  *Costs:* a major version is a migration you are asking every consumer to do

## operation-id-changed

**An endpoint kept its path and method but changed its operationId.**

Severity `error`, fires from `surface` upward.

Nothing changes on the wire. Code generators derive method names from operationId, so this renames a function in every generated client — a compile error for consumers, and invisible to anyone testing over HTTP.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`keep-the-name`** — leave the field named as it is — the rename buys nothing on the wire and costs every generated client a code change
  *Costs:* you live with a name you no longer like
- **`major-version`** — if the break is genuinely intended, ship it as a new major version and tell consumers, rather than moving the endpoint underneath them
  *Costs:* a major version is a migration you are asking every consumer to do

## endpoint-added

**A new endpoint appeared.**

Severity `warning`, fires from `strict` upward.

Purely additive, and safe for every consumer. It fires only under `strict`, where the contract is frozen and any change at all needs to be a deliberate, reviewed act.

## param-added-required

**A new required request parameter or field was added.**

Severity `error`, fires from `wire` upward.

Every existing caller omits it, because it did not exist when they were written. Add it as optional with a default, and require it in a later release once callers have migrated.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`optional-with-default`** — keep the field optional and give it a default, then require it in a later release once callers send it
  *Costs:* the default has to be a value that is correct for existing callers
- **`version-the-endpoint`** — serve the change at a new path, media type or version header, leaving the endpoint answering as it does today
  *Costs:* two implementations to maintain until the old one is retired

## param-became-required

**An optional request parameter or field became required.**

Severity `error`, fires from `wire` upward.

Callers that legitimately omitted it now fail validation. This is the same break as adding a required parameter, arriving by a different route.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`optional-with-default`** — keep the field optional and give it a default, then require it in a later release once callers send it
  *Costs:* the default has to be a value that is correct for existing callers
- **`version-the-endpoint`** — serve the change at a new path, media type or version header, leaving the endpoint answering as it does today
  *Costs:* two implementations to maintain until the old one is retired

## param-removed

**A request parameter was removed.**

Severity `warning`, fires from `wire-json` upward.

Whether this breaks a caller depends on the server: an API that ignores unknown parameters tolerates it, and one validating with `additionalProperties: false` rejects the request outright. It is a warning rather than an error because brake cannot see which one you are.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`keep-accepting`** — go on accepting the field and ignore it, rather than rejecting requests that still send it
  *Costs:* the input surface keeps a field nothing uses, until you deprecate it properly
- **`deprecate-then-remove`** — mark the field deprecated now and remove it in a later release, once consumers have had a version to migrate
  *Costs:* the removal waits for a deprecation window you have to actually observe

## param-type-narrowed

**A request parameter or body type stopped accepting values it used to accept.**

Severity `error`, fires from `wire` upward.

Narrowing an input — `string` to `integer`, a smaller `maxLength`, a removed enum member, nullable becoming non-nullable, `additionalProperties` closing — rejects requests that were valid yesterday. Widening an input is always safe; narrowing never is.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`widen-dont-narrow`** — accept both the old and the new form of the field and normalise them inside the handler
  *Costs:* the handler carries the union until the old form is retired
- **`version-the-endpoint`** — serve the change at a new path, media type or version header, leaving the endpoint answering as it does today
  *Costs:* two implementations to maintain until the old one is retired
- **`major-version`** — if the break is genuinely intended, ship it as a new major version and tell consumers, rather than moving the endpoint underneath them
  *Costs:* a major version is a migration you are asking every consumer to do

## param-location-changed

**A parameter moved between query, path, header or cookie.**

Severity `error`, fires from `wire` upward.

The parameter still exists under the same name, so this is easy to read as a harmless move. It is not: a caller sending it in the old location is now sending an unknown parameter and omitting a known one, at the same time.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`accept-both-locations`** — read the field from both the old and the new location for a release, preferring the new one
  *Costs:* two places to look, and a rule for what happens when both are sent
- **`expand-then-contract`** — add the replacement alongside the field, move readers across, and remove the field only when nothing reads it
  *Costs:* both shapes are live at once, and the second half is easy to forget

## param-added-optional

**A new optional request parameter or field was added.**

Severity `info`, fires from `strict` upward.

Additive and safe: callers that ignore it are unaffected. It fires only under `strict`, where the contract is frozen.

## request-media-type-removed

**A request media type is no longer accepted.**

Severity `error`, fires from `wire` upward.

A caller sending that Content-Type now gets a 415. Dropping XML because 'everyone uses JSON' is the usual way this lands, and the callers who did not get the memo are exactly the ones who will not notice until production.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`keep-accepting`** — go on accepting the field and ignore it, rather than rejecting requests that still send it
  *Costs:* the input surface keeps a field nothing uses, until you deprecate it properly
- **`deprecate-then-remove`** — mark the field deprecated now and remove it in a later release, once consumers have had a version to migrate
  *Costs:* the removal waits for a deprecation window you have to actually observe

## response-field-removed

**A field present in a baseline response is gone.**

Severity `error`, fires from `wire-json` upward.

Any consumer reading that field now gets nothing, and a consumer deserialising into a type with a non-optional field for it fails outright. Deprecate the field for a release before removing it — that is the sanctioned path, and a team that follows it never needs a suppression.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`deprecate-then-remove`** — mark the field deprecated now and remove it in a later release, once consumers have had a version to migrate
  *Costs:* the removal waits for a deprecation window you have to actually observe
- **`expand-then-contract`** — add the replacement alongside the field, move readers across, and remove the field only when nothing reads it
  *Costs:* both shapes are live at once, and the second half is easy to forget
- **`version-the-endpoint`** — serve the change at a new path, media type or version header, leaving the endpoint answering as it does today
  *Costs:* two implementations to maintain until the old one is retired

## response-field-optional

**A response field that was always present became optional.**

Severity `error`, fires from `wire-json` upward.

Nothing was removed, so this looks safe in a diff. But a consumer whose type for this field is non-optional — which is what a generator produces from a `required` field — now fails to deserialise whenever the field is absent. It is a removal that only happens sometimes, which makes it harder to debug, not easier.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`keep-emitting`** — go on producing the field alongside whatever replaces it, so existing readers keep working
  *Costs:* the response carries a field you have stopped using
- **`expand-then-contract`** — add the replacement alongside the field, move readers across, and remove the field only when nothing reads it
  *Costs:* both shapes are live at once, and the second half is easy to forget

## response-field-added

**A response gained a field.**

Severity `info`, fires from `strict` upward.

Safe for any tolerant reader, and a break only for a consumer that rejects unknown fields. It fires only under `strict`, where the contract is frozen and additions are reviewed too.

## response-type-changed

**A response type changed incompatibly.**

Severity `error`, fires from `wire-json` upward.

The bytes a consumer receives no longer match the shape it was written against. Changing a type in place gives consumers no migration window; add a new field alongside the old one and remove the old one after a deprecation period.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`expand-then-contract`** — add the replacement alongside the field, move readers across, and remove the field only when nothing reads it
  *Costs:* both shapes are live at once, and the second half is easy to forget
- **`version-the-endpoint`** — serve the change at a new path, media type or version header, leaving the endpoint answering as it does today
  *Costs:* two implementations to maintain until the old one is retired
- **`major-version`** — if the break is genuinely intended, ship it as a new major version and tell consumers, rather than moving the endpoint underneath them
  *Costs:* a major version is a migration you are asking every consumer to do

## response-enum-extended

**A response enum gained a value.**

Severity `warning`, fires from `wire-json` upward.

A tolerant reader copes. A generated Rust or TypeScript client matching exhaustively on the enum does not — it panics, throws, or fails to compile on upgrade. `graphql-inspector` calls this DANGEROUS rather than breaking for the same reason, and it is a warning here so that teams generating exhaustive clients can raise it deliberately.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`document-open-enum`** — document the field as an open set so consumers parse unknown values instead of matching exhaustively, and add the value once they do
  *Costs:* consumers have to ship the tolerant reader before you ship the value
- **`version-the-endpoint`** — serve the change at a new path, media type or version header, leaving the endpoint answering as it does today
  *Costs:* two implementations to maintain until the old one is retired

## response-status-removed

**A documented response status code is gone.**

Severity `error`, fires from `wire-json` upward.

A consumer with a branch for that status has dead code at best, and at worst is now falling through to an error path for a response it used to handle.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`keep-emitting`** — go on producing the field alongside whatever replaces it, so existing readers keep working
  *Costs:* the response carries a field you have stopped using
- **`deprecate-then-remove`** — mark the field deprecated now and remove it in a later release, once consumers have had a version to migrate
  *Costs:* the removal waits for a deprecation window you have to actually observe

## response-status-added

**A response gained a documented status code.**

Severity `info`, fires from `strict` upward.

Additive documentation of an outcome that may already have been possible. It fires only under `strict`.

## response-media-type-removed

**A response media type is gone.**

Severity `error`, fires from `wire-json` upward.

A consumer sending an Accept header for that type now gets a 406, or silently receives a format it cannot parse.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`keep-emitting`** — go on producing the field alongside whatever replaces it, so existing readers keep working
  *Costs:* the response carries a field you have stopped using
- **`deprecate-then-remove`** — mark the field deprecated now and remove it in a later release, once consumers have had a version to migrate
  *Costs:* the removal waits for a deprecation window you have to actually observe

## field-number-changed

**A field kept its name and changed its wire number.**

Severity `error`, fires from `wire` upward.

In protobuf the field number *is* the field: it is what appears on the wire, and the name exists only in the source. Renumbering a field means every already-deployed client decodes those bytes into a different field, or drops them as unknown. This is the canonical protobuf break and it is invisible in a name-based diff.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`reserve-the-number`** — restore the field to its original field number and add `reserved` for any number you are retiring, so it can never be reused
  *Costs:* none — this is the only correct move; a reused number silently misreads data

## field-renamed

**A field kept its wire number and changed its name.**

Severity `error`, fires from `wire-json` upward.

The binary encoding is unaffected, so this is safe at `wire`. Anything reading the JSON mapping of the message, or any generated struct field, breaks — which is why it fires from `wire-json` upward and not below.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`keep-the-name`** — leave the field named as it is — the rename buys nothing on the wire and costs every generated client a code change
  *Costs:* you live with a name you no longer like
- **`expand-then-contract`** — add the replacement alongside the field, move readers across, and remove the field only when nothing reads it
  *Costs:* both shapes are live at once, and the second half is easy to forget

## security-added

**An endpoint gained a security requirement, or a stronger one.**

Severity `error`, fires from `wire-json` upward.

Every existing caller is now unauthenticated or under-scoped, and gets a 401 or 403. Strengthening authentication is often correct and urgent — but it is a breaking change, and shipping it without telling consumers turns a security improvement into an outage.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`dual-accept-credentials`** — accept the existing credential alongside the new one until consumers have issued themselves new ones
  *Costs:* the weaker credential stays valid for the length of the transition
- **`version-the-endpoint`** — serve the change at a new path, media type or version header, leaving the endpoint answering as it does today
  *Costs:* two implementations to maintain until the old one is retired

## security-scheme-changed

**A security scheme's type, transport or flow changed.**

Severity `error`, fires from `wire-json` upward.

Consumers built credentials for the old scheme. Swapping an API key for OAuth, or moving a token from a header to a cookie, invalidates every one of them.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`dual-accept-credentials`** — accept the existing credential alongside the new one until consumers have issued themselves new ones
  *Costs:* the weaker credential stays valid for the length of the transition
- **`version-the-endpoint`** — serve the change at a new path, media type or version header, leaving the endpoint answering as it does today
  *Costs:* two implementations to maintain until the old one is retired

## security-removed

**An endpoint lost a security requirement.**

Severity `warning`, fires from `wire-json` upward.

Not a compatibility break — existing callers keep working, and that is precisely the problem. An endpoint that quietly stopped requiring authentication is almost always an accident, and nothing else in the pipeline will notice.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`confirm-intended`** — confirm this was deliberate — if it was, record why in a `[[contract.allow]]` entry so the next reviewer does not have to work it out
  *Costs:* none, but it is a decision someone has to actually make

## removed-without-deprecation

**Something was removed that was not marked deprecated in the baseline.**

Severity `error`, fires from `wire-json` upward.

This is the rule that makes the rest of the gate humane. The sanctioned path for any removal is deprecate, ship, wait, remove — and a team that follows it never needs a suppression, because by the time the removal lands the baseline already says `deprecated: true`. If you are seeing this, the removal skipped a step rather than the gate being wrong.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`deprecate-then-remove`** — mark the field deprecated now and remove it in a later release, once consumers have had a version to migrate
  *Costs:* the removal waits for a deprecation window you have to actually observe
- **`add-sunset-date`** — give the field an `x-sunset` date, and announce it, so the eventual removal is something consumers were told about
  *Costs:* you are committing to a date

## deprecated-no-sunset

**An endpoint is deprecated with no `x-sunset` date.**

Severity `info`, fires from `wire-json` upward.

A deprecation with no date is a deprecation that never ends, and consumers correctly read it as no reason to move. Give it a date so the eventual removal is something they were told about rather than something that happens to them.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`add-sunset-date`** — give the field an `x-sunset` date, and announce it, so the eventual removal is something consumers were told about
  *Costs:* you are committing to a date

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

## consumer-endpoint-unmet

**A declared consumer calls an endpoint the contract does not document.**

Severity `error`, fires from `wire` upward.

The consumer's own tests record a call this contract has no operation for, so either the contract is missing something that is live in production or the consumer is calling something that never existed. Unlike every other rule here this one needs no baseline: it compares the contract as it is now against what a consumer says it relies on, which is why it fires on a brand-new contract with no history at all.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`document-the-endpoint`** — document the endpoint in the contract, since a consumer is calling it already — an undocumented endpoint is one nothing gates
  *Costs:* documenting it is also committing to it
- **`confirm-intended`** — confirm this was deliberate — if it was, record why in a `[[contract.allow]]` entry so the next reviewer does not have to work it out
  *Costs:* none, but it is a decision someone has to actually make

## consumer-status-unmet

**A declared consumer expects a response the contract does not document — a status code, or that status in the media type the consumer reads.**

Severity `error`, fires from `wire-json` upward.

The consumer has a branch for a response the contract says cannot happen. Either it can, and the contract is wrong, or it cannot, and the consumer has dead code sitting on an error path. brake matches the consumer's status against the documented `4XX` and `default` classes before reporting, so a contract that documents the class is satisfied.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`keep-emitting`** — go on producing the field alongside whatever replaces it, so existing readers keep working
  *Costs:* the response carries a field you have stopped using
- **`confirm-intended`** — confirm this was deliberate — if it was, record why in a `[[contract.allow]]` entry so the next reviewer does not have to work it out
  *Costs:* none, but it is a decision someone has to actually make

## consumer-field-unmet

**A declared consumer reads a response field the contract does not produce.**

Severity `error`, fires from `wire-json` upward.

This is the finding the whole consumer-demand input exists for: not 'this might break somebody' but 'this breaks web-checkout, at pacts/web-checkout-payments.json:88'. The span points at the interaction that declares the expectation, because that is the evidence — the contract line is where you would make the change, and the pact line is why you have to.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`keep-emitting`** — go on producing the field alongside whatever replaces it, so existing readers keep working
  *Costs:* the response carries a field you have stopped using
- **`deprecate-then-remove`** — mark the field deprecated now and remove it in a later release, once consumers have had a version to migrate
  *Costs:* the removal waits for a deprecation window you have to actually observe

## consumer-request-rejected

**The contract would reject a request a declared consumer sends.**

Severity `error`, fires from `wire` upward.

A required field or parameter the consumer omits, a value outside a narrowed type, or a media type no longer accepted. The consumer is not hypothetical here: it sends this request today, and the contract as written says the request is invalid.

**Ways to make the change safely.** brake names these and does not choose between them: which one fits depends on whether you control every consumer and whether you have a version scheme, and it can see neither.

- **`widen-dont-narrow`** — accept both the old and the new form of the field and normalise them inside the handler
  *Costs:* the handler carries the union until the old form is retired
- **`optional-with-default`** — keep the field optional and give it a default, then require it in a later release once callers send it
  *Costs:* the default has to be a value that is correct for existing callers

## consumer-unreachable

**A declared consumer source does not resolve or fails to parse.**

Severity `error`, fires from `wire` upward.

A consumer declaration that cannot be read cannot be verified, and reporting clean would be reporting a verification that did not happen. The CI workflow where a prior step pulls pacts from a broker rests entirely on this: a failed pull leaves the declared file absent, and that has to be loud rather than clean.

## consumer-partial

**An interaction contains a construct brake cannot model.**

Severity `warning`, fires from `wire` upward.

A plugin-backed content type, an `arrayContains` matcher, a message interaction that constrains a broker topic rather than an HTTP surface. The interaction is named with its pointer rather than skipped, so the verdict says 'not fully verified' instead of 'clean'. A tool that silently ignores what it cannot parse manufactures confidence.

## consumer-path-ambiguous

**A consumer's concrete path matches more than one contract template.**

Severity `warning`, fires from `wire` upward.

`/payments/status` matches both `/payments/{id}` and `/payments/status` when neither is more literal than the other, so the expectation was not verified against either. brake never guesses here: a guessed binding attributes a break to the wrong endpoint, which is worse than declining to attribute it.

## consumer-provider-unmatched

**A consumer declaration names a provider no `[[contract]]` declares.**

Severity `error`, fires from `wire` upward.

A configuration error rather than a compatibility one: the declaration was read and there is nothing to check it against, so the consumer it names is unguarded. Either the provider name in the artifact does not match the contract name in brake.toml, or the contract it constrains is not declared at all.

## consumer-undeclared

**A file parses as a consumer declaration but brake.toml does not declare it.**

Severity `info`, fires from `wire` upward.

brake only checks what it is told about, and a pact sitting in the tree that no `[[consumer]]` declares is a consumer nobody is protecting. Identified by parsing, never by filename: the first version of contract detection called `.github/workflows/api-tests.yaml` an API, and a heuristic that called a fixture a pact would be the same mistake with a new file extension.

## consumer-surface-unused

**No declared consumer uses this endpoint.**

Severity `info`, fires from `wire` upward.

The one rule here that reports a suspected *absence*, which the thesis forbids at commit time — so it is excluded from `check` and emitted by `analyze` and `brake consumers` only, and then only under an explicit `completeness = "closed-world"` declaration. Without that declaration it would be a confident statement about consumers brake has never heard of.
