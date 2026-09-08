#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Point a Homebrew formula at a newly released version.

Used by the release workflow to bump `fdddf/homebrew-tap`, and runnable by hand
when a release has to be re-cut:

    scripts/bump-formula.py Formula/spotless.rb v0.2.0 <sha256>

The three fields are rewritten in place rather than the file being regenerated
from a template, so anything hand-added to the formula — a caveat, an extra
test, a dependency — survives the bump.
"""

import pathlib
import re
import sys

URL = (
    "https://github.com/fdddf/spotless-cli/releases/download/"
    "{tag}/spotless-macos-universal.tar.gz"
)


def bump(text: str, tag: str, sha256: str) -> str:
    """Return `text` with its url, sha256 and version pointing at `tag`."""
    version = tag.lstrip("v")
    replacements = [
        (r'url "[^"]*"', f'url "{URL.format(tag=tag)}"'),
        (r'sha256 "[^"]*"', f'sha256 "{sha256}"'),
        (r'version "[^"]*"', f'version "{version}"'),
    ]
    for pattern, replacement in replacements:
        # A formula that does not carry all three fields is not one this knows
        # how to edit, and half-editing it would publish a version whose
        # checksum belongs to another build.
        text, count = re.subn(pattern, replacement, text, count=1)
        if count != 1:
            raise SystemExit(f"error: formula has no field matching /{pattern}/")
    return text


def main(argv: list[str]) -> int:
    if len(argv) != 4:
        raise SystemExit(f"usage: {argv[0]} <formula.rb> <tag> <sha256>")
    _, formula, tag, sha256 = argv

    if not re.fullmatch(r"[0-9a-f]{64}", sha256):
        raise SystemExit(f"error: {sha256!r} is not a sha256 digest")

    path = pathlib.Path(formula)
    path.write_text(bump(path.read_text(), tag, sha256))
    print(f"{path} now points at {tag}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
