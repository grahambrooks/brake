//! The rule catalogue: one entry per rule ID, carrying severity, the minimum
//! compatibility level at which it fires, and the rationale `brake explain`
//! prints.
//!
//! The explanation is the thing a developer reads at the moment they are
//! blocked, which is when someone actually wants to know why the constraint
//! exists. It says why the rule exists, not just what it caught.

use crate::Severity;
use crate::compare::ChangeKind;
use crate::config::Compatibility;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub id: &'static str,
    pub severity: Severity,
    /// The lowest compatibility level at which this rule fires at all.
    ///
    /// A rule not applicable at the selected level does not fire — it is not
    /// downgraded to a warning, because a warning is a thing a human has to
    /// read and dismiss.
    pub level: Compatibility,
    pub summary: &'static str,
    pub explanation: &'static str,
}

impl Rule {
    /// The documentation anchor for this rule, used as the SARIF `helpUri`.
    #[must_use]
    pub fn help_uri(&self) -> String {
        format!(
            "https://github.com/grahambrooks/brake/blob/main/docs/rules.md#{}",
            self.id
        )
    }
}

pub static RULES: &[Rule] = &[
    // ── §5.1 endpoint surface ───────────────────────────────────────────────
    Rule {
        id: "endpoint-removed",
        severity: Severity::Error,
        level: Compatibility::Wire,
        summary: "A baseline endpoint is absent in the head contract.",
        explanation: "Every consumer that still calls this endpoint now gets a 404. There is no \
version of this change that an existing caller survives, which is why it fires at every \
compatibility level. Deprecate the endpoint, ship that, wait for consumers to migrate, and \
remove it afterwards.",
    },
    Rule {
        id: "method-removed",
        severity: Severity::Error,
        level: Compatibility::Wire,
        summary: "A method on a surviving path is absent in the head contract.",
        explanation: "The path still answers, so a consumer gets a 405 rather than a 404 — which \
is harder to diagnose, because the endpoint looks alive. Treat it exactly like an endpoint \
removal: deprecate first.",
    },
    Rule {
        id: "endpoint-path-changed",
        severity: Severity::Error,
        level: Compatibility::Wire,
        summary: "An operationId survived but its path template changed.",
        explanation: "The operation still exists, so this reads as a rename rather than a \
removal — but a consumer is pinned to the old URL and does not know about the new one. If both \
paths must work, serve both for a release rather than moving the operation in one step.",
    },
    Rule {
        id: "path-parameter-renamed",
        severity: Severity::Error,
        level: Compatibility::Surface,
        summary: "A path template kept its shape but renamed a parameter.",
        explanation: "The URL a consumer builds is unchanged, so nothing breaks on the wire. \
Generated clients are a different matter: the parameter name becomes an argument name, so \
renaming it breaks every caller that passes arguments by name. That is why this is a `surface` \
rule and not a `wire` one.",
    },
    Rule {
        id: "operation-id-changed",
        severity: Severity::Error,
        level: Compatibility::Surface,
        summary: "An endpoint kept its path and method but changed its operationId.",
        explanation: "Nothing changes on the wire. Code generators derive method names from \
operationId, so this renames a function in every generated client — a compile error for \
consumers, and invisible to anyone testing over HTTP.",
    },
    Rule {
        id: "endpoint-added",
        severity: Severity::Warning,
        level: Compatibility::Strict,
        summary: "A new endpoint appeared.",
        explanation: "Purely additive, and safe for every consumer. It fires only under `strict`, \
where the contract is frozen and any change at all needs to be a deliberate, reviewed act.",
    },
    // ── §5.2 request compatibility ──────────────────────────────────────────
    Rule {
        id: "param-added-required",
        severity: Severity::Error,
        level: Compatibility::Wire,
        summary: "A new required request parameter or field was added.",
        explanation: "Every existing caller omits it, because it did not exist when they were \
written. Add it as optional with a default, and require it in a later release once callers \
have migrated.",
    },
    Rule {
        id: "param-became-required",
        severity: Severity::Error,
        level: Compatibility::Wire,
        summary: "An optional request parameter or field became required.",
        explanation: "Callers that legitimately omitted it now fail validation. This is the same \
break as adding a required parameter, arriving by a different route.",
    },
    Rule {
        id: "param-removed",
        severity: Severity::Warning,
        level: Compatibility::WireJson,
        summary: "A request parameter was removed.",
        explanation: "Whether this breaks a caller depends on the server: an API that ignores \
unknown parameters tolerates it, and one validating with `additionalProperties: false` rejects \
the request outright. It is a warning rather than an error because brake cannot see which \
one you are.",
    },
    Rule {
        id: "param-type-narrowed",
        severity: Severity::Error,
        level: Compatibility::Wire,
        summary: "A request parameter or body type stopped accepting values it used to accept.",
        explanation: "Narrowing an input — `string` to `integer`, a smaller `maxLength`, a \
removed enum member, nullable becoming non-nullable, `additionalProperties` closing — rejects \
requests that were valid yesterday. Widening an input is always safe; narrowing never is.",
    },
    Rule {
        id: "param-location-changed",
        severity: Severity::Error,
        level: Compatibility::Wire,
        summary: "A parameter moved between query, path, header or cookie.",
        explanation: "The parameter still exists under the same name, so this is easy to read as \
a harmless move. It is not: a caller sending it in the old location is now sending an unknown \
parameter and omitting a known one, at the same time.",
    },
    Rule {
        id: "param-added-optional",
        severity: Severity::Info,
        level: Compatibility::Strict,
        summary: "A new optional request parameter or field was added.",
        explanation: "Additive and safe: callers that ignore it are unaffected. It fires only \
under `strict`, where the contract is frozen.",
    },
    Rule {
        id: "request-media-type-removed",
        severity: Severity::Error,
        level: Compatibility::Wire,
        summary: "A request media type is no longer accepted.",
        explanation: "A caller sending that Content-Type now gets a 415. Dropping XML because \
'everyone uses JSON' is the usual way this lands, and the callers who did not get the memo are \
exactly the ones who will not notice until production.",
    },
    // ── §5.3 response compatibility ─────────────────────────────────────────
    Rule {
        id: "response-field-removed",
        severity: Severity::Error,
        level: Compatibility::WireJson,
        summary: "A field present in a baseline response is gone.",
        explanation: "Any consumer reading that field now gets nothing, and a consumer \
deserialising into a type with a non-optional field for it fails outright. Deprecate the field \
for a release before removing it — that is the sanctioned path, and a team that follows it \
never needs a suppression.",
    },
    Rule {
        id: "response-field-optional",
        severity: Severity::Error,
        level: Compatibility::WireJson,
        summary: "A response field that was always present became optional.",
        explanation: "Nothing was removed, so this looks safe in a diff. But a consumer whose \
type for this field is non-optional — which is what a generator produces from a `required` \
field — now fails to deserialise whenever the field is absent. It is a removal that only \
happens sometimes, which makes it harder to debug, not easier.",
    },
    Rule {
        id: "response-field-added",
        severity: Severity::Info,
        level: Compatibility::Strict,
        summary: "A response gained a field.",
        explanation: "Safe for any tolerant reader, and a break only for a consumer that rejects \
unknown fields. It fires only under `strict`, where the contract is frozen and additions are \
reviewed too.",
    },
    Rule {
        id: "response-type-changed",
        severity: Severity::Error,
        level: Compatibility::WireJson,
        summary: "A response type changed incompatibly.",
        explanation: "The bytes a consumer receives no longer match the shape it was written \
against. Changing a type in place gives consumers no migration window; add a new field \
alongside the old one and remove the old one after a deprecation period.",
    },
    Rule {
        id: "response-enum-extended",
        severity: Severity::Warning,
        level: Compatibility::WireJson,
        summary: "A response enum gained a value.",
        explanation: "A tolerant reader copes. A generated Rust or TypeScript client matching \
exhaustively on the enum does not — it panics, throws, or fails to compile on upgrade. \
`graphql-inspector` calls this DANGEROUS rather than breaking for the same reason, and it is a \
warning here so that teams generating exhaustive clients can raise it deliberately.",
    },
    Rule {
        id: "response-status-removed",
        severity: Severity::Error,
        level: Compatibility::WireJson,
        summary: "A documented response status code is gone.",
        explanation: "A consumer with a branch for that status has dead code at best, and at \
worst is now falling through to an error path for a response it used to handle.",
    },
    Rule {
        id: "response-status-added",
        severity: Severity::Info,
        level: Compatibility::Strict,
        summary: "A response gained a documented status code.",
        explanation: "Additive documentation of an outcome that may already have been possible. \
It fires only under `strict`.",
    },
    Rule {
        id: "response-media-type-removed",
        severity: Severity::Error,
        level: Compatibility::WireJson,
        summary: "A response media type is gone.",
        explanation: "A consumer sending an Accept header for that type now gets a 406, or \
silently receives a format it cannot parse.",
    },
    // ── wire identity ───────────────────────────────────────────────────────
    Rule {
        id: "field-number-changed",
        severity: Severity::Error,
        level: Compatibility::Wire,
        summary: "A field kept its name and changed its wire number.",
        explanation: "In protobuf the field number *is* the field: it is what appears on the \
wire, and the name exists only in the source. Renumbering a field means every already-deployed \
client decodes those bytes into a different field, or drops them as unknown. This is the \
canonical protobuf break and it is invisible in a name-based diff.",
    },
    Rule {
        id: "field-renamed",
        severity: Severity::Error,
        level: Compatibility::WireJson,
        summary: "A field kept its wire number and changed its name.",
        explanation: "The binary encoding is unaffected, so this is safe at `wire`. Anything \
reading the JSON mapping of the message, or any generated struct field, breaks — which is why \
it fires from `wire-json` upward and not below.",
    },
    // ── §5.4 security ───────────────────────────────────────────────────────
    Rule {
        id: "security-added",
        severity: Severity::Error,
        level: Compatibility::WireJson,
        summary: "An endpoint gained a security requirement, or a stronger one.",
        explanation: "Every existing caller is now unauthenticated or under-scoped, and gets a \
401 or 403. Strengthening authentication is often correct and urgent — but it is a breaking \
change, and shipping it without telling consumers turns a security improvement into an outage.",
    },
    Rule {
        id: "security-scheme-changed",
        severity: Severity::Error,
        level: Compatibility::WireJson,
        summary: "A security scheme's type, transport or flow changed.",
        explanation: "Consumers built credentials for the old scheme. Swapping an API key for \
OAuth, or moving a token from a header to a cookie, invalidates every one of them.",
    },
    Rule {
        id: "security-removed",
        severity: Severity::Warning,
        level: Compatibility::WireJson,
        summary: "An endpoint lost a security requirement.",
        explanation: "Not a compatibility break — existing callers keep working, and that is \
precisely the problem. An endpoint that quietly stopped requiring authentication is almost \
always an accident, and nothing else in the pipeline will notice.",
    },
    // ── §5.5 deprecation hygiene ────────────────────────────────────────────
    Rule {
        id: "removed-without-deprecation",
        severity: Severity::Error,
        level: Compatibility::WireJson,
        summary: "Something was removed that was not marked deprecated in the baseline.",
        explanation: "This is the rule that makes the rest of the gate humane. The sanctioned \
path for any removal is deprecate, ship, wait, remove — and a team that follows it never needs \
a suppression, because by the time the removal lands the baseline already says `deprecated: \
true`. If you are seeing this, the removal skipped a step rather than the gate being wrong.",
    },
    Rule {
        id: "deprecated-no-sunset",
        severity: Severity::Info,
        level: Compatibility::WireJson,
        summary: "An endpoint is deprecated with no `x-sunset` date.",
        explanation: "A deprecation with no date is a deprecation that never ends, and consumers \
correctly read it as no reason to move. Give it a date so the eventual removal is something \
they were told about rather than something that happens to them.",
    },
    // ── §5.6 integrity ──────────────────────────────────────────────────────
    Rule {
        id: "contract-unreachable",
        severity: Severity::Error,
        level: Compatibility::Wire,
        summary: "The configured contract source could not be read or parsed.",
        explanation: "A contract that cannot be read cannot be verified, and reporting clean \
would be reporting a verification that did not happen. This also covers a `$ref` that resolves \
over the network or outside the source directory: brake refuses those rather than fetching \
them, because remote refs are the largest source of non-determinism in OpenAPI tooling.",
    },
    Rule {
        id: "contract-partial",
        severity: Severity::Warning,
        level: Compatibility::Wire,
        summary: "A compared path contains a construct brake cannot model.",
        explanation: "The comparison happened, but not over the whole payload — so the result is \
'not fully verified', never 'clean'. A tool that silently ignores what it cannot parse is worse \
than no tool, because it manufactures confidence. The finding names the construct and its JSON \
pointer so you can decide whether the unverified part matters.",
    },
    Rule {
        id: "stale-allow",
        severity: Severity::Error,
        level: Compatibility::Wire,
        summary: "A suppression matches nothing.",
        explanation: "The break it was written for is gone, so the suppression is now a blanket \
permission for a finding nobody has reviewed. Dead suppressions accumulate into a list that \
hides live problems, which is the failure mode this rule exists to prevent.",
    },
    Rule {
        id: "expired-allow",
        severity: Severity::Error,
        level: Compatibility::Wire,
        summary: "A suppression is past its `expires` date.",
        explanation: "The exception was granted until a date, and that date has passed. Either \
the migration finished and the suppression should go, or it did not and that is worth someone \
knowing about.",
    },
    Rule {
        id: "baseline-unconfigured",
        severity: Severity::Info,
        level: Compatibility::Wire,
        summary: "A contract has no baseline, so nothing was compared.",
        explanation: "This is a user who has not opted in, not a broken gate, and the two must \
never share an exit code. A *missing* baseline — one configured but unresolvable — is a tool \
failure and exits 2. An *unconfigured* baseline exits 0 with this note, because failing a build \
over configuration nobody has written yet teaches a team to disable the tool.",
    },
    Rule {
        id: "contract-new",
        severity: Severity::Info,
        level: Compatibility::Wire,
        summary: "A contract has no previous version in the baseline.",
        explanation: "The contract is new: it does not exist in the baseline, so there is nothing \
it could have broken. This is deliberately not a tool failure — a `git-merge-base` baseline does \
not contain a file added by the change being checked, and failing there would make every new API \
file fail CI on the commit that introduces it. The next commit compares normally.",
    },
    Rule {
        id: "contract-unconfigured",
        severity: Severity::Info,
        level: Compatibility::Wire,
        summary: "A file that looks like an API contract is not declared in brake.toml.",
        explanation: "brake only checks what it is told about. A new OpenAPI, proto or GraphQL \
file that no `[[contract]]` declares is silently ungated, and the whole point of a gate is that \
its coverage is not a matter of luck. Declare it, or move it somewhere the hook does not watch.",
    },
    Rule {
        id: "generated-drift",
        severity: Severity::Error,
        level: Compatibility::Wire,
        summary: "Generated contract output differs from the checked-in artifact.",
        explanation: "The committed contract no longer matches what the code produces, so every \
check brake ran was against a stale document. Regenerate and commit the result.",
    },
];

/// The rule a change maps to. One kind, one rule.
#[must_use]
pub fn rule_for(kind: ChangeKind) -> &'static Rule {
    let id = match kind {
        ChangeKind::EndpointRemoved => "endpoint-removed",
        ChangeKind::MethodRemoved => "method-removed",
        ChangeKind::EndpointPathChanged => "endpoint-path-changed",
        ChangeKind::EndpointAdded => "endpoint-added",
        ChangeKind::OperationIdChanged => "operation-id-changed",
        ChangeKind::PathParameterRenamed => "path-parameter-renamed",
        ChangeKind::ParamAddedRequired => "param-added-required",
        ChangeKind::ParamBecameRequired => "param-became-required",
        ChangeKind::ParamRemoved => "param-removed",
        ChangeKind::ParamTypeNarrowed => "param-type-narrowed",
        ChangeKind::ParamLocationChanged => "param-location-changed",
        ChangeKind::ParamAddedOptional => "param-added-optional",
        ChangeKind::RequestMediaTypeRemoved => "request-media-type-removed",
        ChangeKind::ResponseFieldRemoved => "response-field-removed",
        ChangeKind::ResponseFieldOptional => "response-field-optional",
        ChangeKind::ResponseFieldAdded => "response-field-added",
        ChangeKind::ResponseTypeChanged => "response-type-changed",
        ChangeKind::ResponseEnumExtended => "response-enum-extended",
        ChangeKind::ResponseStatusRemoved => "response-status-removed",
        ChangeKind::ResponseStatusAdded => "response-status-added",
        ChangeKind::ResponseMediaTypeRemoved => "response-media-type-removed",
        ChangeKind::FieldRenamed => "field-renamed",
        ChangeKind::FieldNumberChanged => "field-number-changed",
        ChangeKind::SecurityAdded => "security-added",
        ChangeKind::SecurityRemoved => "security-removed",
        ChangeKind::SecuritySchemeChanged => "security-scheme-changed",
        ChangeKind::RemovedWithoutDeprecation => "removed-without-deprecation",
        ChangeKind::DeprecatedNoSunset => "deprecated-no-sunset",
        ChangeKind::ContractPartial => "contract-partial",
    };
    lookup(id).expect("every ChangeKind maps to a catalogued rule")
}

/// The catalogue as Markdown, with one `#`-anchor per rule id.
///
/// `docs/rules.md` is generated from this rather than maintained by hand: it
/// exists because the SARIF `helpUri` needs somewhere to point, and a
/// hand-written copy of the catalogue would be wrong within a month. A test
/// asserts the committed file still matches.
#[must_use]
pub fn markdown() -> String {
    let mut out = String::new();
    out.push_str("# Rule catalogue\n\n");
    out.push_str(
        "<!-- Generated from `src/rules/catalogue.rs` by `make docs`. Do not edit by hand. -->\n\n",
    );
    out.push_str(
        "Every rule brake can report, why it exists, and the lowest compatibility level at which \
it fires. A rule outside the selected level does not fire at all — it is not downgraded to a \
warning, because a warning is a thing a human has to read and dismiss. The levels are specified \
in [design/02-contract-gates.md](../design/02-contract-gates.md).\n\n",
    );
    out.push_str(
        "Run `brake explain <rule-id>` to read any of this at the moment you are blocked by it, \
or `brake explain` with no argument to list the catalogue.\n\n",
    );
    out.push_str("| Rule | Severity | Fires from | Summary |\n| --- | --- | --- | --- |\n");

    for rule in RULES {
        out.push_str(&format!(
            "| [`{}`](#{}) | {} | `{}` | {} |\n",
            rule.id,
            rule.id,
            severity_name(rule.severity),
            level_name(rule.level),
            rule.summary,
        ));
    }

    for rule in RULES {
        out.push_str(&format!(
            "\n## {}\n\n**{}**\n\nSeverity `{}`, fires from `{}` upward.\n\n{}\n",
            rule.id,
            rule.summary,
            severity_name(rule.severity),
            level_name(rule.level),
            rule.explanation,
        ));
    }
    out
}

fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Info => "info",
        Severity::Warning => "warning",
        Severity::Error => "error",
    }
}

fn level_name(level: Compatibility) -> &'static str {
    match level {
        Compatibility::Wire => "wire",
        Compatibility::WireJson => "wire-json",
        Compatibility::Surface => "surface",
        Compatibility::Strict => "strict",
    }
}

#[must_use]
pub fn lookup(id: &str) -> Option<&'static Rule> {
    RULES.iter().find(|rule| rule.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_rule_has_a_real_explanation() {
        for rule in RULES {
            assert!(
                !rule.summary.trim().is_empty(),
                "empty summary: {}",
                rule.id
            );
            assert!(
                rule.explanation.trim().len() > 80,
                "placeholder explanation for {}",
                rule.id
            );
            assert!(
                !rule.explanation.to_lowercase().contains("todo"),
                "placeholder explanation for {}",
                rule.id
            );
        }
    }

    #[test]
    fn rule_ids_are_unique_and_kebab_case() {
        let mut seen = std::collections::BTreeSet::new();
        for rule in RULES {
            assert!(seen.insert(rule.id), "duplicate rule id {}", rule.id);
            assert!(
                rule.id.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "rule id is not kebab-case: {}",
                rule.id
            );
        }
    }

    #[test]
    fn every_change_kind_resolves_to_a_rule() {
        // Exhaustive by construction: `rule_for` panics on an unmapped kind,
        // and this list must be updated when a kind is added.
        for kind in [
            ChangeKind::EndpointRemoved,
            ChangeKind::MethodRemoved,
            ChangeKind::EndpointPathChanged,
            ChangeKind::EndpointAdded,
            ChangeKind::OperationIdChanged,
            ChangeKind::PathParameterRenamed,
            ChangeKind::ParamAddedRequired,
            ChangeKind::ParamBecameRequired,
            ChangeKind::ParamRemoved,
            ChangeKind::ParamTypeNarrowed,
            ChangeKind::ParamLocationChanged,
            ChangeKind::ParamAddedOptional,
            ChangeKind::RequestMediaTypeRemoved,
            ChangeKind::ResponseFieldRemoved,
            ChangeKind::ResponseFieldOptional,
            ChangeKind::ResponseFieldAdded,
            ChangeKind::ResponseTypeChanged,
            ChangeKind::ResponseEnumExtended,
            ChangeKind::ResponseStatusRemoved,
            ChangeKind::ResponseStatusAdded,
            ChangeKind::ResponseMediaTypeRemoved,
            ChangeKind::FieldRenamed,
            ChangeKind::FieldNumberChanged,
            ChangeKind::SecurityAdded,
            ChangeKind::SecurityRemoved,
            ChangeKind::SecuritySchemeChanged,
            ChangeKind::RemovedWithoutDeprecation,
            ChangeKind::DeprecatedNoSunset,
            ChangeKind::ContractPartial,
        ] {
            let rule = rule_for(kind);
            assert!(!rule.id.is_empty(), "unmapped kind {kind:?}");
        }
    }

    #[test]
    fn all_four_levels_are_used_by_some_rule() {
        for level in [
            Compatibility::Wire,
            Compatibility::WireJson,
            Compatibility::Surface,
            Compatibility::Strict,
        ] {
            assert!(
                RULES.iter().any(|rule| rule.level == level),
                "no rule fires first at {level:?}, so selecting it changes nothing"
            );
        }
    }

    #[test]
    fn help_uri_is_stable_and_anchored_on_the_rule_id() {
        let rule = lookup("endpoint-removed").expect("known rule");
        assert!(rule.help_uri().ends_with("#endpoint-removed"));
    }
}
