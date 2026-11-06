"""What a user — or another tool — does to a windows cloud-files (cfapi) sync root,
driven from a test through the same Win32 calls, never through the agent.

A windows on-demand binding makes the bound directory a cfapi sync root the
agent registers and serves (`on-demand-files.md` § On-Demand Files). Explorer's
**"Free up space"** acts on such a root from OUTSIDE the agent: it is a pure
attribute write (the file goes *unpinned*), and the agent's pin reaction
observes it and frees the bytes (`bridge.rs::react_to_pin`).
:func:`free_up_space` writes the same attribute. Whether a directory IS such a
root is the cloud-files filter's own answer, read by :func:`is_sync_root`.

Process safety: this module spawns nothing.
"""

from __future__ import annotations

import ctypes
import ctypes.wintypes
from pathlib import Path

#: `FILE_ATTRIBUTE_PINNED` / `FILE_ATTRIBUTE_UNPINNED` — the two pin bits
#: Explorer's "Always keep on this device" / "Free up space" verbs write.
_FILE_ATTRIBUTE_PINNED = 0x00080000
_FILE_ATTRIBUTE_UNPINNED = 0x00100000
_INVALID_FILE_ATTRIBUTES = 0xFFFFFFFF

#: `CF_SYNC_ROOT_INFO_BASIC` — the smallest info class; its struct is one
#: `LARGE_INTEGER` (the root's file id), which this module never reads.
_CF_SYNC_ROOT_INFO_BASIC = 0
#: `HRESULT_FROM_WIN32(ERROR_CLOUD_FILE_NOT_UNDER_SYNC_ROOT)` — the filter's "no".
_NOT_UNDER_SYNC_ROOT = 0x80070186


def _under_sync_root(path: Path) -> bool:
    """Whether the cloud-files filter places ``path`` inside a registered sync
    root — ``CfGetSyncRootInfoByPath``, the probe `fauna_cfapi::is_filter_registered`
    makes. Any answer but yes or no raises, naming the HRESULT."""
    cld = ctypes.WinDLL("cldapi")
    probe = cld.CfGetSyncRootInfoByPath
    probe.argtypes = [
        ctypes.wintypes.LPCWSTR,
        ctypes.c_int,
        ctypes.c_void_p,
        ctypes.wintypes.DWORD,
        ctypes.POINTER(ctypes.wintypes.DWORD),
    ]
    # A plain signed result, never `ctypes.HRESULT`, which raises on the "no".
    probe.restype = ctypes.c_long
    info = ctypes.c_longlong()
    hr = probe(str(path), _CF_SYNC_ROOT_INFO_BASIC, ctypes.byref(info), ctypes.sizeof(info), None)
    if hr == 0:
        return True
    if hr & 0xFFFFFFFF == _NOT_UNDER_SYNC_ROOT:
        return False
    raise OSError(f"CfGetSyncRootInfoByPath({path}) failed: HRESULT 0x{hr & 0xFFFFFFFF:08X}")


def is_sync_root(path: Path) -> bool:
    """Whether ``path`` is itself a registered cfapi sync root right now — the
    windows on-demand root standing over a bound directory, as every other
    process on the box sees it (Explorer's cloud verbs key off the same
    registration). The filter answers "inside a root" for every path under one
    and cfapi refuses to nest roots, so ``path`` is the root exactly when it is
    inside one and its parent is not."""
    path = Path(path)
    return _under_sync_root(path) and not _under_sync_root(path.parent)


def free_up_space(path: Path) -> None:
    """Mark ``path`` unpinned (and not pinned) — Explorer's "Free up space",
    which is an attribute write and nothing more: the provider does the byte
    work when it sees it."""
    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    k32.GetFileAttributesW.restype = ctypes.wintypes.DWORD
    k32.GetFileAttributesW.argtypes = [ctypes.wintypes.LPCWSTR]
    k32.SetFileAttributesW.argtypes = [ctypes.wintypes.LPCWSTR, ctypes.wintypes.DWORD]
    attrs = k32.GetFileAttributesW(str(path))
    if attrs == _INVALID_FILE_ATTRIBUTES:
        raise OSError(f"GetFileAttributesW({path}) failed: Win32 error {ctypes.get_last_error()}")
    new = (attrs & ~_FILE_ATTRIBUTE_PINNED) | _FILE_ATTRIBUTE_UNPINNED
    if not k32.SetFileAttributesW(str(path), new):
        raise OSError(f"SetFileAttributesW({path}) failed: Win32 error {ctypes.get_last_error()}")

