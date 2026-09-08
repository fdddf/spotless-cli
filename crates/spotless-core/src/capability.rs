//! What this build is allowed to do.
//!
//! Spotless ships as two variants — a notarized Developer ID app that runs
//! outside the sandbox, and a sandboxed Mac App Store app. Rather than spraying
//! `#[cfg(feature = "mas")]` across the codebase and the UI, the difference is
//! expressed once, here, as a set of [`Capability`] flags that the rest of the
//! app (including the frontend, via a Tauri command) reads at runtime.
//!
//! Two rules keep this honest:
//!
//! 1. **This module is the only place that branches on the variant.** Everything
//!    else asks [`has`]. The one exception is code that must not be *compiled*
//!    into the App Store binary at all — the SMC and IOAccelerator bindings —
//!    because shipping those symbols is an App Store review risk in itself.
//! 2. **The frontend never hardcodes the variant.** It fetches this set and
//!    hides UI accordingly, so a capability moving between variants needs no
//!    frontend change.
//!
//! See `docs/capability-gate-plan.md` for the full design and
//! `docs/app-store-feasibility.md` for why each capability lands where it does.

use serde::{Deserialize, Serialize};

/// A thing this build may or may not be able to do.
///
/// The serialized (kebab-case) form is a wire contract with the frontend's
/// `Capability` union in `src/lib/capabilities.ts`; [`tests::wire_format_is_stable`]
/// pins it so the two cannot drift silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    /// Clean cache directories under the root domain (`/Library/Caches`).
    /// The sandbox confines us to the user's home.
    SystemCaches,
    /// Delete Xcode Simulator runtimes. They live under
    /// `/Library/Developer/CoreSimulator` and are managed via `simctl`.
    SimulatorRuntimes,
    /// Run system maintenance actions such as rebuilding the LaunchServices
    /// database, which shells out to `lsregister`.
    MaintenanceActions,
    /// Signal other applications to quit. The sandbox forbids sending signals
    /// to processes that are not our own descendants.
    ProcessControl,
    /// Read fan speed, SMC temperatures and GPU utilisation. These go through
    /// IOKit user clients that the sandbox denies — and that App Store review
    /// treats as private API use.
    HardwareSensors,
    /// Detect and guide the user through granting Full Disk Access. The concept
    /// does not exist for a sandboxed app, which asks for directory access
    /// through Powerbox instead.
    FullDiskAccess,
    /// Manage login items in the root domain (`/Library/LaunchAgents`). The
    /// per-user `~/Library/LaunchAgents` is available to both variants.
    SystemLoginItems,
    /// Enumerate other processes — what the Overview's "top processes" card
    /// is built on.
    ///
    /// Distinct from [`Capability::ProcessControl`], which is about *signalling*
    /// a process: the sandbox denies both, but the Developer ID build can lose
    /// the ability to quit an app while still listing it, so a single flag
    /// would gate the wrong thing. Measured: `sysinfo`'s process list comes
    /// back empty in a sandboxed build.
    ProcessEnumeration,
    /// Reach any path the user can, without asking first.
    ///
    /// The Developer ID build reads the whole disk once the user grants Full
    /// Disk Access. The sandboxed build cannot: every directory it touches has
    /// to be handed to it through a Powerbox open panel and persisted as a
    /// security-scoped bookmark (see `grants` in the Tauri layer). The UI keys
    /// its whole access story off this one flag — absent means "show the grant
    /// flow and gate scans on it".
    UnrestrictedFileAccess,
    /// Enumerate and empty `~/.Trash`.
    ///
    /// Unreachable in the sandbox, and not for want of the right grant: macOS
    /// omits the Trash from the Powerbox open panel altogether, so the user has
    /// no way to select it and no grant for it can exist. Measured twice —
    /// denied under a home-folder grant, then absent from the panel when asked
    /// for directly (`docs/app-store-feasibility.md` §3.6).
    ///
    /// The first measurement alone would have been too weak to conclude this:
    /// `~/Library/Safari` is also denied under a home grant, yet opens fine
    /// when the user picks it directly. "Denied" and "ungrantable" are
    /// different findings and only the second one closes a feature.
    TrashContents,
    /// Ask for an administrator password and act as root — what uninstalling
    /// an app's `/Library` leftovers (root-owned support files, launch daemons,
    /// privileged helpers) requires.
    ///
    /// The Developer ID build escalates through an authorization prompt. A
    /// sandboxed app cannot: the sandbox denies the escalation itself, and
    /// `/Library` is not grantable through Powerbox in any useful form. Absent
    /// means the uninstaller confines itself to `~/Library` and never lists a
    /// leftover it could not remove.
    AdminEscalation,
    /// Ask Finder to move an item to the Trash, over Apple Events.
    ///
    /// The uninstaller's first choice for an `.app` bundle and for a root-owned
    /// `/Library` leftover, because Finder clears two walls that nothing else
    /// does: it is exempt from the App Management TCC check that stops every
    /// other process from deleting another app's bundle, and when the item
    /// really does need an administrator it asks with its own panel — the one
    /// that offers Touch ID, rather than the password-only dialog
    /// [`Capability::AdminEscalation`] can produce. See `findertrash` in the
    /// Tauri layer.
    ///
    /// Absent from the sandboxed build: it carries no
    /// `com.apple.security.automation.apple-events` entitlement, and per the
    /// note in `entitlements.mas.plist` it should not start carrying one.
    FinderRemoval,
    /// Render the menu-bar popover with the transparent, rounded vibrancy
    /// effect. Backed by Tauri's `macos-private-api`, which uses private AppKit
    /// interfaces and cannot ship to the App Store.
    Vibrancy,
}

/// Every capability that exists, regardless of variant. Used for exhaustiveness
/// checks in tests and as the `direct` build's set.
pub const ALL: &[Capability] = &[
    Capability::SystemCaches,
    Capability::SimulatorRuntimes,
    Capability::MaintenanceActions,
    Capability::ProcessControl,
    Capability::HardwareSensors,
    Capability::FullDiskAccess,
    Capability::SystemLoginItems,
    Capability::ProcessEnumeration,
    Capability::UnrestrictedFileAccess,
    Capability::TrashContents,
    Capability::AdminEscalation,
    Capability::FinderRemoval,
    Capability::Vibrancy,
];

/// The capabilities this build actually has.
///
/// A function rather than a constant on purpose: the App Store set is expected
/// to grow once the sandbox spike settles which of these survive in practice
/// (see `docs/app-store-feasibility.md` §6), and this is the single line that
/// will change when it does.
#[cfg(feature = "direct")]
pub fn enabled() -> &'static [Capability] {
    // The unsandboxed build can do everything; that is the point of shipping it.
    ALL
}

/// The capabilities this build actually has. See the `direct` variant above.
#[cfg(feature = "mas")]
pub fn enabled() -> &'static [Capability] {
    // Conservatively empty. Some of these — `ProcessControl` most plausibly —
    // may turn out to work under the sandbox, but nothing goes back in without
    // being measured first: a capability that is advertised and then fails at
    // runtime is worse than one that was never offered.
    &[]
}

/// Whether this build has `capability`.
pub fn has(capability: Capability) -> bool {
    enabled().contains(&capability)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_is_exhaustive() {
        // A new variant added to the enum without being listed in ALL would
        // silently be missing from the `direct` build. Match exhaustively so
        // the compiler forces this test to be updated alongside the enum.
        for capability in ALL {
            match capability {
                Capability::SystemCaches
                | Capability::SimulatorRuntimes
                | Capability::MaintenanceActions
                | Capability::ProcessControl
                | Capability::HardwareSensors
                | Capability::FullDiskAccess
                | Capability::SystemLoginItems
                | Capability::ProcessEnumeration
                | Capability::UnrestrictedFileAccess
                | Capability::TrashContents
                | Capability::AdminEscalation
                | Capability::FinderRemoval
                | Capability::Vibrancy => {}
            }
        }
        assert_eq!(ALL.len(), 13, "ALL must list every Capability variant");
    }

    #[test]
    fn all_has_no_duplicates() {
        let mut seen = ALL.to_vec();
        seen.sort_by_key(|c| format!("{c:?}"));
        seen.dedup();
        assert_eq!(seen.len(), ALL.len());
    }

    #[cfg(feature = "direct")]
    #[test]
    fn direct_build_has_everything() {
        assert_eq!(enabled(), ALL);
        assert!(has(Capability::HardwareSensors));
        assert!(has(Capability::Vibrancy));
    }

    #[cfg(feature = "mas")]
    #[test]
    fn mas_build_has_nothing_yet() {
        assert!(enabled().is_empty());
        assert!(!has(Capability::HardwareSensors));
        assert!(!has(Capability::Vibrancy));
    }

    #[test]
    fn wire_format_is_stable() {
        // These strings are consumed verbatim by src/lib/capabilities.ts.
        // Changing one is a breaking change to the frontend, so it has to be a
        // deliberate edit here rather than a side effect of renaming a variant.
        let pairs = [
            (Capability::SystemCaches, "system-caches"),
            (Capability::SimulatorRuntimes, "simulator-runtimes"),
            (Capability::MaintenanceActions, "maintenance-actions"),
            (Capability::ProcessControl, "process-control"),
            (Capability::HardwareSensors, "hardware-sensors"),
            (Capability::FullDiskAccess, "full-disk-access"),
            (Capability::SystemLoginItems, "system-login-items"),
            (Capability::ProcessEnumeration, "process-enumeration"),
            (Capability::UnrestrictedFileAccess, "unrestricted-file-access"),
            (Capability::TrashContents, "trash-contents"),
            (Capability::AdminEscalation, "admin-escalation"),
            (Capability::FinderRemoval, "finder-removal"),
            (Capability::Vibrancy, "vibrancy"),
        ];
        assert_eq!(pairs.len(), ALL.len());
        for (capability, wire) in pairs {
            assert_eq!(
                serde_json::to_string(&capability).unwrap(),
                format!("\"{wire}\"")
            );
            assert_eq!(
                serde_json::from_str::<Capability>(&format!("\"{wire}\"")).unwrap(),
                capability
            );
        }
    }
}
