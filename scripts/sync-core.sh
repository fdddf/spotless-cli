#!/usr/bin/env bash
#
# Vendor the shared core modules from the Spotless app repository.
#
# `crates/spotless-core` is a copy of the modules the GUI build and the CLI have
# in common. This script is the one-way road between them: it copies the files
# listed in MODULES (plus the TOML ruleset) and then reports what moved, so the
# diff can be reviewed like any other change rather than landing unseen.
#
# What it deliberately does NOT touch:
#   - src/lib.rs      — the CLI's crate root lists a subset of the modules
#   - Cargo.toml      — the CLI's dependency set is smaller
#
# The copies are byte-identical, which is what makes `--check` meaningful, so
# never run `cargo fmt` across the whole workspace: format the CLI crate alone
# (`cargo fmt -p spotless`). Reformatting a vendored file would show up here as
# permanent drift from a source that never changed.
#
# Usage:
#   scripts/sync-core.sh [path-to-app-repo]      # default: ../MacCleaner
#   scripts/sync-core.sh --check                 # exit 1 if anything differs

set -euo pipefail

MODULES=(
  apps capability cleaner devscan dupes model mounts orphans
  paths rules safety scanner trash usage
)

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
check_only=false
if [[ "${1:-}" == "--check" ]]; then
  check_only=true
  shift
fi
app="${1:-$here/../MacCleaner}"

src="$app/crates/maccleaner-core/src"
rules_src="$app/src-tauri/rules"
dst="$here/crates/spotless-core/src"
rules_dst="$here/crates/spotless-core/rules"

if [[ ! -d "$src" ]]; then
  echo "error: no core crate at $src" >&2
  echo "       pass the app repository as the first argument" >&2
  exit 1
fi

changed=0
copy() {
  local from="$1" to="$2"
  if ! cmp -s "$from" "$to"; then
    changed=$((changed + 1))
    if $check_only; then
      echo "differs: ${to#"$here"/}"
    else
      cp "$from" "$to"
      echo "updated: ${to#"$here"/}"
    fi
  fi
}

for module in "${MODULES[@]}"; do
  copy "$src/$module.rs" "$dst/$module.rs"
done
for rules in "$rules_src"/*.toml; do
  copy "$rules" "$rules_dst/$(basename "$rules")"
done

if [[ $changed -eq 0 ]]; then
  echo "core is in sync with $app"
  exit 0
fi

if $check_only; then
  echo
  echo "$changed file(s) differ — run scripts/sync-core.sh to pull them in"
  exit 1
fi

echo
echo "$changed file(s) updated — run 'cargo test --workspace' and review the diff"
