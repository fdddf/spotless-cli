# Spotless CLI

**A fast, transparent macOS cleaner for the terminal.** Reclaim disk space from
caches, developer build artifacts, duplicate files, and the support files apps
leave behind — with an interactive TUI, JSON output for scripts, and a cleaning
ruleset you can read.

```
$ spotless scan

       SIZE  SAFETY    TARGET                                ID
      15 GB  caution   XCTest runner devices                 xctest-devices
     6.0 GB  caution   Xcode iOS DeviceSupport               xcode-device-support
     2.6 GB  caution   Tool cache (~/.cache)                 xdg-cache
     2.4 GB  safe      User application caches               user-caches
     836 MB  caution   Go module cache                       go-module-cache
     351 MB  caution   Maven repository cache                maven-repository

  28 GB reclaimable across 18 targets
```

Run `spotless` with no arguments for the interactive TUI.

## Why

Mac cleaners are a category full of heavy, closed-source suites that push
subscriptions and, in some cases, invent risks to sell a fix. This one takes the
opposite position:

- **Nothing is deleted without you saying so.** Every destructive command prints
  the plan first and then asks. `--yes` skips the question, never the plan.
- **Removals go to the Trash**, so a mistake costs a "Put Back", not a restore
  from backup. The few things that cannot be trashed are labelled as such.
- **The rules are data, not code.** `spotless rules` prints the complete list of
  everything the program is willing to touch, and `--toml` prints the source it
  was compiled from. There is no hidden list.
- **A safety layer stands between every path and the filesystem.** Paths are
  canonicalized (so a symlink cannot escape), system-critical roots are
  hard-blocked, and a path must live inside an approved root to be removable.
- **It knows what developers actually fill their disks with**: `node_modules`,
  `target/`, `DerivedData`, simulator device sets, Docker, Gradle, Go module
  cache, and thirty more.

## Install

```bash
# Prebuilt universal binary (Apple Silicon + Intel)
curl -fsSL https://github.com/fdddf/spotless-cli/releases/latest/download/spotless-macos-universal.tar.gz \
  | tar -xz && sudo mv spotless /usr/local/bin/

# Or from source (needs Rust 1.87+)
cargo install --git https://github.com/fdddf/spotless-cli spotless
```

macOS 11+, Apple Silicon or Intel.

**Full Disk Access.** macOS keeps some of the biggest caches behind TCC. Without
Full Disk Access, Spotless still works — it just reports smaller numbers and
prints how many paths it could not read. To grant it: System Settings → Privacy
& Security → Full Disk Access → add your terminal (Terminal, iTerm, Ghostty, …).

## Commands

| | |
|---|---|
| `spotless` | Interactive TUI: scan, developer junk, apps, Trash |
| `spotless scan` | What could be reclaimed. Changes nothing |
| `spotless clean` | Remove it. Prints the plan, then asks |
| `spotless dev [PATH]` | Developer build artifacts under a folder |
| `spotless dupes [PATH]` | Byte-identical duplicates |
| `spotless du [PATH]` | Where the disk went, as a tree |
| `spotless apps` | Installed applications by size |
| `spotless uninstall <APP>` | An app *and* its leftovers |
| `spotless orphans` | Support files whose app is already gone |
| `spotless trash` | What is in the Trash; `--empty` to empty it |
| `spotless rules` | Every rule the binary ships with |

### Examples

```bash
spotless scan --safe-only            # only regenerable data
spotless clean --safe-only --yes     # clean it, no questions

spotless dev ~/Code --min-size 100MB --older-than 90
spotless dev ~/Code --clean          # asks before removing

spotless du ~/Library --depth 2
spotless dupes ~/Downloads --min-size 10MB

spotless uninstall Slack             # bundle + caches + preferences
spotless orphans --clean

spotless scan --json | jq '.total_bytes'
```

Every command takes `--json` (machine-readable on stdout; progress stays on
stderr) and `--no-color` (also honours `NO_COLOR`).

## The TUI

`spotless` with no command opens a four-tab terminal UI:

| Tab | What it does |
|---|---|
| **Scan** | Rule-based targets. Safe ones are pre-ticked |
| **Developer** | Build artifacts under your home directory |
| **Apps** | Installed apps by size; `u` uninstalls one with its leftovers |
| **Trash** | What is in it; `e` empties it |

`space` selects · `a` selects all · `c` cleans · `r` rescans · `tab` switches ·
`q` quits. Scans run on a worker thread, so the list stays responsive and long
walks can be watched rather than waited out.

## Safety model

Three layers, in this order:

1. **The ruleset** bounds what is even a candidate. It ships as TOML inside the
   binary and is printable with `spotless rules --toml`.
2. **The safety guard** (`spotless-core::safety`) canonicalizes each path and
   refuses it unless it is inside an approved root and outside every denied one.
   `/System`, `/usr`, `/bin`, `/etc`, the TCC database, Documents, Desktop,
   Pictures and Keychains are denied outright. The guard runs immediately before
   removal, on every path, treating the scan result as untrusted input.
3. **The backend** moves to the Trash. Permanent deletion happens only where
   there is no Trash to move to (the Trash itself), or when you pass
   `--permanent`, and both say so before they act.

Found a hole in this? Open an issue — that is the most valuable bug report this
project can receive.

## Relationship to the Spotless app

This is the open-source core and terminal front end of
[Spotless](https://mzjpg.com), a macOS app. The engine — scanning, the ruleset,
the safety guard, the cleaner, the developer scan, duplicates, disk usage, the
uninstaller — is the same code, MIT-licensed, and lives here. The paid app adds
a graphical disk visualizer, live system monitoring, scheduled reminders and the
rest of its UI on top of it.

The CLI is not a demo or a crippled build: it is the whole engine, and nothing
here expires or asks for a licence.

## Development

```bash
cargo test --workspace      # 110+ tests, all offline
cargo clippy --workspace --all-targets
cargo fmt -p spotless       # the CLI crate only — see below
```

`crates/spotless-core` is vendored from the Spotless app's core crate by
`scripts/sync-core.sh`, which copies the shared modules and reports what
changed; `--check` fails if the two have drifted. The copies are byte-identical
on purpose, so format the CLI crate alone rather than the whole workspace.
Fixes to the core modules are welcome here — the sync is how they travel back.

## License

MIT. See [LICENSE](./LICENSE).
