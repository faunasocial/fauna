"""Read windows' taskbar badge from outside the app, and register a build output as
the full package whose identity the badge needs.

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
identity. A shipped install has it natively: the app IS a full MSIX — the Store
package or the direct-download bundle, both rendered from one template
(`installer/store/AppxManifest.xml.in`) by one packer
(`scripts/build-store-package.py`). The harness launches a Debug build output,
which has none — so :func:`registered_package` stages that build as a full package
with the packer's own `render_manifest` and `_stage` (the two functions a release
build runs; the `makeappx` container is out of scope) and registers the loose
layout non-elevated (`Add-AppxPackage -Register`, Developer Mode — deployment path
3 of `real_session/test_store_package_registration.py`'s module docstring). A full
package's identity reaches only a process started in its context
(`installers/windows.md` § Package identity for FaunaApp.exe), so the caller then
launches the layout's exe with the package's family name and app id, and the
windows driver starts it through `Invoke-CommandInDesktopPackage`
(`flaui-bridge/SessionManager.cs::StartInPackageContext`). Every caller names a
TEST-ONLY identity, so a registration can neither collide with nor displace a real
install, and a leftover from a failed run is removed by that name before the next
registration.

**Write virtualization.** A packaged process's writes under the real
`%LocalAppData%` are redirected into the package's private store (the direct
channel excepts `%LocalAppData%\\Fauna` alone), so anything the PACKAGED app writes
and the test reads — the driver's relocated `LOCALAPPDATA`, the credential store —
lives under :func:`unvirtualized_scratch`, in the repo's `build/` tree, outside it.

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

# The package-catalogue verbs every windows package witness shares: this box's
# package architecture, and giving a registration back by its identity name.
from helpers.windows_store_install import host_arch, remove_package  # noqa: F401 (re-exported)

_REPO = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", ".."))


def _load_script(name: str, filename: str):
    """A script under `scripts/`, imported by path — the harness uses the build's own
    code rather than a copy of it."""
    spec = importlib.util.spec_from_file_location(name, os.path.join(_REPO, "scripts", filename))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def store_packer():
    """`scripts/build-store-package.py`, the one packer of both full-MSIX channels."""
    return _load_script("build_store_package", "build-store-package.py")


# The publisher every package this harness registers carries unless its caller names
# another: the dev DN, read from its one build-time home
# (`installer/PackageIdentity.props`, through `scripts/_windows_package_identity.py`),
# the value a dev build of either channel packs with.
IDENTITY_PUBLISHER = _load_script("_windows_package_identity",
                                  "_windows_package_identity.py").publisher()
# The Application/@Id both channels declare, so every AUMID ends in it.
APPLICATION_ID = "FaunaApp"

# The Windows App SDK framework package the FRAMEWORK-DEPENDENT Debug build loads
# WinUI from — `Microsoft.WindowsAppSDK` 1.7.* in FaunaApp.csproj. Under package
# identity the bootstrapper is a no-op (the csproj's OnPackageIdentity_NoOp, measured
# 2026-09-26: without it the process exits 0x80070032), so the runtime has to be in
# the process's package graph already; a registered dev package declares it. A
# shipped install never needs this: the release publish is self-contained and carries
# the runtime beside the exe, which is why the shipped template declares no framework
# dependency (a box without the framework could not install it).
WINDOWS_APP_RUNTIME = "Microsoft.WindowsAppRuntime.1.7"

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
    arch = "Arm64" if host_arch() == "arm64" else "X64"
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
    a package dependency — what a FRAMEWORK-DEPENDENT Debug build needs inside any
    package it is registered in. Inserted as text before ``</Dependencies>`` so the
    manifest's namespace prefixes and comments survive verbatim."""
    fw_name, fw_version, fw_publisher = _installed_windows_app_runtime()
    dependency = (
        f'    <PackageDependency Name="{fw_name}" MinVersion="{fw_version}" '
        f'Publisher="{fw_publisher}" />\n'
    )
    marker = "  </Dependencies>"
    assert marker in manifest_text, "the package manifest lost its <Dependencies> element"
    return manifest_text.replace(marker, dependency + marker, 1)


def unvirtualized_scratch(name: str) -> str:
    """A fresh directory, keyed by ``name`` and this process, that a PACKAGED app may
    write and the test may read: under the repo's `build/` tree, outside the
    `%LocalAppData%` write virtualization redirects (module doc). The caller removes
    it (``shutil.rmtree(..., ignore_errors=True)`` in a ``finally``)."""
    base = os.path.join(_REPO, "build", "package-identity", f"{name}-{os.getpid()}")
    shutil.rmtree(base, ignore_errors=True)
    os.makedirs(base)
    return base


def registered_package(exe_path: str, work_dir: str, identity_name: str,
                       publisher: str = IDENTITY_PUBLISHER, channel: str = "direct",
                       payload_files: dict[str, str] | None = None) -> dict:
    """Register the build directory holding ``exe_path`` as a full package of
    ``channel`` under ``identity_name``; returns ``{"pfn", "exe", "aumid",
    "layout"}`` — the package's family name, the exe INSIDE the registered layout
    (the one to launch, in the package's context), the app's AUMID and the layout
    folder (the package's install location). The caller removes it with
    :func:`remove_package` (unconditionally, in a ``finally``).

    The layout is the packer's own: the channel's manifest rendered by
    `render_manifest`, the WINDOWS_APP_RUNTIME framework declared on it, and the
    payload laid out by `_stage` under ``work_dir`` — the build directory copied in as
    `App\\`, a placeholder for every other payload entry the packer names (the sync
    agent, the shell extension, the licence files: nothing here runs them; the
    registration test covers them against a real payload) unless ``payload_files``
    names a real file for it, keyed by the entry's name in the package (the sync
    agent a test lets the packaged app spawn: ``{"fauna-sync-agent.exe": <built
    agent>}``). The Debug build carries the e2e agent by design, so the dev package
    opts out of the packer's automation-surface refusal; it is registered, never
    uploaded.
    """
    packer = store_packer()
    payload_files = dict(payload_files or {})
    replaceable = {outside for _inside, outside in packer.PAYLOAD} - {"App"}
    if not set(payload_files) <= replaceable:
        raise ValueError(f"payload_files names {sorted(set(payload_files) - replaceable)}, "
                         f"none of the replaceable payload entries {sorted(replaceable)}")
    payload = os.path.join(work_dir, "payload")
    shutil.rmtree(payload, ignore_errors=True)
    shutil.copytree(os.path.dirname(os.path.abspath(exe_path)), os.path.join(payload, "App"))
    for _inside, outside in packer.PAYLOAD:
        if outside == "App":
            continue
        if outside in payload_files:
            shutil.copy2(payload_files[outside], os.path.join(payload, outside))
            continue
        with open(os.path.join(payload, outside), "wb") as fh:
            fh.write(b"placeholder: a registered dev package never runs it")

    with open(os.path.join(packer.STORE, "AppxManifest.xml.in"), encoding="utf-8") as fh:
        manifest = packer.render_manifest(fh.read(), identity_name, publisher,
                                          packer._product_version(), host_arch(),
                                          channel=channel)
    manifest = declare_windows_app_runtime(manifest)
    layout = os.path.join(work_dir, "layout")
    packer._stage(layout, payload, manifest, allow_test_surface=True)
    shutil.rmtree(payload, ignore_errors=True)

    rc, out, err = _ps(
        "$ErrorActionPreference='Stop'; "
        f"Get-AppxPackage -Name '{identity_name}' | Remove-AppxPackage -ErrorAction SilentlyContinue; "
        f"Add-AppxPackage -Register '{os.path.join(layout, 'AppxManifest.xml')}'; "
        f"(Get-AppxPackage -Name '{identity_name}').PackageFamilyName",
        timeout=600,
    )
    if rc != 0 or not out:
        raise RuntimeError(
            f"could not register the {channel} package {identity_name} around the build "
            f"output (Add-AppxPackage -Register {layout}): rc={rc}\n"
            f"stdout: {out}\nstderr: {err}"
        )
    pfn = out.splitlines()[-1].strip()
    return {"pfn": pfn, "exe": os.path.join(layout, "App", os.path.basename(exe_path)),
            "aumid": f"{pfn}!{APPLICATION_ID}", "layout": layout}
