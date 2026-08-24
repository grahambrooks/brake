//! One set of guidance, in the three places agents look for it.
//!
//! Claude Code reads `CLAUDE.md` and `.claude/skills/`; other agents read
//! `AGENTS.md`; Junie reads `.junie/skills/`. Nothing in any of those tools
//! follows a pointer between them reliably, so the copies are real files —
//! which means the only thing standing between them and silent drift is this
//! test. `CLAUDE.md` and `.claude/skills/` are canonical; regenerate the
//! copies with `make agents`.
//!
//! The same shape as `docs_match_the_catalogue`: generated, compared, and
//! blessed with `BRAKE_BLESS=1`.

use std::fs;
use std::path::{Path, PathBuf};

fn repo(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

/// `AGENTS.md`, as it should be: `CLAUDE.md` under its own name, with a banner
/// saying where it came from.
fn expected_agents_md() -> String {
    let canonical = read(&repo("CLAUDE.md"));
    let body = canonical
        .split_once('\n')
        .map_or(canonical.as_str(), |(_heading, rest)| rest);
    format!(
        "# AGENTS.md\n\
         \n\
         <!-- Generated from CLAUDE.md by `make agents`. Do not edit by hand. -->\n\
         {body}"
    )
}

/// Read a file with line endings normalised.
///
/// git checks these out with CRLF on Windows while everything here generates
/// LF, and comparing raw bytes would assert the checkout convention rather
/// than whether the copy is current.
fn read(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_default()
        .replace("\r\n", "\n")
}

fn skills() -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(repo(".claude/skills"))
        .expect("read .claude/skills")
        .filter_map(|entry| {
            let entry = entry.ok()?;
            entry.path().is_dir().then(|| entry.file_name())
        })
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no skills found under .claude/skills");
    names
}

#[test]
fn agents_md_matches_claude_md() {
    let expected = expected_agents_md();
    let path = repo("AGENTS.md");

    if std::env::var_os("BRAKE_BLESS").is_some() {
        fs::write(&path, &expected).expect("write AGENTS.md");
        return;
    }

    assert_eq!(
        read(&path),
        expected,
        "AGENTS.md has drifted from CLAUDE.md — run `make agents`"
    );
}

#[test]
fn junie_skills_match_claude_skills() {
    let blessing = std::env::var_os("BRAKE_BLESS").is_some();

    for skill in skills() {
        let canonical = repo(&format!(".claude/skills/{skill}/SKILL.md"));
        let copy = repo(&format!(".junie/skills/{skill}/SKILL.md"));

        if blessing {
            fs::create_dir_all(copy.parent().expect("skill directory")).expect("create dir");
            fs::copy(&canonical, &copy).expect("copy skill");
            continue;
        }

        assert_eq!(
            read(&copy),
            read(&canonical),
            "`.junie/skills/{skill}` has drifted from `.claude/skills/{skill}` — run `make agents`"
        );
    }
}

/// A skill that exists only for Junie is guidance Claude Code never sees, and
/// the next edit to the canonical set will not reach it.
#[test]
fn junie_has_no_skill_of_its_own() {
    let junie = repo(".junie/skills");
    if !junie.is_dir() {
        return;
    }
    let canonical = skills();
    for entry in fs::read_dir(&junie).expect("read .junie/skills") {
        let entry = entry.expect("valid entry");
        if !entry.path().is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        assert!(
            canonical.contains(&name),
            "`.junie/skills/{name}` has no counterpart under `.claude/skills` — \
             move it there and run `make agents`"
        );
    }
}
