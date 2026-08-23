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

/// A shell command that creates `path` as a marker, in whichever shell
/// `--drift` uses.
///
/// `mkdir` rather than `touch`: it is a builtin in both `sh` and `cmd`, needs
/// no redirection, and depends on nothing being on PATH. `touch` is not a cmd
/// builtin, which made this guard half-vacuous on Windows — the "did not run"
/// assertion passed because the command failed rather than because brake
/// refused it.
///
/// Embedded in a TOML *literal* string (single quotes) by the caller, because
/// a Windows path is full of backslashes and `\U` in `C:\Users` is not a valid
/// escape in a TOML basic string. That parse failure is what made this test
/// fail on Windows — and made its first assertion pass for the wrong reason,
/// since a config that does not load runs no generator either.
fn create_file_command(path: &Path) -> String {
    format!("mkdir \"{}\"", path.display())
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
                 [contract.generated]\ncommand = '{}'\n",
                create_file_command(&marker)
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

/// A repository with two releases and a break after the newer one.
fn released_repository() -> TempDir {
    let repo = repo(&[
        (
            "brake.toml",
            "[[contract]]\nname=\"c\"\nformat=\"openapi\"\nsource=\"api/c.yaml\"\n\
             baseline={latest-tag=\"v*\"}\n",
        ),
        ("api/c.yaml", SPEC),
    ]);
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo.path())
            .output()
            .expect("git should launch");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-b", "main"]);
    git(&["config", "user.name", "Brake Test"]);
    git(&["config", "user.email", "brake@example.com"]);
    git(&["add", "."]);
    git(&["commit", "-m", "v9"]);
    git(&["tag", "-a", "v9.0.0", "-m", "release nine"]);
    git(&["commit", "--allow-empty", "-m", "v10"]);
    git(&["tag", "v10.0.0"]);

    fs::write(repo.path().join("api/c.yaml"), BROKEN).expect("write");
    git(&["add", "."]);
    git(&["commit", "-m", "break the api"]);
    repo
}

#[test]
fn a_release_baseline_gates_against_the_last_tag() {
    let repo = released_repository();
    let output = brake(repo.path(), &["check", "--format", "json"]);
    assert_eq!(code(&output), 1);

    let parsed: serde_json::Value = serde_json::from_str(&stdout(&output)).expect("json");
    let file = parsed["findings"][0]["file"].as_str().expect("file");
    assert_eq!(
        file, "tag:v10.0.0",
        "latest-tag must pick v10.0.0, not v9.0.0 — byte order gets this backwards"
    );
}

#[test]
fn the_baseline_flag_accepts_a_tag_a_glob_and_a_revision() {
    let repo = released_repository();
    for (flag, expected_label) in [
        ("v9.0.0", "tag:v9.0.0"),
        ("v10.0.0", "tag:v10.0.0"),
        ("latest-tag:v*", "tag:v10.0.0"),
    ] {
        let output = brake(
            repo.path(),
            &["check", "--baseline", flag, "--format", "json"],
        );
        assert_eq!(code(&output), 1, "--baseline {flag}");
        let parsed: serde_json::Value = serde_json::from_str(&stdout(&output)).expect("json");
        // A `rev` override and a `tag` entry label differently; both are fine
        // so long as the flag resolved to the right commit.
        let file = parsed["findings"][0]["file"].as_str().expect("file");
        assert!(
            file.ends_with(expected_label.trim_start_matches("tag:")),
            "--baseline {flag} resolved to {file}, wanted {expected_label}"
        );
    }
}

#[test]
fn a_release_baseline_with_no_matching_tag_is_a_tool_failure() {
    let repo = released_repository();
    fs::write(
        repo.path().join("brake.toml"),
        "[[contract]]\nname=\"c\"\nformat=\"openapi\"\nsource=\"api/c.yaml\"\n\
         baseline={latest-tag=\"release-*\"}\n",
    )
    .expect("write");

    let output = brake(repo.path(), &["check", "--format", "text"]);
    assert_eq!(
        code(&output),
        2,
        "an unresolvable release baseline must not report clean"
    );
    assert!(
        stdout(&output).contains("fetch-depth"),
        "the message should name the usual cause:\n{}",
        stdout(&output)
    );
}

#[test]
fn a_baseline_setting_two_shapes_at_once_is_rejected() {
    let repo = repo(&[
        (
            "brake.toml",
            "[[contract]]\nname=\"c\"\nformat=\"openapi\"\nsource=\"api/c.yaml\"\n\
             baseline={tag=\"v1.0.0\", rev=\"abc123\"}\n",
        ),
        ("api/c.yaml", SPEC),
    ]);
    let output = brake(repo.path(), &["check"]);
    assert_eq!(code(&output), 2);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("several"),
        "an ambiguous baseline must say so rather than picking one"
    );
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

#[test]
fn every_breaking_finding_carries_a_way_out() {
    // A finding that blocks a commit and suggests nothing is a finding people
    // argue with rather than act on. Walk the catalogue and assert that every
    // rule which reports a break names at least one strategy, and that no
    // strategy reaches a user with an unbound placeholder in it.
    let here = tempdir().expect("tempdir");
    let listing = stdout(&brake(here.path(), &["explain"]));

    for line in listing.lines() {
        let rule_id = line.split_whitespace().next().expect("a rule id");
        let text = stdout(&brake(here.path(), &["explain", rule_id]));

        assert!(
            !text.contains("{subject}") && !text.contains("{endpoint}"),
            "`{rule_id}` renders an unbound placeholder"
        );

        if text.contains("ways to make the change safely:") {
            assert!(
                text.contains("costs:"),
                "`{rule_id}` lists strategies with no costs, which reads as though \
                 they are all free"
            );
        }
    }
}

#[test]
fn a_finding_names_the_field_its_advice_is_about() {
    let repo = repo(&[
        (
            "brake.toml",
            "[[contract]]\nname=\"c\"\nformat=\"openapi\"\nsource=\"api/c.yaml\"\n\
             baseline={file=\"api/c.baseline.yaml\"}\n",
        ),
        (
            "api/c.baseline.yaml",
            "openapi: 3.1.0\npaths:\n  /p:\n    post:\n      operationId: createPayment\n\
             \x20     responses:\n        \"201\": {description: ok}\n",
        ),
        (
            "api/c.yaml",
            "openapi: 3.1.0\npaths:\n  /p:\n    post:\n      operationId: createPayment\n\
             \x20     parameters:\n        - {name: tenant, in: query, required: true, \
             schema: {type: string}}\n      responses:\n        \"201\": {description: ok}\n",
        ),
    ]);

    let output = brake(repo.path(), &["check", "--format", "json"]);
    assert_eq!(code(&output), 1);
    let parsed: serde_json::Value = serde_json::from_str(&stdout(&output)).expect("json");
    let finding = &parsed["findings"][0];

    assert_eq!(finding["subject"], "tenant");
    let summary = finding["remediation"][0]["summary"]
        .as_str()
        .expect("a bound summary");
    assert!(
        summary.contains("`tenant`"),
        // A parameter's JSON pointer ends in its index, so deriving the
        // subject from it once produced "keep `0` optional".
        "the advice must name the parameter, not its position: {summary}"
    );

    // The text rendering agrees with the structured one.
    let text = stdout(&brake(repo.path(), &["check", "--format", "text"]));
    assert!(text.contains("optional-with-default"), "{text}");
    assert!(text.contains("`tenant`"), "{text}");
}

#[test]
fn the_mcp_subcommand_is_listed_whether_or_not_it_is_built() {
    // A capability that silently does not exist is one nobody can discover,
    // which is the CLI equivalent of a clean result brake cannot justify. The
    // subcommand is always registered; only its implementation is gated.
    let here = tempdir().expect("tempdir");
    let help = stdout(&brake(here.path(), &["--help"]));
    assert!(
        help.contains("mcp"),
        "`brake --help` does not mention the MCP server:\n{help}"
    );
}

#[test]
fn brake_mcp_either_serves_or_says_how_to_get_it() {
    let here = tempdir().expect("tempdir");

    if cfg!(feature = "mcp") {
        // Built in: it would block on stdio, so assert on the help text
        // rather than starting it. tests/mcp.rs drives the real thing.
        let help = stdout(&brake(here.path(), &["mcp", "--help"]));
        assert!(help.contains("--as-of"), "{help}");
        return;
    }

    let output = brake(here.path(), &["mcp", "."]);
    assert_eq!(
        code(&output),
        2,
        "a subcommand that cannot run must exit 2, not pretend to succeed"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The message has to be actionable: naming the feature is not enough if
    // the reader still has to work out the command.
    assert!(stderr.contains("--features mcp"), "{stderr}");
    assert!(stderr.contains("cargo install brake"), "{stderr}");
}

/// A repository the way someone actually meets brake: real specs, ordinary
/// CI config, no brake.toml yet.
fn unconfigured_repo() -> TempDir {
    let repo = repo(&[
        ("api/payments-openapi.yaml", SPEC),
        ("api/payments-openapi.baseline.yaml", SPEC),
        (
            ".github/workflows/api-tests.yaml",
            "name: api-tests\non: push\n",
        ),
        ("package.json", r#"{"name":"app","version":"1.0.0"}"#),
        (
            "docker-compose.yml",
            "services:\n  api:\n    image: nginx\n",
        ),
    ]);
    let git = |args: &[&str]| {
        let ok = Command::new("git")
            .args(args)
            .current_dir(repo.path())
            .output()
            .expect("git should launch")
            .status
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-b", "main"]);
    git(&["config", "user.name", "Brake Test"]);
    git(&["config", "user.email", "brake@example.com"]);
    git(&["add", "."]);
    git(&["commit", "-m", "init"]);
    repo
}

#[test]
fn the_first_command_a_new_user_runs_names_the_one_that_fixes_it() {
    let repo = unconfigured_repo();
    let output = brake(repo.path(), &["check"]);

    assert_eq!(code(&output), 2);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("brake init"),
        "\"create one\" is not actionable without saying how:\n{stderr}"
    );
}

#[test]
fn init_declares_the_real_contracts_and_ignores_everything_else() {
    let repo = unconfigured_repo();
    let output = brake(repo.path(), &["init"]);
    assert_eq!(
        code(&output),
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let written = fs::read_to_string(repo.path().join("brake.toml")).expect("brake.toml");
    assert!(written.contains("api/payments-openapi.yaml"), "{written}");

    // The files a filename heuristic would have claimed.
    assert!(!written.contains("api-tests.yaml"), "{written}");
    assert!(!written.contains("package.json"), "{written}");
    assert!(!written.contains("docker-compose"), "{written}");
    // A baseline is a previous version, not a second contract.
    assert!(!written.contains("baseline.yaml\""), "{written}");
}

#[test]
fn what_init_writes_works_on_the_very_next_command() {
    // The loop that matters: if the generated config does not survive contact
    // with `check`, init has moved the wall rather than removed it.
    let repo = unconfigured_repo();
    assert_eq!(code(&brake(repo.path(), &["init"])), 0);

    let output = brake(
        repo.path(),
        &["check", "api/payments-openapi.yaml", "--format", "text"],
    );
    assert_eq!(
        code(&output),
        0,
        "the generated config failed immediately:\nstdout: {}\nstderr: {}",
        stdout(&output),
        String::from_utf8_lossy(&output.stderr)
    );

    // …and it is a real gate, not one that passes because it checked nothing.
    fs::write(repo.path().join("api/payments-openapi.yaml"), BROKEN).expect("write");
    let output = brake(
        repo.path(),
        &["check", "api/payments-openapi.yaml", "--format", "text"],
    );
    assert_eq!(code(&output), 1, "{}", stdout(&output));
    assert!(stdout(&output).contains("endpoint-removed"));
}

#[test]
fn init_refuses_to_overwrite_without_being_told_to() {
    let repo = unconfigured_repo();
    assert_eq!(code(&brake(repo.path(), &["init"])), 0);

    fs::write(
        repo.path().join("brake.toml"),
        "# hand-written, do not lose\n",
    )
    .expect("write");
    let output = brake(repo.path(), &["init"]);
    assert_eq!(code(&output), 2);
    assert!(String::from_utf8_lossy(&output.stderr).contains("--force"));
    assert_eq!(
        fs::read_to_string(repo.path().join("brake.toml")).expect("read"),
        "# hand-written, do not lose\n",
        "suppressions and their reasons are the most expensive thing in that file"
    );

    assert_eq!(code(&brake(repo.path(), &["init", "--force"])), 0);
}

#[test]
fn init_dry_run_writes_nothing() {
    let repo = unconfigured_repo();
    let output = brake(repo.path(), &["init", "--dry-run"]);

    assert_eq!(code(&output), 0);
    assert!(
        stdout(&output).contains("[[contract]]"),
        "{}",
        stdout(&output)
    );
    assert!(!repo.path().join("brake.toml").exists());
}

#[test]
fn an_ordinary_ci_file_does_not_trip_the_contract_notice() {
    // `01-thesis.md`: false positives are how a hook gets uninstalled. This
    // one was loud, and fired on any commit touching CI in a repo with `api`
    // in a path.
    let repo = unconfigured_repo();
    assert_eq!(code(&brake(repo.path(), &["init"])), 0);

    let output = brake(
        repo.path(),
        &[
            "check",
            ".github/workflows/api-tests.yaml",
            "package.json",
            "docker-compose.yml",
            "--format",
            "text",
        ],
    );
    assert_eq!(code(&output), 0);
    assert!(
        !stdout(&output).contains("contract-unconfigured"),
        "brake called ordinary files APIs:\n{}",
        stdout(&output)
    );
}

#[test]
fn a_genuinely_undeclared_contract_is_still_pointed_out() {
    let repo = unconfigured_repo();
    assert_eq!(code(&brake(repo.path(), &["init"])), 0);
    fs::write(repo.path().join("api/ledger-openapi.yaml"), SPEC).expect("write");

    let output = brake(
        repo.path(),
        &["check", "api/ledger-openapi.yaml", "--format", "text"],
    );
    assert_eq!(code(&output), 0, "a note must not block a commit");
    let text = stdout(&output);
    assert!(text.contains("contract-unconfigured"), "{text}");
    // The note is about a file; naming it `contract:` claimed it was one.
    assert!(text.contains("file: `api/ledger-openapi.yaml`"), "{text}");
}

#[test]
fn a_field_level_finding_points_at_the_field_not_the_response() {
    // The JSON pointer was always exact; the underlined line was the enclosing
    // response, which is the right file and the wrong line — and the line is
    // what a reader checks first.
    let spec = |fields: &str| {
        format!(
            "openapi: 3.1.0\npaths:\n  /payments/{{id}}:\n    get:\n\
             \x20     operationId: getPayment\n      responses:\n        \"200\":\n\
             \x20         description: ok\n          content:\n\
             \x20           application/json:\n              schema:\n\
             \x20               type: object\n                required: [id]\n\
             \x20               properties:\n{fields}"
        )
    };
    let repo = repo(&[
        (
            "brake.toml",
            "[[contract]]\nname=\"c\"\nformat=\"openapi\"\nsource=\"api/c.yaml\"\n\
             baseline={file=\"api/c.baseline.yaml\"}\n",
        ),
        (
            "api/c.baseline.yaml",
            &spec(
                "                  id: { type: string }\n                  customer_id: { type: string }\n",
            ),
        ),
        (
            "api/c.yaml",
            &spec("                  id: { type: string }\n"),
        ),
    ]);

    let output = brake(repo.path(), &["check", "--format", "json"]);
    assert_eq!(code(&output), 1);
    let parsed: serde_json::Value = serde_json::from_str(&stdout(&output)).expect("json");
    let finding = &parsed["findings"][0];
    assert_eq!(finding["rule"], "response-field-removed");

    // `customer_id` is declared on line 16 of the baseline; the response
    // block it lives in starts at line 7, which is what used to be reported.
    let line = finding["line"].as_u64().expect("line");
    assert_eq!(
        line,
        16,
        "expected the line declaring `customer_id`, got {line}:\n{}",
        stdout(&output)
    );

    let text = stdout(&brake(repo.path(), &["check", "--format", "text"]));
    assert!(
        text.contains("customer_id: { type: string }"),
        "the diagnostic should quote the field's own line:\n{text}"
    );
}

#[test]
fn a_finding_with_no_field_span_still_locates_the_payload() {
    // protobuf supplies no field-level spans, so those findings fall back to
    // the method's span. That is the previous behaviour and still the right
    // file — the fallback must not produce a missing or zero location.
    let proto = |payment: &str| {
        format!(
            "syntax = \"proto3\";\npackage pay;\nmessage Req {{ string id = 1; }}\n\
             message Payment {{ {payment} }}\n\
             service S {{ rpc Get(Req) returns (Payment); }}\n"
        )
    };
    let repo = repo(&[
        (
            "brake.toml",
            "[[contract]]\nname=\"p\"\nformat=\"proto\"\nsource=\"api/p.proto\"\n\
             baseline={file=\"api/p.baseline.proto\"}\n",
        ),
        (
            "api/p.baseline.proto",
            &proto("string id = 1; string note = 2;"),
        ),
        ("api/p.proto", &proto("string id = 1;")),
    ]);

    let output = brake(repo.path(), &["check", "--format", "json"]);
    assert_eq!(code(&output), 1);
    let parsed: serde_json::Value = serde_json::from_str(&stdout(&output)).expect("json");
    let finding = &parsed["findings"][0];

    assert!(
        finding["file"]
            .as_str()
            .is_some_and(|f| f.ends_with(".proto"))
    );
    assert!(
        finding["line"].as_u64().is_some_and(|line| line > 0),
        "a fallback span must still be a real location: {finding}"
    );
}
