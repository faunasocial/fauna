#!/usr/bin/env python3
"""A macOS binary or dylib this checkout links names no other checkout.

Why this check exists
---------------------
A Mach-O file names each dylib it loads by the path baked into it at link time
(LC_LOAD_DYLIB), copied from the dylib's own install name (LC_ID_DYLIB). An
absolute install name beats every `-rpath` the consumer's link set. On 2026-10-28
`otool -L` on one checkout's cgo bridge named
`<other checkout>/target/release/build/fauna-ffi/<hash>/out/libfauna_ffi.dylib`:
the compile cache had restored a sibling checkout's cdylib link, the install name
`ld` had derived from that checkout's output path came with it, and the bridge
aborted on start while the sibling rebuilt the file. The cdylib is now linked with
`@rpath/libfauna_ffi.dylib` (libs/fauna-ffi-cdylib/build.rs); this check is the
guard that keeps it so, run by the recipes that produce each file
(`mail-bridge-ffi` on the dylib, `_go-cgo-build` on every bridge binary).

Rule: every dylib reference and the file's own install name is `@rpath/`,
`@loader_path/` or `@executable_path/`-relative, a system path (`/usr/lib/`,
`/System/`), or a path under one of the `--root` directories this checkout owns.
Anything else is an error naming the file and the path. Off macOS there is no
install-name mechanism to guard (ELF records a soname, not a path), so the check
passes without running anything.

Usage: check_linked_paths.py --root DIR [--root DIR ...] FILE [FILE ...]
"""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

RELATIVE_PREFIXES = ("@rpath/", "@loader_path/", "@executable_path/")
SYSTEM_PREFIXES = ("/usr/lib/", "/System/")


def referenced_paths(otool_output: str) -> list[str]:
    """The paths of an `otool -L` or `otool -D` listing, header lines dropped.

    Both print the file name (per architecture, on a fat file) on a line ending
    in `:`, then one path per line — `-L` indented and followed by a
    `(compatibility version …)` note, `-D` flush left.
    """
    paths = []
    for line in otool_output.splitlines():
        line = line.strip()
        if not line or line.endswith(":"):
            continue
        paths.append(line.split(" (compatibility version", 1)[0].strip())
    return paths


def offending(paths: list[str], roots: list[Path]) -> list[str]:
    """The paths naming a location this checkout does not own."""
    bad = []
    for path in paths:
        if path.startswith(RELATIVE_PREFIXES + SYSTEM_PREFIXES):
            continue
        resolved = Path(os.path.normpath(path))
        if any(resolved.is_relative_to(root) for root in roots):
            continue
        bad.append(path)
    return bad


def _otool(flag: str, file: Path) -> str:
    return subprocess.run(
        ["otool", flag, str(file)], check=True, capture_output=True, text=True
    ).stdout


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", action="append", required=True, type=Path)
    parser.add_argument("files", nargs="+", type=Path)
    args = parser.parse_args(argv)
    if sys.platform != "darwin":
        return 0
    roots = [Path(os.path.normpath(r.resolve())) for r in args.root]
    failed = False
    for file in args.files:
        # `-L` on a dylib lists its own install name too; each path once.
        paths = list(dict.fromkeys(referenced_paths(_otool("-L", file)) + referenced_paths(_otool("-D", file))))
        for path in offending(paths, roots):
            failed = True
            print(
                f"check_linked_paths: {file} names {path} — outside this checkout "
                f"({', '.join(str(r) for r in roots)}); link the dylib with an "
                "@rpath install name (libs/fauna-ffi-cdylib/build.rs) and rebuild",
                file=sys.stderr,
            )
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
