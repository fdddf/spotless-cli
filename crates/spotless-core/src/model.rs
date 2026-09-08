//! Core domain types shared across the scanner, rules engine, and cleaner.
//!
//! These types are serializable so they can cross the Tauri IPC boundary
//! unchanged and be mirrored by TypeScript types on the frontend.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::capability::Capability;

/// How dangerous it is to remove a target. Surfaced directly in the UI so the
/// user always understands what a given clean will do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SafetyTier {
    /// Regenerated automatically, contains no user data (caches, logs,
    /// DerivedData). Safe to enable by default.
    Safe,
    /// Recoverable but may cost time to rebuild (dev caches, downloads, old
    /// files). Off by default; user opts in.
    #[default]
    Caution,
    /// Advanced/maintenance items. Always gated behind an explicit confirm.
    Expert,
}

/// What part of the target path a clean operates on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Remove the entries inside the directory but keep the directory itself.
    /// This is the correct behavior for cache folders — apps expect the folder
    /// to exist.
    #[default]
    Contents,
    /// Remove the target path (file or directory) entirely.
    Path,
}

/// High-level grouping used to organize targets in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    SystemJunk,
    UserCache,
    Logs,
    Trash,
    Downloads,
    Developer,
    Browser,
    #[default]
    Other,
}

/// A declarative cleaning target loaded from the data-driven ruleset.
///
/// Targets are intentionally *data*, not code, so the full list of what
/// Spotless will ever remove can be reviewed, versioned, and audited.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanTarget {
    /// Stable identifier, e.g. `user-caches`.
    pub id: String,
    /// Human-readable name shown in the UI.
    pub name: String,
    /// Path, possibly starting with `~` for the user's home directory.
    pub path: String,
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub safety: SafetyTier,
    #[serde(default)]
    pub category: Category,
    /// One-line explanation of what this is and why it's safe to remove.
    #[serde(default)]
    pub description: String,
    /// Whether the owning app must be quit before cleaning (e.g. app caches).
    #[serde(default)]
    pub requires_app_quit: bool,
    /// Whether this target's items can only be removed permanently.
    ///
    /// Most targets move to the Trash. A few have nowhere to move *to*: the
    /// Trash itself (re-trashing an item is a no-op), and simulator runtimes,
    /// which live in SIP-protected storage and are removed by `simctl`. Those
    /// set this, and the UI has to say so before the user commits — the
    /// permanent/recoverable distinction is otherwise invisible to them.
    #[serde(default)]
    pub permanent: bool,
    /// A capability this target depends on, if any.
    ///
    /// Targets outside the sandbox's reach — anything in the root domain —
    /// declare it here, and [`RuleSet`](crate::rules::RuleSet) drops them when
    /// the running build lacks the capability. Keeping it on the target means a
    /// rule that only one variant can act on stays visible in the ruleset,
    /// which is the point of the rules being reviewable data.
    #[serde(default)]
    pub requires: Option<Capability>,
}

/// A discovered, removable item produced by scanning a [`ScanTarget`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanItem {
    /// The target this item belongs to.
    pub target_id: String,
    /// Absolute, resolved path on disk.
    pub path: PathBuf,
    /// Total size in bytes (recursive for directories).
    pub size_bytes: u64,
    /// Whether this path is a directory.
    pub is_dir: bool,
}

/// Result of scanning a single target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetScan {
    pub target: ScanTarget,
    pub items: Vec<ScanItem>,
    /// Sum of all item sizes, in bytes.
    pub total_bytes: u64,
    /// Non-fatal issues (e.g. permission denied on a subpath).
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Aggregate result of a full scan across many targets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanReport {
    pub targets: Vec<TargetScan>,
    pub total_bytes: u64,
}

/// Outcome of cleaning a set of items.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CleanReport {
    /// Paths that were successfully removed (moved to Trash).
    pub removed: Vec<PathBuf>,
    /// Paths that were refused by the safety layer, with the reason.
    pub refused: Vec<RefusedItem>,
    /// Paths that failed to remove, with the error message.
    pub failed: Vec<FailedItem>,
    /// Total bytes reclaimed from successful removals.
    pub bytes_reclaimed: u64,
    /// Whether this was a dry run (nothing actually removed).
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefusedItem {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedItem {
    pub path: PathBuf,
    pub error: String,
}
