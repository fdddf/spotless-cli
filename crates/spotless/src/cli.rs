//! The command-line surface.
//!
//! Two conventions hold everywhere and are worth stating once:
//!
//! - **Nothing is removed without `--yes`.** Every destructive command runs as
//!   a dry run by default and prints what it *would* do. `--yes` is the only
//!   way to skip the interactive confirmation, and there is no way to remove
//!   something without one or the other.
//! - **`--json` writes to stdout, progress writes to stderr**, so output can be
//!   piped without losing the human-readable half.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "spotless",
    version,
    about = "A fast, transparent macOS cleaner for the terminal",
    long_about = "Spotless reclaims disk space from caches, logs, developer build artifacts, \
duplicate files, and the support files apps leave behind.\n\n\
Every removal goes to the Trash, every rule is inspectable data (`spotless rules`), \
and nothing is deleted without an explicit confirmation.\n\n\
Run with no command for the interactive terminal UI.",
    after_help = "Examples:\n  \
spotless                       # interactive TUI\n  \
spotless scan                  # what could be reclaimed\n  \
spotless clean --safe-only     # dry run of the safe targets\n  \
spotless clean --safe-only --yes\n  \
spotless dev ~/Code --min-size 100MB\n  \
spotless du ~/Library --depth 2\n  \
spotless dupes ~/Downloads"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Emit machine-readable JSON on stdout.
    #[arg(long, global = true)]
    pub json: bool,

    /// Never emit ANSI colour (also honours NO_COLOR).
    #[arg(long, global = true)]
    pub no_color: bool,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Measure what could be reclaimed, without changing anything.
    Scan(ScanArgs),

    /// Remove scanned junk. A dry run unless you pass --yes.
    Clean(CleanArgs),

    /// Find developer build artifacts: node_modules, target/, DerivedData, …
    Dev(DevArgs),

    /// Find byte-identical duplicate files.
    Dupes(DupesArgs),

    /// Show where the disk went, as a tree.
    Du(DuArgs),

    /// List installed applications with their sizes.
    Apps(AppsArgs),

    /// Remove an application and the support files it leaves behind.
    Uninstall(UninstallArgs),

    /// Find support files belonging to apps that are no longer installed.
    Orphans(OrphansArgs),

    /// Show what is in the Trash, or empty it.
    Trash(TrashArgs),

    /// Print the built-in cleaning rules.
    Rules(RulesArgs),

    /// Interactive terminal UI (the default with no command).
    Tui,
}

/// Which of the built-in targets a scan or clean applies to.
#[derive(Args, Debug, Clone, Default)]
pub struct Selection {
    /// Only this target id; repeat for several. See `spotless rules`.
    #[arg(short = 't', long = "target", value_name = "ID")]
    pub targets: Vec<String>,

    /// Only targets marked `safe` — regenerable data, no user files.
    #[arg(long)]
    pub safe_only: bool,

    /// Skip targets smaller than this, e.g. 100MB.
    #[arg(long, value_name = "SIZE", value_parser = parse_size)]
    pub min_size: Option<u64>,
}

#[derive(Args, Debug)]
pub struct ScanArgs {
    #[command(flatten)]
    pub selection: Selection,

    /// List every item inside each target, not just the totals.
    #[arg(long)]
    pub items: bool,
}

#[derive(Args, Debug)]
pub struct CleanArgs {
    #[command(flatten)]
    pub selection: Selection,

    /// Actually remove. Without this the command only reports.
    #[arg(long, short = 'y')]
    pub yes: bool,

    /// Delete outright instead of moving to the Trash. Not recoverable.
    #[arg(long)]
    pub permanent: bool,
}

#[derive(Args, Debug)]
pub struct DevArgs {
    /// Where to look. Defaults to your home directory.
    pub path: Option<PathBuf>,

    /// How deep to descend while searching for projects.
    #[arg(long, default_value_t = 7)]
    pub depth: usize,

    /// Skip artifacts smaller than this, e.g. 50MB.
    #[arg(long, value_name = "SIZE", value_parser = parse_size)]
    pub min_size: Option<u64>,

    /// Only artifacts untouched for this many days.
    #[arg(long, value_name = "DAYS")]
    pub older_than: Option<u64>,

    /// Only artifacts from this toolchain, e.g. cargo, npm, xcode.
    #[arg(long, value_name = "TOOL")]
    pub tool: Option<String>,

    /// Show at most this many rows.
    #[arg(long, default_value_t = 40)]
    pub limit: usize,

    /// Remove what was found (still needs --yes).
    #[arg(long)]
    pub clean: bool,

    /// Confirm removal non-interactively.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Args, Debug)]
pub struct DupesArgs {
    /// Where to look. Defaults to the current directory.
    pub path: Option<PathBuf>,

    /// Ignore files smaller than this.
    #[arg(long, value_name = "SIZE", value_parser = parse_size, default_value = "1MB")]
    pub min_size: u64,

    /// Show at most this many groups.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,

    /// Keep one copy of each group and remove the rest (still needs --yes).
    ///
    /// The copy kept is the one with the shortest path, which is almost always
    /// the original rather than a "file (2)" beside it.
    #[arg(long)]
    pub prune: bool,

    /// Confirm removal non-interactively.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Args, Debug)]
pub struct DuArgs {
    /// Where to measure. Defaults to the current directory.
    pub path: Option<PathBuf>,

    /// How many levels to print.
    #[arg(long, short = 'd', default_value_t = 2)]
    pub depth: usize,

    /// Entries to show per level before folding the rest into "Other".
    #[arg(long, default_value_t = 12)]
    pub top: usize,
}

#[derive(Args, Debug)]
pub struct AppsArgs {
    /// Show at most this many applications.
    #[arg(long, default_value_t = 40)]
    pub limit: usize,

    /// Sort by name instead of size.
    #[arg(long)]
    pub by_name: bool,
}

#[derive(Args, Debug)]
pub struct UninstallArgs {
    /// Application name, or a path to the .app bundle.
    pub app: String,

    /// Confirm removal non-interactively.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Args, Debug)]
pub struct OrphansArgs {
    /// Show at most this many apps' worth of leftovers.
    #[arg(long, default_value_t = 30)]
    pub limit: usize,

    /// Remove what was found (still needs --yes).
    #[arg(long)]
    pub clean: bool,

    /// Confirm removal non-interactively.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Args, Debug)]
pub struct TrashArgs {
    /// Empty the Trash. This is a permanent delete (still needs --yes).
    #[arg(long)]
    pub empty: bool,

    /// Confirm emptying non-interactively.
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Args, Debug)]
pub struct RulesArgs {
    /// Print the raw TOML rather than a table.
    #[arg(long)]
    pub toml: bool,
}

/// Parse a human size like `500`, `20k`, `100MB`, `1.5G`.
///
/// Both `MB` and `M` are accepted and both mean 1000², matching how the sizes
/// are printed. Nobody typing `--min-size 100MB` means 104,857,600 bytes, and a
/// filter that silently disagreed with the column beside it would be worse than
/// one that is a few percent off any particular disk-utility convention.
pub fn parse_size(input: &str) -> Result<u64, String> {
    let text = input.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: f64 = number
        .parse()
        .map_err(|_| format!("`{input}` is not a size (try 100MB)"))?;
    let scale = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" | "kb" => 1e3,
        "m" | "mb" => 1e6,
        "g" | "gb" => 1e9,
        "t" | "tb" => 1e12,
        other => return Err(format!("unknown size unit `{other}` (try B, KB, MB, GB)")),
    };
    Ok((number * scale) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sizes_with_and_without_units() {
        assert_eq!(parse_size("500").unwrap(), 500);
        assert_eq!(parse_size("20k").unwrap(), 20_000);
        assert_eq!(parse_size("100MB").unwrap(), 100_000_000);
        assert_eq!(parse_size("1.5G").unwrap(), 1_500_000_000);
        assert_eq!(parse_size(" 2 TB ").unwrap(), 2_000_000_000_000);
    }

    #[test]
    fn rejects_nonsense() {
        assert!(parse_size("big").is_err());
        assert!(parse_size("10x").is_err());
    }

    #[test]
    fn cli_definition_is_valid() {
        // clap panics on a malformed command definition, and it would do so at
        // runtime rather than at build time; this catches it in CI instead.
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
