//! `spotless rules` — print the complete list of what this program may remove.
//!
//! The rules being inspectable data rather than code is the product's central
//! claim, and a claim nobody can check is worth nothing. This command is how it
//! is checked: it prints the shipped TOML, byte for byte, out of the binary.

use anyhow::Result;

use crate::cli::RulesArgs;
use crate::targets::safety_label;
use crate::ui;

pub fn run(args: &RulesArgs, json: bool) -> Result<()> {
    let set = spotless_core::builtin_ruleset()?;

    if args.toml {
        print!("{}", spotless_core::SYSTEM_RULES);
        print!("{}", spotless_core::DEVELOPER_RULES);
        return Ok(());
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&set.targets)?);
        return Ok(());
    }

    println!();
    println!(
        "  {} targets — everything Spotless will ever remove.",
        ui::bold(&set.targets.len().to_string())
    );
    println!();
    for target in &set.targets {
        println!(
            "  {} {}  {}",
            ui::lpad(&safety_label(target.safety), 8),
            ui::bold(&target.name),
            ui::dim(&target.id)
        );
        println!("           {}", ui::dim(&target.path));
        if !target.description.is_empty() {
            println!("           {}", target.description);
        }
        if target.permanent {
            println!(
                "           {}",
                ui::yellow("removed permanently — there is no Trash to move this to")
            );
        }
    }
    println!();
    println!(
        "  {}",
        ui::dim("`spotless rules --toml` prints the source these came from.")
    );
    println!();
    Ok(())
}
