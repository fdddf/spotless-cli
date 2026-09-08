//! # spotless-core
//!
//! The pure-Rust heart of Spotless. It knows how to:
//! - load the data-driven cleaning ruleset ([`rules`]),
//! - scan targets to measure reclaimable space ([`scanner`]),
//! - decide whether any given path is safe to remove ([`safety`]),
//! - and remove selected items to the Trash ([`cleaner`]).
//!
//! On top of that it can find developer build artifacts ([`devscan`]),
//! byte-identical duplicates ([`dupes`]), where the disk actually went
//! ([`usage`]), and what an app leaves behind when you drag it to the Trash
//! ([`apps`], [`orphans`]).
//!
//! It has **no** dependency on any GUI toolkit, so every operation can be unit
//! tested in isolation and driven equally well from a terminal.
//!
//! ## Safety contract
//! Nothing in this crate ever performs a permanent delete of its own accord.
//! Removal goes through a [`cleaner::RemovalBackend`], and every path is
//! validated by [`safety::SafetyGuard`] immediately before removal.

// `orphans.rs` sorts with an explicit comparator where clippy would prefer a
// key function. The module is vendored verbatim from the GUI build (see
// `scripts/sync-core.sh`), so the exemption belongs here rather than in a file
// the next sync would overwrite.
#![allow(clippy::unnecessary_sort_by)]

// The two variants exist so the modules below can be shared verbatim with the
// GUI build, which additionally ships a sandboxed App Store variant. The CLI is
// only ever `direct`.
#[cfg(all(feature = "direct", feature = "mas"))]
compile_error!("features `direct` and `mas` are mutually exclusive");
#[cfg(not(any(feature = "direct", feature = "mas")))]
compile_error!("exactly one of the `direct` / `mas` features must be enabled");

pub mod apps;
pub mod capability;
pub mod cleaner;
pub mod devscan;
pub mod dupes;
pub mod model;
pub mod mounts;
pub mod orphans;
pub mod paths;
pub mod rules;
pub mod safety;
pub mod scanner;
pub mod trash;
pub mod usage;

pub use apps::{AppInfo, Leftover, UninstallPlan};
pub use capability::Capability;
pub use devscan::{DevArtifact, DevProgress, DevScanOptions};
pub use dupes::{DupeGroup, DupeOptions};
pub use model::{
    Category, CleanReport, FailedItem, RefusedItem, SafetyTier, ScanItem, ScanReport, ScanTarget,
    Scope, TargetScan,
};
pub use rules::RuleSet;
pub use safety::{RefusalReason, SafetyGuard};
pub use usage::{UsageNode, UsageOptions};

/// The system ruleset, as shipped.
pub const SYSTEM_RULES: &str = include_str!("../rules/00-system.toml");
/// The developer ruleset, as shipped.
pub const DEVELOPER_RULES: &str = include_str!("../rules/10-developer.toml");

/// Errors produced by the core crate.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse ruleset: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("invalid rule: {0}")]
    Rule(String),
}

/// The built-in ruleset: every target Spotless is willing to touch.
///
/// The two TOML files are compiled in rather than read from disk, so a
/// single-file binary carries its own rules and there is nothing to install
/// alongside it. `spotless rules` prints them back out, which is the point of
/// the rules being data.
pub fn builtin_ruleset() -> Result<RuleSet, CoreError> {
    let mut set = RuleSet::from_toml(SYSTEM_RULES)?;
    let dev = RuleSet::from_toml(DEVELOPER_RULES)?;
    set.targets.extend(dev.targets);
    Ok(set)
}

/// Run a full scan of the given ruleset's targets.
pub fn scan(ruleset: &RuleSet) -> ScanReport {
    scanner::scan_targets(&ruleset.targets)
}

#[cfg(test)]
mod integration_tests {
    use super::*;

    #[test]
    fn builtin_ruleset_parses_and_has_targets() {
        let set = builtin_ruleset().unwrap();
        assert!(set.targets.len() > 10, "shipped ruleset looks truncated");
        // Ids are what `--target` selects on, so they must survive the merge.
        assert!(set.targets.iter().any(|t| t.id == "user-caches"));
    }

    #[test]
    fn every_builtin_target_has_a_description() {
        // The product promise is that the user can always see what a target is
        // before cleaning it; an undescribed rule breaks that silently.
        for target in builtin_ruleset().unwrap().targets {
            assert!(
                !target.description.trim().is_empty(),
                "target `{}` ships without a description",
                target.id
            );
        }
    }
}
