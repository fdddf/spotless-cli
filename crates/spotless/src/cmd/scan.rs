//! `spotless scan` — measure what could be reclaimed, changing nothing.

use anyhow::Result;

use crate::cli::ScanArgs;
use crate::targets;
use crate::ui;

pub fn run(args: &ScanArgs, json: bool) -> Result<()> {
    let selected = targets::resolve(&args.selection)?;
    let mut report = targets::scan(&selected, json);
    targets::apply_min_size(&mut report, args.selection.min_size);

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    // Biggest first: the reason to run a cleaner is the top two or three rows.
    report
        .targets
        .sort_by_key(|t| std::cmp::Reverse(t.total_bytes));

    let found: Vec<_> = report
        .targets
        .iter()
        .filter(|t| t.total_bytes > 0)
        .collect();

    if found.is_empty() {
        println!("{}", ui::green("Nothing to reclaim — already spotless."));
        return Ok(());
    }

    println!();
    println!(
        "  {}  {}  {}  {}",
        ui::rpad(&ui::dim("SIZE"), 9),
        ui::lpad(&ui::dim("SAFETY"), 8),
        ui::lpad(&ui::dim("TARGET"), 36),
        ui::dim("ID")
    );

    for scan in &found {
        println!(
            "  {}  {}  {}  {}",
            ui::rpad(&ui::bold(&ui::bytes(scan.total_bytes)), 9),
            ui::lpad(&targets::safety_label(scan.target.safety), 8),
            ui::lpad(&ui::truncate_path(&scan.target.name, 36), 36),
            ui::dim(&scan.target.id)
        );

        if args.items {
            let mut items = scan.items.clone();
            items.sort_by_key(|i| std::cmp::Reverse(i.size_bytes));
            for item in items.iter().take(10) {
                println!(
                    "  {}  {}",
                    ui::rpad(&ui::dim(&ui::bytes(item.size_bytes)), 9),
                    ui::dim(&ui::truncate_path(&item.path.display().to_string(), 60))
                );
            }
            if items.len() > 10 {
                println!(
                    "  {}",
                    ui::dim(&format!("  … and {} more", items.len() - 10))
                );
            }
        }
    }

    println!();
    println!(
        "  {} reclaimable across {}",
        ui::bold(&ui::bytes(report.total_bytes)),
        ui::count(found.len(), "target", "targets")
    );

    let warnings: usize = report.targets.iter().map(|t| t.warnings.len()).sum();
    if warnings > 0 {
        println!(
            "  {}",
            ui::yellow(&format!(
                "{} could not be read — grant Full Disk Access for a complete figure",
                ui::count(warnings, "path", "paths")
            ))
        );
    }

    println!();
    println!(
        "  {}",
        ui::dim("Next: `spotless clean --safe-only` to preview a clean.")
    );
    println!();
    Ok(())
}
