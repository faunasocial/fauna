"""Read windows' taskbar badge from outside the app, and lend a build output the
package identity the badge needs.

The windows home-screen widget is the numeric badge on Fauna's taskbar button:
the app hands `BadgeUpdateManager` a `<badge value="N"/>` and the OS paints N on
the app's taskbar icon, running or not, until the next update
(`docs/goal/architecture/apps/windows.md` § Home-screen widget). The taskbar's
paint is the last inch; the mechanism is the badge the OS has on record, and
the OS keeps that record in the per-user notification platform store,
`%LocalAppData%\\Microsoft\\Windows\\Notifications\\wpndatabase.db` — a SQLite
database whose `Notification` rows of `Type = 'badge'` hold each app's current
badge payload, keyed through `NotificationHandler.PrimaryId` on the app's
AUMID (`<PackageFamilyName>!<ApplicationId>`). Reading that row is what a
badge-painting shell reads, so :class:`TaskbarBadgeReader` IS the taskbar for
witness purposes — the windows twin of `helpers/launcher_entry.py`, where a
bus subscriber is the dock. (Measured 2026-09-26 on Windows 11 26200: a badge
update from a process with identity lands as exactly one such row; a process
without identity gets `0x80070490` from the updater and no row.)

**Identity.** Badges are keyed on an AUMID, so the app must run with package
identity. A shipped install has one (the Store package natively; the MSI
through the sparse identity package it registers, plus the `msix` element in
`FaunaApp/app.manifest.in` that binds the exe to it). The harness launches a bare
build output, which has none — so :func:`registered_identity` lends it one for
the test's duration: the sparse manifest template, retargeted at the build
directory as its external location and registered non-elevated the way the
Store-package registration test does (`Add-AppxPackage -Register` on a loose
manifest, Developer Mode). The exe's embedded `msix` element names ONE
identity (`FaunaSocial.Fauna`, the dev publisher DN), so that is the identity
lent — which is also why a box holding a *real* registration of that name (an
MSI install) is left alone: the test steps aside rather than displace it.

Both halves read the OS's own records and never a file the app writes, so a
passing read means the OS accepted the badge, not that the app believes it
sent one.
"""

from __future__ import annotations

import importlib.util
import os
import re
import shutil
import sqlite3
import subprocess
import tempfile
import time
import xml.etree.ElementTree as ET


def _publisher_home():
    """`scripts/_windows_package_identity.py`, loaded by path — the harness's read
    of the one build-time home of the publisher DN."""
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "..",
                        "scripts", "_windows_package_identity.py")
    spec = importlib.util.spec_from_file_location("_windows_package_identity", path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


# The identity FaunaApp/app.manifest.in's msix element binds the exe to — the sparse
# package's Identity/@Name, the publisher DN (read from its one build-time home,
# `installer/PackageIdentity.props`, the same value the exe's embedded manifest is
# generated with) and the Application/@Id both channel manifests share. The name and
# id are kept in step by the structural suites; a mismatch here means "no identity",
# never a different one.
IDENTITY_NAME = "FaunaSocial.Fauna"
IDENTITY_PUBLISHER = _publisher_home().publisher()
APPLICATION_ID = "FaunaApp"

# The Windows App SDK framework package the FRAMEWORK-DEPENDENT Debug build loads
# WinUI from — `Microsoft.WindowsAppSDK` 1.7.* in FaunaApp.csproj. Under package
# identity the bootstrapper is a no-op (the csproj's OnPackageIdentity_NoOp, measured
# 2026-09-26: without it the process exits 0x80070032), so the runtime has to be in
# the process's package graph already; the lent identity declares it. A shipped
# install never needs this: the release publish is self-contained and carries the
# runtime beside the exe, which is why the SHIPPED sparse manifest declares no
# framework dependency (a box without the framework could not register it).
WINDOWS_APP_RUNTIME = "Microsoft.WindowsAppRuntime.1.7"
_MANIFEST_NS = "http://schemas.microsoft.com/appx/manifest/foundation/windows10"

_WPN_DB = os.path.join(
    os.environ.get("LOCALAPPDATA", ""), "Microsoft", "Windows", "Notifications", "wpndatabase.db"
)


def _ps(script: str, timeout: float = 120.0) -> tuple[int, str, str]:
    """Run PowerShell 7 and return (rc, stdout, stderr) — the store-registration
    test's helper, same flags."""
    r = subprocess.run(
        ["pwsh", "-NoProfile", "-NonInteractive", "-Command", script],
        capture_output=True, text=True, timeout=timeout,
    )
    return r.returncode, (r.stdout or "").strip(), (r.stderr or "").strip()


class TaskbarBadgeReader:
    """The badge the OS currently holds for ``aumid``, read off the notification
    platform's own store."""

    def __init__(self, aumid: str, db_path: str = _WPN_DB):
        self.aumid = aumid
        self.db_path = db_path

    def read(self) -> int | None:
        """The badge's numeric value, or ``None`` when the OS holds no badge for the
        app (no row, or a ``none`` glyph — both are "nothing painted").

        The live database is open by the notification platform in WAL mode, so
        the read is off a copy of the db + its WAL/SHM, taken per call: a copy is
        what a shell would see, and it never contends with the writer.
        """
        if not os.path.exists(self.db_path):
            return None
        with tempfile.TemporaryDirectory(prefix="wpn-badge-") as tmp:
            dst = os.path.join(tmp, "wpn.db")
            for suffix in ("", "-wal", "-shm"):
                src = self.db_path + suffix
                if os.path.exists(src):
                    shutil.copy2(src, dst + suffix)
            con = sqlite3.connect(dst)
            try:
                rows = con.execute(
                    "SELECT n.Payload FROM Notification n "
                    "JOIN NotificationHandler h ON n.HandlerId = h.RecordId "
                    "WHERE n.Type = 'badge' AND h.PrimaryId = ? "
                    "ORDER BY n.\"Order\" DESC LIMIT 1",
                    (self.aumid,),
                ).fetchall()
            finally:
                con.close()
        if not rows:
            return None
        payload = rows[0][0]
        if isinstance(payload, bytes):
            payload = payload.decode("utf-8", "replace")
        m = re.search(r'value="([^"]*)"', payload or "")
        if not m or not m.group(1).isdigit():
            return None
        return int(m.group(1))

    def await_value(self, expected: int | None, timeout: float) -> int | None:
        """Block until the OS's badge for the app reads ``expected`` (``None`` =
        cleared), up to ``timeout``; returns the last value read. Latency-independent
        (convention 14): a poll on the store's own state, never a sleep-then-assert."""
        deadline = time.monotonic() + timeout
        last: int | None = None
        while True:
            last = self.read()
            if last == expected:
                return last
            if time.monotonic() >= deadline:
                return last
            time.sleep(0.2)


def _installed_windows_app_runtime() -> tuple[str, str, str]:
    """(Name, Version, Publisher) of the installed WINDOWS_APP_RUNTIME framework
    package for this machine's architecture — the newest one, read from the OS."""
    arch = "Arm64" if os.environ.get("PROCESSOR_ARCHITECTURE", "").upper() == "ARM64" else "X64"
    rc, out, err = _ps(
        f"$p = Get-AppxPackage -Name '{WINDOWS_APP_RUNTIME}' | "
        f"Where-Object {{ \"$($_.Architecture)\" -eq '{arch}' }} | "
        "Sort-Object { [version]$_.Version } -Descending | Select-Object -First 1; "
        "if ($p) { \"$($p.Name)|$($p.Version)|$($p.Publisher)\" }"
    )
    if rc != 0 or not out or "|" not in out:
        raise RuntimeError(
            f"no {WINDOWS_APP_RUNTIME} framework package is installed for {arch} — the "
            "framework-dependent Debug build cannot run under package identity without "
            f"it (Get-AppxPackage rc={rc}: {out!r} {err})"
        )
    name, version, publisher = out.splitlines()[-1].strip().split("|", 2)
    return name, version, publisher


def declare_windows_app_runtime(manifest_text: str) -> str:
    """``manifest_text`` with the installed WINDOWS_APP_RUNTIME framework declared as
    a package dependency — what a FRAMEWORK-DEPENDENT Debug build needs under ANY
    package identity it is registered with (the sparse identity lent here, or the
    Store-shape package `test_store_package_taskbar_badge.py` registers). Inserted as
    text before ``</Dependencies>`` so the manifest's namespace prefixes and comments
    survive verbatim."""
    fw_name, fw_version, fw_publisher = _installed_windows_app_runtime()
    dependency = (
        f'    <PackageDependency Name="{fw_name}" MinVersion="{fw_version}" '
        f'Publisher="{fw_publisher}" />\n'
    )
    marker = "  </Dependencies>"
    assert marker in manifest_text, "the package manifest lost its <Dependencies> element"
    return manifest_text.replace(marker, dependency + marker, 1)


def foreign_registration(name: str = IDENTITY_NAME) -> str | None:
    """The install location of a package registered under ``name`` for this user,
    or ``None`` when none is — the guard that keeps a test from displacing a real
    (MSI-installed) identity registration on a developer's box."""
    rc, out, _err = _ps(
        f"$p = Get-AppxPackage -Name '{name}'; if ($p) {{ $p.InstallLocation }}"
    )
    out = out.strip()
    return out or None


def registered_identity(exe_path: str, sparse_template: str, assets_dir: str,
                        work_dir: str) -> str:
    """Register the sparse identity package around the build directory holding
    ``exe_path`` and return the app's AUMID. Caller removes it with
    :func:`remove_identity` (unconditionally, in a ``finally``).

    ``sparse_template`` is `apps/fauna-windows/installer/sparse/AppxManifest.xml.in`;
    its three build tokens are substituted the way the sparse-package build script
    does, and its `Application/@Executable` (`App\\FaunaApp.exe`, the MSI's layout)
    is retargeted at the bare exe name, since the build directory IS the external
    location here. The manifest and its logo assets are staged in ``work_dir``;
    nothing is written into the build directory.
    """
    exe_dir = os.path.dirname(os.path.abspath(exe_path))
    exe_name = os.path.basename(exe_path)
    with open(sparse_template, encoding="utf-8") as f:
        text = f.read()
    text = (
        text.replace("@@Publisher@@", IDENTITY_PUBLISHER)
        .replace("@@PackageVersion@@", "0.0.0.1")
        .replace("@@ShellDllName@@", "fauna_shell.dll")
        .replace('Executable="App\\FaunaApp.exe"', f'Executable="{exe_name}"')
    )
    # Guard the substitution against a template drift the replace above would miss.
    root = ET.fromstring(text)
    ns = {"m": _MANIFEST_NS}
    app = root.find("m:Applications/m:Application", ns)
    assert app is not None and app.get("Executable") == exe_name, (
        "the sparse template's Application/@Executable was not retargeted at the "
        f"build output — got {None if app is None else app.get('Executable')!r}"
    )
    assert app.get("Id") == APPLICATION_ID and root.find("m:Identity", ns).get("Name") == IDENTITY_NAME, (
        "the sparse template's Identity/@Name or Application/@Id no longer match the "
        "identity FaunaApp/app.manifest.in binds the exe to"
    )
    text = declare_windows_app_runtime(text)
    os.makedirs(os.path.join(work_dir, "Assets"), exist_ok=True)
    for png in os.listdir(assets_dir):
        if png.lower().endswith(".png"):
            shutil.copy2(os.path.join(assets_dir, png), os.path.join(work_dir, "Assets", png))
    manifest = os.path.join(work_dir, "AppxManifest.xml")
    with open(manifest, "w", encoding="utf-8") as f:
        f.write(text)

    rc, out, err = _ps(
        "$ErrorActionPreference='Stop'; "
        f"Get-AppxPackage -Name '{IDENTITY_NAME}' | Remove-AppxPackage -ErrorAction SilentlyContinue; "
        f"Add-AppxPackage -Register '{manifest}' -ExternalLocation '{exe_dir}'; "
        f"(Get-AppxPackage -Name '{IDENTITY_NAME}').PackageFamilyName"
    )
    if rc != 0 or not out:
        raise RuntimeError(
            "could not lend the build output package identity "
            f"(Add-AppxPackage -Register, external location {exe_dir}): rc={rc}\n"
            f"stdout: {out}\nstderr: {err}"
        )
    pfn = out.splitlines()[-1].strip()
    return f"{pfn}!{APPLICATION_ID}"


def remove_identity(name: str = IDENTITY_NAME) -> None:
    """Give the lent identity back. Keyed on the identity NAME, so a registration a
    failed run left behind is cleaned up too."""
    _ps(f"Get-AppxPackage -Name '{name}' | Remove-AppxPackage -ErrorAction SilentlyContinue")
