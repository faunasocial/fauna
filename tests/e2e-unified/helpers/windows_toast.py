"""A per-run windows toast identity, and the notification centre's record of it.

The windows sync agent has no toast identity of its own: it posts under the AUMID
the app names when it attaches (``windows.md`` § Notifications, the agent's half).
A witness therefore registers a throwaway identity the way an unpackaged app's
toast registration is kept (``HKCU\\Software\\Classes\\AppUserModelId\\<aumid>``),
hands it to the agent, and reads what landed under it back from the
notification-centre history — from a second process, which is the point: the
history is the platform's record, not the poster's.

The read goes through Windows PowerShell 5.1 (``powershell.exe``), whose WinRT
type projection reaches ``ToastNotificationManager.History``; pwsh 7 dropped that
projection, and no Python WinRT binding is a dependency here.

Process safety: ``reg.exe`` and ``powershell.exe`` are this module's own
short-lived children, run to completion; nothing else is touched.
"""

from __future__ import annotations

import os
import subprocess
import xml.etree.ElementTree as ET
from contextlib import contextmanager

_AUMID_KEY = r"HKCU\Software\Classes\AppUserModelId"

#: One `powershell.exe` round trip — process start plus one WinRT call. Sized for
#: a busy Windows host, where a cold start can take seconds; a green run spends ~1 s.
_POWERSHELL_S = 60


@contextmanager
def registered_identity(tag: str):
    """A per-run unpackaged toast identity, unregistered — and its toasts
    cleared from the notification centre — on exit."""
    aumid = f"Fauna.AgentSinkE2e.{tag}.{os.getpid()}"
    key = rf"{_AUMID_KEY}\{aumid}"
    subprocess.run(
        ["reg", "add", key, "/v", "DisplayName", "/t", "REG_SZ", "/d", "Fauna e2e", "/f"],
        check=True, capture_output=True,
    )
    try:
        yield aumid
    finally:
        try:
            _powershell(
                f"[Windows.UI.Notifications.ToastNotificationManager]::History.Clear('{aumid}')"
            )
        finally:
            subprocess.run(["reg", "delete", key, "/f"], capture_output=True)


def toast_history(aumid: str) -> list[list[str]]:
    """The toasts the notification centre holds for ``aumid``, oldest first,
    each as its text lines (a posted frame's title, then its body)."""
    out = _powershell(
        f"foreach ($t in [Windows.UI.Notifications.ToastNotificationManager]::"
        f"History.GetHistory('{aumid}')) {{ $t.Content.GetXml() }}"
    )
    return [
        [text.text or "" for text in ET.fromstring(line).iter("text")]
        for line in out.splitlines()
        if line.strip()
    ]


def _powershell(statement: str) -> str:
    """Run ``statement`` in Windows PowerShell 5.1 with the toast types loaded;
    returns its stdout. Raises with PowerShell's own error text on failure."""
    script = (
        "$ErrorActionPreference = 'Stop'; "
        "[void][Windows.UI.Notifications.ToastNotificationManager, "
        "Windows.UI.Notifications, ContentType = WindowsRuntime]; "
        + statement
    )
    done = subprocess.run(
        ["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", script],
        capture_output=True, text=True, timeout=_POWERSHELL_S,
    )
    if done.returncode != 0:
        raise RuntimeError(
            f"powershell.exe exited {done.returncode} reading the toast history:\n"
            f"{done.stderr.strip() or done.stdout.strip()}"
        )
    return done.stdout
