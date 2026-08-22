//! `brake` — a brake on breaking API changes.
//!
//! Compares an API contract against its previous version and reports changes
//! that would break a consumer. Hermetic by construction: no network, no
//! toolchain, no running service.
//!
//! Nothing is implemented yet. See `design/` for the specification and
//! `design/03-implementation-plan.md` for the build order. The public API this
//! crate will expose is fixed in that document's §3 and is deliberately small,
//! because every item in it is a compatibility obligation on a tool whose
//! entire subject is compatibility obligations.

pub mod baseline;
pub mod check;
pub mod compare;
pub mod config;
pub mod contract;
pub mod render;
pub mod report;
pub mod rules;

/// The version reported by `brake --version`, from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

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
}
