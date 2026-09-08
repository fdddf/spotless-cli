#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Install Spotless from a GitHub release.
#
#   curl -fsSL https://raw.githubusercontent.com/fdddf/spotless-cli/main/install.sh | sh
#
# No Rust toolchain required -- this fetches the prebuilt universal binary. The
# script is deliberately plain POSIX sh and does nothing a reader cannot check
# in one sitting: pipe-to-shell is a lot of trust to ask for, so the least it
# can do is be short.
#
# Environment:
#   SPOTLESS_VERSION   tag to install (default: the latest release)
#   SPOTLESS_BIN_DIR   where to put the binary (default: /usr/local/bin, or
#                      ~/.local/bin when that is not writable)
#   SPOTLESS_BASE_URL  where to fetch the archive from, for testing this script
#                      or installing from a fork's releases

set -eu

REPO="fdddf/spotless-cli"
ASSET="spotless-macos-universal.tar.gz"

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = "Darwin" ] || die "Spotless is macOS-only (this is $(uname -s))."
command -v curl >/dev/null 2>&1 || die "curl is required."

version="${SPOTLESS_VERSION:-latest}"
if [ -n "${SPOTLESS_BASE_URL:-}" ]; then
  base="$SPOTLESS_BASE_URL"
elif [ "$version" = "latest" ]; then
  base="https://github.com/$REPO/releases/latest/download"
else
  base="https://github.com/$REPO/releases/download/$version"
fi

# Pick a destination the user can actually write to, rather than assuming sudo.
if [ -n "${SPOTLESS_BIN_DIR:-}" ]; then
  bin_dir="$SPOTLESS_BIN_DIR"
elif [ -w /usr/local/bin ] 2>/dev/null; then
  bin_dir="/usr/local/bin"
else
  bin_dir="$HOME/.local/bin"
fi
mkdir -p "$bin_dir" || die "cannot create $bin_dir"

tmp="$(mktemp -d)"
# shellcheck disable=SC2064  # $tmp is expanded now on purpose.
trap "rm -rf '$tmp'" EXIT INT TERM

say "Downloading $ASSET..."
curl -fsSL "$base/$ASSET" -o "$tmp/$ASSET" \
  || die "download failed -- is there a release at $base?"

# The checksum is published beside the archive. A mismatch means the download
# is not the artifact CI built, which is worth stopping for.
if curl -fsSL "$base/$ASSET.sha256" -o "$tmp/$ASSET.sha256" 2>/dev/null; then
  say "Verifying checksum..."
  expected="$(cut -d' ' -f1 <"$tmp/$ASSET.sha256")"
  actual="$(shasum -a 256 "$tmp/$ASSET" | cut -d' ' -f1)"
  [ "$expected" = "$actual" ] || die "checksum mismatch -- refusing to install."
else
  say "warning: no published checksum to verify against."
fi

tar -xzf "$tmp/$ASSET" -C "$tmp"
[ -f "$tmp/spotless" ] || die "the archive did not contain a spotless binary."
chmod +x "$tmp/spotless"

# Only present when the archive arrived through a browser; harmless otherwise.
xattr -d com.apple.quarantine "$tmp/spotless" 2>/dev/null || true

if mv "$tmp/spotless" "$bin_dir/spotless" 2>/dev/null; then
  :
elif command -v sudo >/dev/null 2>&1; then
  say "Installing to $bin_dir needs sudo."
  sudo mv "$tmp/spotless" "$bin_dir/spotless" || die "could not install to $bin_dir"
else
  die "cannot write to $bin_dir -- set SPOTLESS_BIN_DIR to somewhere you can."
fi

say ""
say "Installed $("$bin_dir/spotless" --version) to $bin_dir/spotless"

case ":$PATH:" in
  *":$bin_dir:"*) say "Run 'spotless' to start." ;;
  *)
    say ""
    say "$bin_dir is not on your PATH. Add it:"
    say "  echo 'export PATH=\"$bin_dir:\$PATH\"' >> ~/.zshrc && exec zsh"
    ;;
esac
