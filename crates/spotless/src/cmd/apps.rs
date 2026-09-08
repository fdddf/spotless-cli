//! `spotless apps` and `spotless uninstall`.
//!
//! Dragging an app to the Trash leaves its caches, preferences, containers and
//! login items behind — routinely more than the bundle itself. `uninstall`
//! plans the whole removal, prints it, and asks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use spotless_core::cleaner;
use spotless_core::{apps, AppInfo, SafetyGuard, ScanItem, UninstallPlan};

use crate::backend::TrashBackend;
use crate::cli::{AppsArgs, UninstallArgs};
use crate::ui;

/// `spotless apps` — installed applications, largest first.
pub fn list(args: &AppsArgs, json: bool) -> Result<()> {
    if !json {
        ui::status("Measuring installed applications…");
    }
    let mut installed = apps::list_apps(&apps::default_app_dirs());
    let sizes: HashMap<PathBuf, u64> =
        apps::app_sizes(&installed.iter().map(|a| a.path.clone()).collect::<Vec<_>>())
            .into_iter()
            .collect();
    for app in &mut installed {
        app.size_bytes = sizes.get(&app.path).copied();
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&installed)?);
        return Ok(());
    }

    if !args.by_name {
        installed.sort_by_key(|a| std::cmp::Reverse(a.size_bytes.unwrap_or(0)));
    }

    let total: u64 = installed.iter().filter_map(|a| a.size_bytes).sum();
    println!();
    for app in installed.iter().take(args.limit) {
        println!(
            "  {}  {}  {}",
            ui::rpad(&ui::bytes(app.size_bytes.unwrap_or(0)), 9),
            ui::lpad(&app.name, 32),
            ui::dim(app.bundle_id.as_deref().unwrap_or(""))
        );
    }
    if installed.len() > args.limit {
        println!(
            "  {}",
            ui::dim(&format!("… and {} more", installed.len() - args.limit))
        );
    }
    println!();
    println!(
        "  {} across {}",
        ui::bold(&ui::bytes(total)),
        ui::count(installed.len(), "application", "applications")
    );
    println!(
        "  {}",
        ui::dim("Next: `spotless uninstall <name>` to remove one with its leftovers.")
    );
    println!();
    Ok(())
}

/// `spotless uninstall` — remove one app and its support files.
pub fn uninstall(args: &UninstallArgs, json: bool) -> Result<()> {
    let home = spotless_core::paths::home_dir().context("cannot locate your home directory")?;
    let app = find(&args.app)?;
    let plan = apps::plan_uninstall(app, &home);

    if json && !args.yes {
        println!("{}", serde_json::to_string_pretty(&plan)?);
        return Ok(());
    }
    if !json {
        print_plan(&plan);
    }

    if !args.yes
        && !ui::confirm(&format!(
            "Move {} and its leftovers ({}) to the Trash?",
            ui::bold(&plan.app.name),
            ui::bytes(plan.total_bytes)
        ))
    {
        println!("{}", ui::dim("Nothing was removed."));
        return Ok(());
    }

    let report = remove(&plan);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "{} {} reclaimed from {}, moved to the Trash.",
        ui::green("✓"),
        ui::bold(&ui::bytes(report.bytes_reclaimed)),
        ui::count(report.removed.len(), "item", "items")
    );
    for failed in &report.failed {
        println!(
            "  {} {} — {}",
            ui::red("failed"),
            ui::truncate_path(&failed.path.display().to_string(), 52),
            failed.error
        );
    }
    let admin = plan.leftovers.iter().filter(|l| l.requires_admin).count();
    if admin > 0 {
        println!(
            "  {}",
            ui::yellow(&format!(
                "{} left in place — remove them with sudo if you want the space",
                ui::count(admin, "root-owned leftover", "root-owned leftovers")
            ))
        );
    }
    Ok(())
}

/// Resolve what the user typed to one installed application.
///
/// A path is taken literally; anything else is matched against the installed
/// names, exactly first and then as a case-insensitive substring. An ambiguous
/// substring is an error rather than a guess: uninstalling the wrong app is not
/// something a "did you mean" can undo.
fn find(query: &str) -> Result<AppInfo> {
    let as_path = Path::new(query);
    if as_path.exists() && apps::is_app_bundle(as_path) {
        let installed = apps::list_apps(&[as_path.parent().unwrap_or(as_path).to_path_buf()]);
        if let Some(app) = installed.into_iter().find(|a| a.path == as_path) {
            return Ok(app);
        }
    }

    let installed = apps::list_apps(&apps::default_app_dirs());
    if let Some(app) = installed
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(query))
    {
        return Ok(app.clone());
    }

    let needle = query.to_lowercase();
    let matches: Vec<&AppInfo> = installed
        .iter()
        .filter(|a| a.name.to_lowercase().contains(&needle))
        .collect();

    match matches.as_slice() {
        [] => bail!("no installed application matches `{query}`"),
        [one] => Ok((*one).clone()),
        many => {
            let names: Vec<&str> = many.iter().map(|a| a.name.as_str()).collect();
            bail!("`{query}` matches several apps: {}", names.join(", "))
        }
    }
}

fn print_plan(plan: &UninstallPlan) {
    println!();
    println!(
        "  {}  {}",
        ui::rpad(&ui::bold(&ui::bytes(plan.app.size_bytes.unwrap_or(0))), 9),
        ui::bold(&format!("{} (the app itself)", plan.app.name))
    );
    for leftover in &plan.leftovers {
        let admin = if leftover.requires_admin {
            ui::yellow(" needs sudo")
        } else {
            String::new()
        };
        println!(
            "  {}  {} {}{}",
            ui::rpad(&ui::bytes(leftover.size_bytes), 9),
            ui::lpad(&ui::cyan(&leftover.kind), 18),
            ui::truncate_path(&leftover.path.display().to_string(), 46),
            admin
        );
    }
    println!();
    println!(
        "  {} across {}",
        ui::bold(&ui::bytes(plan.total_bytes)),
        ui::count(plan.leftovers.len() + 1, "item", "items")
    );
    println!();
}

/// Trash the bundle and its user-level leftovers.
///
/// Root-owned leftovers are left alone: removing them needs an administrator
/// password, and a CLI that silently invoked `sudo` on the user's behalf would
/// be a worse tool than one that says what it skipped.
pub fn remove(plan: &UninstallPlan) -> spotless_core::CleanReport {
    let mut guard = SafetyGuard::for_uninstall();
    let mut items = Vec::new();

    if apps::is_app_bundle(&plan.app.path) {
        guard.approve_root(plan.app.path.clone());
        items.push(ScanItem {
            target_id: "uninstall".into(),
            path: plan.app.path.clone(),
            size_bytes: plan.app.size_bytes.unwrap_or(0),
            is_dir: true,
        });
    }

    for leftover in &plan.leftovers {
        if leftover.requires_admin {
            continue;
        }
        guard.approve_root(leftover.path.clone());
        items.push(ScanItem {
            target_id: "uninstall".into(),
            path: leftover.path.clone(),
            size_bytes: leftover.size_bytes,
            is_dir: leftover.path.is_dir(),
        });
    }

    cleaner::clean_items(&items, &guard, &TrashBackend, false)
}
