//! `brake init` — discover the contracts in a repository and write a
//! `brake.toml` that declares them.
//!
//! The first command a new user runs used to be `brake check`, which failed
//! with "no brake.toml found. Create one" and no indication of what to put in
//! it. For a tool whose pitch is drop-in adoption, that was a wall at step one.
//!
//! **Detection is by parsing, not by filename.** A file counts as a contract
//! only if the ingester that would gate it can actually read it. Guessing from
//! the path is how `.github/workflows/api-tests.yaml` gets called an API — and
//! a tool that opens by misidentifying your files has spent its credibility
//! before it has found anything.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::check::display_path;
use crate::config::ContractFormat;

/// Directories never worth walking. Cheap to list, and the difference between
/// a command that answers instantly and one that reads `node_modules`.
const SKIP_DIRECTORIES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "target",
    "node_modules",
    "vendor",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".tox",
    ".idea",
    ".vscode",
    ".next",
    ".cache",
];

/// How deep to walk. Deep enough for any real layout, bounded so a symlinked
/// or pathological tree cannot make `init` appear to hang.
const MAX_DEPTH: usize = 8;

/// How many candidate files to actually parse.
///
/// Parsing is the accurate test and also the expensive one. On a monorepo the
/// cap is what keeps `init` instant; when it bites, [`Discovery::truncated`]
/// says so rather than quietly reporting a partial answer.
const MAX_CANDIDATES: usize = 2_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredContract {
    pub name: String,
    pub format: ContractFormat,
    /// Repository-relative, `/`-separated.
    pub source: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Discovery {
    pub contracts: Vec<DiscoveredContract>,
    /// Files that parsed but were skipped as checked-in baselines.
    pub baselines_skipped: Vec<String>,
    /// The candidate cap was reached, so this is a partial answer.
    pub truncated: bool,
}

/// Walk `root` and return every file an ingester can read.
#[must_use]
pub fn discover(root: &Path) -> Discovery {
    let mut candidates = walk(root, &has_candidate_extension);
    candidates.sort();

    let truncated = candidates.len() > MAX_CANDIDATES;
    candidates.truncate(MAX_CANDIDATES);

    let mut discovery = Discovery {
        truncated,
        ..Discovery::default()
    };
    let mut used_names = BTreeSet::new();

    for path in candidates {
        let relative = display_path(path.strip_prefix(root).unwrap_or(&path));
        let Some(format) = identify(&path) else {
            continue;
        };

        // A checked-in baseline is the *previous* version of a contract, not a
        // second contract to gate. Declaring it would compare it against
        // itself and report nothing, forever.
        if is_baseline(&relative) {
            discovery.baselines_skipped.push(relative);
            continue;
        }

        let mut name = contract_name(&relative);
        let mut suffix = 2;
        while !used_names.insert(name.clone()) {
            name = format!("{}-{suffix}", contract_name(&relative));
            suffix += 1;
        }

        discovery.contracts.push(DiscoveredContract {
            name,
            format,
            source: relative,
        });
    }

    discovery
}

/// Every file under `root` whose name `accept` allows.
///
/// Shared with `demand::load`, which looks for consumer declarations rather
/// than contracts: the skip list, the depth bound and the symlink rule are
/// properties of walking a repository, not of what is being looked for, and
/// two copies of them would drift.
#[must_use]
pub fn walk(root: &Path, accept: &dyn Fn(&str) -> bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect(root, 0, accept, &mut out);
    out
}

fn collect(directory: &Path, depth: usize, accept: &dyn Fn(&str) -> bool, out: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();

        if kind.is_dir() {
            // Not `is_symlink` on the path: a symlinked directory can point
            // back up the tree, and following it walks forever.
            if kind.is_symlink() || SKIP_DIRECTORIES.contains(&name.as_str()) {
                continue;
            }
            collect(&path, depth + 1, accept, out);
        } else if kind.is_file() && accept(&name) {
            out.push(path);
        }
    }
}

fn has_candidate_extension(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    [
        ".yaml",
        ".yml",
        ".json",
        ".proto",
        ".graphql",
        ".graphqls",
        ".gql",
    ]
    .iter()
    .any(|extension| lowered.ends_with(extension))
}

/// Which ingester, if any, can read this file.
///
/// The whole point of `init`: a `.yaml` is an OpenAPI contract when the
/// OpenAPI ingester parses it, and not because of where it lives or what it
/// is called.
#[must_use]
pub fn identify(path: &Path) -> Option<ContractFormat> {
    let Ok(bytes) = fs::read(path) else {
        return None;
    };
    // A contract large enough to be worth gating is nowhere near this; a file
    // that is, is not one.
    if bytes.len() > 8 * 1024 * 1024 {
        return None;
    }

    let name = path.to_string_lossy().to_ascii_lowercase();
    let source = display_path(path);

    let format = if name.ends_with(".proto") {
        ContractFormat::Proto
    } else if name.ends_with(".graphql") || name.ends_with(".graphqls") || name.ends_with(".gql") {
        ContractFormat::Graphql
    } else {
        ContractFormat::Openapi
    };

    crate::parse(format, &source, &bytes).ok().map(|_| format)
}

/// `api/payments.baseline.yaml`, `api/openapi-baseline.json`.
fn is_baseline(relative: &str) -> bool {
    let lowered = relative.to_ascii_lowercase();
    lowered.contains(".baseline.")
        || lowered.contains("-baseline.")
        || lowered.contains("_baseline.")
}

/// Words that name a *format* rather than an API.
///
/// A contract called `openapi` tells a reader nothing they cannot see from
/// the `format` line directly beneath it.
const UNINFORMATIVE: &[&str] = &[
    "openapi",
    "swagger",
    "schema",
    "spec",
    "api",
    "apis",
    "proto",
    "graphql",
    "contract",
    "contracts",
    "service",
    "index",
    "main",
];

/// A short, stable name from the path.
///
/// `api/payments-openapi.yaml` becomes `payments`, because the format is
/// already declared on the next line and repeating it in the name makes every
/// finding read `contract: payments-openapi`.
///
/// Where the filename says nothing — `payments/openapi.yaml` — the directory
/// does, and it is very often the better name anyway.
fn contract_name(relative: &str) -> String {
    let segments: Vec<&str> = relative
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let file_stem = segments
        .last()
        .and_then(|file| file.split('.').next())
        .unwrap_or("");

    let from_file = clean(file_stem);
    if !from_file.is_empty() && !UNINFORMATIVE.contains(&from_file.as_str()) {
        return from_file;
    }

    // Walk outward: the nearest enclosing directory that names something.
    for directory in segments.iter().rev().skip(1) {
        let candidate = clean(directory);
        if !candidate.is_empty() && !UNINFORMATIVE.contains(&candidate.as_str()) {
            return candidate;
        }
    }

    // Every segment is a format word, so none of them names this contract.
    // `api` is at least true, and the user renames it if they care.
    "api".to_owned()
}

/// Strip format noise and reduce to a kebab-case identifier.
fn clean(raw: &str) -> String {
    let lowered = raw.to_ascii_lowercase();
    let mut name = lowered.as_str();
    for noise in [
        "-openapi", "_openapi", "-swagger", "_swagger", "-schema", "_schema", "-api", "_api",
    ] {
        name = name.strip_suffix(noise).unwrap_or(name);
    }
    for noise in ["openapi-", "openapi_", "swagger-", "swagger_"] {
        name = name.strip_prefix(noise).unwrap_or(name);
    }

    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_owned()
}

/// Render a `brake.toml` for what was discovered.
///
/// Commented, because the file is the user's from the moment it is written and
/// the comments are the only explanation they get without opening the docs.
#[must_use]
pub fn render_config(discovery: &Discovery, baseline_reference: &str) -> String {
    let mut out = String::new();
    out.push_str(
        "# brake — what to gate, and against what.\n\
         # Written by `brake init`. Edit freely; brake never rewrites it.\n\
         #\n\
         # Docs: https://github.com/grahambrooks/brake\n\
         # Rules: https://github.com/grahambrooks/brake/blob/main/docs/rules.md\n\n",
    );

    out.push_str(&format!(
        "[defaults]\n\
         # wire | wire-json | surface | strict — each catches everything the\n\
         # one before it does. `brake explain <rule>` says what a rule is for.\n\
         compatibility = \"wire-json\"\n\
         # The merge-base does not fire on breaks another pull request already\n\
         # landed, and advances on every merge — a ratchet with no state file.\n\
         baseline = {{ git-merge-base = \"{baseline_reference}\" }}\n"
    ));

    if discovery.contracts.is_empty() {
        out.push_str(
            "\n# No contracts were found. Declare one by hand:\n\
             #\n\
             # [[contract]]\n\
             # name = \"payments\"\n\
             # format = \"openapi\"          # openapi | proto | graphql\n\
             # source = \"api/openapi.yaml\"\n",
        );
        return out;
    }

    for contract in &discovery.contracts {
        out.push_str(&format!(
            "\n[[contract]]\nname = \"{}\"\nformat = \"{}\"\nsource = \"{}\"\n",
            contract.name,
            format_name(contract.format),
            contract.source,
        ));
    }

    out.push_str(
        "\n# To gate a release rather than a commit, add a second entry over the\n\
         # same source with `baseline = { latest-tag = \"v*\" }`. It asks a\n\
         # different question: not \"is this change safe?\" but \"has the API\n\
         # broken since the last version anyone is running?\"\n",
    );
    out
}

#[must_use]
pub fn format_name(format: ContractFormat) -> &'static str {
    match format {
        ContractFormat::Openapi => "openapi",
        ContractFormat::Proto => "proto",
        ContractFormat::Graphql => "graphql",
    }
}

/// The ref a merge-base baseline should point at.
///
/// Written into the generated config, so it has to be a ref that actually
/// resolves *here*. Guessing `origin/main` produces a file whose very next
/// command fails — on a repository whose trunk is `master`, and on one with no
/// remote at all, which is what a fresh repository and a first trial both look
/// like.
///
/// The local fallbacks are not a lesser answer. With no remote,
/// `merge-base(HEAD, main)` is `HEAD`, so the working tree is compared against
/// the last commit — which is exactly what a pre-commit hook wants.
#[must_use]
pub fn default_baseline_reference(root: &Path) -> String {
    let Ok(repository) = gix::open(root) else {
        return "origin/main".to_owned();
    };

    for candidate in [
        "origin/main",
        "origin/master",
        "origin/trunk",
        "main",
        "master",
        "trunk",
    ] {
        if repository.rev_parse_single(candidate).is_ok() {
            return candidate.to_owned();
        }
    }

    // Detached, or a branch under some other name. `HEAD` always resolves
    // where anything does, and still compares the working tree.
    if repository.rev_parse_single("HEAD").is_ok() {
        return "HEAD".to_owned();
    }
    "origin/main".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{TempDir, tempdir};

    const OPENAPI: &str = r#"
openapi: 3.1.0
info: { title: payments, version: "1.0" }
paths:
  /payments:
    get:
      operationId: listPayments
      responses:
        "200": { description: ok }
"#;

    const PROTO: &str = r#"
syntax = "proto3";
package events;
message Ping { string id = 1; }
service Events { rpc Send(Ping) returns (Ping); }
"#;

    const GRAPHQL: &str = "type Query { payment(id: ID!): String! }\n";

    fn repo(files: &[(&str, &str)]) -> TempDir {
        let repo = tempdir().expect("tempdir");
        for (path, body) in files {
            let full = repo.path().join(path);
            fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
            fs::write(full, body).expect("write");
        }
        repo
    }

    fn names(discovery: &Discovery) -> Vec<&str> {
        discovery
            .contracts
            .iter()
            .map(|contract| contract.source.as_str())
            .collect()
    }

    #[test]
    fn finds_each_format_by_parsing_it() {
        let repo = repo(&[
            ("api/payments-openapi.yaml", OPENAPI),
            ("api/events.proto", PROTO),
            ("api/schema.graphql", GRAPHQL),
        ]);

        let discovery = discover(repo.path());
        assert_eq!(
            names(&discovery),
            vec![
                "api/events.proto",
                "api/payments-openapi.yaml",
                "api/schema.graphql"
            ]
        );

        let formats: Vec<_> = discovery
            .contracts
            .iter()
            .map(|contract| contract.format)
            .collect();
        assert!(formats.contains(&ContractFormat::Openapi));
        assert!(formats.contains(&ContractFormat::Proto));
        assert!(formats.contains(&ContractFormat::Graphql));
    }

    #[test]
    fn does_not_mistake_ordinary_yaml_for_an_api() {
        // The failure this whole approach exists to avoid: a filename
        // heuristic calls these contracts, and a tool that opens by
        // misidentifying your files has spent its credibility already.
        let repo = repo(&[
            (
                ".github/workflows/api-tests.yaml",
                "name: api-tests\non: push\n",
            ),
            ("openapi-notes.yaml", "just: some notes\n"),
            ("package.json", r#"{"name":"app","version":"1.0.0"}"#),
            (
                "docker-compose.yml",
                "services:\n  api:\n    image: nginx\n",
            ),
            ("api/openapi.yaml", OPENAPI),
        ]);

        let discovery = discover(repo.path());
        assert_eq!(
            names(&discovery),
            vec!["api/openapi.yaml"],
            "only the file an ingester can actually read is a contract"
        );
    }

    #[test]
    fn skips_checked_in_baselines() {
        let repo = repo(&[
            ("api/payments.yaml", OPENAPI),
            ("api/payments.baseline.yaml", OPENAPI),
            ("api/other-baseline.yaml", OPENAPI),
        ]);

        let discovery = discover(repo.path());
        assert_eq!(names(&discovery), vec!["api/payments.yaml"]);
        assert_eq!(discovery.baselines_skipped.len(), 2);
    }

    #[test]
    fn does_not_walk_into_dependency_directories() {
        let repo = repo(&[
            ("api/openapi.yaml", OPENAPI),
            ("node_modules/pkg/api/openapi.yaml", OPENAPI),
            ("target/debug/build/openapi.yaml", OPENAPI),
            (".git/openapi.yaml", OPENAPI),
        ]);
        assert_eq!(names(&discover(repo.path())), vec!["api/openapi.yaml"]);
    }

    #[test]
    fn derives_a_name_that_does_not_repeat_the_format() {
        assert_eq!(contract_name("api/payments-openapi.yaml"), "payments");
        assert_eq!(contract_name("api/payments.yaml"), "payments");
        assert_eq!(contract_name("contracts/events.proto"), "events");
        assert_eq!(contract_name("swagger-ledger.json"), "ledger");

        // The filename says only which format it is, so the directory names
        // the contract — and usually names it better.
        assert_eq!(contract_name("payments/openapi.yaml"), "payments");
        assert_eq!(contract_name("services/ledger/api/openapi.yaml"), "ledger");
        // Every segment is a format word; `api` is at least true.
        assert_eq!(contract_name("api/openapi.yaml"), "api");
        assert_eq!(contract_name("openapi.yaml"), "api");
    }

    #[test]
    fn makes_duplicate_names_unique() {
        let repo = repo(&[
            ("service-a/openapi.yaml", OPENAPI),
            ("service-b/openapi.yaml", OPENAPI),
        ]);
        let discovery = discover(repo.path());
        let names: Vec<_> = discovery
            .contracts
            .iter()
            .map(|contract| contract.name.as_str())
            .collect();

        assert_eq!(names.len(), 2);
        assert_ne!(names[0], names[1], "two contracts cannot share a name");
    }

    #[test]
    fn the_rendered_config_parses_back_and_declares_what_was_found() {
        let repo = repo(&[
            ("api/payments-openapi.yaml", OPENAPI),
            ("api/events.proto", PROTO),
        ]);
        let discovery = discover(repo.path());
        let rendered = render_config(&discovery, "origin/main");

        // The point of generating it: it has to be valid input to the very
        // next command the user runs.
        let config = crate::config::Config::parse(&rendered).expect("generated config must parse");
        assert_eq!(config.contracts.len(), 2);
        assert_eq!(
            config.defaults.compatibility,
            crate::config::Compatibility::WireJson
        );

        let sources: Vec<_> = config
            .contracts
            .iter()
            .map(|contract| display_path(&contract.source))
            .collect();
        assert!(sources.contains(&"api/payments-openapi.yaml".to_owned()));
        assert!(sources.contains(&"api/events.proto".to_owned()));
    }

    #[test]
    fn an_empty_repository_still_renders_a_usable_starting_point() {
        let repo = tempdir().expect("tempdir");
        let rendered = render_config(&discover(repo.path()), "origin/main");

        // Must parse — a commented-out example is not a contract entry.
        let config = crate::config::Config::parse(&rendered).expect("must parse");
        assert!(config.contracts.is_empty());
        // …and must show what one looks like, since that is the whole reason
        // the user ran init.
        assert!(rendered.contains("[[contract]]"));
        assert!(rendered.contains("format = \"openapi\""));
    }

    #[test]
    fn the_baseline_reference_falls_back_when_there_is_no_repository() {
        let repo = tempdir().expect("tempdir");
        assert_eq!(default_baseline_reference(repo.path()), "origin/main");
    }

    #[test]
    fn the_baseline_reference_resolves_in_a_repository_with_no_remote() {
        // The common first-trial shape: `git init`, one commit, no origin.
        // Writing `origin/main` here produces a config whose very next command
        // fails, which is the wall init exists to remove.
        let repo = repo(&[("api/openapi.yaml", OPENAPI)]);
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(args)
                .current_dir(repo.path())
                .output()
                .expect("git")
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.name", "Brake Test"]);
        git(&["config", "user.email", "brake@example.com"]);
        git(&["add", "."]);
        git(&["commit", "-m", "init"]);

        let reference = default_baseline_reference(repo.path());
        assert_eq!(reference, "main");

        // And it must actually resolve, which is the whole point.
        let repository = gix::open(repo.path()).expect("open");
        assert!(repository.rev_parse_single(&*reference).is_ok());
    }

    #[test]
    fn identify_rejects_a_file_no_ingester_can_read() {
        let repo = repo(&[("api/broken.yaml", "openapi: 3.1.0\npaths: [not, a, map]\n")]);
        assert!(identify(&repo.path().join("api/broken.yaml")).is_none());
    }
}
