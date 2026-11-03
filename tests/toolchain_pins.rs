//! The Rust version is declared in two places, and they must agree exactly.
//!
//! `CLAUDE.md` states the convention — "MSRV tracks latest stable and is
//! pinned in `rust-toolchain.toml`. Keep `channel` identical to `rust-version`
//! in `Cargo.toml`" — and a convention nothing checks is one that drifts. It
//! already did: a dependency-bot PR moved `Cargo.toml` and
//! `rust-toolchain.toml` to 1.98 and left the CI job verifying 1.97, so the
//! job was testing a floor the project no longer declared.
//!
//! The `msrv` CI job no longer carries a third copy of the version. It reads
//! the floor from these two files (failing if they differ) and builds with
//! `cargo +<floor>`. A bare `cargo` there would be overridden by
//! `rust-toolchain.toml`, so the job would pass without testing anything —
//! which is what the earlier `dtolnay/rust-toolchain@1.9x` + `cargo check`
//! job did.

use std::fs;
use std::path::PathBuf;

fn read(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

/// The value of a `key = "value"` line, ignoring comments that mention the key.
fn toml_string(source: &str, key: &str) -> String {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .find_map(|line| {
            let (name, value) = line.split_once('=')?;
            (name.trim() == key).then(|| value.trim().trim_matches('"').to_owned())
        })
        .unwrap_or_else(|| panic!("no `{key}` found"))
}

#[test]
fn the_declared_rust_version_agrees_everywhere_it_is_written() {
    let manifest = toml_string(&read("Cargo.toml"), "rust-version");
    let toolchain = toml_string(&read("rust-toolchain.toml"), "channel");

    // Exact, not `major.minor`: the CI job compares the strings verbatim, and
    // `1.99` in a toolchain file means "latest 1.99.x", not the declared floor.
    assert_eq!(
        manifest, toolchain,
        "Cargo.toml `rust-version` and rust-toolchain.toml `channel` disagree"
    );
}

/// The `msrv` job's block of the build workflow.
fn msrv_job() -> String {
    let workflow = read(".github/workflows/build.yml");
    let block = workflow
        .split("\n  msrv:")
        .nth(1)
        .expect("the workflow has an `msrv` job");
    // Up to the next job: a line indented by exactly two spaces.
    block
        .lines()
        .skip(1)
        .take_while(|line| !(line.starts_with("  ") && !line.starts_with("   ")))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_msrv_job_builds_with_the_declared_floor() {
    let job = msrv_job();

    // The floor comes from the files above, so there is no third copy to drift.
    assert!(
        job.contains("rust-toolchain.toml") && job.contains("rust-version"),
        "the msrv job must read the floor from Cargo.toml and rust-toolchain.toml"
    );
    assert!(
        !job.contains("dtolnay/rust-toolchain@"),
        "the msrv job must not pin its own toolchain copy"
    );

    // Every cargo invocation must name the floor explicitly; a bare `cargo`
    // is redirected by rust-toolchain.toml and proves nothing.
    let cargo_runs: Vec<&str> = job
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix("- run: "))
        .filter(|run| run.starts_with("cargo"))
        .collect();
    assert!(!cargo_runs.is_empty(), "the msrv job runs cargo");
    for run in cargo_runs {
        assert!(
            run.starts_with("cargo +${{ steps.floor.outputs.version }} "),
            "msrv job runs `{run}` without `cargo +<floor>`"
        );
    }
}

/// The next release version must never be one already released.
///
/// `MICRO` used to be the *count* of this month's tags, which assumes none is
/// ever deleted or skipped. Deleting `v2026.8.2` — a tag whose release failed
/// and published nothing — made `make version` report `2026.8.3`, a version
/// already on crates.io. The only thing between that and a bad publish was
/// `release-guard` refusing with a message about a tag existing for reasons
/// nobody would remember.
#[test]
fn the_next_version_is_not_one_that_is_already_tagged() {
    let next = std::process::Command::new("make")
        .arg("version")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output();

    let Ok(output) = next else {
        return; // `make` unavailable; nothing to assert.
    };
    if !output.status.success() {
        return;
    }
    let next = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    assert!(!next.is_empty(), "`make version` printed nothing");

    let tags = std::process::Command::new("git")
        .args(["tag", "-l"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("git tag -l");
    let tags: Vec<String> = String::from_utf8_lossy(&tags.stdout)
        .lines()
        .map(str::to_owned)
        .collect();

    assert!(
        !tags.contains(&format!("v{next}")),
        "`make version` says {next}, which is already tagged — a release would \
         either be refused or, worse, reuse a published version"
    );
}
