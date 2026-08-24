//! `brake consumers` — the inventory.
//!
//! Non-gating and always exit `0`, joining `diff` in that family. It answers
//! the question everybody actually has: who uses this, and what of it?
//!
//! The closing line is not decoration. Without it the inventory reads as a
//! complete census, and it is a list of files somebody remembered to declare.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::json;

use super::{load, policy};
use crate::check::display_path;
use crate::config::{Completeness, Config};
use crate::rules::Finding;

/// What brake knows about who consumes what.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inventory {
    pub contracts: Vec<ContractEntry>,
    /// `consumer-unreachable` and `consumer-provider-unmatched`, reported here
    /// too: an inventory that silently omitted a declaration it could not read
    /// would be exactly the false census this command exists to avoid.
    pub findings: Vec<Finding>,
    pub closed_world: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractEntry {
    pub name: String,
    pub source: String,
    /// Endpoints the contract documents, or `None` when it could not be read.
    pub endpoints: Option<usize>,
    pub covered: usize,
    pub consumers: Vec<ConsumerEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumerEntry {
    pub consumer: String,
    pub source: String,
    pub digest: String,
    pub uses: Vec<EndpointUse>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointUse {
    pub method: String,
    pub path: String,
    pub statuses: Vec<String>,
    pub reads: Vec<String>,
    pub sends: Vec<String>,
}

/// Build the inventory.
///
/// `only_contracts` and `only_consumers` mirror `--contract` and `--consumer`;
/// empty means everything.
#[must_use]
pub fn build(
    repo_root: &Path,
    config: &Config,
    only_contracts: &[String],
    only_consumers: &[String],
) -> Inventory {
    let loaded = load::load(repo_root, config);
    let closed_world = config.consumer_options.completeness == Completeness::ClosedWorld;
    let mut inventory = Inventory {
        findings: loaded.findings.clone(),
        closed_world,
        ..Inventory::default()
    };

    for contract in &config.contracts {
        if !only_contracts.is_empty() && !only_contracts.contains(&contract.name) {
            continue;
        }
        let source = display_path(&contract.source);
        let parsed = std::fs::read(repo_root.join(&contract.source))
            .ok()
            .and_then(|bytes| crate::parse(contract.format, &source, &bytes).ok());

        let mut consumers = Vec::new();
        let mut used = BTreeSet::new();
        for declared in &loaded.declared {
            if declared.provider != contract.name {
                continue;
            }
            if !only_consumers.is_empty() && !only_consumers.contains(&declared.consumer) {
                continue;
            }
            let Some(parsed) = &parsed else {
                continue;
            };
            let bound = declared.bind(parsed);
            let uses = bound
                .usage_index
                .iter()
                .map(|(key, usages)| {
                    used.insert(key.clone());
                    EndpointUse {
                        method: key.method.clone(),
                        path: key.path.clone(),
                        statuses: usages.statuses.iter().cloned().collect(),
                        reads: usages.reads.iter().cloned().collect(),
                        sends: usages.sends.iter().cloned().collect(),
                    }
                })
                .collect();
            consumers.push(ConsumerEntry {
                consumer: declared.consumer.clone(),
                source: declared.source.clone(),
                digest: declared.digest.clone(),
                uses,
            });
        }

        inventory.contracts.push(ContractEntry {
            name: contract.name.clone(),
            source,
            endpoints: parsed.as_ref().map(|parsed| parsed.endpoints.len()),
            covered: used.len(),
            consumers,
        });
    }

    // Under an explicit closed-world declaration the uncovered surface is
    // worth naming; without one it would be a confident statement about
    // consumers brake has never heard of.
    if closed_world {
        for contract in &config.contracts {
            if !only_contracts.is_empty() && !only_contracts.contains(&contract.name) {
                continue;
            }
            let source = display_path(&contract.source);
            let Some(parsed) = std::fs::read(repo_root.join(&contract.source))
                .ok()
                .and_then(|bytes| crate::parse(contract.format, &source, &bytes).ok())
            else {
                continue;
            };
            let bound: Vec<_> = loaded
                .declared
                .iter()
                .filter(|declared| declared.provider == contract.name)
                .map(|declared| declared.bind(&parsed))
                .collect();
            inventory.findings.extend(policy::unused_surface(
                &contract.name,
                parsed.endpoints.keys().cloned(),
                &bound,
            ));
        }
    }

    inventory.findings.sort();
    inventory.findings.dedup();
    inventory
}

/// The human-readable form of `design/05-consumer-demand.md` §9.
#[must_use]
pub fn render_text(inventory: &Inventory) -> String {
    let mut out = String::new();

    for contract in &inventory.contracts {
        out.push_str(&format!("{} — {}\n\n", contract.name, contract.source));

        if contract.consumers.is_empty() {
            out.push_str("  no declared consumer\n\n");
        }
        for consumer in &contract.consumers {
            out.push_str(&format!(
                "  {:<14} {}  {}\n",
                consumer.consumer, consumer.source, consumer.digest
            ));
            for entry in &consumer.uses {
                let statuses = if entry.statuses.is_empty() {
                    String::new()
                } else {
                    entry.statuses.join(",")
                };
                let mut detail = Vec::new();
                if !entry.reads.is_empty() {
                    detail.push(format!("reads: {}", entry.reads.join(", ")));
                }
                if !entry.sends.is_empty() {
                    detail.push(format!("sends: {}", entry.sends.join(", ")));
                }
                out.push_str(&format!(
                    "    {:<4} {:<22} {:<5} {}\n",
                    entry.method,
                    entry.path,
                    statuses,
                    detail.join("  ")
                ));
            }
            out.push('\n');
        }

        match contract.endpoints {
            Some(total) => out.push_str(&format!(
                "  {} of {} endpoint{} have a declared consumer.\n",
                contract.covered,
                total,
                if total == 1 { "" } else { "s" }
            )),
            None => out.push_str("  the contract could not be read, so coverage is unknown.\n"),
        }
        out.push_str("  brake knows about the consumers declared in brake.toml and no others.\n\n");
    }

    if inventory.contracts.is_empty() {
        out.push_str("brake.toml declares no contracts.\n\n");
    }

    for finding in &inventory.findings {
        out.push_str(&format!("{}: {}\n", finding.rule_id, finding.message));
    }
    out
}

/// The machine-readable form.
#[must_use]
pub fn render_json(inventory: &Inventory) -> String {
    let payload = json!({
        "closed_world": inventory.closed_world,
        // Stated rather than implied: a caller that treats this as a census
        // would be wrong, and there is nowhere else to say so in JSON.
        "note": "brake knows about the consumers declared in brake.toml and no others",
        "contracts": inventory
            .contracts
            .iter()
            .map(|contract| json!({
                "name": contract.name,
                "source": contract.source,
                "endpoints": contract.endpoints,
                "endpoints_with_a_consumer": contract.covered,
                "consumers": contract
                    .consumers
                    .iter()
                    .map(|consumer| json!({
                        "consumer": consumer.consumer,
                        "source": consumer.source,
                        "digest": consumer.digest,
                        "uses": consumer
                            .uses
                            .iter()
                            .map(|entry| json!({
                                "method": entry.method,
                                "path": entry.path,
                                "statuses": entry.statuses,
                                "reads": entry.reads,
                                "sends": entry.sends,
                            }))
                            .collect::<Vec<_>>(),
                    }))
                    .collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>(),
        "findings": inventory
            .findings
            .iter()
            .map(|finding| json!({
                "rule": finding.rule_id,
                "message": finding.message,
                "contract": finding.contract,
                "method": finding.method,
                "path": finding.path,
            }))
            .collect::<Vec<_>>(),
    });
    format!("{payload}\n")
}
