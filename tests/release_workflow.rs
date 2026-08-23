//! The release workflow and the Homebrew formula it renders.
//!
//! A release runs once per version and fails in front of users, so the parts
//! that can be checked without running it are checked here: that the archive
//! name the matrix builds is the one the formula asks for, and that every
//! platform the formula serves is a platform something is built for.
//!
//! A mismatch between those two is invisible until `brew install` fails on
//! someone else's machine.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn repo_file(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

/// The targets the build matrix produces artefacts for.
fn built_targets() -> BTreeSet<String> {
    repo_file(".github/workflows/release.yml")
        .lines()
        .filter_map(|line| line.trim().strip_prefix("- target: "))
        .map(|target| target.trim().to_owned())
        .collect()
}

/// The targets the formula script serves.
fn formula_targets() -> BTreeSet<String> {
    repo_file(".github/render-formula.py")
        .lines()
        .skip_while(|line| !line.contains("PLATFORMS = ["))
        .take_while(|line| !line.trim_start().starts_with(']'))
        .filter_map(|line| {
            let start = line.rfind('"')?;
            let head = &line[..start];
            let open = head.rfind('"')?;
            Some(line[open + 1..start].to_owned())
        })
        .filter(|value| value.contains('-'))
        .collect()
}

#[test]
fn every_platform_the_formula_serves_is_actually_built() {
    let built = built_targets();
    let served = formula_targets();

    assert!(!built.is_empty(), "no targets found in the build matrix");
    assert!(
        !served.is_empty(),
        "no platforms found in the formula script"
    );

    for target in &served {
        assert!(
            built.contains(target),
            "the formula serves `{target}`, which the build matrix does not produce — \
             `brew install` would 404 on that platform"
        );
    }
}

#[test]
fn the_formula_renders_and_names_the_archives_the_matrix_builds() {
    let served = formula_targets();
    let checksums: String = format!(
        "{{{}}}",
        served
            .iter()
            .map(|target| format!("\"{target}\":\"0000\""))
            .collect::<Vec<_>>()
            .join(",")
    );

    let output = Command::new("python3")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".github/render-formula.py"))
        .env("VERSION", "2026.8.99")
        .env("CHECKSUMS", &checksums)
        .output()
        .expect("python3 should run the formula script");

    assert!(
        output.status.success(),
        "the formula did not render: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let formula = String::from_utf8_lossy(&output.stdout);

    for target in &served {
        // The matrix packages `brake-${GITHUB_REF_NAME}-${target}` — that is,
        // `brake-v<version>-<target>`. The formula must ask for exactly that.
        let expected = format!("brake-v2026.8.99-{target}.tar.gz");
        assert!(
            formula.contains(&expected),
            "the formula does not reference `{expected}`:\n{formula}"
        );
    }
    assert!(formula.contains(r#"bin.install "brake""#), "{formula}");
}

#[test]
fn the_formula_refuses_to_reference_an_archive_that_was_not_built() {
    // Emitting a formula with a missing checksum defers the failure to
    // `brew install`, on someone else's machine, at the worst moment.
    let output = Command::new("python3")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".github/render-formula.py"))
        .env("VERSION", "2026.8.99")
        .env("CHECKSUMS", r#"{"aarch64-apple-darwin":"0000"}"#)
        .output()
        .expect("python3 should run");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no checksum for"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_released_binaries_include_the_mcp_server() {
    // The feature is off by default so a library consumer does not pay for an
    // async runtime. Someone downloading a binary has already accepted its
    // size, and `brake mcp` failing on a released build would be a poor joke.
    let workflow = repo_file(".github/workflows/release.yml");
    assert!(
        workflow.contains("--features mcp"),
        "the release build does not enable the mcp feature"
    );
}
