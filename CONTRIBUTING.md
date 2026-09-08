# Contributing

Thanks for looking. A few things that will save you time.

## The shape of the repository

```
crates/spotless-core   the engine: rules, scanner, safety guard, cleaner
crates/spotless        the binary: argument parsing, printing, the TUI
```

`spotless-core` is **vendored** from the Spotless app's core crate — the files
are byte-identical copies, kept in step by `scripts/sync-core.sh`. That has two
consequences:

- Run `cargo fmt -p spotless`, never `cargo fmt --all`. Reformatting a vendored
  file makes `scripts/sync-core.sh --check` fail forever against a source that
  never changed.
- A fix to a core module is still welcome here. It travels back to the app on
  the next sync, which is the point of the arrangement.

## Before opening a pull request

```bash
cargo fmt -p spotless
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The tests are offline and hermetic: they build fixtures in temporary
directories and never touch your real home directory. Keep new ones that way.

## Adding a cleaning rule

Rules live in `crates/spotless-core/rules/*.toml` and are the one part of this
program that decides what may be removed, so they are held to a higher standard
than code:

- **Regenerable only.** If losing it costs the user anything they cannot get
  back by running a build or reopening an app, it does not belong in a rule.
- **Say what it is.** Every target needs a `description` that explains what the
  data is and what regenerates it. A test enforces this.
- **Pick the tier honestly.** `safe` means "no user data, rebuilt
  automatically". Anything that costs real time to rebuild is `caution`.
- **Prove the path.** Include, in the PR, where the path comes from — the tool's
  documentation, or the code that writes it.

## Licensing of contributions

This repository is **GPL-3.0-or-later**, and a contribution that touches only
`crates/spotless` (the CLI and TUI) is simply that: GPL, like everything around
it.

`crates/spotless-core` is the exception, and it is worth being blunt about why.
Those modules are also shipped inside the closed-source Spotless app, which
their copyright holder dual-licenses; the GPL is a one-way door, so a patch
offered only under the GPL would permanently fork the core in two and end the
sync described above. **By opening a pull request that changes a file under
`crates/spotless-core`, you agree that your change may also be distributed
under a proprietary licence as part of that app.** You keep your copyright, and
your change stays GPL here.

If that is not something you want to grant, say so in the pull request — a bug
report with a failing test, or a fix aimed at `crates/spotless`, is just as
welcome and carries no such condition.

## Reporting a safety bug

A path that gets removed and should not have been is the most serious kind of
bug this project can have. Please open an issue with the exact path, the command
you ran, and `spotless rules --toml` output if you changed the rules. If you
would rather not do that in public, the repository's security advisories are
enabled.
