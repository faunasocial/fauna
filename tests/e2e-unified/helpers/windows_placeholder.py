"""Observe + drive a Windows cfapi placeholder from OUTSIDE the provider process.

Why this exists at all: **cfapi fires no callback for I/O originating in the
provider's own process** (``docs/goal/behavior/file-sync.md`` § *A placeholder is
present-but-unreadable*), and the sync host (``fauna-sync-agent.exe``) *is* the
provider. So a placeholder can only be hydrated by some *other* process, and a
hydration test that reads the file in-process is testing nothing. The pytest
process is not the provider, so a read driven from here is a genuine
OS→provider fetch — the same event a user's double-click in Explorer raises.

**Why ``cmd /c type`` rather than ``start``.** The user's gesture is a
double-click, whose cfapi-visible effect is "the shell opened the file → read →
``FETCH_DATA``". ``start`` reproduces that by launching the file's *associated
GUI app*, which on a shared dev box leaks a GUI process per call and makes the
read's timing nondeterministic. ``cmd /c type`` raises the identical fetch from
an equally-external process, deterministically, and exits. Both are "another
process reads it"; only one leaves nothing behind. (Neither uses FlaUI
``SendInput``, which is unreliable on long win-arm64 OS-shell runs —
``reference_windows_e2e_flake``.)

**The ground truth is the OS, not the UI.** A cloud-only placeholder carries
``FILE_ATTRIBUTE_OFFLINE`` / ``FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS``; *"a
hydrated file no longer carries [them]"* (``file-sync.md`` § *A placeholder is
present-but-unreadable*). That is what these helpers assert on — an OS file
attribute a third process can read — never a byte count (a placeholder's
``metadata.len()`` is already the file's *real* size, so size proves nothing)
and never a screenshot.
"""

from __future__ import annotations

import ctypes
import subprocess
import time

# winnt.h. A cloud-only placeholder carries these; a hydrated file does not.
FILE_ATTRIBUTE_OFFLINE = 0x00001000
FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS = 0x00400000
_INVALID_FILE_ATTRIBUTES = 0xFFFFFFFF

# GetFileAttributesW returns a DWORD, and its error sentinel is the all-ones
# INVALID_FILE_ATTRIBUTES. ctypes defaults an unprototyped call's restype to the
# SIGNED c_int, which turns that sentinel into -1 — so an `== 0xFFFFFFFF` check
# silently never fires and, far worse, `-1 & (OFFLINE|RECALL)` is non-zero, i.e.
# an ABSENT file would read as a placeholder and a never-delivered file would
# satisfy wait_for_placeholder(). Declaring the real signature is what keeps the
# sentinel comparable. (`use_last_error` likewise: ctypes.get_last_error() only
# returns a real GetLastError for a WinDLL that captures it.)
_k32 = ctypes.WinDLL("kernel32", use_last_error=True)
_k32.GetFileAttributesW.argtypes = [ctypes.c_wchar_p]
_k32.GetFileAttributesW.restype = ctypes.c_uint32

# A cfapi fetch is served by the provider over the network; the platform's own
# recall timeout is 60 s (file-sync.md), so a read that is going to succeed has
# succeeded well inside this. Bounded always, unbounded never (testing.md).
_DEFAULT_TIMEOUT = 45.0


def file_attributes(path: str) -> int:
    """Raw ``GetFileAttributesW`` for ``path``.

    Raises ``FileNotFoundError`` if the file is absent, so an absent file can
    never be mistaken for "no placeholder bits set" (i.e. for a hydrated one).
    """
    attrs = _k32.GetFileAttributesW(str(path))
    if attrs == _INVALID_FILE_ATTRIBUTES:
        raise FileNotFoundError(
            f"GetFileAttributesW failed for {path!r} "
            f"(WinError {ctypes.get_last_error()}) — the file is absent, not dehydrated"
        )
    return attrs


def is_placeholder(path: str) -> bool:
    """Whether ``path`` is a cloud-only placeholder (bytes not on disk).

    Mirrors the shared-Rust predicate ``fauna_sync_engine::placeholder`` uses, so
    the test and the engine agree on what "cloud-only" means.
    """
    attrs = file_attributes(path)
    return bool(attrs & (FILE_ATTRIBUTE_OFFLINE | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS))


def wait_for_placeholder(path: str, timeout: float = _DEFAULT_TIMEOUT) -> bool:
    """Poll until ``path`` exists AND reads as a cloud-only placeholder.

    Population is asynchronous (the host folds the set's ``changes.list`` into
    ``Placeholder`` rows, then ``CfExecute(TRANSFER_PLACEHOLDERS)`` materializes
    them), so the file appears a beat after the bind. Returns the final state
    rather than raising, so the caller's assertion surfaces the mismatch.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            if is_placeholder(path):
                return True
        except FileNotFoundError:
            pass
        time.sleep(0.25)
    try:
        return is_placeholder(path)
    except FileNotFoundError:
        return False


def hydrate_via_shell(path: str, timeout: float = 30.0) -> subprocess.CompletedProcess:
    """Read ``path`` from ANOTHER process, raising the cfapi fetch a double-click raises.

    ``cmd /c type`` streams the file's bytes, which is exactly the OS→provider
    ``FETCH_DATA`` a shell open produces — but from a process that exits, with no
    GUI app left behind (see module docstring). Output is discarded: the bytes
    are not the assertion, the attribute flip is.

    Does NOT assert success — a cfapi fetch that no provider answers blocks for
    the platform's 60 s recall timeout and then fails
    (``ERROR_CLOUD_FILE_REQUEST_TIMEOUT``); the caller decides what that means.
    The ``timeout`` here is deliberately *under* that 60 s so a dead provider
    surfaces as our own bounded failure rather than a mystery stall.
    """
    return subprocess.run(
        ["cmd", "/c", "type", str(path)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        timeout=timeout,
        check=False,
    )


def wait_for_hydrated(path: str, timeout: float = _DEFAULT_TIMEOUT) -> bool:
    """Poll until ``path`` has SHED its placeholder bits — i.e. is really local.

    This is the hydration assertion: the OS itself reporting the bytes are on
    disk. Returns the final state so the caller's assertion can print it.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            if not is_placeholder(path):
                return True
        except FileNotFoundError:
            pass
        time.sleep(0.25)
    try:
        return not is_placeholder(path)
    except FileNotFoundError:
        return False


def describe(path: str) -> str:
    """One-line diagnostic for a failure message — failures must diagnose
    themselves (testing.md § convention 6), and "expected hydrated, got
    placeholder" is useless without the actual bits."""
    try:
        attrs = file_attributes(path)
    except FileNotFoundError:
        return f"{path}: ABSENT"
    flags = []
    if attrs & FILE_ATTRIBUTE_OFFLINE:
        flags.append("OFFLINE")
    if attrs & FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS:
        flags.append("RECALL_ON_DATA_ACCESS")
    return f"{path}: attrs=0x{attrs:08X} [{', '.join(flags) or 'no placeholder bits'}]"
