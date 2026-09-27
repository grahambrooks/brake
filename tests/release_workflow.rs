//! The release workflow and the Homebrew formula it renders.
//!
//! Releases are built by the standard release-kit v2 workflow: the byte-identical
//! `.github/workflows/release.yml` and `scripts/release.py`, configured by
//! `.release.env`. A release runs once per version and fails in front of users,
//! so the parts that can be checked without running it are checked here: that
//! every platform the formula serves is a platform something is built for, that
//! the formula names the archives the build uploads, and that the released binary
//! carries the MCP server.
//!
//! A mismatch between those is invisible until `brew install` fails on someone
//! else's machine.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TAG: &str = "v2026.8.99";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repo_file(relative: &str) -> String {
    let path = repo_root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

/// Runs `scripts/release.py` from `dir`, which must hold a `.release.env`.
fn release_py(dir: &Path, args: &[&str]) -> Output {
    Command::new("python3")
        .arg(repo_root().join("scripts/release.py"))
        .args(args)
        .current_dir(dir)
        .env_remove("GITHUB_OUTPUT")
        .env("GITHUB_REPOSITORY", "grahambrooks/brake")
        .output()
        .expect("python3 should run scripts/release.py")
}

/// The targets the formula serves: the values of `FORMULA_TARGETS` in release.py.
fn formula_targets() -> BTreeSet<String> {
    repo_file("scripts/release.py")
        .lines()
        .skip_while(|line| !line.starts_with("FORMULA_TARGETS = {"))
        .skip(1)
        .take_while(|line| !line.trim_start().starts_with('}'))
        .filter_map(|line| {
            let value = line.split_once(':')?.1.trim().trim_end_matches(',');
            Some(value.trim_matches('"').to_owned())
        })
        .collect()
}

/// The targets `release.py plan` puts in the build matrix.
fn built_targets() -> BTreeSet<String> {
    let output = release_py(&repo_root(), &["plan", TAG]);
    assert!(
        output.status.success(),
        "release.py plan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let matrix = stdout
        .lines()
        .find_map(|line| line.strip_prefix("matrix="))
        .expect("plan should print a matrix");
    matrix
        .split("\"target\":\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .map(str::to_owned)
        .collect()
}

/// A scratch directory holding this repository's `.release.env`, so the
/// formula is rendered there rather than over the committed one.
fn scratch_with_config() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::copy(
        repo_root().join(".release.env"),
        dir.path().join(".release.env"),
    )
    .expect("copy .release.env");
    dir
}

#[test]
fn every_platform_the_formula_serves_is_actually_built() {
    let built = built_targets();
    let served = formula_targets();

    assert!(!built.is_empty(), "no targets found in the build matrix");
    assert_eq!(
        served.len(),
        4,
        "expected four formula platforms: {served:?}"
    );

    for target in &served {
        assert!(
            built.contains(target),
            "the formula serves `{target}`, which the build matrix does not produce — \
             `brew install` would 404 on that platform"
        );
    }
    assert!(
        built.contains("x86_64-pc-windows-msvc"),
        "Windows binaries are advertised in the README but not built"
    );
}

#[test]
fn the_formula_renders_and_names_the_archives_the_matrix_builds() {
    let served = formula_targets();
    let dir = scratch_with_config();
    let sums: String = served
        .iter()
        .map(|target| format!("{}  brake-{TAG}-{target}.tar.gz\n", "0".repeat(64)))
        .collect();
    fs::write(dir.path().join("SHA256SUMS"), sums).expect("write SHA256SUMS");

    let output = release_py(dir.path(), &["formula", TAG, "SHA256SUMS"]);
    assert!(
        output.status.success(),
        "the formula did not render: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let formula = fs::read_to_string(dir.path().join("Formula/brake.rb")).expect("formula");

    for target in &served {
        // upload-rust-binary-action names archives `<bin>-<tag>-<target>`.
        let expected = format!("brake-{TAG}-{target}.tar.gz");
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
    let dir = scratch_with_config();
    fs::write(
        dir.path().join("SHA256SUMS"),
        format!(
            "{}  brake-{TAG}-aarch64-apple-darwin.tar.gz\n",
            "0".repeat(64)
        ),
    )
    .expect("write SHA256SUMS");

    let output = release_py(dir.path(), &["formula", TAG, "SHA256SUMS"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("is missing from SHA256SUMS"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!dir.path().join("Formula/brake.rb").exists());
}

#[test]
fn the_released_binaries_include_the_mcp_server() {
    // The feature is off by default so a library consumer does not pay for an
    // async runtime. Someone downloading a binary has already accepted its
    // size, and `brake mcp` failing on a released build would be a poor joke.
    let features: BTreeSet<String> = repo_file(".release.env")
        .lines()
        .filter_map(|line| line.trim().strip_prefix("FEATURES="))
        .flat_map(|value| {
            value
                .split([',', ' '])
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(
        features.contains("mcp"),
        "the release build does not enable the mcp feature: {features:?}"
    );
}
