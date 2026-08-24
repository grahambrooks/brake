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

    // Line endings are normalised on both sides: git checks this file out
    // with CRLF on Windows, and the catalogue generates LF. Comparing raw
    // bytes made the check assert the platform's checkout convention rather
    // than whether the document is current.
    let committed = fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        committed.replace("\r\n", "\n"),
        generated.replace("\r\n", "\n"),
        "docs/rules.md is out of date — run `make docs`"
    );
}

#[test]
fn all_design_documents_are_linked_in_design_readme() {
    let design_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("design");
    let readme = fs::read_to_string(design_dir.join("README.md")).expect("read design/README.md");
    for entry in fs::read_dir(&design_dir).expect("read design dir") {
        let entry = entry.expect("valid entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".md") && name != "README.md" {
            assert!(
                readme.contains(&name),
                "design document `{name}` is not referenced in design/README.md"
            );
        }
    }
}

#[test]
fn roadmap_milestone_definitions_are_consistent() {
    let roadmap_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("design/06-architectural-evolution.md");
    let content = fs::read_to_string(&roadmap_path).expect("read roadmap");
    assert!(
        content.contains("## 9. Implementation roadmap & milestones"),
        "roadmap section missing in 06-architectural-evolution.md"
    );
    for milestone in ["M16", "M17", "M18", "M19", "M20", "M21"] {
        assert!(
            content.contains(&format!("**{milestone}**")),
            "milestone `{milestone}` missing from roadmap table"
        );
    }
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
