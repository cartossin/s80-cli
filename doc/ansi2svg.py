#!/usr/bin/env python3
"""Render a pty capture of s80 (truecolor ANSI) into an SVG for the README.

Usage:  script -q cap.txt ./target/release/s80 -c 280 1.1.1.1
        python3 doc/ansi2svg.py cap.txt "s80 1.1.1.1" > doc/example.svg

Implements just enough of a terminal to replay s80's output: SGR truecolor,
reset, CR/LF, backspace, and the save/up/column/restore sequence the late-
reply repaint uses.
"""

import re
import sys

FG_DEFAULT = "#c9d1d9"
BG = "#0d1117"
PROMPT = "#7ee787"
MUTED = "#8b949e"
FONT = "SF Mono, Menlo, DejaVu Sans Mono, Consolas, monospace"
FS = 14          # font size
CH = 8.43        # monospace advance at FS
LH = 19          # line height
PAD = 16

ESC = re.compile(
    r"\x1b\[38;2;(\d+);(\d+);(\d+)m"   # 1-3: truecolor fg
    r"|\x1b\[0m"                        # reset
    r"|\x1b7"                           # save cursor
    r"|\x1b8"                           # restore cursor
    r"|\x1b\[(\d+)A"                    # 4: cursor up
    r"|\x1b\[(\d+)G"                    # 5: cursor to column (1-based)
    r"|\x1b\[38;5;(\d+)m"               # 6: 256-color fg (unused in captures)
    r"|\x1b\[\??[0-9;]*[A-Za-z]"        # any other CSI: ignore
)


def replay(data: str):
    grid = {}
    row = col = 0
    color = None
    saved = (0, 0)
    i = 0
    while i < len(data):
        m = ESC.match(data, i)
        if m:
            if m.group(1) is not None:
                color = (int(m.group(1)), int(m.group(2)), int(m.group(3)))
            elif m.group(0) == "\x1b[0m":
                color = None
            elif m.group(0) == "\x1b7":
                saved = (row, col)
            elif m.group(0) == "\x1b8":
                row, col = saved
            elif m.group(4) is not None:
                row = max(0, row - int(m.group(4)))
            elif m.group(5) is not None:
                col = int(m.group(5)) - 1
            i = m.end()
            continue
        c = data[i]
        if c == "\n":
            row += 1
            col = 0
        elif c == "\r":
            col = 0
        elif c == "\x08":
            col = max(0, col - 1)
        elif c.isprintable():
            grid[(row, col)] = (c, color)
            col += 1
        i += 1
    return grid


def esc(s: str) -> str:
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def main():
    data = open(sys.argv[1], encoding="utf-8", errors="replace").read()
    cmd = sys.argv[2] if len(sys.argv) > 2 else None
    grid = replay(data)
    rows = max(r for r, _ in grid) + 1 if grid else 0

    lines = []  # list of list[(start_col, text, color)]
    if cmd:
        lines.append([(0, "$ ", PROMPT), (2, cmd, FG_DEFAULT)])
    for r in range(rows):
        cols = [c for (rr, c) in grid if rr == r]
        if not cols:
            lines.append([])
            continue
        runs, cur, curcol, start = [], "", "start", 0
        for c in range(max(cols) + 1):
            ch, colr = grid.get((r, c), (" ", None))
            hexc = "#{:02x}{:02x}{:02x}".format(*colr) if colr else FG_DEFAULT
            if hexc != curcol:
                if cur:
                    runs.append((start, cur, curcol))
                cur, curcol, start = ch, hexc, c
            else:
                cur += ch
        if cur:
            runs.append((start, cur, curcol))
        # drop pure-space runs: explicit x positioning makes them dead weight
        lines.append([(st, t, colr) for st, t, colr in runs if t.strip()])

    width = round(80 * CH + 2 * PAD)
    height = len(lines) * LH + 2 * PAD
    out = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" '
        f'width="{width}" height="{height}" role="img" aria-label="s80 terminal output">',
        f'<rect width="{width}" height="{height}" rx="8" fill="{BG}"/>',
        f'<g font-family="{FONT}" font-size="{FS}" xml:space="preserve">',
    ]
    for n, runs in enumerate(lines):
        y = PAD + (n + 1) * LH - 5
        if not runs:
            continue
        # x pins each run to its column; textLength forces the run to
        # occupy exactly its cells no matter which monospace font the
        # viewer has — without it, wide fonts overflow the card
        spans = "".join(
            f'<tspan x="{PAD + st * CH:.1f}" textLength="{len(text) * CH:.1f}" '
            f'lengthAdjust="spacingAndGlyphs" fill="{colr}">{esc(text)}</tspan>'
            for st, text, colr in runs
        )
        out.append(f'<text y="{y}">{spans}</text>')
    out.append("</g></svg>")
    print("\n".join(out))


if __name__ == "__main__":
    main()
