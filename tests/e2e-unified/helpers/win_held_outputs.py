"""Free a build output a running process holds, so the build's retry can land.

Windows refuses to delete or overwrite an image a live process has mapped — a
running exe, a loaded dll — but allows a RENAME, and the holder keeps running on
the renamed file. Linux and macOS replace a running binary silently, so on those
machines a second e2e run from the same checkout simply relinks it; on Windows the
same relink failed the run:

- cargo's uplift cannot remove the old artifact: ``error: failed to remove file
  `…\\target\\debug\\fauna-sync-agent.exe``` — measured 2026-11-10, a tui run
  started beside a windows run from one checkout; both of `tui-debug`'s attempts
  died on it and every tui test errored at setup;
- a recipe's plain ``cp`` over a mapped dll: ``cp: cannot create regular file
  '…/target/fauna_ffi.dll': Device or resource busy`` (the Go/cgo recipes stage
  through ``scripts/win-stage-dll.sh`` since; this catches any copy still bare).

`move_aside_held_outputs` reads a failed build attempt's output for those two
signatures and renames each named file aside (``<name>.old.<pid>.<n>``) so the
retry can write it; earlier asides nothing holds any more are deleted
best-effort, as ``win-stage-dll.sh`` does. It touches only files under the
roots it is given (the checkout's cargo target dir), never anything else a
message names. The build-or-raise retry in ``conftest.py`` calls it between its
two attempts (``build-target-layout-windows.md`` § Cargo target dir layout (win)).
"""

from __future__ import annotations

import os
import re
import sys
import time
from pathlib import Path

# cargo's uplift (`std::fs::remove_file` → DeleteFileW, refused on a mapped image).
_CARGO_REMOVE = re.compile(r"failed to remove file `([^`]+)`")
# MSYS cp over a mapped image.
_CP_BUSY = re.compile(r"cp: cannot create regular file '([^']+)': Device or resource busy")


def held_paths(lines) -> list[str]:
    """Every file a build output says it could not replace, in order, once each."""
    found: list[str] = []
    for line in lines:
        for pattern in (_CARGO_REMOVE, _CP_BUSY):
            for match in pattern.finditer(line):
                if match.group(1) not in found:
                    found.append(match.group(1))
    return found


def _resolved(path: str) -> Path | None:
    # MSYS spells a drive path `/d/src/...`; cargo and cp spell it `D:\...` or `D:/...`.
    msys = re.match(r"^/([a-zA-Z])/(.*)$", path)
    if msys:
        path = f"{msys.group(1)}:/{msys.group(2)}"
    try:
        return Path(path).resolve()
    except OSError:
        return None


def _under(path: Path, roots) -> bool:
    for root in roots:
        try:
            path.relative_to(Path(root).resolve())
            return True
        except ValueError:
            continue
    return False


def _sweep_asides(path: Path) -> None:
    for old in path.parent.glob(f"{path.name}.old.*"):
        try:
            old.unlink()
        except OSError:
            pass  # still mapped by its holder: the next pass retries it


def move_aside_held_outputs(lines, roots, *, platform: str = sys.platform) -> list[Path]:
    """Rename aside each file the output says it could not replace, if it lies
    under one of ``roots``; return the files moved. A no-op off windows."""
    if platform != "win32":
        return []
    moved: list[Path] = []
    for raw in held_paths(lines):
        path = _resolved(raw)
        if path is None or not path.is_file() or not _under(path, roots):
            continue
        aside = path.with_name(f"{path.name}.old.{os.getpid()}.{time.time_ns()}")
        try:
            os.replace(path, aside)
        except OSError:
            continue
        moved.append(path)
        _sweep_asides(path)
    return moved
