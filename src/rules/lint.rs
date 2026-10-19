//! Static structural lint pass.
//!
//! Checks the head contract for defects that break consumers regardless of
//! version history. See `design/07-structural-lint.md`.

use std::collections::{BTreeMap, BTreeSet};

use crate::config::Compatibility;
use crate::contract::{Contract, LintCandidateKind};
use crate::rules::{Finding, catalogue};

/// Run all structural lint rules against a single contract.
///
/// Returns findings at or above the given compatibility level; rules below it
/// do not fire (consistent with `rules::evaluate`).
#[must_use]
pub fn lint(contract: &Contract, contract_name: &str, level: Compatibility) -> Vec<Finding> {
    let mut findings = Vec::new();

    findings.extend(from_candidates(contract, contract_name, level));
    findings.extend(check_operation_id_duplicate(contract, contract_name, level));
    findings.extend(check_path_param_undeclared(contract, contract_name, level));

    findings.sort();
    findings.dedup();
    findings
}

/// The lint findings `head` has and `base` does not.
///
/// A check reports what a change introduced, never what was already there. A
/// defect already in the baseline fails no commit, which is the ratchet every
/// comparison rule follows (`design/01-thesis.md`, "Scope is a better ratchet
/// than a baseline"). Reporting the whole file instead blocked every edit to a
/// contract that had one old duplicate `operationId`.
#[must_use]
pub fn introduced(
    base: &Contract,
    head: &Contract,
    contract_name: &str,
    level: Compatibility,
) -> Vec<Finding> {
    let base_findings = lint(base, contract_name, level);
    let existing: BTreeSet<_> = base_findings.iter().map(identity).collect();
    lint(head, contract_name, level)
        .into_iter()
        .filter(|finding| !existing.contains(&identity(finding)))
        .collect()
}

/// What makes two lint findings the same defect.
///
/// Not the span and not the message: a defect that moved down the file, or a
/// duplicate `operationId` that gained a third endpoint, is still the defect
/// the baseline already had.
fn identity(finding: &Finding) -> (&'static str, &str, Option<&str>, Option<&str>, Option<&str>) {
    (
        finding.rule_id,
        finding.pointer.as_str(),
        finding.subject.as_deref(),
        finding.method.as_deref(),
        finding.path.as_deref(),
    )
}

fn from_candidates(contract: &Contract, contract_name: &str, level: Compatibility) -> Vec<Finding> {
    contract
        .lint_candidates
        .iter()
        .filter_map(|candidate| {
            let rule_id = match candidate.kind {
                LintCandidateKind::EnumTypeMismatch => "enum-type-mismatch",
                LintCandidateKind::EnumDuplicateValue => "enum-duplicate-value",
            };
            let rule = catalogue::lookup(rule_id)?;
            if level < rule.level {
                return None;
            }
            Some(Finding {
                affects: Vec::new(),
                note: None,
                rule_id: rule.id,
                severity: rule.severity,
                contract: contract_name.to_owned(),
                message: candidate.detail.clone(),
                method: None,
                path: None,
                pointer: candidate.pointer.clone(),
                subject: None,
                span: Some(candidate.span.clone()),
            })
        })
        .collect()
}

fn check_operation_id_duplicate(
    contract: &Contract,
    contract_name: &str,
    level: Compatibility,
) -> Vec<Finding> {
    let rule = catalogue::lookup("operation-id-duplicate").expect("catalogued");
    if level < rule.level {
        return Vec::new();
    }

    // Collect every (operation_id → list of endpoints that use it).
    let mut by_id: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (key, endpoint) in &contract.endpoints {
        if let Some(id) = endpoint.operation_id.as_deref() {
            by_id
                .entry(id)
                .or_default()
                .push(format!("{} {}", key.method, key.path));
        }
    }

    by_id
        .into_iter()
        .filter(|(_, endpoints)| endpoints.len() > 1)
        .map(|(id, endpoints)| {
            let list = endpoints.join(", ");
            Finding {
                affects: Vec::new(),
                note: None,
                rule_id: rule.id,
                severity: rule.severity,
                contract: contract_name.to_owned(),
                message: format!("operationId `{id}` is used by multiple operations: {list}"),
                method: None,
                path: None,
                pointer: String::new(),
                subject: Some(id.to_owned()),
                span: None,
            }
        })
        .collect()
}

fn check_path_param_undeclared(
    contract: &Contract,
    contract_name: &str,
    level: Compatibility,
) -> Vec<Finding> {
    let rule = catalogue::lookup("path-param-undeclared").expect("catalogued");
    if level < rule.level {
        return Vec::new();
    }

    let mut findings = Vec::new();
    for (key, endpoint) in &contract.endpoints {
        let declared_path_params: std::collections::BTreeSet<&str> = endpoint
            .parameters
            .iter()
            .filter(|p| p.location == "path")
            .map(|p| p.name.as_str())
            .collect();

        for param in path_template_params(&key.path) {
            if !declared_path_params.contains(param) {
                findings.push(Finding {
                    affects: Vec::new(),
                    note: None,
                    rule_id: rule.id,
                    severity: rule.severity,
                    contract: contract_name.to_owned(),
                    message: format!(
                        "path template `{path}` uses `{{{param}}}` but no `in: path` \
                         parameter named `{param}` is declared on `{method} {path}`",
                        path = key.path,
                        method = key.method,
                    ),
                    method: Some(key.method.clone()),
                    path: Some(key.path.clone()),
                    pointer: format!(
                        "/paths/{}/{}",
                        key.path.replace('~', "~0").replace('/', "~1"),
                        key.method.to_ascii_lowercase()
                    ),
                    subject: Some(param.to_owned()),
                    span: Some(endpoint.span.clone()),
                });
            }
        }
    }
    findings
}

/// Extract `param` names from `{param}` placeholders in an OpenAPI path template.
fn path_template_params(path: &str) -> impl Iterator<Item = &str> {
    path.split('{')
        .skip(1)
        .filter_map(|segment| segment.split('}').next())
        .filter(|name| !name.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Compatibility;
    use crate::contract::Contract;

    fn parse(yaml: &str) -> Contract {
        crate::contract::openapi::ingest("test.yaml", yaml.as_bytes())
            .expect("valid OpenAPI fixture")
    }

    // ── introduced ───────────────────────────────────────────────────────────

    const DUPLICATED: &str = "openapi: 3.1.0
info: {title: T, version: 0}
paths:
  /a:
    get:
      operationId: doThing
      responses: {'200': {description: ok}}
  /b:
    get:
      operationId: doThing
      responses: {'200': {description: ok}}
";

    #[test]
    fn a_defect_already_in_the_baseline_is_not_introduced() {
        let contract = parse(DUPLICATED);
        let introduced = introduced(&contract, &contract, "api", Compatibility::Surface);
        assert!(introduced.is_empty(), "{introduced:?}");
    }

    #[test]
    fn a_defect_the_change_adds_is_introduced() {
        let base = parse(&DUPLICATED.replace(
            "operationId: doThing\n      responses: {'200': {description: ok}}\n  /b",
            "operationId: doOther\n      responses: {'200': {description: ok}}\n  /b",
        ));
        let head = parse(DUPLICATED);
        let ids: Vec<_> = introduced(&base, &head, "api", Compatibility::Surface)
            .iter()
            .map(|finding| finding.rule_id)
            .collect();
        assert_eq!(ids, vec!["operation-id-duplicate"]);
    }

    #[test]
    fn an_old_defect_that_moved_is_still_the_old_defect() {
        let base = parse(DUPLICATED);
        let head = parse(&DUPLICATED.replace(
            "paths:\n",
            "paths:\n  /z:\n    get:\n      operationId: unrelated\n      responses: {'200': {description: ok}}\n",
        ));
        let introduced = introduced(&base, &head, "api", Compatibility::Surface);
        assert!(introduced.is_empty(), "{introduced:?}");
    }

    // ── operation-id-duplicate ───────────────────────────────────────────────

    #[test]
    fn operation_id_duplicate_fires_when_two_ops_share_an_id() {
        let contract = parse(
            "openapi: 3.1.0
info: {title: T, version: 0}
paths:
  /a:
    get:
      operationId: doThing
      responses: {'200': {description: ok}}
  /b:
    get:
      operationId: doThing
      responses: {'200': {description: ok}}
",
        );
        let findings = lint(&contract, "api", Compatibility::Surface);
        let ids: Vec<_> = findings.iter().map(|f| f.rule_id).collect();
        assert!(
            ids.contains(&"operation-id-duplicate"),
            "expected operation-id-duplicate, got: {ids:?}"
        );
    }

    #[test]
    fn operation_id_duplicate_silent_when_ids_unique() {
        let contract = parse(
            "openapi: 3.1.0
info: {title: T, version: 0}
paths:
  /a:
    get:
      operationId: getA
      responses: {'200': {description: ok}}
  /b:
    get:
      operationId: getB
      responses: {'200': {description: ok}}
",
        );
        let findings = lint(&contract, "api", Compatibility::Surface);
        assert!(
            findings
                .iter()
                .all(|f| f.rule_id != "operation-id-duplicate"),
            "should not fire when all operationIds are unique"
        );
    }

    #[test]
    fn operation_id_duplicate_not_fire_below_surface_level() {
        let contract = parse(
            "openapi: 3.1.0
info: {title: T, version: 0}
paths:
  /a:
    get:
      operationId: doThing
      responses: {'200': {description: ok}}
  /b:
    get:
      operationId: doThing
      responses: {'200': {description: ok}}
",
        );
        // At WireJson level, Surface rules do not fire.
        let findings = lint(&contract, "api", Compatibility::WireJson);
        assert!(
            findings
                .iter()
                .all(|f| f.rule_id != "operation-id-duplicate"),
            "Surface rule must not fire below Surface level"
        );
    }

    // ── path-param-undeclared ────────────────────────────────────────────────

    #[test]
    fn path_param_undeclared_fires_when_template_param_missing() {
        let contract = parse(
            "openapi: 3.1.0
info: {title: T, version: 0}
paths:
  /payments/{id}:
    get:
      responses: {'200': {description: ok}}
",
        );
        let findings = lint(&contract, "api", Compatibility::Surface);
        let ids: Vec<_> = findings.iter().map(|f| f.rule_id).collect();
        assert!(
            ids.contains(&"path-param-undeclared"),
            "expected path-param-undeclared, got: {ids:?}"
        );
        assert_eq!(
            findings
                .iter()
                .find(|f| f.rule_id == "path-param-undeclared")
                .and_then(|f| f.subject.as_deref()),
            Some("id")
        );
    }

    #[test]
    fn path_param_undeclared_silent_when_param_declared() {
        let contract = parse(
            "openapi: 3.1.0
info: {title: T, version: 0}
paths:
  /payments/{id}:
    get:
      parameters:
        - name: id
          in: path
          required: true
          schema: {type: string}
      responses: {'200': {description: ok}}
",
        );
        let findings = lint(&contract, "api", Compatibility::Surface);
        assert!(
            findings
                .iter()
                .all(|f| f.rule_id != "path-param-undeclared"),
            "should not fire when path param is properly declared"
        );
    }

    #[test]
    fn path_param_undeclared_accepts_path_item_level_parameters() {
        let contract = parse(
            "openapi: 3.1.0
info: {title: T, version: 0}
paths:
  /payments/{id}:
    parameters:
      - name: id
        in: path
        required: true
        schema: {type: string}
    get:
      responses: {'200': {description: ok}}
",
        );
        let findings = lint(&contract, "api", Compatibility::Surface);
        assert!(
            findings
                .iter()
                .all(|f| f.rule_id != "path-param-undeclared"),
            "path-item-level parameters satisfy the path template"
        );
    }

    #[test]
    fn path_param_undeclared_not_fire_below_surface_level() {
        let contract = parse(
            "openapi: 3.1.0
info: {title: T, version: 0}
paths:
  /payments/{id}:
    get:
      responses: {'200': {description: ok}}
",
        );
        let findings = lint(&contract, "api", Compatibility::WireJson);
        assert!(
            findings
                .iter()
                .all(|f| f.rule_id != "path-param-undeclared"),
            "Surface rule must not fire below Surface level"
        );
    }

    // ── path_template_params helper ──────────────────────────────────────────

    #[test]
    fn extracts_params_from_path_template() {
        let params: Vec<&str> =
            path_template_params("/payments/{id}/receipts/{receiptId}").collect();
        assert_eq!(params, vec!["id", "receiptId"]);
    }

    #[test]
    fn no_params_in_plain_path() {
        let params: Vec<&str> = path_template_params("/payments").collect();
        assert!(params.is_empty());
    }

    // ── enum-type-mismatch ───────────────────────────────────────────────────

    #[test]
    fn enum_type_mismatch_fires_on_string_in_integer_enum() {
        let contract = parse(
            "openapi: 3.1.0
info: {title: T, version: 0}
paths:
  /a:
    get:
      responses:
        '200':
          content:
            application/json:
              schema:
                type: integer
                enum: [1, 2, three]
",
        );
        let findings = lint(&contract, "api", Compatibility::Wire);
        let ids: Vec<_> = findings.iter().map(|f| f.rule_id).collect();
        assert!(
            ids.contains(&"enum-type-mismatch"),
            "expected enum-type-mismatch, got: {ids:?}"
        );
    }

    #[test]
    fn enum_type_mismatch_silent_when_values_match_type() {
        let contract = parse(
            "openapi: 3.1.0
info: {title: T, version: 0}
paths:
  /a:
    get:
      responses:
        '200':
          content:
            application/json:
              schema:
                type: string
                enum: [active, inactive]
",
        );
        let findings = lint(&contract, "api", Compatibility::Wire);
        assert!(
            findings.iter().all(|f| f.rule_id != "enum-type-mismatch"),
            "should not fire when all enum values match declared type"
        );
    }

    // ── enum-duplicate-value ─────────────────────────────────────────────────

    #[test]
    fn enum_duplicate_value_fires_on_repeated_member() {
        let contract = parse(
            "openapi: 3.1.0
info: {title: T, version: 0}
paths:
  /a:
    get:
      responses:
        '200':
          content:
            application/json:
              schema:
                type: string
                enum: [active, inactive, active]
",
        );
        let findings = lint(&contract, "api", Compatibility::Wire);
        let ids: Vec<_> = findings.iter().map(|f| f.rule_id).collect();
        assert!(
            ids.contains(&"enum-duplicate-value"),
            "expected enum-duplicate-value, got: {ids:?}"
        );
    }

    #[test]
    fn enum_duplicate_value_silent_when_all_unique() {
        let contract = parse(
            "openapi: 3.1.0
info: {title: T, version: 0}
paths:
  /a:
    get:
      responses:
        '200':
          content:
            application/json:
              schema:
                type: string
                enum: [active, inactive, pending]
",
        );
        let findings = lint(&contract, "api", Compatibility::Wire);
        assert!(
            findings.iter().all(|f| f.rule_id != "enum-duplicate-value"),
            "should not fire when all enum values are unique"
        );
    }
}
