//! `spotless dev` — the developer-junk mode.
//!
//! `node_modules`, `target/`, `DerivedData`, `build/`, package-manager caches:
//! directories that are large, regenerable, and invisible to every general
//! cleaner. This is the command most people will run first.

use std::io::{IsTerminal, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use spotless_core::cleaner;
use spotless_core::devscan::{self, DevScanOptions};
use spotless_core::{DevArtifact, SafetyGuard, ScanItem};

use crate::backend::TrashBackend;
use crate::cli::DevArgs;
use crate::ui;

pub fn run(args: &DevArgs, json: bool) -> Result<()> {
    let root = match &args.path {
        Some(path) => path.clone(),
        None => spotless_core::paths::home_dir().context("cannot locate your home directory")?,
    };
    let root = root
        .canonicalize()
        .with_context(|| format!("cannot read {}", root.display()))?;

    let found = scan(&root, args.depth, json);
    let artifacts = filter(found, args);

    if json {
        println!("{}", serde_json::to_string_pretty(&artifacts)?);
        return Ok(());
    }

    if artifacts.is_empty() {
        println!(
            "{}",
            ui::green(&format!("No build artifacts under {}.", root.display()))
        );
        return Ok(());
    }

    print_table(&artifacts, args.limit, &root);

    if !args.clean {
        println!(
            "  {}",
            ui::dim("Add --clean to remove these (they rebuild on the next build).")
        );
        println!();
        return Ok(());
    }

    let total: u64 = artifacts.iter().map(|a| a.size_bytes).sum();
    if !args.yes
        && !ui::confirm(&format!(
            "Move {} of build artifacts to the Trash?",
            ui::bold(&ui::bytes(total))
        ))
    {
        println!("{}", ui::dim("Nothing was removed."));
        return Ok(());
    }

    let report = clean(&artifacts);
    println!(
        "{} {} reclaimed from {}, moved to the Trash.",
        ui::green("✓"),
        ui::bold(&ui::bytes(report.bytes_reclaimed)),
        ui::count(report.removed.len(), "artifact", "artifacts")
    );
    for refused in &report.refused {
        println!(
            "  {} {} — {}",
            ui::yellow("skipped"),
            ui::truncate_path(&refused.path.display().to_string(), 52),
            refused.reason
        );
    }
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

/// Walk `root`, showing live counters while it goes.
fn scan(root: &Path, depth: usize, quiet: bool) -> Vec<DevArtifact> {
    let live = !quiet && std::io::stderr().is_terminal();
    let opts = DevScanOptions { max_depth: depth };

    let artifacts = devscan::scan_dev_artifacts_with(
        root,
        opts,
        &|_| {},
        &|progress| {
            if live {
                let mut err = std::io::stderr();
                let _ = write!(
                    err,
                    "\r\x1b[2K{} folders · {} found · {} — {}",
                    progress.dirs_scanned,
                    progress.found,
                    ui::bytes(progress.bytes_found),
                    ui::truncate_path(&progress.current, 40)
                );
                let _ = err.flush();
            }
        },
        &|| false,
    );

    if live {
        eprint!("\r\x1b[2K");
    }
    artifacts
}

/// Apply the size / age / toolchain filters.
fn filter(artifacts: Vec<DevArtifact>, args: &DevArgs) -> Vec<DevArtifact> {
    let cutoff = args.older_than.map(|days| {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        now.saturating_sub(days * 86_400)
    });

    artifacts
        .into_iter()
        .filter(|a| args.min_size.is_none_or(|min| a.size_bytes >= min))
        .filter(|a| {
            args.tool
                .as_ref()
                .is_none_or(|tool| a.tool.to_lowercase().contains(&tool.to_lowercase()))
        })
        .filter(|a| match cutoff {
            // An artifact whose mtime cannot be read is kept: not knowing how
            // old something is is not evidence that it is new.
            Some(cutoff) => a.modified_secs.is_none_or(|m| m <= cutoff),
            None => true,
        })
        .collect()
}

fn print_table(artifacts: &[DevArtifact], limit: usize, root: &std::path::Path) {
    let total: u64 = artifacts.iter().map(|a| a.size_bytes).sum();
    let largest = artifacts.first().map(|a| a.size_bytes).unwrap_or(0);

    println!();
    println!(
        "  {}  {}  {}  {}",
        ui::rpad(&ui::dim("SIZE"), 9),
        ui::lpad(&ui::dim("TOOL"), 10),
        ui::lpad(&ui::dim(""), 8),
        ui::dim("PATH")
    );

    for artifact in artifacts.iter().take(limit) {
        // Paths are shown relative to the scan root: the shared prefix is the
        // one part of a project path that carries no information.
        let shown = artifact
            .path
            .strip_prefix(root)
            .unwrap_or(&artifact.path)
            .display()
            .to_string();
        println!(
            "  {}  {}  {}  {}",
            ui::rpad(&ui::bold(&ui::bytes(artifact.size_bytes)), 9),
            ui::lpad(&ui::cyan(&artifact.tool), 10),
            ui::dim(&ui::bar(artifact.size_bytes, largest, 8)),
            ui::truncate_path(&shown, 56)
        );
    }

    if artifacts.len() > limit {
        println!(
            "  {}",
            ui::dim(&format!(
                "… and {} more (--limit to show them)",
                artifacts.len() - limit
            ))
        );
    }

    println!();
    println!(
        "  {} across {}",
        ui::bold(&ui::bytes(total)),
        ui::count(artifacts.len(), "artifact", "artifacts")
    );
    println!();
}

/// Remove the artifacts, re-checking that each one really is build output.
///
/// The guard denies system-critical roots and approves nothing by default, so
/// each path has to be approved individually — and it is only approved after
/// [`devscan::is_artifact_path`] confirms the directory is a recognised
/// artifact with its marker file in place. A path that arrived here some other
/// way (a stale scan, a hand-edited JSON list) is refused.
pub fn clean(artifacts: &[DevArtifact]) -> spotless_core::CleanReport {
    let mut guard = SafetyGuard::system_only();
    let mut items = Vec::new();
    for artifact in artifacts {
        if !devscan::is_artifact_path(&artifact.path) {
            continue;
        }
        guard.approve_root(artifact.path.clone());
        items.push(ScanItem {
            target_id: "developer".into(),
            path: artifact.path.clone(),
            size_bytes: artifact.size_bytes,
            is_dir: true,
        });
    }
    cleaner::clean_items(&items, &guard, &TrashBackend, false)
}
