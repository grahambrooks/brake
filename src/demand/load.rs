//! Reading declared demand off the filesystem.
//!
//! `brake` will not pull these files itself, under any flag, and that is not a
//! capability waiting for a use case: the moment a contract gate can be
//! pointed at a URL it stops being reproducible on a laptop, in an air-gapped
//! build, or three years from now when the broker has been decommissioned. A
//! prior CI step writes the directory; this reads it. A failed pull leaves the
//! declared file absent, which is `consumer-unreachable` and exit `1` — loud,
//! not clean. See `design/05-consumer-demand.md` §5.1.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use super::{Demand, Usages, digest, verify};
use crate::check::display_path;
use crate::config::{Config, ConsumerConfig, DemandFormat};
use crate::contract::EndpointKey;
use crate::rules::{self, Finding};

/// One consumer declaration, read and bound to the contract it constrains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    /// The name findings use: the `[[consumer]]` name where one is given, and
    /// whatever the artifact calls itself otherwise.
    pub consumer: String,
    /// The configured contract this constrains.
    pub provider: String,
    /// Repository-relative source path.
    pub source: String,
    /// A short content digest, so `brake consumers` can say what the verdict
    /// rested on without pretending to measure freshness.
    pub digest: String,
    pub demand: Demand,
}

/// What a run knows about declared consumers.
#[derive(Debug, Clone, Default)]
pub struct Loaded {
    pub declared: Vec<Declared>,
    /// `consumer-unreachable` and `consumer-provider-unmatched`.
    pub findings: Vec<Finding>,
    /// The text of each source, so a renderer can quote the interaction.
    pub sources: BTreeMap<String, String>,
}

/// Read every declared consumer.
///
/// Globs are expanded and sorted byte-wise before use, and the result is
/// ordered by `(consumer, source)`, so guarantee G3 holds over a directory
/// listing.
#[must_use]
pub fn load(repo_root: &Path, config: &Config) -> Loaded {
    let mut loaded = Loaded::default();
    let contracts: BTreeSet<&str> = config
        .contracts
        .iter()
        .map(|contract| contract.name.as_str())
        .collect();

    for declaration in &config.consumers {
        let matched = expand(repo_root, &declaration.source);
        if matched.is_empty() {
            loaded.findings.push(rules::synthetic(
                "consumer-unreachable",
                "",
                format!(
                    "consumer source `{}` matched no file. brake never fetches a declaration: \
                     if CI pulls these, the pull failed",
                    display_path(&declaration.source)
                ),
            ));
            continue;
        }

        for path in matched {
            let source = display_path(&path);
            let Ok(bytes) = fs::read(repo_root.join(&path)) else {
                loaded.findings.push(rules::synthetic(
                    "consumer-unreachable",
                    "",
                    format!("consumer source `{source}` could not be read"),
                ));
                continue;
            };
            let demand = match super::ingest(declaration.format, &source, &bytes) {
                Ok(demand) => demand,
                Err(error) => {
                    loaded.findings.push(rules::synthetic(
                        "consumer-unreachable",
                        "",
                        format!("consumer source `{source}` did not parse: {error}"),
                    ));
                    continue;
                }
            };

            let consumer = declaration
                .name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| demand.consumer.clone());
            let provider = declaration
                .provider
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| demand.provider.clone());

            if !contracts.contains(provider.as_str()) {
                loaded.findings.push(rules::synthetic(
                    "consumer-provider-unmatched",
                    "",
                    provider_message(&consumer, &source, &provider, &contracts),
                ));
                continue;
            }

            if let Ok(text) = String::from_utf8(bytes.clone()) {
                loaded.sources.insert(source.clone(), text);
            }
            loaded.declared.push(Declared {
                consumer,
                provider,
                digest: digest::short(&bytes),
                source,
                demand,
            });
        }
    }

    loaded
        .declared
        .sort_by(|a, b| (&a.consumer, &a.source).cmp(&(&b.consumer, &b.source)));
    loaded.findings.sort();
    loaded.findings.dedup();
    loaded
}

fn provider_message(
    consumer: &str,
    source: &str,
    provider: &str,
    contracts: &BTreeSet<&str>,
) -> String {
    let known = if contracts.is_empty() {
        "brake.toml declares no contracts".to_owned()
    } else {
        format!(
            "declared contracts: {}",
            contracts
                .iter()
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    if provider.trim().is_empty() {
        format!(
            "`{source}` does not name a provider and its `[[consumer]]` entry does not set \
             one, so `{consumer}` guards nothing. Add `provider = \"…\"` ({known})"
        )
    } else {
        format!(
            "`{consumer}` (`{source}`) names provider `{provider}`, which no `[[contract]]` \
             declares, so it was not checked ({known})"
        )
    }
}

/// Expand a `*` glob against the repository, sorted byte-wise.
///
/// `*` matches within one path segment and never across `/`, which is what
/// makes `services/*/pacts/*-payments.json` mean what it looks like it means.
#[must_use]
pub fn expand(repo_root: &Path, pattern: &Path) -> Vec<PathBuf> {
    let pattern = display_path(pattern);
    if !pattern.contains('*') {
        let candidate = repo_root.join(&pattern);
        return if candidate.is_file() {
            vec![PathBuf::from(pattern)]
        } else {
            Vec::new()
        };
    }

    let segments: Vec<&str> = pattern.split('/').filter(|part| !part.is_empty()).collect();
    let mut frontier = vec![PathBuf::new()];
    for (index, segment) in segments.iter().enumerate() {
        let last = index + 1 == segments.len();
        let mut next = Vec::new();
        for prefix in &frontier {
            if !segment.contains('*') {
                let candidate = prefix.join(segment);
                let full = repo_root.join(&candidate);
                if (last && full.is_file()) || (!last && full.is_dir()) {
                    next.push(candidate);
                }
                continue;
            }
            let Ok(entries) = fs::read_dir(repo_root.join(prefix)) else {
                continue;
            };
            let mut here = Vec::new();
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_symlink() || (last && !kind.is_file()) || (!last && !kind.is_dir()) {
                    continue;
                }
                if matches_segment(segment, &name) {
                    here.push(prefix.join(&name));
                }
            }
            here.sort();
            next.extend(here);
        }
        frontier = next;
    }
    frontier.sort();
    frontier.dedup();
    frontier
}

/// `*-payments.json` against `web-checkout-payments.json`.
fn matches_segment(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    let Some((first, rest)) = parts.split_first() else {
        return pattern == name;
    };
    if !name.starts_with(first) {
        return false;
    }
    let mut cursor = &name[first.len()..];
    for (index, part) in rest.iter().enumerate() {
        if index + 1 == rest.len() {
            // The final literal has to land at the end, or `*.json` would
            // match `a.json.bak`.
            return cursor.len() >= part.len() && cursor.ends_with(part);
        }
        if part.is_empty() {
            continue;
        }
        match cursor.find(part) {
            Some(at) => cursor = &cursor[at + part.len()..],
            None => return false,
        }
    }
    true
}

/// Files in the tree that parse as a demand but no `[[consumer]]` declares.
///
/// `paths` restricts the search to a scoped run's files; `None` walks the
/// repository, which is what `analyze` does.
#[must_use]
pub fn undeclared(repo_root: &Path, config: &Config, paths: Option<&[String]>) -> Vec<Finding> {
    let declared: BTreeSet<String> = config
        .consumers
        .iter()
        .flat_map(|consumer| expand(repo_root, &consumer.source))
        .map(|path| display_path(&path))
        .collect();
    let providers: BTreeSet<&str> = config
        .contracts
        .iter()
        .map(|contract| contract.name.as_str())
        .collect();

    let candidates: Vec<String> = match paths {
        Some(paths) => paths
            .iter()
            .filter(|path| looks_like_one(path))
            .cloned()
            .collect(),
        None => crate::init::walk(repo_root, &|name| looks_like_one(name))
            .into_iter()
            .filter_map(|path| path.strip_prefix(repo_root).ok().map(display_path))
            .collect(),
    };

    let mut findings: Vec<Finding> = candidates
        .into_iter()
        .filter(|path| !declared.contains(path))
        .filter_map(|path| {
            let format = super::identify(&repo_root.join(&path))?;
            let bytes = fs::read(repo_root.join(&path)).ok()?;
            let demand = super::ingest(format, &path, &bytes).ok()?;
            // Only for a provider this repository actually gates: a pact for
            // somebody else's API is not this repository's business.
            if !providers.contains(demand.provider.as_str()) {
                return None;
            }
            Some(rules::about_file(
                "consumer-undeclared",
                &path,
                format!(
                    "`{path}` parses as a {} declaration for `{}` but no `[[consumer]]` in \
                     brake.toml declares it, so nothing was verified against it",
                    format_name(format),
                    demand.provider
                ),
            ))
        })
        .collect();
    findings.sort();
    findings.dedup();
    findings
}

fn looks_like_one(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    [".json", ".toml", ".graphql", ".gql"]
        .iter()
        .any(|extension| lowered.ends_with(extension))
}

#[must_use]
pub fn format_name(format: DemandFormat) -> &'static str {
    match format {
        DemandFormat::Pact => "pact",
        DemandFormat::GraphqlOperations => "graphql-operations",
        DemandFormat::Manifest => "manifest",
    }
}

/// The declaration in `config` a path belongs to, if any.
///
/// A path scope that names a pact file selects the contracts that pact
/// constrains, so a hook run on a pact-updating commit verifies the right
/// thing.
#[must_use]
pub fn providers_for_path(repo_root: &Path, config: &Config, path: &str) -> BTreeSet<String> {
    config
        .consumers
        .iter()
        .filter(|consumer| {
            expand(repo_root, &consumer.source)
                .iter()
                .any(|matched| display_path(matched) == path)
        })
        .filter_map(|consumer: &ConsumerConfig| {
            consumer.provider.clone().or_else(|| {
                let bytes = fs::read(repo_root.join(path)).ok()?;
                super::ingest(consumer.format, path, &bytes)
                    .ok()
                    .map(|demand| demand.provider)
            })
        })
        .collect()
}

/// Everything one consumer declared about one contract, bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundConsumer {
    pub consumer: String,
    pub source: String,
    pub usage_index: BTreeMap<EndpointKey, Usages>,
}

impl Declared {
    /// Bind this declaration against a contract, for attribution.
    #[must_use]
    pub fn bind(&self, contract: &crate::Contract) -> BoundConsumer {
        BoundConsumer {
            consumer: self.consumer.clone(),
            source: self.source.clone(),
            usage_index: verify::bind(&self.demand, contract).usage_index,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn repo(files: &[&str]) -> tempfile::TempDir {
        let repo = tempdir().expect("tempdir");
        for path in files {
            let full = repo.path().join(path);
            fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
            fs::write(full, "{}").expect("write");
        }
        repo
    }

    #[test]
    fn a_glob_expands_within_segments_and_sorts() {
        let repo = repo(&[
            "services/checkout/pacts/checkout-payments.json",
            "services/billing/pacts/billing-payments.json",
            "services/billing/pacts/billing-ledger.json",
            "services/billing/notes.txt",
        ]);
        let matched = expand(repo.path(), Path::new("services/*/pacts/*-payments.json"));
        let rendered: Vec<String> = matched.iter().map(|path| display_path(path)).collect();
        assert_eq!(
            rendered,
            vec![
                "services/billing/pacts/billing-payments.json",
                "services/checkout/pacts/checkout-payments.json",
            ],
            "expansion must be byte-sorted and must not cross a segment"
        );
    }

    #[test]
    fn a_literal_source_that_is_absent_expands_to_nothing() {
        let repo = repo(&["pacts/present.json"]);
        assert!(expand(repo.path(), Path::new("pacts/absent.json")).is_empty());
        assert_eq!(
            expand(repo.path(), Path::new("pacts/present.json")).len(),
            1
        );
    }

    #[test]
    fn a_trailing_literal_must_land_at_the_end() {
        assert!(matches_segment("*.json", "a.json"));
        assert!(!matches_segment("*.json", "a.json.bak"));
        assert!(matches_segment(
            "*-payments.json",
            "web-checkout-payments.json"
        ));
        assert!(!matches_segment(
            "*-payments.json",
            "web-checkout-ledger.json"
        ));
    }
}
