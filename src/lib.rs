//! `brake` — a brake on breaking API changes.
//!
//! Compares an API contract against its previous version and reports changes
//! that would break a consumer. Hermetic by construction: no network, no
//! toolchain, no running service.
//!
//! The public surface below is the one `design/03-implementation-plan.md` §3
//! fixes, and it is deliberately small — every item in it is a compatibility
//! obligation on a tool whose entire subject is compatibility obligations.
//!
//! ```
//! use brake::{Format, Level};
//!
//! let base = brake::parse(Format::Openapi, "api/openapi.yaml", b"
//! openapi: 3.1.0
//! paths:
//!   /payments:
//!     get:
//!       operationId: listPayments
//!       responses:
//!         \"200\": { description: ok }
//! ").expect("base parses");
//! let head = brake::parse(Format::Openapi, "api/openapi.yaml", b"
//! openapi: 3.1.0
//! paths: {}
//! ").expect("head parses");
//!
//! let changes = brake::compare(&base, &head);
//! let findings = brake::evaluate(&changes, "payments", Level::WireJson);
//! assert!(findings.iter().any(|f| f.rule_id == "endpoint-removed"));
//! ```

pub mod baseline;
pub mod check;
pub mod compare;
pub mod config;
pub mod contract;
pub mod demand;
pub mod init;
pub mod mcp;
pub mod render;
pub mod report;
pub mod rules;

pub use crate::check::{Options, Scope};
pub use crate::compare::Change;
pub use crate::config::{Compatibility as Level, Config};
pub use crate::contract::Contract;
pub use crate::report::Report;
pub use crate::rules::Finding;

/// The version reported by `brake --version`, from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// A contract format brake can ingest.
pub type Format = crate::config::ContractFormat;

/// Ingest one contract artifact from bytes.
///
/// No filesystem, no network. Taking bytes rather than a path is what lets a
/// consumer feed a file it has already read, and what makes every ingest test
/// a string literal rather than a temporary directory.
///
/// `source` is the label that appears in spans, and should be a
/// repository-relative path.
///
/// # Errors
///
/// Returns the ingester's message when the document cannot be parsed, declares
/// an unsupported version, or contains a `$ref` that would require the network
/// or a read outside the source's directory.
pub fn parse(format: Format, source: &str, bytes: &[u8]) -> Result<Contract, String> {
    match format {
        Format::Openapi => contract::openapi::ingest(source, bytes).map_err(|e| e.to_string()),
        Format::Proto => contract::proto::ingest(source, bytes).map_err(|e| e.to_string()),
        Format::Graphql => contract::graphql::ingest(source, bytes).map_err(|e| e.to_string()),
    }
}

/// Compare two contracts, reporting everything that differs.
///
/// Level gating happens in [`evaluate`], not here: this reports what changed
/// and the level decides what is worth saying about it.
#[must_use]
pub fn compare(base: &Contract, head: &Contract) -> Vec<Change> {
    let mut changes = compare::compare_contracts(base, head);
    changes.extend(compare::partial_changes(head));
    changes.sort();
    changes.dedup();
    changes
}

/// Turn changes into findings at a compatibility level.
#[must_use]
pub fn evaluate(changes: &[Change], contract: &str, level: Level) -> Vec<Finding> {
    rules::evaluate(changes, contract, level)
}

/// Severity of a finding.
///
/// Ordered so that `>=` against a threshold is the reporting test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Worth knowing, never worth failing a build over.
    Info,
    /// Should be looked at; not a compatibility break on its own.
    Warning,
    /// A change that breaks a consumer.
    Error,
}

/// The process exit code contract, which is the CI contract.
///
/// The `Findings` / `ToolFailure` split is the one that matters: CI has to be
/// able to distinguish "your API broke" from "the gate is broken", because the
/// correct response differs and conflating them trains a team to ignore both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// No finding at or above the threshold.
    Clean,
    /// At least one finding at or above the threshold.
    Findings,
    /// Baseline unresolvable, source unreadable, or an internal error.
    ToolFailure,
}

impl Verdict {
    /// Map to the documented exit codes: `0`, `1`, `2`.
    #[must_use]
    pub fn exit_code(self) -> i32 {
        match self {
            Verdict::Clean => 0,
            Verdict::Findings => 1,
            Verdict::ToolFailure => 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_match_the_documented_contract() {
        assert_eq!(Verdict::Clean.exit_code(), 0);
        assert_eq!(Verdict::Findings.exit_code(), 1);
        assert_eq!(Verdict::ToolFailure.exit_code(), 2);
    }

    #[test]
    fn severity_orders_for_threshold_comparison() {
        assert!(Severity::Error > Severity::Warning);
        assert!(Severity::Warning > Severity::Info);
    }

    #[test]
    fn version_is_calver_not_a_placeholder() {
        assert!(
            VERSION.starts_with("2026."),
            "unexpected version: {VERSION}"
        );
        assert_ne!(VERSION, "0.0.0");
    }

    #[test]
    fn the_public_api_round_trips_bytes_to_findings() {
        let base = parse(
            Format::Openapi,
            "api/openapi.yaml",
            b"openapi: 3.1.0\npaths:\n  /p:\n    get:\n      operationId: getP\n      responses:\n        \"200\": { description: ok }\n",
        )
        .expect("base");
        let head = parse(
            Format::Openapi,
            "api/openapi.yaml",
            b"openapi: 3.1.0\npaths: {}\n",
        )
        .expect("head");

        let findings = evaluate(&compare(&base, &head), "payments", Level::WireJson);
        assert!(findings.iter().any(|f| f.rule_id == "endpoint-removed"));
        assert!(findings.iter().all(|f| f.contract == "payments"));
    }

    #[test]
    fn parse_reports_a_refused_ref_rather_than_returning_a_partial_contract() {
        let error = parse(
            Format::Openapi,
            "api/openapi.yaml",
            b"openapi: 3.1.0\npaths:\n  /p:\n    get:\n      responses:\n        \"200\":\n          content:\n            application/json:\n              schema:\n                $ref: 'http://example.com/x.yaml'\n",
        )
        .expect_err("a remote ref must not parse");
        assert!(error.contains("network"), "{error}");
    }
}
