//! Tests for the binary's own surface: argument wiring, exit codes, and the
//! contract between the two.
//!
//! `main.rs` may not decide whether something is a breaking change, but it does
//! decide what gets checked and what the process exits with — and nothing else
//! exercises that. The dropped `paths` argument that made `brake check <file>`
//! check everything was invisible to library tests.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::{TempDir, tempdir};

const BRAKE: &str = env!("CARGO_BIN_EXE_brake");

const SPEC: &str = r#"
openapi: 3.1.0
paths:
  /payments/{id}:
    get:
      operationId: getPayment
      responses:
        "200":
          description: ok
"#;

const BROKEN: &str = r#"
openapi: 3.1.0
paths: {}
"#;

fn repo(files: &[(&str, &str)]) -> TempDir {
    let repo = tempdir().expect("tempdir");
    for (path, body) in files {
        let full = repo.path().join(path);
        fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
        fs::write(full, body).expect("write");
    }
    repo
}

fn brake(cwd: &Path, args: &[&str]) -> Output {
    Command::new(BRAKE)
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("brake should launch")
}

fn code(output: &Output) -> i32 {
    output.status.code().expect("brake should exit normally")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

const TWO_CONTRACTS: &str = r#"
[[contract]]
name = "a"
format = "openapi"
source = "api/a.yaml"
baseline = { file = "api/a.baseline.yaml" }

[[contract]]
name = "b"
format = "openapi"
source = "api/b.yaml"
baseline = { file = "api/b.baseline.yaml" }
"#;

/// `a` is broken against its baseline, `b` is not.
fn two_contracts() -> TempDir {
    repo(&[
        ("brake.toml", TWO_CONTRACTS),
        ("api/a.baseline.yaml", SPEC),
        ("api/a.yaml", BROKEN),
        ("api/b.baseline.yaml", SPEC),
        ("api/b.yaml", SPEC),
    ])
}

#[test]
fn check_with_a_path_checks_only_that_contract() {
    let repo = two_contracts();

    let scoped = brake(repo.path(), &["check", "api/b.yaml", "--format", "text"]);
    assert_eq!(
        code(&scoped),
        0,
        "a run scoped to an untouched contract leaked another's finding:\n{}",
        stdout(&scoped)
    );

    let also_scoped = brake(repo.path(), &["check", "api/a.yaml", "--format", "text"]);
    assert_eq!(code(&also_scoped), 1);
    assert!(stdout(&also_scoped).contains("endpoint-removed"));
}

#[test]
fn check_with_no_paths_checks_everything() {
    let repo = two_contracts();
    let output = brake(repo.path(), &["check", "--format", "text"]);
    assert_eq!(code(&output), 1);
}

#[test]
fn check_accepts_the_many_paths_a_hook_passes_at_once() {
    let repo = two_contracts();
    // pre-commit passes every changed file it watches, contracts or not.
    let output = brake(
        repo.path(),
        &[
            "check",
            "api/b.yaml",
            "README.md",
            "src/main.rs",
            "--format",
            "text",
        ],
    );
    assert_eq!(code(&output), 0, "{}", stdout(&output));
}

#[test]
fn exit_codes_separate_a_broken_api_from_a_broken_gate() {
    // 1 — the API broke.
    let broken_api = two_contracts();
    assert_eq!(code(&brake(broken_api.path(), &["check"])), 1);

    // 2 — the gate broke: a configured baseline that is not there.
    let broken_gate = repo(&[
        ("brake.toml", TWO_CONTRACTS),
        ("api/a.yaml", SPEC),
        ("api/b.baseline.yaml", SPEC),
        ("api/b.yaml", SPEC),
    ]);
    assert_eq!(
        code(&brake(broken_gate.path(), &["check"])),
        2,
        "an unresolvable baseline must not be reported as an API break"
    );

    // 0 — nothing to say.
    let clean = repo(&[
        (
            "brake.toml",
            "[[contract]]\nname=\"b\"\nformat=\"openapi\"\nsource=\"api/b.yaml\"\nbaseline={file=\"api/b.baseline.yaml\"}\n",
        ),
        ("api/b.baseline.yaml", SPEC),
        ("api/b.yaml", SPEC),
    ]);
    assert_eq!(code(&brake(clean.path(), &["check"])), 0);
}

#[test]
fn brake_toml_is_found_from_a_subdirectory() {
    let repo = two_contracts();
    let nested = repo.path().join("api/nested/deeper");
    fs::create_dir_all(&nested).expect("mkdir");

    let output = brake(&nested, &["check", "--format", "text"]);
    assert_eq!(
        code(&output),
        1,
        "brake.toml was not discovered from a subdirectory:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_missing_config_is_a_tool_failure_with_an_actionable_message() {
    let empty = tempdir().expect("tempdir");
    let output = brake(empty.path(), &["check"]);
    assert_eq!(code(&output), 2);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("brake.toml"), "{stderr}");
}

#[test]
fn contract_flag_restricts_the_run() {
    let repo = two_contracts();
    let output = brake(
        repo.path(),
        &["check", "--contract", "b", "--format", "text"],
    );
    assert_eq!(code(&output), 0, "{}", stdout(&output));
}

#[test]
fn compatibility_flag_changes_the_verdict() {
    let repo = repo(&[
        (
            "brake.toml",
            "[[contract]]\nname=\"c\"\nformat=\"openapi\"\nsource=\"api/c.yaml\"\nbaseline={file=\"api/c.baseline.yaml\"}\n",
        ),
        ("api/c.baseline.yaml", SPEC),
        ("api/c.yaml", &SPEC.replace("getPayment", "fetchPayment")),
    ]);

    assert_eq!(code(&brake(repo.path(), &["check"])), 0);
    assert_eq!(
        code(&brake(
            repo.path(),
            &["check", "--compatibility", "surface"]
        )),
        1,
        "an operationId rename is a surface break"
    );
    assert_eq!(
        code(&brake(
            repo.path(),
            &["check", "--compatibility", "nonsense"]
        )),
        2,
        "an unknown level is a tool failure, not a silent default"
    );
}

#[test]
fn format_auto_is_json_when_piped() {
    let repo = two_contracts();
    let output = brake(repo.path(), &["check"]);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("piped output should be JSON");
    assert!(parsed["findings"].is_array());
}

#[test]
fn every_format_renders_and_agrees_on_the_verdict() {
    let repo = two_contracts();
    for format in ["text", "json", "sarif"] {
        let output = brake(repo.path(), &["check", "--format", format]);
        assert_eq!(code(&output), 1, "format {format} disagreed on the verdict");
        assert!(
            !stdout(&output).is_empty(),
            "format {format} rendered nothing"
        );
    }
    let bad = brake(repo.path(), &["check", "--format", "yaml"]);
    assert_eq!(code(&bad), 2);
}

#[test]
fn sarif_output_parses_and_carries_fingerprints() {
    let repo = two_contracts();
    let output = brake(repo.path(), &["check", "--format", "sarif"]);
    let sarif: serde_json::Value =
        serde_json::from_str(&stdout(&output)).expect("valid SARIF JSON");

    assert_eq!(sarif["version"], "2.1.0");
    let results = sarif["runs"][0]["results"].as_array().expect("results");
    assert!(!results.is_empty());
    for result in results {
        assert!(result["partialFingerprints"]["brakeFindingV1"].is_string());
    }
}

#[test]
fn diff_reports_changes_and_never_gates() {
    let repo = two_contracts();
    let output = brake(repo.path(), &["diff", "--format", "text"]);
    assert_eq!(code(&output), 0, "diff must never fail a build");
    assert!(
        stdout(&output).contains("endpoint-removed"),
        "diff should still describe the change:\n{}",
        stdout(&output)
    );
}

#[test]
fn analyze_covers_the_repository_and_honours_fail_on() {
    let repo = two_contracts();
    assert_eq!(code(&brake(repo.path(), &["analyze", "."])), 1);
    assert_eq!(
        code(&brake(repo.path(), &["analyze", ".", "--fail-on", "error"])),
        1
    );
}

#[test]
fn explain_covers_every_rule_with_no_placeholder_text() {
    let here = tempdir().expect("tempdir");

    let listing = brake(here.path(), &["explain"]);
    assert_eq!(code(&listing), 0);
    let listed = stdout(&listing);

    for line in listed.lines() {
        let rule_id = line.split_whitespace().next().expect("a rule id");
        let output = brake(here.path(), &["explain", rule_id]);
        assert_eq!(code(&output), 0, "`brake explain {rule_id}` failed");

        let text = stdout(&output);
        assert!(text.contains(rule_id), "{rule_id}: id missing from output");
        assert!(
            !text.to_lowercase().contains("todo") && !text.to_lowercase().contains("tbd"),
            "{rule_id}: placeholder explanation"
        );
        assert!(
            text.len() > 200,
            "{rule_id}: explanation is too thin to be useful"
        );
    }

    assert!(
        listed.lines().count() > 25,
        "the catalogue should cover the specified ruleset, got {} rules",
        listed.lines().count()
    );
}

#[test]
fn explain_rejects_an_unknown_rule_rather_than_inventing_one() {
    let here = tempdir().expect("tempdir");
    let output = brake(here.path(), &["explain", "no-such-rule"]);
    assert_eq!(code(&output), 2);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no-such-rule"),
        "the error should name what was asked for"
    );
}

#[test]
fn every_rule_the_tool_can_emit_is_explainable() {
    // A finding whose rule `brake explain` does not know is a dead end for
    // whoever is blocked by it.
    let here = tempdir().expect("tempdir");
    let listing = stdout(&brake(here.path(), &["explain"]));
    let known: Vec<&str> = listing
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect();

    let repo = two_contracts();
    let output = brake(repo.path(), &["check", "--format", "json"]);
    let parsed: serde_json::Value = serde_json::from_str(&stdout(&output)).expect("json");
    for finding in parsed["findings"].as_array().expect("findings") {
        let rule = finding["rule"].as_str().expect("rule");
        assert!(known.contains(&rule), "`{rule}` is not in the catalogue");
    }
}

#[test]
fn drift_is_unreachable_without_the_flag() {
    let witness = tempdir().expect("tempdir");
    let marker = witness.path().join("generator-ran");
    let repo = repo(&[
        (
            "brake.toml",
            &format!(
                "[[contract]]\nname=\"c\"\nformat=\"openapi\"\nsource=\"api/c.yaml\"\n\
                 baseline={{file=\"api/c.baseline.yaml\"}}\n\
                 [contract.generated]\ncommand = \"touch {}\"\n",
                marker.display()
            ),
        ),
        ("api/c.baseline.yaml", SPEC),
        ("api/c.yaml", SPEC),
    ]);

    brake(repo.path(), &["check"]);
    assert!(
        !marker.exists(),
        "brake check ran a config-declared command without --drift"
    );

    brake(repo.path(), &["check", "--drift"]);
    assert!(marker.exists(), "--drift should run the declared generator");
}

#[test]
fn version_prints_the_calver() {
    let here = tempdir().expect("tempdir");
    let output = brake(here.path(), &["--version"]);
    assert_eq!(code(&output), 0);
    assert!(stdout(&output).contains("2026."), "{}", stdout(&output));
}

#[test]
fn as_of_defaults_to_today_so_expiry_actually_expires() {
    let repo = repo(&[
        (
            "brake.toml",
            "[[contract]]\nname=\"c\"\nformat=\"openapi\"\nsource=\"api/c.yaml\"\n\
             baseline={file=\"api/c.baseline.yaml\"}\n\
             [[contract.allow]]\nrule=\"endpoint-removed\"\nreason=\"migrated\"\n\
             expires=\"2020-01-01\"\n",
        ),
        ("api/c.baseline.yaml", SPEC),
        ("api/c.yaml", BROKEN),
    ]);

    let output = brake(repo.path(), &["check", "--format", "json"]);
    let parsed: serde_json::Value = serde_json::from_str(&stdout(&output)).expect("json");
    let rules: Vec<&str> = parsed["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .filter_map(|finding| finding["rule"].as_str())
        .collect();

    assert!(
        rules.contains(&"expired-allow"),
        "a suppression that expired in 2020 must not still be suppressing: {rules:?}"
    );

    // …and `--as-of` before the expiry still honours it. The removal also
    // trips `removed-without-deprecation`, which is a different rule and is
    // not covered by this suppression, so assert on the rule rather than the
    // exit code.
    let before = brake(
        repo.path(),
        &["check", "--as-of", "2019-06-01", "--format", "json"],
    );
    let parsed: serde_json::Value = serde_json::from_str(&stdout(&before)).expect("json");
    let rules: Vec<&str> = parsed["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .filter_map(|finding| finding["rule"].as_str())
        .collect();
    assert!(
        !rules.contains(&"expired-allow") && !rules.contains(&"endpoint-removed"),
        "before its expiry the suppression should still apply: {rules:?}"
    );
}

#[test]
fn a_suppression_for_an_unknown_rule_is_rejected_at_parse_time() {
    let repo = repo(&[
        (
            "brake.toml",
            "[[contract]]\nname=\"c\"\nformat=\"openapi\"\nsource=\"api/c.yaml\"\n\
             baseline={file=\"api/c.baseline.yaml\"}\n\
             [[contract.allow]]\nrule=\"endpoint-remvoed\"\nreason=\"typo\"\n",
        ),
        ("api/c.baseline.yaml", SPEC),
        ("api/c.yaml", SPEC),
    ]);

    let output = brake(repo.path(), &["check"]);
    assert_eq!(code(&output), 2);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("endpoint-remvoed"),
        "a misspelled rule must be named, not silently ignored"
    );
}

#[test]
fn a_suppression_without_a_reason_is_rejected() {
    let repo = repo(&[
        (
            "brake.toml",
            "[[contract]]\nname=\"c\"\nformat=\"openapi\"\nsource=\"api/c.yaml\"\n\
             baseline={file=\"api/c.baseline.yaml\"}\n\
             [[contract.allow]]\nrule=\"endpoint-removed\"\n",
        ),
        ("api/c.baseline.yaml", SPEC),
        ("api/c.yaml", SPEC),
    ]);
    assert_eq!(code(&brake(repo.path(), &["check"])), 2);
}

#[test]
fn output_is_byte_identical_across_repeated_invocations() {
    let repo = two_contracts();
    for format in ["text", "json", "sarif"] {
        let first = stdout(&brake(repo.path(), &["check", "--format", format]));
        let second = stdout(&brake(repo.path(), &["check", "--format", format]));
        assert_eq!(first, second, "format {format} is not byte-stable");
        assert!(
            !first.contains(&repo.path().to_string_lossy().to_string()),
            "format {format} embeds the absolute checkout path"
        );
    }
}
