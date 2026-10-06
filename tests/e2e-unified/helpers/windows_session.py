"""Detect whether this box's automation session has an attached interactive desktop.

``SendInput`` (used by ``test_flaui_input_lock_windows.py`` — the one windows
test with no UIA equivalent for what it drives)
needs an ATTACHED desktop, not merely the right foreground window. Handing a
disconnected RDP session back to the console is
``tscon %SESSIONNAME% /dest:console``,
and a forgotten hand-off is an expected, recurring state — the session reads
``Disc`` exactly as `query session` would show, not a misconfiguration.

This probes the same fact programmatically instead of shelling out to
`query session` and parsing its column layout: ``WTSQuerySessionInformationW``
against ``WTS_CURRENT_SESSION`` answers for the session that owns THIS
process — the same session that owns the bridge and its spawned apps — so a
forgotten hand-off reads as a declared `skip_environment` (convention 7)
rather than a bare `Win32Exception(5)`.
"""

from __future__ import annotations

import ctypes
from ctypes import wintypes

# wtsapi32.h. Declaring argtypes/restype explicitly (rather than relying on
# ctypes' default signed-int guess) is what keeps the output POINTER intact on
# 64-bit — an undeclared call truncates it and corrupts the free() that follows
# (`windows_placeholder.py`'s `GetFileAttributesW` comment measured the same
# class of bug for a return value).
#
# Bound lazily, never at module scope: this file collects on every host OS
# (it sits outside the `tests/platform/<os>/` collect-ignore subtree), and
# `ctypes` has no `WinDLL` attribute off Windows — a module-scope bind broke
# `suite_collect_check` the moment this helper gained an importer outside
# that subtree.
_wtsapi32 = None


def _wts():
    global _wtsapi32
    if _wtsapi32 is None:
        dll = ctypes.WinDLL("wtsapi32", use_last_error=True)
        dll.WTSQuerySessionInformationW.argtypes = [
            wintypes.HANDLE,
            wintypes.DWORD,
            ctypes.c_int,
            ctypes.POINTER(ctypes.c_void_p),
            ctypes.POINTER(wintypes.DWORD),
        ]
        dll.WTSQuerySessionInformationW.restype = wintypes.BOOL
        dll.WTSFreeMemory.argtypes = [ctypes.c_void_p]
        dll.WTSFreeMemory.restype = None
        _wtsapi32 = dll
    return _wtsapi32

_WTS_CURRENT_SERVER_HANDLE = wintypes.HANDLE(0)
_WTS_CURRENT_SESSION = wintypes.DWORD(0xFFFFFFFF)
_WTS_CONNECT_STATE = 8  # WTS_INFO_CLASS.WTSConnectState

# WTS_CONNECTSTATE_CLASS. Only WTSDisconnected is the "no attached desktop"
# signal `query session` shows as `Disc` — every other value (Active on the
# console, Active/Connected over RDP, ...) has a desktop SendInput can reach.
WTS_ACTIVE = 0
WTS_CONNECTED = 1
WTS_CONNECT_QUERY = 2
WTS_SHADOW = 3
WTS_DISCONNECTED = 4
WTS_IDLE = 5
WTS_LISTEN = 6
WTS_RESET = 7
WTS_DOWN = 8
WTS_INIT = 9

_STATE_NAMES = {
    WTS_ACTIVE: "Active",
    WTS_CONNECTED: "Connected",
    WTS_CONNECT_QUERY: "ConnectQuery",
    WTS_SHADOW: "Shadow",
    WTS_DISCONNECTED: "Disconnected",
    WTS_IDLE: "Idle",
    WTS_LISTEN: "Listen",
    WTS_RESET: "Reset",
    WTS_DOWN: "Down",
    WTS_INIT: "Init",
}


def connect_state() -> tuple[int, str]:
    """This process's own WTS session connect state, as ``(value, name)``.

    Raises ``OSError`` if the query itself fails (wrapped Win32 error) — the
    caller decides whether "couldn't tell" should skip or proceed.
    """
    wtsapi32 = _wts()
    buf = ctypes.c_void_p()
    length = wintypes.DWORD()
    ok = wtsapi32.WTSQuerySessionInformationW(
        _WTS_CURRENT_SERVER_HANDLE,
        _WTS_CURRENT_SESSION,
        _WTS_CONNECT_STATE,
        ctypes.byref(buf),
        ctypes.byref(length),
    )
    if not ok:
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        state = ctypes.cast(buf, ctypes.POINTER(ctypes.c_int)).contents.value
    finally:
        wtsapi32.WTSFreeMemory(buf)
    return state, _STATE_NAMES.get(state, f"Unknown({state})")


def skip_unless_attached_desktop() -> None:
    """``skip_environment`` iff this session is WTS-disconnected.

    Runs the caller normally (returns) on every other connect state,
    including the query itself failing — an inability to probe is not
    evidence of disconnection, and a genuine disconnection that slips past
    this probe still turns into a self-diagnosing error at the actual
    `SendInput` call (`Actions.cs`'s `PhysicalOnly`), so a false negative
    here is not a silent one.
    """
    from helpers.app_surface import skip_environment

    try:
        state, _name = connect_state()
    except OSError:
        return
    if state == WTS_DISCONNECTED:
        skip_environment(
            "this box's automation session is WTS-disconnected (`query "
            "session` would show `Disc`) — SendInput needs an attached "
            "interactive desktop. Reattach with `tscon %SESSIONNAME% "
            "/dest:console` from an elevated prompt (the project's internal "
            "Windows dev-setup notes — the SendInput/attached-desktop "
            "paragraph) and re-run."
        )
