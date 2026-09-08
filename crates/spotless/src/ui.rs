//! Terminal presentation: sizes, colour, tables, and the confirmation prompt.
//!
//! Everything here is deliberately dependency-free. A cleaner that prints a
//! wrong number is worse than one that prints an ugly one, so the formatting
//! rules live in one small, tested place rather than in each command.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};

static COLOR: AtomicBool = AtomicBool::new(true);

/// Decide once, at startup, whether output may carry ANSI colour.
///
/// Honours `NO_COLOR` (the de-facto standard) and the `--no-color` flag, and
/// falls back to "not a terminal means not coloured" so piping into a file or
/// `grep` yields plain text.
pub fn init_color(disabled_by_flag: bool) {
    let on = !disabled_by_flag
        && std::env::var_os("NO_COLOR").is_none()
        && std::io::stdout().is_terminal();
    COLOR.store(on, Ordering::Relaxed);
}

fn colored() -> bool {
    COLOR.load(Ordering::Relaxed)
}

/// Wrap `text` in an SGR sequence, or return it untouched when colour is off.
fn sgr(code: &str, text: &str) -> String {
    if colored() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

pub fn bold(text: &str) -> String {
    sgr("1", text)
}
pub fn dim(text: &str) -> String {
    sgr("2", text)
}
pub fn green(text: &str) -> String {
    sgr("32", text)
}
pub fn yellow(text: &str) -> String {
    sgr("33", text)
}
pub fn red(text: &str) -> String {
    sgr("31", text)
}
pub fn cyan(text: &str) -> String {
    sgr("36", text)
}

/// Format a byte count the way macOS does: decimal units, at most one decimal
/// place, and never a decimal point on bytes.
///
/// Matching Finder matters more than matching `ls -h`: the number a user
/// checks this against is the one in "About This Mac", which is decimal.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if n < 1000 {
        return format!("{n} B");
    }
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    // 9.9 GB, but 10 GB — a tenth of a gigabyte is noise at that scale.
    if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// `n` with its unit, pluralised. Both forms are spelled out because the ones
/// this program counts are not all regular — "copies", not "copys".
pub fn count(n: usize, singular: &str, plural: &str) -> String {
    if n == 1 {
        format!("{n} {singular}")
    } else {
        format!("{n} {plural}")
    }
}

/// Right-align `text` in a field of `width` display columns.
pub fn rpad(text: &str, width: usize) -> String {
    let len = display_width(text);
    if len >= width {
        text.to_string()
    } else {
        format!("{}{}", " ".repeat(width - len), text)
    }
}

/// Left-align `text` in a field of `width` display columns.
pub fn lpad(text: &str, width: usize) -> String {
    let len = display_width(text);
    if len >= width {
        text.to_string()
    } else {
        format!("{}{}", text, " ".repeat(width - len))
    }
}

/// Shorten `text` to `width` columns, marking the cut with an ellipsis.
///
/// Paths are cut from the *left*: the tail (the folder that identifies it) is
/// what the reader needs, and the leading `/Users/name/Library/…` is not.
pub fn truncate_path(text: &str, width: usize) -> String {
    if display_width(text) <= width || width < 2 {
        return text.to_string();
    }
    let keep: String = text
        .chars()
        .rev()
        .take(width - 1)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…{keep}")
}

/// Column width of a string, counting escape sequences as zero.
///
/// Not a full grapheme implementation — it counts `char`s — which is right for
/// paths and rule names and wrong for emoji, none of which this prints.
fn display_width(text: &str) -> usize {
    let mut width = 0;
    let mut in_escape = false;
    for ch in text.chars() {
        if in_escape {
            in_escape = ch != 'm';
        } else if ch == '\x1b' {
            in_escape = true;
        } else {
            width += 1;
        }
    }
    width
}

/// A horizontal bar `width` columns wide showing `value` out of `total`.
pub fn bar(value: u64, total: u64, width: usize) -> String {
    if total == 0 || width == 0 {
        return " ".repeat(width);
    }
    let filled = ((value as f64 / total as f64) * width as f64).round() as usize;
    let filled = filled.min(width);
    format!("{}{}", "█".repeat(filled), "·".repeat(width - filled))
}

/// Ask the user to confirm a destructive action.
///
/// Returns false whenever the answer is not an explicit yes, including when
/// stdin is not a terminal: a cleaner that treats a closed pipe as consent is
/// exactly the failure mode this whole program is built to avoid. Scripts pass
/// `--yes` instead.
pub fn confirm(question: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        eprintln!(
            "{} not a terminal, so there is nobody to confirm with — pass --yes to proceed",
            yellow("refusing:")
        );
        return false;
    }
    print!("{} {} [y/N] ", bold("?"), question);
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Print a one-line status to stderr, so `--json` on stdout stays parseable.
pub fn status(message: &str) {
    eprintln!("{}", dim(message));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_uses_decimal_units_like_finder() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1000), "1.0 KB");
        assert_eq!(bytes(1_500_000), "1.5 MB");
        assert_eq!(bytes(12_000_000_000), "12 GB");
    }

    #[test]
    fn bytes_switches_to_whole_numbers_at_ten() {
        // The boundary the formatting rule turns on; easy to get wrong by one.
        assert_eq!(bytes(9_900_000_000), "9.9 GB");
        assert_eq!(bytes(10_000_000_000), "10 GB");
    }

    #[test]
    fn count_pluralises_both_ways() {
        assert_eq!(count(1, "item", "items"), "1 item");
        assert_eq!(count(0, "item", "items"), "0 items");
        assert_eq!(count(2, "copy", "copies"), "2 copies");
    }

    #[test]
    fn padding_ignores_escape_sequences() {
        // Colour must not eat into the column width, or every coloured table
        // row comes out misaligned.
        COLOR.store(true, Ordering::Relaxed);
        let padded = rpad(&green("ok"), 5);
        assert_eq!(display_width(&padded), 5);
        COLOR.store(false, Ordering::Relaxed);
    }

    #[test]
    fn truncate_keeps_the_tail_of_a_path() {
        assert_eq!(truncate_path("/a/very/long/path/target", 10), "…th/target");
        assert_eq!(truncate_path("short", 10), "short");
    }

    #[test]
    fn bar_is_always_exactly_the_requested_width() {
        for value in [0u64, 1, 50, 100] {
            assert_eq!(bar(value, 100, 10).chars().count(), 10);
        }
        assert_eq!(bar(5, 0, 4), "    ");
    }
}
