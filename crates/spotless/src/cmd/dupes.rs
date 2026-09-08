//! `spotless dupes` — byte-identical files, and optionally pruning them.
//!
//! Every group reported here has been confirmed byte-for-byte, not by hash
//! alone. A duplicate finder that acts on a hash collision deletes real data,
//! so the extra read is not optional.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use spotless_core::cleaner;
use spotless_core::dupes::{self, DupeOptions};
use spotless_core::{DupeGroup, SafetyGuard, ScanItem};

use crate::backend::TrashBackend;
use crate::cli::DupesArgs;
use crate::ui;

pub fn run(args: &DupesArgs, json: bool) -> Result<()> {
    let root = args
        .path
        .clone()
        .unwrap_or_else(|| PathBuf::from("."))
        .canonicalize()
        .context("cannot read that folder")?;

    if !json {
        ui::status(&format!("Comparing files under {}…", root.display()));
    }
    let groups = dupes::find_duplicates(
        &root,
        DupeOptions {
            min_bytes: args.min_size,
        },
    );

    if json {
        println!("{}", serde_json::to_string_pretty(&groups)?);
        return Ok(());
    }

    if groups.is_empty() {
        println!(
            "{}",
            ui::green(&format!(
                "No duplicates over {} under {}.",
                ui::bytes(args.min_size),
                root.display()
            ))
        );
        return Ok(());
    }

    let reclaimable: u64 = groups.iter().map(|g| g.reclaimable_bytes).sum();
    print_groups(&groups, args.limit, &root);
    println!(
        "  {} reclaimable across {}",
        ui::bold(&ui::bytes(reclaimable)),
        ui::count(groups.len(), "group", "groups")
    );

    if !args.prune {
        println!(
            "  {}",
            ui::dim("Add --prune to keep one copy of each group and trash the rest.")
        );
        println!();
        return Ok(());
    }

    println!();
    let doomed = prune_list(&groups);
    println!(
        "  {} would keep {} and trash {}.",
        ui::bold("--prune"),
        ui::count(groups.len(), "file", "files"),
        ui::count(doomed.len(), "copy", "copies")
    );
    if !args.yes
        && !ui::confirm(&format!(
            "Move {} ({}) to the Trash?",
            ui::count(doomed.len(), "extra copy", "extra copies"),
            ui::bytes(reclaimable)
        ))
    {
        println!("{}", ui::dim("Nothing was removed."));
        return Ok(());
    }

    let report = prune(&doomed, &root);
    println!(
        "{} {} reclaimed from {}, moved to the Trash.",
        ui::green("✓"),
        ui::bold(&ui::bytes(report.bytes_reclaimed)),
        ui::count(report.removed.len(), "copy", "copies")
    );
    for failed in &report.failed {
        println!(
            "  {} {} — {}",
            ui::red("failed"),
            ui::truncate_path(&failed.path.display().to_string(), 52),
            failed.error
        );
    }
    Ok(())
}

fn print_groups(groups: &[DupeGroup], limit: usize, root: &Path) {
    println!();
    for group in groups.iter().take(limit) {
        println!(
            "  {} × {}  {}",
            ui::bold(&ui::bytes(group.size_bytes)),
            group.files.len(),
            ui::dim(&format!(
                "({} reclaimable)",
                ui::bytes(group.reclaimable_bytes)
            ))
        );
        let keep = keeper(group);
        for file in &group.files {
            let shown = file
                .path
                .strip_prefix(root)
                .unwrap_or(&file.path)
                .display()
                .to_string();
            let marker = if file.path == keep {
                ui::green("keep")
            } else {
                ui::dim("copy")
            };
            println!("    {} {}", marker, ui::truncate_path(&shown, 64));
        }
        println!();
    }
    if groups.len() > limit {
        println!(
            "  {}",
            ui::dim(&format!(
                "… and {} more groups (--limit to show them)",
                groups.len() - limit
            ))
        );
        println!();
    }
}

/// The copy `--prune` keeps: the shortest path, ties broken alphabetically.
///
/// Shortest wins because the extra copies are the ones that picked up a
/// `foo (2).pdf` or landed a folder deeper. Ties are broken deterministically
/// so two runs over the same folder keep the same file.
fn keeper(group: &DupeGroup) -> PathBuf {
    group
        .files
        .iter()
        .min_by_key(|f| {
            let path = f.path.display().to_string();
            (path.len(), path.clone())
        })
        .map(|f| f.path.clone())
        .unwrap_or_default()
}

/// Every file `--prune` would remove: all but the keeper of each group.
fn prune_list(groups: &[DupeGroup]) -> Vec<(PathBuf, u64)> {
    let mut doomed = Vec::new();
    for group in groups {
        let keep = keeper(group);
        for file in &group.files {
            if file.path != keep {
                doomed.push((file.path.clone(), file.size_bytes));
            }
        }
    }
    doomed
}

/// Trash the extra copies.
///
/// The guard approves nothing by default; each path is approved individually,
/// and only after it is confirmed to still be a regular file inside the folder
/// the user pointed at. Symlinks are excluded on purpose — a link that resolves
/// somewhere outside `root` would otherwise carry the approval with it.
fn prune(doomed: &[(PathBuf, u64)], root: &Path) -> spotless_core::CleanReport {
    let mut guard = SafetyGuard::system_only();
    let mut items = Vec::new();
    for (path, size) in doomed {
        let inside = path.starts_with(root);
        let is_file = std::fs::symlink_metadata(path)
            .map(|m| m.is_file())
            .unwrap_or(false);
        if !inside || !is_file {
            continue;
        }
        guard.approve_root(path.clone());
        items.push(ScanItem {
            target_id: "duplicates".into(),
            path: path.clone(),
            size_bytes: *size,
            is_dir: false,
        });
    }
    cleaner::clean_items(&items, &guard, &TrashBackend, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use spotless_core::dupes::DupeFile;

    fn group(paths: &[&str]) -> DupeGroup {
        DupeGroup {
            size_bytes: 10,
            reclaimable_bytes: 10 * (paths.len() as u64 - 1),
            files: paths
                .iter()
                .map(|p| DupeFile {
                    path: PathBuf::from(p),
                    size_bytes: 10,
                })
                .collect(),
        }
    }

    #[test]
    fn keeps_the_shortest_path() {
        let g = group(&["/a/photo (2).jpg", "/a/photo.jpg", "/a/b/photo.jpg"]);
        assert_eq!(keeper(&g), PathBuf::from("/a/photo.jpg"));
    }

    #[test]
    fn keeper_is_stable_when_lengths_tie() {
        // Same length, so the tie-break decides — and must decide the same way
        // every run, or two prunes of one folder keep different files.
        let g = group(&["/a/bbb.jpg", "/a/aaa.jpg"]);
        assert_eq!(keeper(&g), PathBuf::from("/a/aaa.jpg"));
    }

    #[test]
    fn prune_list_spares_exactly_one_per_group() {
        let groups = vec![
            group(&["/a/x.jpg", "/a/y.jpg", "/a/z.jpg"]),
            group(&["/b/1", "/b/22"]),
        ];
        let doomed = prune_list(&groups);
        assert_eq!(doomed.len(), 3);
        assert!(!doomed.iter().any(|(p, _)| p == &PathBuf::from("/a/x.jpg")));
        assert!(!doomed.iter().any(|(p, _)| p == &PathBuf::from("/b/1")));
    }
}
