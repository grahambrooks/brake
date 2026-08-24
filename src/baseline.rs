//! Resolving a contract's previous version.
//!
//! Two families, answering two different questions. `file` and
//! `git-merge-base` answer *"is this change safe to merge?"* — the merge-base
//! forgives anything already on the trunk, which is what makes the commit gate
//! adoptable. `tag`, `latest-tag` and `rev` answer *"has the published API
//! broken since the last version consumers actually have?"*, which the
//! merge-base cannot: a break merged three weeks ago is still a break for
//! someone upgrading from the last release.
//!
//! Everything here is `gix`, in-process. `brake` never shells out to `git`.

use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::config::{Baseline, ContractConfig, Defaults};
use crate::contract::{DocumentResolver, FileSystemResolver, SingleDocumentResolver};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBaseline {
    /// How the baseline is named in output: a repository-relative path for a
    /// file, or a `tag:` / `rev:` / `merge-base:` descriptor for a blob. Never
    /// absolute — an absolute path in a span would break guarantees G4 and G6.
    pub label: String,
    pub bytes: Vec<u8>,
    /// Where the *rest* of a multi-document contract lives.
    ///
    /// Carried because a cross-file `$ref` on the baseline side has to be read
    /// from the same place the baseline itself came from. Resolving it against
    /// the working tree instead would put today's shared schema on both sides
    /// of the comparison, and a field deleted from a shared file would then be
    /// missing from both — a clean result brake cannot justify.
    pub origin: BaselineOrigin,
}

/// Where a baseline's sibling documents are read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaselineOrigin {
    /// A `file` baseline is a file in the working tree, so its siblings are
    /// the working tree's too.
    WorkingTree,
    /// Everything else came out of a commit, and its siblings must come out of
    /// that same commit.
    Commit(String),
    /// The baseline came from somewhere no sibling can be located — the legacy
    /// `ref:path` form, whose revision is not recoverable from the spec.
    /// A cross-file `$ref` is then reported as unmodelled rather than resolved
    /// from a revision brake would be guessing at.
    Isolated,
}

impl BaselineOrigin {
    /// A resolver that reads sibling documents from wherever this baseline came
    /// from, rooted at the contract document's own directory.
    ///
    /// `document_dir` is repository-relative and `/`-separated — the parent of
    /// the contract's `source`. Rooting there rather than at the repository
    /// keeps the boundary the one that is documented: a `$ref` reaches the
    /// document's directory and no further.
    #[must_use]
    pub fn resolver(&self, repo_root: &Path, document_dir: &str) -> Box<dyn DocumentResolver> {
        match self {
            Self::WorkingTree => Box::new(FileSystemResolver::new(repo_root.join(document_dir))),
            Self::Commit(id) => Box::new(CommitResolver {
                repo_root: repo_root.to_path_buf(),
                commit: id.clone(),
                prefix: document_dir.to_owned(),
            }),
            Self::Isolated => Box::new(SingleDocumentResolver),
        }
    }
}

/// Reads sibling documents out of one commit's tree.
///
/// The repository is opened per lookup rather than held, so the resolver stays
/// `Send + Sync` without a lock. The cost is bounded: an ingester reads each
/// external document once and caches it.
#[derive(Debug, Clone)]
struct CommitResolver {
    repo_root: PathBuf,
    commit: String,
    /// The contract document's directory, so a lookup is resolved the same way
    /// the working-tree resolver resolves it.
    prefix: String,
}

impl DocumentResolver for CommitResolver {
    fn resolve(&self, relative_path: &str) -> Option<Vec<u8>> {
        if Path::new(relative_path).is_absolute() {
            return None;
        }
        let repo = gix::open(&self.repo_root).ok()?;
        let id = gix::ObjectId::from_hex(self.commit.as_bytes()).ok()?;
        let tree = repo
            .find_object(id)
            .ok()?
            .peel_to_kind(gix::object::Kind::Commit)
            .ok()?
            .try_into_commit()
            .ok()?
            .tree()
            .ok()?;
        let full = if self.prefix.is_empty() {
            relative_path.to_owned()
        } else {
            format!("{}/{relative_path}", self.prefix)
        };
        let entry = tree.lookup_entry_by_path(Path::new(&full)).ok()??;
        let mut blob = entry.object().ok()?.try_into_blob().ok()?;
        Some(blob.take_data())
    }
}

/// Resolve the baseline for a contract, honouring a command-line override.
pub fn resolve_for_contract(
    repo_root: &Path,
    defaults: &Defaults,
    contract: &ContractConfig,
    override_baseline: Option<&Baseline>,
) -> Result<ResolvedBaseline, BaselineError> {
    let baseline = override_baseline
        .or_else(|| contract.effective_baseline(defaults))
        .ok_or_else(|| BaselineError::MissingBaseline {
            contract: contract.name.clone(),
        })?;

    match baseline {
        Baseline::File(file) => resolve_file(repo_root, contract, file),
        Baseline::Git { spec } => resolve_git_spec(repo_root, contract, spec),
        Baseline::GitMergeBase { reference } => resolve_merge_base(repo_root, contract, reference),
        Baseline::Tag { name } => {
            resolve_revision(repo_root, contract, name, &format!("tag:{name}"))
        }
        Baseline::Rev { rev } => resolve_revision(repo_root, contract, rev, &format!("rev:{rev}")),
        Baseline::LatestTag { pattern } => resolve_latest_tag(repo_root, contract, pattern),
    }
}

fn resolve_file(
    repo_root: &Path,
    contract: &ContractConfig,
    file: &Path,
) -> Result<ResolvedBaseline, BaselineError> {
    let bytes = fs::read(repo_root.join(file)).map_err(|source| BaselineError::ReadFile {
        contract: contract.name.clone(),
        path: file.to_path_buf(),
        source,
    })?;
    Ok(ResolvedBaseline {
        label: crate::check::display_path(file),
        bytes,
        origin: BaselineOrigin::WorkingTree,
    })
}

/// The legacy `ref:path` form, which is the only shape that takes a path.
///
/// Kept because removing it would break existing configuration, and this is a
/// tool about not breaking things. `tag` / `rev` are preferred: a path written
/// twice is a path that can drift out of step with `source`, and when it does
/// brake compares two unrelated files and reports the difference as a break.
fn resolve_git_spec(
    repo_root: &Path,
    contract: &ContractConfig,
    spec: &str,
) -> Result<ResolvedBaseline, BaselineError> {
    let repo = open(repo_root, contract)?;
    let describe = |details: String| BaselineError::ResolveGitSpec {
        contract: contract.name.clone(),
        spec: spec.to_owned(),
        details,
    };

    let mut blob = repo
        .rev_parse_single(spec)
        .map_err(|error| describe(error.to_string()))?
        .object()
        .map_err(|error| describe(error.to_string()))?
        .try_into_blob()
        .map_err(|error| describe(error.to_string()))?;

    Ok(ResolvedBaseline {
        label: format!("git:{spec}"),
        bytes: blob.take_data(),
        // `rev_parse_single` resolved straight to a blob, so the commit it came
        // from is not in hand. Guessing at one would compare a baseline
        // document against sibling schemas from a different revision.
        origin: BaselineOrigin::Isolated,
    })
}

/// Read `contract.source` from whatever commit `revspec` names.
///
/// Shared by `tag`, `rev` and `latest-tag`: none of them repeats the path,
/// because the path is already in `source`.
fn resolve_revision(
    repo_root: &Path,
    contract: &ContractConfig,
    revspec: &str,
    label: &str,
) -> Result<ResolvedBaseline, BaselineError> {
    let repo = open(repo_root, contract)?;
    let commit =
        repo.rev_parse_single(revspec)
            .map_err(|error| BaselineError::ResolveRevision {
                contract: contract.name.clone(),
                revision: revspec.to_owned(),
                details: error.to_string(),
            })?;
    read_source_at(&repo, contract, commit.detach(), revspec, label)
}

/// The newest tag matching `pattern` that HEAD descends from.
///
/// Naming a tag literally means editing `brake.toml` on every release, and a
/// version somebody forgot to bump is a gate quietly comparing against ancient
/// history. See `design/02-contract-gates.md` §2.1.
fn resolve_latest_tag(
    repo_root: &Path,
    contract: &ContractConfig,
    pattern: &str,
) -> Result<ResolvedBaseline, BaselineError> {
    let repo = open(repo_root, contract)?;
    let unavailable = |details: String| BaselineError::ResolveLatestTag {
        contract: contract.name.clone(),
        pattern: pattern.to_owned(),
        details,
    };

    let head = repo
        .head_id()
        .map_err(|error| unavailable(format!("failed to resolve HEAD: {error}")))?;

    let mut candidates = Vec::new();
    let platform = repo
        .references()
        .map_err(|error| unavailable(format!("failed to read references: {error}")))?;
    let tags = platform
        .tags()
        .map_err(|error| unavailable(format!("failed to read tags: {error}")))?;

    for tag in tags.flatten() {
        let full_name = tag.name().as_bstr().to_string();
        let short = full_name
            .strip_prefix("refs/tags/")
            .unwrap_or(&full_name)
            .to_owned();
        if !glob_matches(pattern, &short) {
            continue;
        }
        // An annotated tag is a tag object, not a commit; peeling is what
        // makes both kinds comparable.
        let Ok(peeled) = tag.into_fully_peeled_id() else {
            continue;
        };
        candidates.push((version_key(&short), short, peeled.detach()));
    }

    if candidates.is_empty() {
        // A shallow or `--no-tags` clone has no tags at all. Reporting clean
        // here would be a verdict brake cannot justify, so it is a failure.
        return Err(unavailable(
            "no tag matches this pattern. A shallow or `--no-tags` clone carries no tags; \
             CI needs `fetch-depth: 0`"
                .to_owned(),
        ));
    }

    // Newest first, so the ancestry check below usually runs exactly once.
    candidates.sort_by(|a, b| compare_version_keys(&b.0, &a.0).then_with(|| b.1.cmp(&a.1)));

    for (_, name, id) in &candidates {
        // A tag cut on an unrelated release branch is not a version this
        // commit evolved from; comparing against it reports a divergence as a
        // break.
        let is_ancestor = repo
            .merge_base(head.detach(), *id)
            .is_ok_and(|base| base.detach() == *id);
        if is_ancestor {
            return read_source_at(&repo, contract, *id, name, &format!("tag:{name}"));
        }
    }

    Err(unavailable(format!(
        "{} tag(s) match, but HEAD descends from none of them (newest checked: `{}`)",
        candidates.len(),
        candidates[0].1
    )))
}

fn resolve_merge_base(
    repo_root: &Path,
    contract: &ContractConfig,
    reference: &str,
) -> Result<ResolvedBaseline, BaselineError> {
    let repo = open(repo_root, contract)?;
    let describe = |details: String| BaselineError::ResolveMergeBase {
        contract: contract.name.clone(),
        reference: reference.to_owned(),
        details,
    };

    let head = repo
        .head_id()
        .map_err(|error| describe(error.to_string()))?;
    let target = repo
        .rev_parse_single(reference)
        .map_err(|_| BaselineError::UnknownReference {
            contract: contract.name.clone(),
            reference: reference.to_owned(),
        })?;
    let merge_base = repo
        .merge_base(head.detach(), target.detach())
        .map_err(|error| describe(error.to_string()))?;

    read_source_at(
        &repo,
        contract,
        merge_base.detach(),
        &format!("the merge-base with `{reference}`"),
        &format!(
            "merge-base:{reference}:{}",
            crate::check::display_path(&contract.source)
        ),
    )
}

fn open(repo_root: &Path, contract: &ContractConfig) -> Result<gix::Repository, BaselineError> {
    gix::open(repo_root).map_err(|source| BaselineError::OpenGitRepository {
        contract: contract.name.clone(),
        path: repo_root.to_path_buf(),
        details: source.to_string(),
    })
}

/// Read `contract.source` out of the tree of the commit `id` names.
///
/// `reference` is how the commit is described in an error; `label` is how it
/// appears in a span, and must stay free of absolute paths.
fn read_source_at(
    repo: &gix::Repository,
    contract: &ContractConfig,
    id: gix::ObjectId,
    reference: &str,
    label: &str,
) -> Result<ResolvedBaseline, BaselineError> {
    let describe = |details: String| BaselineError::ResolveRevisionPath {
        contract: contract.name.clone(),
        reference: reference.to_owned(),
        path: contract.source.clone(),
        details,
    };

    let commit = repo
        .find_object(id)
        .map_err(|error| describe(error.to_string()))?
        // A tag ref may point at a tag object rather than a commit.
        .peel_to_kind(gix::object::Kind::Commit)
        .map_err(|error| describe(error.to_string()))?
        .try_into_commit()
        .map_err(|error| describe(error.to_string()))?;
    // Captured after peeling: a tag object's own id would not name a tree.
    let commit_id = commit.id().to_hex().to_string();
    let tree = commit.tree().map_err(|error| describe(error.to_string()))?;

    let entry = tree
        .lookup_entry_by_path(&contract.source)
        .map_err(|error| describe(error.to_string()))?
        // The commit resolved and the file simply was not in it: the contract
        // is new. `check` turns this into a note, not a failure.
        .ok_or_else(|| BaselineError::AbsentFromBaseline {
            contract: contract.name.clone(),
            reference: reference.to_owned(),
            path: contract.source.clone(),
        })?;

    let mut blob = entry
        .object()
        .map_err(|error| describe(error.to_string()))?
        .try_into_blob()
        .map_err(|error| describe(error.to_string()))?;

    Ok(ResolvedBaseline {
        label: label.to_owned(),
        bytes: blob.take_data(),
        origin: BaselineOrigin::Commit(commit_id),
    })
}

/// Sort key that compares numeric runs numerically.
///
/// Byte order puts `v10.0.0` below `v9.0.0`, which would silently gate a 10.x
/// release against a 9.x baseline. CalVer (`2026.8.1`) needs the same
/// treatment. Non-numeric runs compare bytewise, so `v1.0.0-rc1` orders below
/// `v1.0.0` — a prerelease is not the release.
fn version_key(name: &str) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut chars = name.char_indices().peekable();
    while let Some((start, character)) = chars.next() {
        if character.is_ascii_digit() {
            let mut end = start + character.len_utf8();
            while let Some((next, digit)) = chars.peek() {
                if !digit.is_ascii_digit() {
                    break;
                }
                end = next + digit.len_utf8();
                chars.next();
            }
            segments.push(Segment::Number(
                name[start..end].parse().unwrap_or(u64::MAX),
            ));
        } else {
            let mut end = start + character.len_utf8();
            while let Some((next, other)) = chars.peek() {
                if other.is_ascii_digit() {
                    break;
                }
                end = next + other.len_utf8();
                chars.next();
            }
            segments.push(Segment::Text(name[start..end].to_owned()));
        }
    }
    segments
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Segment {
    Text(String),
    Number(u64),
}

/// Order two version keys.
///
/// Not `Vec`'s ordering, which compares lexicographically and therefore ranks
/// the *longer* `v1.0.0-rc1` above `v1.0.0` — exactly backwards, and enough to
/// gate a release against its own release candidate.
///
/// Where one key runs out, its tail decides. The distinction is not "text or
/// number": `.` is itself a text run, so `v1.0.0.1` and `v1.0.0-rc1` both
/// continue with text. What separates them is whether that run is punctuation
/// joining another component, or a qualifier naming a prerelease.
fn compare_version_keys(left: &[Segment], right: &[Segment]) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    for (a, b) in left.iter().zip(right) {
        match a.cmp(b) {
            Ordering::Equal => {}
            other => return other,
        }
    }

    let extends = |tail: &Segment| match tail {
        // `v1.0.0.1` continues the version and outranks `v1.0.0`.
        Segment::Text(run) => run.chars().all(|c| c == '.'),
        Segment::Number(_) => true,
    };

    match left.len().cmp(&right.len()) {
        Ordering::Equal => Ordering::Equal,
        // `v1.0.0-rc1` is a qualifier on `v1.0.0`, and a prerelease is not
        // the release.
        Ordering::Greater if extends(&left[right.len()]) => Ordering::Greater,
        Ordering::Greater => Ordering::Less,
        Ordering::Less if extends(&right[left.len()]) => Ordering::Less,
        Ordering::Less => Ordering::Greater,
    }
}

/// `*` and `?` matching, which is what a tag pattern needs and no more.
///
/// A full glob crate would be a dependency for one wildcard; a regex would
/// invite patterns whose behaviour nobody can predict from the config file.
fn glob_matches(pattern: &str, value: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let value: Vec<char> = value.chars().collect();

    // Standard two-cursor wildcard match with backtracking on the last `*`.
    let (mut p, mut v) = (0usize, 0usize);
    let (mut star, mut match_after_star) = (None, 0usize);
    while v < value.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == value[v]) {
            p += 1;
            v += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            match_after_star = v;
            p += 1;
        } else if let Some(star_at) = star {
            p = star_at + 1;
            match_after_star += 1;
            v = match_after_star;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

#[derive(Debug, Error)]
pub enum BaselineError {
    #[error("contract `{contract}` has no configured baseline")]
    MissingBaseline { contract: String },
    #[error(
        "contract `{contract}` source `{path}` does not exist in {reference}, \
         so there is no previous version to compare against"
    )]
    AbsentFromBaseline {
        contract: String,
        reference: String,
        path: PathBuf,
    },
    #[error("failed to read baseline file for contract `{contract}` at {path}: {source}")]
    ReadFile {
        contract: String,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to open git repository at {path} for contract `{contract}`: {details}")]
    OpenGitRepository {
        contract: String,
        path: PathBuf,
        details: String,
    },
    #[error("failed to resolve git baseline spec `{spec}` for contract `{contract}`: {details}")]
    ResolveGitSpec {
        contract: String,
        spec: String,
        details: String,
    },
    #[error(
        "contract `{contract}`: the git ref `{reference}` does not resolve in this \
         repository.\n\n\
         If there is no `origin` remote yet, point the baseline at a local branch \
         (`main`),\nor re-run `brake init` to detect one that exists. In CI, \
         `actions/checkout` needs\n`fetch-depth: 0` for refs other than the checked-out \
         commit to be present."
    )]
    UnknownReference { contract: String, reference: String },
    #[error("failed to resolve revision `{revision}` for contract `{contract}`: {details}")]
    ResolveRevision {
        contract: String,
        revision: String,
        details: String,
    },
    #[error("failed to resolve `latest-tag = \"{pattern}\"` for contract `{contract}`: {details}")]
    ResolveLatestTag {
        contract: String,
        pattern: String,
        details: String,
    },
    #[error(
        "failed to resolve merge-base against `{reference}` for contract `{contract}`: {details}"
    )]
    ResolveMergeBase {
        contract: String,
        reference: String,
        details: String,
    },
    #[error("failed to read `{path}` from {reference} for contract `{contract}`: {details}")]
    ResolveRevisionPath {
        contract: String,
        reference: String,
        path: PathBuf,
        details: String,
    },
}
#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use tempfile::tempdir;

    use super::*;
    use crate::config::{Compatibility, ContractFormat};

    fn contract_with(baseline: Option<Baseline>) -> ContractConfig {
        ContractConfig {
            name: "payments".to_owned(),
            format: ContractFormat::Openapi,
            source: PathBuf::from("api/payments-openapi.yaml"),
            compatibility: None,
            baseline,
            allow: Vec::new(),
            generated: None,
        }
    }

    fn run_git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git command should launch");
        assert!(
            output.status.success(),
            "git {:?} failed: {}\n{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn run_git_output(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git command should launch");
        assert!(
            output.status.success(),
            "git {:?} failed: {}\n{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn write_and_commit(repo: &Path, path: &str, contents: &str, message: &str) {
        let absolute = repo.join(path);
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(&absolute, contents).expect("write file");
        run_git(repo, &["add", path]);
        run_git(repo, &["commit", "-m", message]);
    }

    #[test]
    fn resolves_file_baseline_from_contract_override() {
        let root = tempdir().expect("tempdir");
        let baseline_path = root.path().join("api/payments.baseline.yaml");
        fs::create_dir_all(baseline_path.parent().expect("parent")).expect("mkdir");
        fs::write(&baseline_path, b"openapi: 3.1.0").expect("write baseline");

        let defaults = Defaults {
            compatibility: Compatibility::WireJson,
            baseline: Some(Baseline::File(PathBuf::from("api/default.baseline.yaml"))),
        };
        let contract = contract_with(Some(Baseline::File(PathBuf::from(
            "api/payments.baseline.yaml",
        ))));

        let resolved =
            resolve_for_contract(root.path(), &defaults, &contract, None).expect("file baseline");

        assert_eq!(resolved.label, "api/payments.baseline.yaml");
        assert!(
            !resolved
                .label
                .contains(&root.path().to_string_lossy().to_string()),
            "a baseline label must never carry the absolute checkout path"
        );
        assert_eq!(resolved.bytes, b"openapi: 3.1.0");
    }

    #[test]
    fn resolves_file_baseline_from_defaults() {
        let root = tempdir().expect("tempdir");
        let baseline_path = root.path().join("api/default.baseline.yaml");
        fs::create_dir_all(baseline_path.parent().expect("parent")).expect("mkdir");
        fs::write(&baseline_path, b"openapi: 3.1.0").expect("write baseline");

        let defaults = Defaults {
            compatibility: Compatibility::WireJson,
            baseline: Some(Baseline::File(PathBuf::from("api/default.baseline.yaml"))),
        };
        let contract = contract_with(None);

        let resolved =
            resolve_for_contract(root.path(), &defaults, &contract, None).expect("file baseline");

        assert_eq!(resolved.label, "api/default.baseline.yaml");
        assert!(baseline_path.exists());
    }

    #[test]
    fn resolves_git_baseline_from_ref_and_path_spec() {
        let root = tempdir().expect("tempdir");
        run_git(root.path(), &["init", "-b", "main"]);
        run_git(root.path(), &["config", "user.name", "Brake Test"]);
        run_git(root.path(), &["config", "user.email", "brake@example.com"]);

        write_and_commit(
            root.path(),
            "api/payments-openapi.yaml",
            "openapi: 3.1.0\ninfo:\n  title: baseline\n",
            "baseline",
        );
        write_and_commit(
            root.path(),
            "api/payments-openapi.yaml",
            "openapi: 3.1.0\ninfo:\n  title: head\n",
            "head",
        );

        let defaults = Defaults {
            compatibility: Compatibility::WireJson,
            baseline: None,
        };
        let contract = contract_with(Some(Baseline::Git {
            spec: "HEAD~1:api/payments-openapi.yaml".to_owned(),
        }));

        let resolved =
            resolve_for_contract(root.path(), &defaults, &contract, None).expect("git baseline");
        let content = String::from_utf8(resolved.bytes).expect("utf-8 baseline");
        assert!(content.contains("title: baseline"));
    }

    #[test]
    fn resolves_git_merge_base_baseline_against_reference() {
        let root = tempdir().expect("tempdir");
        run_git(root.path(), &["init", "-b", "main"]);
        run_git(root.path(), &["config", "user.name", "Brake Test"]);
        run_git(root.path(), &["config", "user.email", "brake@example.com"]);

        write_and_commit(
            root.path(),
            "api/payments-openapi.yaml",
            "openapi: 3.1.0\ninfo:\n  title: base\n",
            "base",
        );
        let base_sha = run_git_output(root.path(), &["rev-parse", "HEAD"]);

        write_and_commit(
            root.path(),
            "api/payments-openapi.yaml",
            "openapi: 3.1.0\ninfo:\n  title: main\n",
            "main-update",
        );

        run_git(
            root.path(),
            &["checkout", "-b", "feature", base_sha.as_str()],
        );
        write_and_commit(
            root.path(),
            "README.md",
            "# feature branch change\n",
            "feature-change",
        );

        let defaults = Defaults {
            compatibility: Compatibility::WireJson,
            baseline: Some(Baseline::GitMergeBase {
                reference: "refs/heads/main".to_owned(),
            }),
        };
        let contract = contract_with(None);
        let resolved = resolve_for_contract(root.path(), &defaults, &contract, None)
            .expect("merge-base baseline");

        let content = String::from_utf8(resolved.bytes).expect("utf-8 baseline");
        assert!(content.contains("title: base"));
    }

    #[test]
    fn errors_when_baseline_missing() {
        let root = tempdir().expect("tempdir");
        let defaults = Defaults {
            compatibility: Compatibility::WireJson,
            baseline: None,
        };
        let contract = contract_with(None);

        let error = resolve_for_contract(root.path(), &defaults, &contract, None)
            .expect_err("missing baseline must error");
        assert!(matches!(error, BaselineError::MissingBaseline { .. }));
    }

    #[test]
    fn errors_for_invalid_git_spec() {
        let root = tempdir().expect("tempdir");
        run_git(root.path(), &["init", "-b", "main"]);
        run_git(root.path(), &["config", "user.name", "Brake Test"]);
        run_git(root.path(), &["config", "user.email", "brake@example.com"]);
        write_and_commit(
            root.path(),
            "api/payments-openapi.yaml",
            "openapi: 3.1.0\n",
            "init",
        );

        let defaults = Defaults {
            compatibility: Compatibility::WireJson,
            baseline: None,
        };
        let contract = contract_with(Some(Baseline::Git {
            spec: "does-not-exist:api/payments-openapi.yaml".to_owned(),
        }));

        let error = resolve_for_contract(root.path(), &defaults, &contract, None)
            .expect_err("invalid git spec must error");
        assert!(matches!(error, BaselineError::ResolveGitSpec { .. }));
    }

    /// A repository with two tagged releases and an untagged commit on top.
    fn tagged_repository() -> tempfile::TempDir {
        let root = tempdir().expect("tempdir");
        run_git(root.path(), &["init", "-b", "main"]);
        run_git(root.path(), &["config", "user.name", "Brake Test"]);
        run_git(root.path(), &["config", "user.email", "brake@example.com"]);

        write_and_commit(
            root.path(),
            "api/payments-openapi.yaml",
            "openapi: 3.1.0\ninfo:\n  title: v9\n",
            "v9",
        );
        // Annotated, which is a tag *object* rather than a commit — the kind
        // that needs peeling.
        run_git(root.path(), &["tag", "-a", "v9.0.0", "-m", "release 9"]);

        write_and_commit(
            root.path(),
            "api/payments-openapi.yaml",
            "openapi: 3.1.0\ninfo:\n  title: v10\n",
            "v10",
        );
        // Lightweight, which points straight at the commit.
        run_git(root.path(), &["tag", "v10.0.0"]);

        write_and_commit(
            root.path(),
            "api/payments-openapi.yaml",
            "openapi: 3.1.0\ninfo:\n  title: head\n",
            "unreleased work",
        );
        root
    }

    fn resolve(root: &Path, baseline: Baseline) -> Result<ResolvedBaseline, BaselineError> {
        let defaults = Defaults {
            compatibility: Compatibility::WireJson,
            baseline: None,
        };
        resolve_for_contract(root, &defaults, &contract_with(Some(baseline)), None)
    }

    fn title_of(resolved: &ResolvedBaseline) -> String {
        String::from_utf8(resolved.bytes.clone()).expect("utf-8")
    }

    #[test]
    fn resolves_a_named_annotated_tag_without_repeating_the_path() {
        let root = tagged_repository();
        let resolved = resolve(
            root.path(),
            Baseline::Tag {
                name: "v9.0.0".to_owned(),
            },
        )
        .expect("tag baseline");

        assert!(title_of(&resolved).contains("title: v9"));
        assert_eq!(resolved.label, "tag:v9.0.0");
    }

    #[test]
    fn resolves_a_lightweight_tag_too() {
        let root = tagged_repository();
        let resolved = resolve(
            root.path(),
            Baseline::Tag {
                name: "v10.0.0".to_owned(),
            },
        )
        .expect("tag baseline");
        assert!(title_of(&resolved).contains("title: v10"));
    }

    #[test]
    fn resolves_an_explicit_revision() {
        let root = tagged_repository();
        let sha = run_git_output(root.path(), &["rev-parse", "HEAD~1"]);
        let resolved =
            resolve(root.path(), Baseline::Rev { rev: sha.clone() }).expect("rev baseline");

        assert!(title_of(&resolved).contains("title: v10"));
        assert_eq!(resolved.label, format!("rev:{sha}"));
    }

    #[test]
    fn latest_tag_prefers_the_newest_version_not_the_newest_string() {
        let root = tagged_repository();
        let resolved = resolve(
            root.path(),
            Baseline::LatestTag {
                pattern: "v*".to_owned(),
            },
        )
        .expect("latest-tag baseline");

        assert!(
            title_of(&resolved).contains("title: v10"),
            "byte ordering would pick v9.0.0 over v10.0.0"
        );
        assert_eq!(resolved.label, "tag:v10.0.0");
    }

    #[test]
    fn latest_tag_honours_the_glob() {
        let root = tagged_repository();
        run_git(root.path(), &["tag", "nightly-2026-08-23"]);

        let resolved = resolve(
            root.path(),
            Baseline::LatestTag {
                pattern: "v*".to_owned(),
            },
        )
        .expect("latest-tag baseline");
        assert_eq!(
            resolved.label, "tag:v10.0.0",
            "a tag outside the pattern must not win"
        );
    }

    #[test]
    fn latest_tag_ignores_a_tag_head_does_not_descend_from() {
        let root = tagged_repository();
        // A release cut on a branch this commit never saw.
        let base = run_git_output(root.path(), &["rev-parse", "HEAD"]);
        run_git(root.path(), &["checkout", "-q", "-b", "other"]);
        write_and_commit(
            root.path(),
            "api/payments-openapi.yaml",
            "openapi: 3.1.0\ninfo:\n  title: divergent\n",
            "divergent",
        );
        run_git(root.path(), &["tag", "v11.0.0"]);
        run_git(root.path(), &["checkout", "-q", base.as_str()]);

        let resolved = resolve(
            root.path(),
            Baseline::LatestTag {
                pattern: "v*".to_owned(),
            },
        )
        .expect("latest-tag baseline");

        assert_eq!(
            resolved.label, "tag:v10.0.0",
            "a tag on an unrelated branch is not a version HEAD evolved from"
        );
    }

    #[test]
    fn latest_tag_with_no_matching_tag_is_an_error_not_a_clean_result() {
        let root = tagged_repository();
        let error = resolve(
            root.path(),
            Baseline::LatestTag {
                pattern: "release-*".to_owned(),
            },
        )
        .expect_err("no matching tag must not resolve");

        assert!(matches!(error, BaselineError::ResolveLatestTag { .. }));
        // The message has to say what to do about it: a shallow clone is the
        // usual cause and is not obvious from "no tag matched".
        assert!(
            error.to_string().contains("fetch-depth"),
            "unhelpful message: {error}"
        );
    }

    #[test]
    fn a_contract_absent_from_the_tag_is_reported_as_new() {
        let root = tagged_repository();
        let defaults = Defaults {
            compatibility: Compatibility::WireJson,
            baseline: None,
        };
        let mut contract = contract_with(Some(Baseline::Tag {
            name: "v9.0.0".to_owned(),
        }));
        contract.source = PathBuf::from("api/added-later.yaml");

        let error = resolve_for_contract(root.path(), &defaults, &contract, None)
            .expect_err("absent source");
        assert!(matches!(error, BaselineError::AbsentFromBaseline { .. }));
    }

    #[test]
    fn an_unknown_tag_is_an_error() {
        let root = tagged_repository();
        let error = resolve(
            root.path(),
            Baseline::Tag {
                name: "v999.0.0".to_owned(),
            },
        )
        .expect_err("unknown tag");
        assert!(matches!(error, BaselineError::ResolveRevision { .. }));
    }

    #[test]
    fn version_ordering_compares_numeric_runs_numerically() {
        let ordered = |a: &str, b: &str| {
            compare_version_keys(&version_key(a), &version_key(b)) == std::cmp::Ordering::Less
        };

        assert!(
            ordered("v9.0.0", "v10.0.0"),
            "byte order gets this backwards"
        );
        assert!(ordered("v1.9.0", "v1.10.0"));
        assert!(ordered("2026.8.1", "2026.9.0"), "CalVer needs it too");
        assert!(ordered("2026.9.0", "2026.10.0"));
        // A prerelease is not the release.
        assert!(ordered("v1.0.0-rc1", "v1.0.0"));
        assert!(ordered("v1.0.0-rc1", "v1.0.0-rc2"));
        // A further component is not a prerelease: it outranks the shorter.
        assert!(ordered("v1.0.0", "v1.0.0.1"));
        assert_eq!(
            compare_version_keys(&version_key("v1.0.0"), &version_key("v1.0.0")),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn glob_matches_what_a_tag_pattern_needs() {
        assert!(glob_matches("v*", "v1.2.3"));
        assert!(glob_matches("*", "anything"));
        assert!(glob_matches("v?.0.0", "v1.0.0"));
        assert!(glob_matches("v1.*.0", "v1.22.0"));
        assert!(glob_matches("exact", "exact"));

        assert!(!glob_matches("v*", "nightly"));
        assert!(!glob_matches("v?.0.0", "v10.0.0"));
        assert!(!glob_matches("exact", "exactly"));
        assert!(!glob_matches("release-*", "v1.0.0"));
    }

    #[test]
    fn a_release_baseline_is_distinguishable_from_a_merge_baseline() {
        // The distinction drives whether `stale-allow` and the defaults make
        // sense, so it must not be a comment.
        assert!(Baseline::Tag { name: "v1".into() }.is_release_baseline());
        assert!(Baseline::Rev { rev: "abc".into() }.is_release_baseline());
        assert!(
            Baseline::LatestTag {
                pattern: "v*".into()
            }
            .is_release_baseline()
        );
        assert!(
            !Baseline::GitMergeBase {
                reference: "origin/main".into()
            }
            .is_release_baseline()
        );
        assert!(!Baseline::File(PathBuf::from("a.yaml")).is_release_baseline());
    }
}
