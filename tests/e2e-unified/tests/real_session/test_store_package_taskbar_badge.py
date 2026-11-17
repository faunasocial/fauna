"""The Store channel's taskbar badge: FaunaApp running under the Store package's
own identity puts the unread count on record for `<StorePFN>!FaunaApp`.

The windows home-screen widget is the taskbar badge (`docs/goal/architecture/apps/
windows.md` § Home-screen widget), keyed on the app's AUMID, so it exists only for a
process with package identity, which FaunaApp has as a full MSIX
(`installers/windows.md` § Package identity for FaunaApp.exe). Two channels ship
one, from one template and one packer: the direct-download bundle — witnessed by
`tests/test_home_screen_widget_taskbar_badge.py` — and the Store package. This file
is the Store leg: the same badge read, with the app running inside a Store package
registered from the loose layout the real packer stages.

What makes it the Store leg
---------------------------
* **The package is the Store package.** `helpers/taskbar_badge.py::registered_package`
  renders the Store template with `scripts/build-store-package.py`'s own
  `render_manifest` and stages its layout with the packer's own `_stage` — the same
  two functions a Store build runs — then registers it with `Add-AppxPackage
  -Register` (deployment path 3, `test_store_package_registration.py`'s module
  docstring: Developer Mode, no admin). The `makeappx` container is out of scope, as
  it is there.
* **The identity is the package's.** A process started directly from a registered
  package's folder has NO identity (measured on Windows 2026-09-28:
  `GetPackageFullName` → `APPMODEL_ERROR_NO_PACKAGE`); the bridge therefore starts
  the app in the package's context (`flaui-bridge/SessionManager.cs::
  StartInPackageContext`), and the test asserts the premise — the running process
  reports THIS package — before it reads a badge.
* **The AUMID is the Store one**: `<PFN>!FaunaApp`, the Application Id both channel
  manifests share, so the reader is keyed on this package's family name.

Why the payload is the Debug build, not the staged release publish
------------------------------------------------------------------
The badge moves when a message arrives, and planting one goes through the e2e seam
(the app's TestAgent + the driver), which is compiled out of release artifacts
(convention 15). So `App/` is the framework-dependent Debug build — which needs the
Windows App SDK framework in the package graph under identity (the csproj's
`OnPackageIdentity_NoOp`), declared on the package by the helper
(`taskbar_badge.declare_windows_app_runtime`). The sync agent, the shell DLL and the
licence files are placeholders: nothing here runs them, and the sibling
registration test covers them against the real payload.

Real-session category: registering mutates the box's per-user package catalogue.
Borrow and give back — the package is registered under a test-only identity and
removed unconditionally, keyed on its name. Latency-independent (convention 14):
every read polls the OS's own store for the value it expects, up to a budget.
"""

from __future__ import annotations

import os
import shutil
import sys
import uuid

import pytest

from actions.conversations import ConversationsActions
from conftest import _seeded_environment
from drivers import create_driver
from helpers import taskbar_badge
from helpers.budgets import RPC_ROUNDTRIP_S

pytestmark = [
    pytest.mark.skipif(sys.platform != "win32", reason="the taskbar badge is a Windows surface"),
    pytest.mark.tier_4,
    pytest.mark.windows,
]

# Test-only identity, distinct from the sibling registration test's, the direct
# leg's and any real build's, so it can neither collide with nor be mistaken for
# one. Plain DN in the signed namespace — what `-Register` wants.
TEST_IDENTITY = "FaunaSocialTest.FaunaStoreBadge"
TEST_PUBLISHER = "CN=Fauna Social (Real Session Test)"
APP_ID = taskbar_badge.APPLICATION_ID

_FEED = {"nav": {"stack": [{"view": "feed"}]}}
_ON_SCREEN = "feed-tab"


@pytest.fixture
def unvirtualized_dir():
    """Scratch the PACKAGED app writes and the test reads, outside the `%LocalAppData%`
    write virtualization (`taskbar_badge.unvirtualized_scratch`)."""
    base = taskbar_badge.unvirtualized_scratch("store-badge")
    try:
        yield base
    finally:
        shutil.rmtree(base, ignore_errors=True)


@pytest.fixture
def store_package(windows_app_path, unvirtualized_dir):
    """The Store package, staged around the Debug build and registered; yields its
    family name and the exe inside it. Give-back is unconditional."""
    try:
        yield taskbar_badge.registered_package(
            str(windows_app_path), unvirtualized_dir, TEST_IDENTITY,
            publisher=TEST_PUBLISHER, channel="store")
    finally:
        taskbar_badge.remove_package(TEST_IDENTITY)


def _total_unread(conv: ConversationsActions) -> int:
    return sum(t.unread_count for t in conv.list_threads())


def test_store_package_badge_shows_the_unread_count(
        request, nest_instance, test_user, store_package, unvirtualized_dir):
    """Outcome 1 of the home-screen widget, on the Store channel: a message you have
    not read is counted on the badge the OS holds for the Store package's AUMID."""
    pfn = store_package["pfn"]
    reader = taskbar_badge.TaskbarBadgeReader(f"{pfn}!{APP_ID}")
    driver = create_driver("windows")
    try:
        driver.launch({
            "url": nest_instance["url"],
            "app_path": store_package["exe"],
            "environment": _seeded_environment(request, nest_instance),
            "package_family_name": pfn,
            "package_app_id": APP_ID,
            # Both outside %LocalAppData% — see `unvirtualized_dir`.
            "local_appdata": os.path.join(unvirtualized_dir, "localappdata"),
            "credential_dir": os.path.join(unvirtualized_dir, "creds"),
        })

        # The premise, from the OS: the app runs with THIS package's identity. A
        # badge read against an identity-less process would only ever time out.
        package = driver.app_package_full_name()
        assert package and package.startswith(f"{TEST_IDENTITY}_"), (
            f"FaunaApp is not running with the Store package's identity (package "
            f"{package!r}, want {TEST_IDENTITY}_…) — the package-context launch failed"
        )

        driver.set_state({
            "session": {
                "authenticated": True,
                "node_url": nest_instance["url"],
                "secret_hex": test_user["signing_key"].encode().hex(),
                "handle": "e2e-user",
                "actor_id": test_user["actor_id_hex"],
                "device_id": "store-badge-e2e",
            },
            **_FEED,
        })
        driver.wait_for(_ON_SCREEN, timeout=RPC_ROUNDTRIP_S)

        conv = ConversationsActions(driver)
        conv.inject_and_resolve_thread(
            rail="FaunaMls",
            sender=f"store-badge-{uuid.uuid4().hex[:8]}@self-nest.test",
            subject=None,
            body="a message waiting on the Store package's taskbar button",
        )
        expected = _total_unread(conv)
        assert expected >= 1, "the planted inbound must be unread in the app's own list"
        got = reader.await_value(expected, timeout=RPC_ROUNDTRIP_S)
        if got != expected:
            badge_lines = [ln for ln in (driver.app_log_text() or "").splitlines() if "[badge]" in ln]
            pytest.fail(
                f"the OS holds badge {got!r} for {reader.aumid}, the app's own list says "
                f"{expected}; app log [badge] lines: {badge_lines or 'none'}"
            )
    finally:
        try:
            driver.teardown()
        except Exception:
            pass
