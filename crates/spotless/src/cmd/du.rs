//! `spotless du` — where the disk actually went.
//!
//! Unlike `du -sh *`, this stops at volume boundaries, so scanning `/` does not
//! count the data volume twice, and it folds the long tail of small entries
//! into a single "Other" row instead of printing four hundred lines.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use spotless_core::usage::{self, UsageProgress};
use spotless_core::{UsageNode, UsageOptions};

use crate::cli::DuArgs;
use crate::ui;

pub fn run(args: &DuArgs, json: bool) -> Result<()> {
    let root = args
        .path
        .clone()
        .unwrap_or_else(|| PathBuf::from("."))
        .canonicalize()
        .context("cannot read that folder")?;

    let live = !json && std::io::stderr().is_terminal();
    let snapshot = usage::build_usage_snapshot(
        &root,
        &|progress: UsageProgress| {
            if live {
                let mut err = std::io::stderr();
                let _ = write!(
                    err,
                    "\r\x1b[2Kmeasuring — {} folders, {}",
                    progress.dirs_scanned,
                    ui::bytes(progress.bytes_seen)
                );
                let _ = err.flush();
            }
        },
        &|| false,
    )
    .context("the walk was cancelled")?;
    if live {
        eprint!("\r\x1b[2K");
    }

    let tree = snapshot
        .view(
            &root,
            UsageOptions {
                max_depth: args.depth,
                max_children: args.top,
            },
        )
        .context("the walk did not cover its own root")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&tree)?);
        return Ok(());
    }

    println!();
    println!(
        "  {}  {}",
        ui::rpad(&ui::bold(&ui::bytes(tree.size_bytes)), 9),
        ui::bold(&root.display().to_string())
    );
    print_children(&tree, tree.size_bytes, 1);
    println!();
    Ok(())
}

/// Print one level of the tree, indented, with a share bar against the parent.
fn print_children(node: &UsageNode, total: u64, depth: usize) {
    for child in &node.children {
        let indent = "  ".repeat(depth);
        let name = if child.is_mount {
            format!("{} {}", child.name, ui::dim("(another volume)"))
        } else if child.is_dir {
            format!("{}/", child.name)
        } else {
            child.name.clone()
        };
        println!(
            "  {}  {} {}{}",
            ui::rpad(&ui::bytes(child.size_bytes), 9),
            ui::dim(&ui::bar(child.size_bytes, total, 10)),
            indent,
            ui::truncate_path(&name, 52)
        );
        print_children(child, total, depth + 1);
    }
}
