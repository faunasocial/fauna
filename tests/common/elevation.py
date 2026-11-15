r"""Whether this run is elevated, and the one thing an elevated pytest run does differently.

Owner: `docs/goal/architecture/testing.md` § Test temp-dir hygiene, the elevated-run
paragraph.

On Windows, Python (3.13 and later) makes a directory asked for with mode `0o700` under a
protected DACL granting only the directory's owner, SYSTEM and Administrators — pytest's
base temp and every per-test dir, and every `tempfile.mkdtemp`. An elevated process's
objects are owned by Administrators, which the user's ordinary (filtered) token holds
deny-only, so such a dir an elevated run made in the user's shared `%TEMP%` opens to no
unelevated process: the user's later runs skip it in their cleanup without a word, the
disk sweep can neither read nor remove it, and only another elevated shell can.

So an elevated run that names no temp root of its own (no `--basetemp`, no
`PYTEST_DEBUG_TEMPROOT`) keeps its Python temp under `%TEMP%\fauna-elevated-pytest`:
pytest's temp root (its documented `PYTEST_DEBUG_TEMPROOT`) and the process's
`tempfile.tempdir` both point there, set from `pytest_configure` before the first temp
dir is made. Elevated runs reclaim that root themselves: pytest's own cleanup keeps its
newest three numbered dirs there as anywhere, and each elevated run first removes what
earlier ones left beside them (a kept or killed run's `mkdtemp` dirs) once it is a week
old. Child processes keep the user's `TEMP`: what Rust, Go and .NET make there inherits
`%TEMP%`'s own ACL and stays readable. The helper is stdlib-only: the pytest roots outside
the e2e suite load it by path, without the `common` package's imports.
"""

from __future__ import annotations

import os
import shutil
import sys
import tempfile
import time
from collections.abc import MutableMapping

ELEVATED_TEMPROOT_NAME = "fauna-elevated-pytest"
TEMPROOT_ENV = "PYTEST_DEBUG_TEMPROOT"
#: The fleet's scratch backstop (fleet-disk.md § The ladder, rung 0): what an elevated
#: run left beside pytest's own dirs is reclaimed by the next elevated run past it.
STALE_DAYS = 7
_PYTEST_ROOT_PREFIX = "pytest-of-"


def is_elevated() -> bool:
    """Whether this process runs elevated: an administrator's full token in force."""
    if sys.platform != "win32":
        return False
    import ctypes

    return ctypes.windll.shell32.IsUserAnAdmin() != 0


def elevated_temproot(
    *, platform: str, elevated: bool, environ: MutableMapping[str, str], tempdir: str,
) -> str | None:
    """The temp root this run takes, or None to leave the run's temp alone: only an
    elevated run on Windows whose environment names no temp root of its own moves."""
    if platform != "win32" or not elevated or environ.get(TEMPROOT_ENV):
        return None
    return os.path.join(tempdir, ELEVATED_TEMPROOT_NAME)


def reclaim_stale(root: str, *, now: float, days: float = STALE_DAYS) -> list[str]:
    """Remove the top-level entries of the elevated root that earlier elevated runs left
    and nothing has written for `days`, and return their names. pytest's own
    `pytest-of-<user>` is left to pytest's keep; a link is never followed."""
    removed = []
    for entry in os.scandir(root):
        if entry.name.startswith(_PYTEST_ROOT_PREFIX):
            continue
        try:
            if entry.is_symlink() or os.path.isjunction(entry.path):
                continue
            if now - entry.stat(follow_symlinks=False).st_mtime < days * 86400:
                continue
            if entry.is_dir(follow_symlinks=False):
                shutil.rmtree(entry.path)
            else:
                os.remove(entry.path)
        except OSError:
            continue  # held open, or raced away: the next elevated run looks again
        removed.append(entry.name)
    return removed


def take_elevated_temproot(
    config,
    *,
    platform: str = sys.platform,
    elevated: bool | None = None,
    environ: MutableMapping[str, str] | None = None,
    tempdir: str | None = None,
    now: float | None = None,
) -> str | None:
    """Move an elevated run's Python temp to the elevated root and return it (None when
    the run keeps its own). Call it first in `pytest_configure`, before anything makes a
    temp dir; an explicit `--basetemp` wins and is left alone."""
    if config.getoption("basetemp", None):
        return None
    environ = os.environ if environ is None else environ
    root = elevated_temproot(
        platform=platform,
        elevated=is_elevated() if elevated is None else elevated,
        environ=environ,
        tempdir=tempfile.gettempdir() if tempdir is None else tempdir,
    )
    if root is None:
        return None
    os.makedirs(root, exist_ok=True)
    reclaim_stale(root, now=time.time() if now is None else now)
    environ[TEMPROOT_ENV] = root
    tempfile.tempdir = root
    return root
