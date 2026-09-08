//! `spotless clean` — remove what `scan` found.
//!
//! The shape of this command is the product's safety promise in code: scan,
//! print the plan, ask, and only then remove. `--yes` skips the question, never
//! the plan, and never the guard.

use anyhow::Result;
use spotless_core::cleaner::{self, RemovalBackend};
use spotless_core::{paths, CleanReport, SafetyGuard, ScanItem, ScanTarget};

use crate::backend::{DeleteBackend, RoutingBackend};
use crate::cli::CleanArgs;
use crate::targets;
use crate::ui;

pub fn run(args: &CleanArgs, json: bool) -> Result<()> {
    let selected = targets::resolve(&args.selection)?;
    let mut report = targets::scan(&selected, json);
    targets::apply_min_size(&mut report, args.selection.min_size);

    let items: Vec<ScanItem> = report
        .targets
        .iter()
        .flat_map(|t| t.items.iter().cloned())
        .collect();

    if items.is_empty() {
        if json {
            println!("{}", serde_json::to_string_pretty(&CleanReport::default())?);
        } else {
            println!("{}", ui::green("Nothing to clean."));
        }
        return Ok(());
    }

    // The dry run is the plan: it is the same walk over the same guard, so what
    // it reports is exactly what a real clean would do.
    let plan = execute(&selected, &items, args.permanent, true);

    if json && !args.yes {
        println!("{}", serde_json::to_string_pretty(&plan)?);
        return Ok(());
    }

    if !json {
        print_plan(&report, &plan, args.permanent);
    }

    if !args.yes {
        let mixed = !args.permanent && selected.iter().any(|t| t.permanent);
        let question = format!(
            "Remove {} — {}{}?",
            ui::bold(&ui::bytes(plan.bytes_reclaimed)),
            crate::backend::describes_trash(args.permanent),
            if mixed { ", some deleted outright" } else { "" }
        );
        if !ui::confirm(&question) {
            println!("{}", ui::dim("Nothing was removed."));
            return Ok(());
        }
    }

    let done = execute(&selected, &items, args.permanent, false);

    if json {
        println!("{}", serde_json::to_string_pretty(&done)?);
        return Ok(());
    }
    print_result(&done, args.permanent);
    Ok(())
}

/// Clean `items`, which must have come from scanning `selected`.
///
/// The guard starts from the defaults and additionally approves each selected
/// target's resolved root, so items the ruleset produced are cleanable while
/// anything outside those roots is still refused — including an item that was
/// in the list but whose path has since changed underneath us.
///
/// Shared with the TUI so that "clean" means precisely one thing in this
/// program, whichever way the user reached it.
pub fn execute(
    selected: &[ScanTarget],
    items: &[ScanItem],
    permanent: bool,
    dry_run: bool,
) -> CleanReport {
    let mut guard = SafetyGuard::default();
    for target in selected {
        guard.approve_root(paths::resolve(&target.path));
    }

    let backend: Box<dyn RemovalBackend> = if permanent {
        Box::new(DeleteBackend)
    } else {
        Box::new(RoutingBackend {
            permanent_roots: selected
                .iter()
                .filter(|t| t.permanent)
                .map(|t| paths::resolve(&t.path))
                .collect(),
        })
    };

    cleaner::clean_items(items, &guard, backend.as_ref(), dry_run)
}

/// What a clean would do, per target, before anything is removed.
///
/// A few targets have nowhere to move to — the Trash most obviously, since
/// re-trashing an item just shuffles it around inside the same folder — and are
/// deleted outright whatever the user's preference says. That is invisible from
/// the totals, so each such row is labelled and the summary says so again.
fn print_plan(report: &spotless_core::ScanReport, plan: &CleanReport, permanent: bool) {
    let mut rows: Vec<_> = report
        .targets
        .iter()
        .filter(|t| t.total_bytes > 0)
        .collect();
    rows.sort_by_key(|t| std::cmp::Reverse(t.total_bytes));

    println!();
    for scan in &rows {
        let irreversible = if scan.target.permanent && !permanent {
            ui::red(" (deleted permanently)")
        } else {
            String::new()
        };
        println!(
            "  {}  {}  {}{}",
            ui::rpad(&ui::bold(&ui::bytes(scan.total_bytes)), 9),
            ui::lpad(&targets::safety_label(scan.target.safety), 8),
            scan.target.name,
            irreversible
        );
        if !scan.target.description.is_empty() {
            println!("             {}", ui::dim(&scan.target.description));
        }
    }
    println!();
    println!(
        "  {} in {}, {}",
        ui::bold(&ui::bytes(plan.bytes_reclaimed)),
        ui::count(plan.removed.len(), "item", "items"),
        crate::backend::describes_trash(permanent)
    );
    if permanent {
        println!(
            "  {}",
            ui::red("Permanent delete: these files will not be recoverable.")
        );
    } else if rows.iter().any(|t| t.target.permanent) {
        println!(
            "  {}",
            ui::red("The rows marked above have no Trash to move to and are deleted outright.")
        );
    }
    if !plan.refused.is_empty() {
        println!(
            "  {}",
            ui::yellow(&format!(
                "{} refused by the safety guard and will be skipped",
                ui::count(plan.refused.len(), "path", "paths")
            ))
        );
    }
    println!();
}

/// What a clean actually did.
fn print_result(done: &CleanReport, permanent: bool) {
    println!(
        "{} {} reclaimed from {}, {}.",
        ui::green("✓"),
        ui::bold(&ui::bytes(done.bytes_reclaimed)),
        ui::count(done.removed.len(), "item", "items"),
        crate::backend::describes_trash(permanent)
    );

    for refused in &done.refused {
        println!(
            "  {} {} — {}",
            ui::yellow("skipped"),
            ui::truncate_path(&refused.path.display().to_string(), 52),
            refused.reason
        );
    }
    for failed in &done.failed {
        println!(
            "  {} {} — {}",
            ui::red("failed"),
            ui::truncate_path(&failed.path.display().to_string(), 52),
            failed.error
        );
    }

    if !permanent && done.bytes_reclaimed > 0 {
        println!(
            "  {}",
            ui::dim("The space returns when the Trash is emptied: `spotless trash --empty`.")
        );
    }
}
