//! `spotless orphans` — support files whose app is already gone.
//!
//! The reverse of `uninstall`: instead of asking "what will this app leave
//! behind", it asks "what is already lying around from apps that left". These
//! are the files a drag-to-Trash uninstall never removed, sometimes years ago.

use anyhow::{Context, Result};
use spotless_core::cleaner;
use spotless_core::orphans::{self, OrphanApp};
use spotless_core::{SafetyGuard, ScanItem};

use crate::backend::TrashBackend;
use crate::cli::OrphansArgs;
use crate::ui;

pub fn run(args: &OrphansArgs, json: bool) -> Result<()> {
    let home = spotless_core::paths::home_dir().context("cannot locate your home directory")?;

    if !json {
        ui::status("Matching ~/Library against installed applications…");
    }
    let installed = orphans::installed_bundle_ids(&orphans::installed_app_dirs());
    let found = orphans::find_orphans(&home, &installed);

    if json {
        println!("{}", serde_json::to_string_pretty(&found)?);
        return Ok(());
    }

    if found.is_empty() {
        println!("{}", ui::green("No leftovers from uninstalled apps."));
        return Ok(());
    }

    let total: u64 = found.iter().map(|a| a.total_bytes).sum();
    print_table(&found, args.limit);
    println!(
        "  {} across {}",
        ui::bold(&ui::bytes(total)),
        ui::count(found.len(), "vanished app", "vanished apps")
    );

    if !args.clean {
        println!("  {}", ui::dim("Add --clean to move these to the Trash."));
        println!();
        return Ok(());
    }

    if !args.yes
        && !ui::confirm(&format!(
            "Move {} of leftovers to the Trash?",
            ui::bold(&ui::bytes(total))
        ))
    {
        println!("{}", ui::dim("Nothing was removed."));
        return Ok(());
    }

    let report = clean(&found, &home);
    println!(
        "{} {} reclaimed from {}, moved to the Trash.",
        ui::green("✓"),
        ui::bold(&ui::bytes(report.bytes_reclaimed)),
        ui::count(report.removed.len(), "item", "items")
    );
    for refused in &report.refused {
        println!(
            "  {} {} — {}",
            ui::yellow("skipped"),
            ui::truncate_path(&refused.path.display().to_string(), 52),
            refused.reason
        );
    }
    Ok(())
}

fn print_table(found: &[OrphanApp], limit: usize) {
    println!();
    for app in found.iter().take(limit) {
        println!(
            "  {}  {}  {}",
            ui::rpad(&ui::bold(&ui::bytes(app.total_bytes)), 9),
            ui::lpad(&app.display_name, 28),
            ui::dim(&app.bundle_id)
        );
        for item in &app.items {
            println!(
                "  {}  {} {}",
                ui::rpad(&ui::dim(&ui::bytes(item.size_bytes)), 9),
                ui::lpad(&ui::cyan(&item.kind), 20),
                ui::dim(&ui::truncate_path(&item.path.display().to_string(), 44))
            );
        }
    }
    if found.len() > limit {
        println!(
            "  {}",
            ui::dim(&format!("… and {} more", found.len() - limit))
        );
    }
    println!();
}

/// Trash the orphaned files.
///
/// Each path is re-checked with [`orphans::is_orphan_path`] before it is
/// approved, so only files in the `~/Library` directories the scan covers can
/// be removed — the list is treated as untrusted input, exactly as a scan
/// result is everywhere else in this program.
fn clean(found: &[OrphanApp], home: &std::path::Path) -> spotless_core::CleanReport {
    let mut guard = SafetyGuard::system_only();
    let mut items = Vec::new();
    for app in found {
        for item in &app.items {
            if !orphans::is_orphan_path(&item.path, home) {
                continue;
            }
            guard.approve_root(item.path.clone());
            items.push(ScanItem {
                target_id: "orphans".into(),
                path: item.path.clone(),
                size_bytes: item.size_bytes,
                is_dir: item.path.is_dir(),
            });
        }
    }
    cleaner::clean_items(&items, &guard, &TrashBackend, false)
}
