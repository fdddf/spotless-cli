//! `spotless trash` — what is in the Trash, and emptying it.
//!
//! Everything Spotless removes goes here, which means the disk space does not
//! actually come back until the Trash is emptied. A user who cleans 11 GB and
//! then checks "About This Mac" sees no change and reasonably concludes nothing
//! happened; this command closes that gap.

use anyhow::Result;

use crate::cli::TrashArgs;
use crate::ui;

pub fn run(args: &TrashArgs, json: bool) -> Result<()> {
    let summary = spotless_core::trash::summary();

    if !args.empty {
        if json {
            println!("{}", serde_json::to_string_pretty(&summary)?);
        } else if summary.items == 0 {
            println!("{}", ui::green("The Trash is empty."));
        } else {
            println!(
                "  {} in {}.",
                ui::bold(&ui::bytes(summary.bytes)),
                ui::count(summary.items, "item", "items")
            );
            println!(
                "  {}",
                ui::dim("`spotless trash --empty` deletes them permanently.")
            );
        }
        return Ok(());
    }

    if summary.items == 0 {
        println!("{}", ui::green("The Trash is already empty."));
        return Ok(());
    }

    if !args.yes {
        println!(
            "  {} emptying the Trash cannot be undone.",
            ui::red("Permanent:")
        );
        if !ui::confirm(&format!(
            "Delete {} in {}?",
            ui::bold(&ui::bytes(summary.bytes)),
            ui::count(summary.items, "item", "items")
        )) {
            println!("{}", ui::dim("The Trash was left alone."));
            return Ok(());
        }
    }

    let report = spotless_core::trash::empty();
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "{} {} freed from {}.",
        ui::green("✓"),
        ui::bold(&ui::bytes(report.bytes_reclaimed)),
        ui::count(report.removed, "item", "items")
    );
    for (path, error) in &report.failed {
        println!("  {} {path} — {error}", ui::red("failed"));
    }
    Ok(())
}
