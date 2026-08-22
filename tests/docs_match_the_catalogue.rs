//! `docs/rules.md` is generated from the rule catalogue.
//!
//! It is the target of every SARIF `helpUri`, so a stale copy sends a
//! developer following an annotation to a page that does not describe the rule
//! that fired. Regenerate with `make docs`.

use std::fs;
use std::path::PathBuf;

fn docs_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/rules.md")
}

#[test]
fn the_committed_rules_document_matches_the_catalogue() {
    let generated = brake::rules::catalogue::markdown();
    let path = docs_path();

    if std::env::var_os("BRAKE_BLESS").is_some() {
        fs::write(&path, &generated).expect("write docs/rules.md");
        return;
    }

    let committed = fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        committed, generated,
        "docs/rules.md is out of date — run `make docs`"
    );
}

#[test]
fn every_rule_has_an_anchor_its_help_uri_can_reach() {
    let markdown = brake::rules::catalogue::markdown();
    for rule in brake::rules::catalogue::RULES {
        assert!(
            markdown.contains(&format!("\n## {}\n", rule.id)),
            "`{}` has no heading, so its helpUri anchor is dead",
            rule.id
        );
        assert!(
            rule.help_uri().ends_with(&format!("#{}", rule.id)),
            "`{}` helpUri does not match its anchor",
            rule.id
        );
    }
}
