//! The scanner: walks each [`ScanTarget`], measuring what could be reclaimed.
//!
//! Scanning never mutates the filesystem. It only reports sizes, so a scan is
//! always safe to run. Permission errors on individual subpaths are collected
//! as warnings rather than aborting the scan.

use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::model::{ScanItem, ScanReport, ScanTarget, Scope, TargetScan};
use crate::paths;

/// Scan a list of targets and produce an aggregate report.
pub fn scan_targets(targets: &[ScanTarget]) -> ScanReport {
    scan_targets_with(targets, |_| {}, &|| false)
}

/// Scan a list of targets, streaming each completed [`TargetScan`] to
/// `on_target` as it finishes, and stopping as soon as `cancel` returns true.
///
/// This is what the UI drives: results appear incrementally rather than after a
/// single blocking call, and the user can stop a long scan. The report then
/// contains only the targets that finished before the cancel (the aggregate is
/// still internally consistent) — a target caught half-measured is dropped
/// rather than reported at whatever it had summed so far, which would read as a
/// real figure.
///
/// `cancel` is checked *inside* each target's walk, not only between targets.
/// Between-targets alone was not enough to feel like cancelling: a single
/// target can be `~/Library/Caches` with a million files under it, and for the
/// twenty seconds that takes, pressing Cancel did nothing at all.
///
/// Targets can legitimately nest — `~/Library/Caches` is a target, and so are
/// half a dozen of its children — so every target is scanned knowing where the
/// *other* roots are, and the more specific one owns the overlap. Without that,
/// the same bytes would be counted once in the broad target and again in the
/// narrow one, and the headline figure would promise space that only exists
/// once. See [`scan_target_excluding`].
pub fn scan_targets_with<F>(
    targets: &[ScanTarget],
    mut on_target: F,
    cancel: &dyn Fn() -> bool,
) -> ScanReport
where
    F: FnMut(&TargetScan),
{
    let roots: Vec<PathBuf> = targets.iter().map(|t| paths::resolve(&t.path)).collect();

    let mut scans = Vec::with_capacity(targets.len());
    let mut total = 0u64;
    for (i, target) in targets.iter().enumerate() {
        if cancel() {
            break;
        }
        // Only roots strictly *inside* this target's own root can take bytes
        // away from it. An ancestor root (the broad target this one sits under)
        // must not: it is the one that yields.
        let nested: Vec<&Path> = roots
            .iter()
            .enumerate()
            .filter(|(j, r)| *j != i && r.starts_with(&roots[i]) && *r != &roots[i])
            .map(|(_, r)| r.as_path())
            .collect();
        let scan = scan_target_excluding_with(target, &nested, cancel);
        // Cancelled mid-walk: what this target summed is a fraction of what is
        // there, so it is dropped rather than published as a size.
        if cancel() {
            break;
        }
        total += scan.total_bytes;
        on_target(&scan);
        scans.push(scan);
    }
    ScanReport {
        targets: scans,
        total_bytes: total,
    }
}

/// Scan a single target on its own, counting everything under it.
///
/// Callers scanning a *set* of targets should go through [`scan_targets_with`],
/// which additionally resolves overlap between them.
pub fn scan_target(target: &ScanTarget) -> TargetScan {
    scan_target_excluding(target, &[])
}

/// Scan `target`, treating every path in `owned_elsewhere` as belonging to
/// another target.
///
/// Each entry is a root nested under this target's own root. An item that *is*
/// one of them is dropped from this target entirely (the other target lists it,
/// under its own name and safety tier); an item that merely *contains* one has
/// those bytes left out of its size. Either way the sum across targets counts
/// every byte exactly once, and cleaning this target still removes everything
/// it reports.
pub fn scan_target_excluding(target: &ScanTarget, owned_elsewhere: &[&Path]) -> TargetScan {
    scan_target_excluding_with(target, owned_elsewhere, &|| false)
}

/// [`scan_target_excluding`], abandoning the walk as soon as `cancel` says so.
///
/// A cancelled scan returns whatever it had measured up to that point. That is
/// deliberately *not* a usable figure — the caller is expected to discard it —
/// but returning it keeps the function total, so the cancel check stays a plain
/// early return rather than an error path threaded through every branch.
pub fn scan_target_excluding_with(
    target: &ScanTarget,
    owned_elsewhere: &[&Path],
    cancel: &dyn Fn() -> bool,
) -> TargetScan {
    let root = paths::resolve(&target.path);
    let mut items = Vec::new();
    let mut warnings = Vec::new();
    let mut total = 0u64;

    if !root.exists() {
        // Not an error — many optional targets (e.g. a package manager the user
        // doesn't have installed) simply won't be present.
        return TargetScan {
            target: target.clone(),
            items,
            total_bytes: 0,
            warnings,
        };
    }

    match target.scope {
        Scope::Contents => {
            // Each direct child of the directory becomes one removable item.
            match std::fs::read_dir(&root) {
                Ok(entries) => {
                    for entry in entries {
                        if cancel() {
                            break;
                        }
                        match entry {
                            Ok(e) => {
                                let path = e.path();
                                if owned_elsewhere.iter().any(|o| *o == path) {
                                    continue;
                                }
                                let is_dir = path.is_dir();
                                let size = dir_size_excluding(
                                    &path,
                                    owned_elsewhere,
                                    &mut warnings,
                                    cancel,
                                );
                                total += size;
                                items.push(ScanItem {
                                    target_id: target.id.clone(),
                                    path,
                                    size_bytes: size,
                                    is_dir,
                                });
                            }
                            Err(e) => warnings.push(format!("read entry failed: {e}")),
                        }
                    }
                }
                Err(e) => warnings.push(format!("cannot read {}: {e}", root.display())),
            }
        }
        Scope::Path => {
            let is_dir = root.is_dir();
            let size = dir_size_excluding(&root, owned_elsewhere, &mut warnings, cancel);
            total += size;
            items.push(ScanItem {
                target_id: target.id.clone(),
                path: root,
                size_bytes: size,
                is_dir,
            });
        }
    }

    TargetScan {
        target: target.clone(),
        items,
        total_bytes: total,
        warnings,
    }
}

/// Recursively compute the on-disk size of a path. Directory entries that can't
/// be read are recorded as warnings and skipped rather than aborting.
pub fn dir_size(path: &Path, warnings: &mut Vec<String>) -> u64 {
    dir_size_excluding(path, &[], warnings, &|| false)
}

/// [`dir_size`], abandoning the walk as soon as `cancel` says so. The partial
/// sum is returned; the caller is expected to throw it away.
pub fn dir_size_with(path: &Path, warnings: &mut Vec<String>, cancel: &dyn Fn() -> bool) -> u64 {
    dir_size_excluding(path, &[], warnings, cancel)
}

/// [`dir_size`], skipping anything at or under one of `excluded`.
///
/// `cancel` is consulted once per entry rather than on a counter: it is an
/// atomic load behind a vtable, which is nothing beside the `stat` this loop
/// already does for every file, and checking it every time is what makes a
/// cancel land within milliseconds instead of within a directory.
fn dir_size_excluding(
    path: &Path,
    excluded: &[&Path],
    warnings: &mut Vec<String>,
    cancel: &dyn Fn() -> bool,
) -> u64 {
    if path.is_file() {
        return path.metadata().map(|m| m.len()).unwrap_or(0);
    }
    let mut total = 0u64;
    let mut walk = WalkDir::new(path).follow_links(false).into_iter();
    loop {
        if cancel() {
            break;
        }
        let entry = match walk.next() {
            None => break,
            Some(Ok(e)) => e,
            Some(Err(e)) => {
                warnings.push(format!("walk error: {e}"));
                continue;
            }
        };
        if !excluded.is_empty() && excluded.iter().any(|x| *x == entry.path()) {
            // Another target owns this subtree; don't descend and don't count.
            if entry.file_type().is_dir() {
                walk.skip_current_dir();
            }
            continue;
        }
        if entry.file_type().is_file() {
            if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Category, SafetyTier};
    use std::fs;
    use std::io::Write;

    fn write_file(path: &Path, bytes: usize) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(path).unwrap();
        f.write_all(&vec![b'x'; bytes]).unwrap();
    }

    fn target(id: &str, path: &str, scope: Scope) -> ScanTarget {
        ScanTarget {
            id: id.into(),
            name: id.into(),
            path: path.into(),
            scope,
            safety: SafetyTier::Safe,
            category: Category::UserCache,
            description: String::new(),
            requires_app_quit: false,
            permanent: false,
            requires: None,
        }
    }

    #[test]
    fn scans_directory_contents_as_items() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("appA/data.bin"), 100);
        write_file(&dir.path().join("appB/blob.bin"), 250);
        write_file(&dir.path().join("loose.txt"), 30);

        let t = target("caches", dir.path().to_str().unwrap(), Scope::Contents);
        let scan = scan_target(&t);

        // Three direct children: appA, appB, loose.txt
        assert_eq!(scan.items.len(), 3);
        assert_eq!(scan.total_bytes, 380);
    }

    #[test]
    fn scans_whole_path_as_single_item() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("sub/a.bin"), 100);
        write_file(&dir.path().join("sub/b.bin"), 100);

        let t = target("dd", dir.path().join("sub").to_str().unwrap(), Scope::Path);
        let scan = scan_target(&t);

        assert_eq!(scan.items.len(), 1);
        assert_eq!(scan.total_bytes, 200);
        assert!(scan.items[0].is_dir);
    }

    #[test]
    fn missing_target_is_empty_not_error() {
        let t = target("nope", "/definitely/not/here/maccleaner", Scope::Contents);
        let scan = scan_target(&t);
        assert_eq!(scan.total_bytes, 0);
        assert!(scan.items.is_empty());
        assert!(scan.warnings.is_empty());
    }

    #[test]
    fn streams_each_completed_target() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("a/x.bin"), 10);
        write_file(&dir.path().join("b/y.bin"), 20);

        let targets = vec![
            target("a", dir.path().join("a").to_str().unwrap(), Scope::Path),
            target("b", dir.path().join("b").to_str().unwrap(), Scope::Path),
        ];

        let mut streamed = Vec::new();
        let report = scan_targets_with(&targets, |t| streamed.push(t.target.id.clone()), &|| false);

        assert_eq!(streamed, vec!["a", "b"]);
        assert_eq!(report.total_bytes, 30);
    }

    #[test]
    fn a_nested_target_is_counted_once_by_the_more_specific_one() {
        // The shape the real ruleset has: `~/Library/Caches` as a broad target
        // with `~/Library/Caches/Homebrew` as its own named one.
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("caches/Homebrew/bottle.tar"), 500);
        write_file(&dir.path().join("caches/otherapp/blob.bin"), 100);

        let targets = vec![
            target(
                "caches",
                dir.path().join("caches").to_str().unwrap(),
                Scope::Contents,
            ),
            target(
                "homebrew",
                dir.path().join("caches/Homebrew").to_str().unwrap(),
                Scope::Contents,
            ),
        ];
        let report = scan_targets_with(&targets, |_| {}, &|| false);

        // The broad target drops the row the specific one owns...
        let broad = &report.targets[0];
        assert_eq!(broad.items.len(), 1);
        assert!(broad.items[0].path.ends_with("otherapp"));
        assert_eq!(broad.total_bytes, 100);
        // ...which still reports its own contents in full.
        assert_eq!(report.targets[1].total_bytes, 500);
        // 600, not 1100.
        assert_eq!(report.total_bytes, 600);
    }

    #[test]
    fn a_partially_overlapping_item_excludes_only_the_owned_subtree() {
        // The nested target is deeper than a direct child, so the child stays —
        // it holds bytes nothing else reports — but shrinks by what the other
        // target owns.
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("home/.gradle/caches/dep.jar"), 700);
        write_file(&dir.path().join("home/.gradle/daemon/log.txt"), 40);

        let targets = vec![
            target(
                "home",
                dir.path().join("home").to_str().unwrap(),
                Scope::Contents,
            ),
            target(
                "gradle-caches",
                dir.path().join("home/.gradle/caches").to_str().unwrap(),
                Scope::Contents,
            ),
        ];
        let report = scan_targets_with(&targets, |_| {}, &|| false);

        assert_eq!(report.targets[0].items.len(), 1);
        assert_eq!(report.targets[0].total_bytes, 40);
        assert_eq!(report.targets[1].total_bytes, 700);
        assert_eq!(report.total_bytes, 740);
    }

    #[test]
    fn an_ancestor_target_never_takes_bytes_from_the_one_inside_it() {
        // The direction matters: scanning the *specific* target must not treat
        // the broad target's root as an exclusion, or it would report nothing.
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("caches/Homebrew/downloads/x.tar"), 300);

        let targets = vec![
            target(
                "homebrew",
                dir.path().join("caches/Homebrew").to_str().unwrap(),
                Scope::Contents,
            ),
            target(
                "caches",
                dir.path().join("caches").to_str().unwrap(),
                Scope::Contents,
            ),
        ];
        let report = scan_targets_with(&targets, |_| {}, &|| false);

        assert_eq!(report.targets[0].total_bytes, 300);
        assert_eq!(report.targets[1].total_bytes, 0);
        assert_eq!(report.total_bytes, 300);
    }

    #[test]
    fn a_sibling_target_that_merely_shares_a_name_prefix_is_not_nested() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("cache/a.bin"), 10);
        write_file(&dir.path().join("cache-old/b.bin"), 20);

        let targets = vec![
            target(
                "cache",
                dir.path().join("cache").to_str().unwrap(),
                Scope::Contents,
            ),
            target(
                "cache-old",
                dir.path().join("cache-old").to_str().unwrap(),
                Scope::Contents,
            ),
        ];
        let report = scan_targets_with(&targets, |_| {}, &|| false);

        assert_eq!(report.total_bytes, 30);
    }

    #[test]
    fn cancellation_stops_early_with_partial_report() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir.path().join("a/x.bin"), 10);
        write_file(&dir.path().join("b/y.bin"), 20);

        let targets = vec![
            target("a", dir.path().join("a").to_str().unwrap(), Scope::Path),
            target("b", dir.path().join("b").to_str().unwrap(), Scope::Path),
        ];

        // Cancel after the first target completes. A Cell lets both the
        // progress closure and the cancel closure observe the same counter.
        let count = std::cell::Cell::new(0);
        let report = scan_targets_with(&targets, |_| count.set(count.get() + 1), &|| {
            count.get() >= 1
        });

        assert_eq!(report.targets.len(), 1);
        assert_eq!(report.targets[0].target.id, "a");
        assert_eq!(report.total_bytes, 10);
    }

    #[test]
    fn cancellation_lands_inside_a_targets_own_walk() {
        // One target, many files under it: cancelling has nowhere to be
        // observed between targets, so this only stops if the walk itself asks.
        let dir = tempfile::tempdir().unwrap();
        for i in 0..400 {
            write_file(&dir.path().join(format!("sub/{i}.bin")), 10);
        }

        let t = target("one", dir.path().to_str().unwrap(), Scope::Path);
        // Cancel once the walk has looked at a handful of entries.
        let seen = std::cell::Cell::new(0);
        let report = scan_targets_with(&[t], |_| {}, &|| {
            seen.set(seen.get() + 1);
            seen.get() > 8
        });

        // The half-measured target is dropped, not reported at a partial size.
        assert!(report.targets.is_empty());
        assert_eq!(report.total_bytes, 0);
        // And it stopped early rather than walking all 400 files.
        assert!(seen.get() < 400, "walk did not stop: {} checks", seen.get());
    }
}
