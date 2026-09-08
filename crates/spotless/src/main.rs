//! Spotless — a fast, transparent macOS cleaner for the terminal.
//!
//! The binary is a thin shell over [`spotless_core`]: it parses arguments,
//! prints, and asks for confirmation. Every decision about what may be removed
//! belongs to the core's safety layer, not to this crate.

mod backend;
mod cli;
mod cmd;
mod targets;
mod tui;
mod ui;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Command};

fn main() {
    let args = Cli::parse();
    ui::init_color(args.no_color);

    if let Err(error) = dispatch(&args) {
        // One line, on stderr, with the chain of causes — a cleaner that dumps
        // a backtrace at someone who typed a wrong folder name is noise.
        eprintln!("{} {error:#}", ui::red("error:"));
        std::process::exit(1);
    }
}

fn dispatch(args: &Cli) -> Result<()> {
    let json = args.json;
    match &args.command {
        None | Some(Command::Tui) => tui::run(),
        Some(Command::Scan(a)) => cmd::scan::run(a, json),
        Some(Command::Clean(a)) => cmd::clean::run(a, json),
        Some(Command::Dev(a)) => cmd::dev::run(a, json),
        Some(Command::Dupes(a)) => cmd::dupes::run(a, json),
        Some(Command::Du(a)) => cmd::du::run(a, json),
        Some(Command::Apps(a)) => cmd::apps::list(a, json),
        Some(Command::Uninstall(a)) => cmd::apps::uninstall(a, json),
        Some(Command::Orphans(a)) => cmd::orphans::run(a, json),
        Some(Command::Trash(a)) => cmd::trash::run(a, json),
        Some(Command::Rules(a)) => cmd::rules::run(a, json),
    }
}
