"""A background tester's shard runs at normal priority.

`delayed-testing.md` § The background tester: a shard is an e2e run, so it is not
niced and the tester's deference is by stopping. On Windows the win check's launcher
starts its root below normal, and Windows hands that class to every child a
below-normal process creates, so a shard would otherwise run its nest and
its app below every session on the box. Raising this process before the session
spawns anything puts the shard's whole tree where a session's own run stands. The
check's other work stays where it is: the warm step's builds run under their grants,
at idle.
"""

from __future__ import annotations

import sys

NORMAL_PRIORITY_CLASS = 0x00000020


def raise_to_normal() -> bool:
    """Set this process to normal priority: False off Windows, or if it was refused."""
    if sys.platform != "win32":
        return False
    import ctypes
    from ctypes import wintypes

    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    k32.GetCurrentProcess.restype = wintypes.HANDLE
    k32.SetPriorityClass.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    return bool(k32.SetPriorityClass(k32.GetCurrentProcess(), NORMAL_PRIORITY_CLASS))
