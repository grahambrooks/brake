//! The Rust version is declared in three places, and they must agree.
//!
//! `CLAUDE.md` states the convention — "MSRV tracks latest stable and is
//! pinned in `rust-toolchain.toml`. Keep it in step with `rust-version` in
//! `Cargo.toml`" — and a convention nothing checks is one that drifts. It
//! already did: a dependency-bot PR moved `Cargo.toml` and
//! `rust-toolchain.toml` to 1.98 and left the CI job verifying 1.97, so the
//! job was testing a floor the project no longer declared.
//!
//! Compared on `major.minor`, because the three spell the same version
//! differently by convention: `1.98.0` in a manifest, `1.98` in a toolchain
//! file and an action pin.

use std::fs;
use std::path::PathBuf;

fn read(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

/// `1.98.0` and `1.98` both reduce to `1.98`.
fn major_minor(version: &str) -> String {
    version
        .trim()
        .split('.')
        .take(2)
        .collect::<Vec<_>>()
        .join(".")
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
    let manifest = major_minor(&toml_string(&read("Cargo.toml"), "rust-version"));
    let toolchain = major_minor(&toml_string(&read("rust-toolchain.toml"), "channel"));

    // The CI job that actually proves the floor. A pin below the declared
    // version tests something the project does not promise; a pin above it
    // means the promise is untested.
    //
    // Read from the `msrv` job specifically. Scanning the whole file finds
    // the first `rust-toolchain@stable` of another job instead, which is how
    // the first version of this test passed against reintroduced drift.
    let ci = major_minor(&msrv_pin());

    assert_eq!(
        manifest, toolchain,
        "Cargo.toml `rust-version` and rust-toolchain.toml `channel` disagree"
    );
    assert_eq!(
        manifest, ci,
        "the MSRV CI job pins a different version from Cargo.toml `rust-version`"
    );
}

/// The toolchain the `msrv` job installs.
fn msrv_pin() -> String {
    let workflow = read(".github/workflows/build.yml");
    let block = workflow
        .split("\n  msrv:")
        .nth(1)
        .expect("the workflow has an `msrv` job");

    block
        .lines()
        .find_map(|line| line.trim().strip_prefix("- uses: dtolnay/rust-toolchain@"))
        .map(|version| version.trim().to_owned())
        .expect("the msrv job pins a toolchain")
}

#[test]
fn the_msrv_job_pins_a_version_rather_than_stable() {
    // Pinned to `stable`, the job proves nothing: it would pass on whatever
    // the runner happened to install, which is what every other job already
    // uses.
    assert_ne!(
        msrv_pin(),
        "stable",
        "the minimum-supported-version job must pin a version"
    );
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
