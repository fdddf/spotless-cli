#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Record the README's demo as an asciicast, without a human at the keyboard.

    scripts/record-demo.py docs/demo.cast
    agg --speed 1.6 --idle-time-limit 1 docs/demo.cast docs/demo.gif

The TUI needs a real terminal, so this drives one: it allocates a pty, sizes it,
runs `spotless` inside it, and plays a fixed script of keystrokes against it
while timestamping everything that comes back. The result is a plain asciicast
v2 file, which `agg` turns into the GIF.

Scripting it rather than recording by hand means the demo is reproducible: the
same keys land at the same beats every time, so re-recording after a UI change
is one command and the GIF never drifts from what the program does.

The demo deliberately declines the confirmation at the end. Nothing is removed
while recording, and the frame that matters — the program asking before it acts
— is the one thing worth showing anyway.
"""

import fcntl
import json
import os
import pty
import select
import signal
import struct
import sys
import termios
import time

# Wide enough for the size/safety/name columns without wrapping, short enough
# that the GIF stays a readable size on a README.
COLS, ROWS = 92, 26

# ("wait", seconds) pauses while still recording output; ("keys", text, gap)
# types `text` one character at a time, `gap` seconds apart.
SCRIPT = [
    # The opening scan. Its progress line is the point: a cleaner that sits
    # silent for ten seconds looks broken.
    ("wait", 11.5),
    # Tick the two biggest rows. Both are `caution`, so neither arrives ticked
    # — this is the user opting in, and the header total climbing is the whole
    # argument for the tool.
    ("keys", " ", 0.0),
    ("wait", 1.0),
    ("keys", "j", 0.0),
    ("keys", " ", 0.0),
    ("wait", 1.2),
    ("keys", "jj", 0.4),
    ("wait", 0.8),
    ("keys", "c", 0.0),
    ("wait", 2.6),
    # Declined: nothing is removed while recording, and the frame worth showing
    # is the program asking.
    ("keys", "n", 0.0),
    # Ends on the list rather than on another tab: this is the frame the GIF
    # loops on, and "here is what you could reclaim, nothing was touched" is
    # the one worth leaving on screen.
    ("wait", 2.2),
]


def set_size(fd: int, cols: int, rows: int) -> None:
    """Tell the pty how big it is, or the TUI draws into a 0x0 terminal."""
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))


def record(command: list[str]) -> list[tuple[float, str]]:
    """Run `command` in a pty, play the script at it, return timed output."""
    pid, fd = pty.fork()
    if pid == 0:
        os.environ["TERM"] = "xterm-256color"
        # A cleaner reports on the home directory it is pointed at; keep the
        # recording honest by leaving that alone and only fixing the terminal.
        os.environ["COLUMNS"], os.environ["LINES"] = str(COLS), str(ROWS)
        os.execvp(command[0], command)

    set_size(fd, COLS, ROWS)
    events: list[tuple[float, str]] = []
    start = time.time()

    def drain(until: float) -> None:
        """Read whatever the program says until `until`, timestamping it."""
        while True:
            left = until - time.time()
            if left <= 0:
                return
            readable, _, _ = select.select([fd], [], [], left)
            if not readable:
                continue
            try:
                chunk = os.read(fd, 65536)
            except OSError:  # the child closed the pty: it has exited
                return
            if not chunk:
                return
            events.append((time.time() - start, chunk.decode("utf8", "replace")))

    for step in SCRIPT:
        if step[0] == "wait":
            drain(time.time() + step[1])
        else:
            _, keys, gap = step
            for key in ([keys] if len(keys) > 1 and keys[0] == "\x1b" else keys):
                os.write(fd, key.encode())
                drain(time.time() + max(gap, 0.12))

    # Quit outside the recording. The program has to exit for the pty to close,
    # but the alt-screen teardown it prints on the way out is a second of blank
    # terminal at the end of a looping GIF.
    os.write(fd, b"q")
    time.sleep(0.3)
    os.close(fd)
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    os.waitpid(pid, 0)
    return events


def write_cast(path: str, events: list[tuple[float, str]]) -> None:
    """Write asciicast v2: a header line, then one JSON array per chunk."""
    header = {
        "version": 2,
        "width": COLS,
        "height": ROWS,
        "timestamp": int(time.time()),
        "env": {"TERM": "xterm-256color", "SHELL": "/bin/zsh"},
    }
    with open(path, "w", encoding="utf8") as cast:
        cast.write(json.dumps(header) + "\n")
        for at, data in events:
            cast.write(json.dumps([round(at, 6), "o", data]) + "\n")


def main(argv: list[str]) -> int:
    if len(argv) < 2:
        raise SystemExit(f"usage: {argv[0]} <output.cast> [command...]")
    out = argv[1]
    command = argv[2:] or ["spotless"]

    events = record(command)
    if not events:
        raise SystemExit("error: the program produced no output — is it installed?")
    write_cast(out, events)
    print(f"{out}: {len(events)} chunks over {events[-1][0]:.1f}s")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
