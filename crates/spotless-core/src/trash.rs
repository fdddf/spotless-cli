//! The Trash — accounting for what Spotless has put there, and emptying it.
//!
//! Everything the cleaner removes goes to the Trash rather than being deleted,
//! which is the recoverable behaviour the product is built around. The cost is
//! that disk space is not actually returned until the Trash is emptied: a user
//! who cleans 11 GB and then checks "About This Mac" sees no change, and
//! reasonably concludes nothing happened. This module closes that gap.
//!
//! Emptying is a permanent delete: it is deliberately not routed through
//! [`crate::cleaner`]'s Trash backend, and is never invoked as part of a clean.
//! It happens only when the user asks for it, on items they have already seen
//! listed in Finder.
//!
//! Reading and emptying the Trash directly requires the app to run outside the
//! App Sandbox, which Spotless does (see `docs/release.md`).

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::scanner::dir_size;

/// What is sitting in the Trash right now.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TrashSummary {
    /// Total bytes across every trash location.
    pub bytes: u64,
    /// Number of top-level entries (what Finder shows as rows), not files.
    pub items: usize,
}

/// The outcome of emptying. Mirrors the shape of the cleaner's report so the UI
/// can render both the same way.
#[derive(Debug, Clone, Default, Serialize)]
pub struct EmptyReport {
    /// Top-level entries that were removed.
    pub removed: usize,
    /// Entries that could not be removed, with the reason.
    pub failed: Vec<(String, String)>,
    /// Bytes freed, measured before removal.
    pub bytes_reclaimed: u64,
}

/// Every trash directory that belongs to the current user.
///
/// macOS keeps one per volume: `~/.Trash` for the boot disk, and
/// `<volume>/.Trashes/<uid>` for anything else mounted. A cleaner that only
/// looked at the home one would under-report for users who trashed files on an
/// external disk — which is exactly where the multi-GB items tend to live.
fn trash_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = crate::paths::home_dir() {
        let home_trash = home.join(".Trash");
        if home_trash.is_dir() {
            dirs.push(home_trash);
        }
    }
    // SAFETY: `getuid` is always safe — it reads a process property and cannot
    // fail or touch memory we own.
    let uid = unsafe { libc::getuid() };
    if let Ok(volumes) = std::fs::read_dir("/Volumes") {
        for entry in volumes.flatten() {
            let per_user = entry.path().join(".Trashes").join(uid.to_string());
            if per_user.is_dir() {
                dirs.push(per_user);
            }
        }
    }
    dirs
}

/// Top-level entries across all trash directories, as (path, size) pairs.
///
/// Sizes are measured here so [`empty`] can report what it freed without having
/// to stat paths it has already unlinked.
fn entries() -> Vec<(PathBuf, u64)> {
    let mut out = Vec::new();
    for dir in trash_dirs() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let path = entry.path();
            // `.DS_Store` is Finder's own bookkeeping for the Trash window, not
            // a trashed item; removing it would be harmless but it would make
            // the item count disagree with what the user sees in Finder.
            if path.file_name().is_some_and(|n| n == ".DS_Store") {
                continue;
            }
            let mut warnings = Vec::new();
            let size = dir_size(&path, &mut warnings);
            out.push((path, size));
        }
    }
    out
}

/// Measure what the Trash currently holds.
pub fn summary() -> TrashSummary {
    let items = entries();
    TrashSummary {
        bytes: items.iter().map(|(_, size)| size).sum(),
        items: items.len(),
    }
}

/// Permanently delete everything in the Trash.
///
/// A failure on one entry (a file locked by a running process, say) is recorded
/// and the rest still go — a single stubborn item should not leave the user with
/// their space still consumed and no explanation.
pub fn empty() -> EmptyReport {
    let mut report = EmptyReport::default();
    for (path, size) in entries() {
        match remove(&path) {
            Ok(()) => {
                report.removed += 1;
                report.bytes_reclaimed += size;
            }
            Err(e) => report.failed.push((path.display().to_string(), e)),
        }
    }
    report
}

/// Remove one trash entry, directory or file.
fn remove(path: &Path) -> Result<(), String> {
    // `symlink_metadata` rather than `is_dir`: a symlink pointing at a directory
    // must be unlinked, not walked into and recursively deleted.
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    let result = if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    result.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn removes_a_directory_tree_and_a_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("folder");
        fs::create_dir_all(dir.join("nested")).unwrap();
        fs::write(dir.join("nested/a.txt"), b"hello").unwrap();
        let file = tmp.path().join("b.txt");
        fs::write(&file, b"world").unwrap();

        assert!(remove(&dir).is_ok());
        assert!(remove(&file).is_ok());
        assert!(!dir.exists());
        assert!(!file.exists());
    }

    #[test]
    fn unlinks_a_symlink_without_touching_its_target() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("keep.txt"), b"keep").unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert!(remove(&link).is_ok());
        assert!(!link.exists());
        // The link is gone; what it pointed at must not be.
        assert!(target.join("keep.txt").exists());
    }

    #[test]
    fn removing_a_missing_path_reports_rather_than_panicking() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(remove(&tmp.path().join("nope")).is_err());
    }
}
