"""What a user — or another tool — does to a windows cloud-files (cfapi) sync root,
driven from a test through the same Win32 calls, never through the agent.

A windows on-demand binding makes the bound directory a cfapi sync root the
agent registers and serves (`on-demand-files.md` § On-Demand Files). Explorer's
**"Free up space"** acts on such a root from OUTSIDE the agent: it is a pure
attribute write (the file goes *unpinned*), and the agent's pin reaction
observes it and frees the bytes (`bridge.rs::react_to_pin`).
:func:`free_up_space` writes the same attribute.

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

