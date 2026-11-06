"""A temp dir a test makes by hand and the process removes at exit.

`tempfile.mkdtemp` leaves its dir behind; a test that calls it for a throwaway lock,
state or repo dir leaks one entry per run (the unit-test scratch the disk ladder's
scratch rung then counts as unattributed). Authority: `docs/goal/architecture/testing.md`
§ Test temp-dir hygiene — a test removes the temp dir it creates. Used where a helper
returns the dir to several callers and has no fixture to hang a teardown on.
"""

from __future__ import annotations

import atexit
import shutil
import tempfile

_MADE: list[str] = []


def _remove_all() -> None:
    for d in _MADE:
        shutil.rmtree(d, ignore_errors=True)


atexit.register(_remove_all)


def scratch_mkdtemp(prefix: str) -> str:
    """`tempfile.mkdtemp(prefix=…)`, removed when the interpreter exits."""
    d = tempfile.mkdtemp(prefix=prefix)
    _MADE.append(d)
    return d
