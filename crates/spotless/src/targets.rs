//! Turning a [`Selection`] into the set of targets a command acts on.
//!
//! Shared by `scan`, `clean`, and the TUI so that a target id means the same
//! thing everywhere, and so an unknown id is an error rather than a silently
//! empty result.

use anyhow::{bail, Result};
use spotless_core::{RuleSet, SafetyTier, ScanReport, ScanTarget};

use crate::cli::Selection;
use crate::ui;

/// Resolve a selection against the built-in ruleset.
pub fn resolve(selection: &Selection) -> Result<Vec<ScanTarget>> {
    let set: RuleSet = spotless_core::builtin_ruleset()?;
    let mut targets = set.targets;

    if !selection.targets.is_empty() {
        // An id that matches nothing is a typo, and a typo that quietly cleans
        // nothing looks exactly like a clean that found nothing to do.
        for wanted in &selection.targets {
            if !targets.iter().any(|t| &t.id == wanted) {
                bail!("no target with id `{wanted}` — run `spotless rules` to list them");
            }
        }
        targets.retain(|t| selection.targets.contains(&t.id));
    }

    if selection.safe_only {
        targets.retain(|t| t.safety == SafetyTier::Safe);
    }

    Ok(targets)
}

/// Scan `targets`, ticking progress to stderr so a slow walk does not look hung.
///
/// Progress goes to stderr rather than stdout because `--json` owns stdout, and
/// it is rewritten in place on a terminal and suppressed entirely when it is
/// not one — a build log should not collect one line per target.
pub fn scan(targets: &[ScanTarget], quiet: bool) -> ScanReport {
    use std::io::{IsTerminal, Write};

    let live = !quiet && std::io::stderr().is_terminal();
    let mut done = 0usize;
    let total = targets.len();

    let report = spotless_core::scanner::scan_targets_with(
        targets,
        |scan| {
            done += 1;
            if live {
                let mut err = std::io::stderr();
                let _ = write!(
                    err,
                    "\r\x1b[2Kscanning {done}/{total} — {}",
                    ui::truncate_path(&scan.target.name, 48)
                );
                let _ = err.flush();
            }
        },
        &|| false,
    );

    if live {
        eprint!("\r\x1b[2K");
    }
    report
}

/// Drop targets that came back smaller than the selection's floor.
pub fn apply_min_size(report: &mut ScanReport, min_size: Option<u64>) {
    let Some(min) = min_size else { return };
    report.targets.retain(|t| t.total_bytes >= min);
    report.total_bytes = report.targets.iter().map(|t| t.total_bytes).sum();
}

/// The word shown in the safety column, coloured by tier.
pub fn safety_label(tier: SafetyTier) -> String {
    match tier {
        SafetyTier::Safe => ui::green("safe"),
        SafetyTier::Caution => ui::yellow("caution"),
        SafetyTier::Expert => ui::red("expert"),
    }
}
