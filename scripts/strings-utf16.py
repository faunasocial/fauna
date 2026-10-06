#!/usr/bin/env python3
"""A `strings` that also sees UTF-16LE — the .NET half of a store-safe artifact scan.

WHY THIS EXISTS. Every other `*-store-safe-check` column discharges criteria 1-2
(`dynamic-features.md` § What "completely compiled away" means) with
`strings -a <artifact> | grep -c <pattern>`. That works on an ELF, a Mach-O and a
dex because their string data is UTF-8. It does NOT work on a .NET assembly:

  * a `const string` is stored in the `Constant` metadata table as a **UTF-16LE**
    `#Blob` entry, and
  * every `ldstr` literal — which is what the WinUI XAML compiler emits for an
    `AutomationProperties.AutomationId` — lives in the **UTF-16LE** `#US` heap.

GNU `strings` could read those with `-e l`, but the `strings` on Windows is
`llvm-strings`, whose option set has no encoding flag at all (`--bytes`,
`--radix`, `--print-file-name`, and that is the list). So a verbatim copy of
tui's column would report **0 occurrences in both columns** of a two-column
witness — indistinguishable from a perfect excision, and green. That is the
precise vacuity the second column exists to catch, one layer below where it can
catch it: column 2 asserts the DEFAULT artifact carries the pattern, so an
encoding-blind scan turns the whole witness red rather than falsely green. Either
way the witness is useless until the scan can see the bytes.

WHAT IT PRINTS. One line per printable run of at least `--bytes` characters, from
both encodings, in no particular order: ASCII/UTF-8 runs first, then UTF-16LE
runs. It is a scanner for greps, not a faithful `strings` clone — offsets, file
names and radix formatting are deliberately absent because no caller wants them.

UTF-16LE runs are found without assuming alignment: the pattern anchors on a
printable byte followed by NUL, so a string starting at an odd offset is found
exactly as one at an even offset. The two passes cannot double-report a single
run — an ASCII run has no interleaved NULs and a UTF-16LE run has no four
consecutive printable bytes — which matters because every consumer counts
occurrences rather than testing presence.
"""

from __future__ import annotations

import argparse
import pathlib
import re
import sys

PRINTABLE = rb"[\x20-\x7e]"


def runs(data: bytes, minimum: int) -> list[str]:
    out: list[str] = []
    for m in re.finditer(PRINTABLE + rb"{%d,}" % minimum, data):
        out.append(m.group().decode("ascii"))
    for m in re.finditer(rb"(?:" + PRINTABLE + rb"\x00){%d,}" % minimum, data):
        out.append(m.group().decode("utf-16-le"))
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("files", nargs="+", type=pathlib.Path)
    ap.add_argument(
        "-n",
        "--bytes",
        type=int,
        default=4,
        dest="minimum",
        help="minimum run length (default 4, matching strings(1))",
    )
    args = ap.parse_args()
    if args.minimum < 1:
        ap.error("--bytes must be at least 1")
    write = sys.stdout.write
    for path in args.files:
        try:
            data = path.read_bytes()
        except OSError as exc:
            print(f"strings-utf16: {path}: {exc}", file=sys.stderr)
            return 1
        for line in runs(data, args.minimum):
            write(line + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
