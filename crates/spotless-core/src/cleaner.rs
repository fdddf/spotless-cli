//! The cleaner: removes selected items, but only after the safety layer clears
//! each path, and only ever by moving to the Trash.
//!
//! Deletion is abstracted behind the [`RemovalBackend`] trait so that:
//! - the core crate has no hard dependency on a platform trash implementation,
//! - tests can assert on what *would* be removed without touching real files.
//!
//! The Tauri layer supplies a [`RemovalBackend`] backed by the `trash` crate.

use std::path::{Path, PathBuf};

use crate::model::{CleanReport, FailedItem, RefusedItem, ScanItem};
use crate::safety::SafetyGuard;
use crate::scanner;

/// A backend that performs the actual removal of a path. Implementations must
/// move to the Trash (or an equivalent recoverable location) — never a
/// permanent, unrecoverable delete.
pub trait RemovalBackend {
    /// Move `path` to the Trash. Returns an error string on failure.
    fn remove(&self, path: &Path) -> Result<(), String>;
}

/// A backend used for dry runs and tests: records requested removals without
/// touching the filesystem.
#[derive(Debug, Default)]
pub struct RecordingBackend {
    pub removed: std::cell::RefCell<Vec<PathBuf>>,
}

impl RemovalBackend for RecordingBackend {
    fn remove(&self, path: &Path) -> Result<(), String> {
        self.removed.borrow_mut().push(path.to_path_buf());
        Ok(())
    }
}

/// Clean a set of scanned items.
///
/// Every path is re-validated by `guard` immediately before removal — the scan
/// result is treated as untrusted input, so a stale or tampered path cannot
/// bypass the guardrails. When `dry_run` is true, nothing is removed and the
/// report lists what *would* have been reclaimed.
pub fn clean_items(
    items: &[ScanItem],
    guard: &SafetyGuard,
    backend: &dyn RemovalBackend,
    dry_run: bool,
) -> CleanReport {
    let mut report = CleanReport {
        dry_run,
        ..Default::default()
    };

    for item in items {
        match guard.validate(&item.path) {
            Err(reason) => report.refused.push(RefusedItem {
                path: item.path.clone(),
                reason: reason.to_string(),
            }),
            Ok(_canonical) => {
                if dry_run {
                    report.removed.push(item.path.clone());
                    report.bytes_reclaimed += item.size_bytes;
                    continue;
                }
                match backend.remove(&item.path) {
                    Ok(()) => {
                        report.removed.push(item.path.clone());
                        report.bytes_reclaimed += item.size_bytes;
                    }
                    Err(e) => report.failed.push(FailedItem {
                        path: item.path.clone(),
                        error: e,
                    }),
                }
            }
        }
    }

    report
}

/// Convenience: recompute an item's size just before cleaning, so the reported
/// reclaimed bytes reflect the current on-disk state rather than a stale scan.
pub fn fresh_size(path: &Path) -> u64 {
    let mut warnings = Vec::new();
    scanner::dir_size(path, &mut warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ScanItem;
    use crate::safety::SafetyGuard;

    fn guard() -> SafetyGuard {
        SafetyGuard::with_roots(
            vec![PathBuf::from("/System")],
            vec![PathBuf::from("/approved")],
        )
    }

    fn item(path: &str, size: u64) -> ScanItem {
        ScanItem {
            target_id: "t".into(),
            path: PathBuf::from(path),
            size_bytes: size,
            is_dir: false,
        }
    }

    #[test]
    fn dry_run_records_but_removes_nothing() {
        let backend = RecordingBackend::default();
        let items = vec![item("/approved/a", 10), item("/approved/b", 20)];
        let report = clean_items(&items, &guard(), &backend, true);

        assert!(report.dry_run);
        assert_eq!(report.removed.len(), 2);
        assert_eq!(report.bytes_reclaimed, 30);
        // Backend never invoked during a dry run.
        assert!(backend.removed.borrow().is_empty());
    }

    #[test]
    fn refuses_unsafe_paths_and_keeps_going() {
        let backend = RecordingBackend::default();
        let items = vec![
            item("/System/important", 999), // refused
            item("/approved/ok", 40),       // allowed
        ];
        let report = clean_items(&items, &guard(), &backend, false);

        assert_eq!(report.refused.len(), 1);
        assert_eq!(report.removed.len(), 1);
        assert_eq!(report.bytes_reclaimed, 40);
        assert_eq!(backend.removed.borrow().len(), 1);
        assert_eq!(backend.removed.borrow()[0], PathBuf::from("/approved/ok"));
    }

    #[test]
    fn real_removal_invokes_backend() {
        let backend = RecordingBackend::default();
        let items = vec![item("/approved/x", 5)];
        let report = clean_items(&items, &guard(), &backend, false);

        assert_eq!(report.removed.len(), 1);
        assert_eq!(backend.removed.borrow().len(), 1);
    }
}
