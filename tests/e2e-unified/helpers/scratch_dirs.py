"""A temp dir a test makes by hand and the process removes at exit.

`tempfile.mkdtemp` leaves its dir behind; a test that calls it for a throwaway lock,
state or repo dir leaks one entry per run (the unit-test scratch the disk ladder's
scratch rung then counts as unattributed). Authority: `docs/goal/architecture/testing.md`
§ Test temp-dir hygiene — a test removes the temp dir it creates. Used where a helper
returns the dir to several callers and has no fixture to hang a teardown on.

Two things make that hold on Windows too. The removal clears a read-only file's bit and
tries it again: git writes every object file read-only, Windows refuses to delete a
read-only file, and a plain `rmtree(ignore_errors=True)` gave up on the first one and
left the whole repo dir — measured 2026-11-18 as 4,000+ leaked `win-per-gate-`,
`win-loop-`, `per-gate-` and `mac-loop-` trees in `%TEMP%`, each holding only its
`.git/objects`. And every dir is named under `fauna-test-`, the unit-test prefix of the disk
ladder's scratch sweep, so what a killed run never reaches its
exit to remove is attributed and taken after a day (`fleet-disk.md` § Attribution).
"""

from __future__ import annotations

import atexit
import os
import shutil
import stat
import sys
import tempfile

#: The scratch sweep's unit-test prefix: a killed run's leftovers go after a day.
PREFIX = "fauna-test-"

_MADE: list[str] = []


def _writable_retry(func, path, _exc) -> None:
    """Clear the read-only bit Windows refuses to delete through, and try once more."""
    try:
        os.chmod(path, stat.S_IWRITE)
        func(path)
    except OSError:
        pass


def remove_tree(d: str) -> None:
    """Remove `d` whole, read-only files included; whatever still refuses is left."""
    if sys.version_info >= (3, 12):
        shutil.rmtree(d, onexc=_writable_retry)
    else:  # `onerror`, the same handler, before 3.12
        shutil.rmtree(d, onerror=_writable_retry)


def _remove_all() -> None:
    for d in _MADE:
        remove_tree(d)


atexit.register(_remove_all)


def scratch_mkdtemp(prefix: str) -> str:
    """`tempfile.mkdtemp(prefix=…)` under `fauna-test-`, removed when the interpreter exits."""
    d = tempfile.mkdtemp(prefix=PREFIX + prefix)
    _MADE.append(d)
    return d
