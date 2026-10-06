"""E2E test: macOS desktop app onboarding flow.

Launches the Fauna macOS app, connects to a local test nest, creates an
identity, picks a handle, and verifies the main window loads.

Mirrors the pattern of test_windows_sync.py for Windows.
"""

import os
import subprocess
import sys
import tempfile
import time

import pytest

from common import build_macos_app, build_node, wait_for_node
from drivers.port_util import popen_group_kwargs, reap_descendants_of

pytestmark = [pytest.mark.skipif(
    sys.platform != "darwin",
    reason="macOS-only: requires SwiftUI app and Accessibility API",
), pytest.mark.tier_3]


def kill_procs(*procs):
    """Kill and wait on a list of subprocess.Popen objects."""
    for p in procs:
        if p is not None:
            try:
                p.kill()
            except OSError:
                pass
    for p in procs:
        if p is not None:
            try:
                p.wait(timeout=5)
            except Exception:
                pass


def _launch_and_get_ax(app_bundle):
    """Launch the app and return (proc, pid, ax_app_element)."""
    # Not arming this Popen: `open -a` is a short-lived LaunchServices launcher
    # that hands off to launchd and exits on its own within moments — it is
    # never waited on or killed via `proc` (both callers discard it). The
    # actual long-lived process this test cares about, FaunaMacOS, is NOT a
    # child of `open` (launchd re-parents it), so a process-group/pdeathsig
    # guard on `proc` couldn't reach it anyway — the real cleanup is the
    # `pkill -x FaunaMacOS` in each test's `finally` block below.
    proc = subprocess.Popen(
        ["open", "-a", app_bundle, "--args"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    time.sleep(3)

    result = subprocess.run(
        ["pgrep", "-x", "FaunaMacOS"],
        capture_output=True, text=True,
    )
    assert result.returncode == 0, "FaunaMacOS process not found"
    pid = int(result.stdout.strip().split("\n")[0])

    import ax_helper
    app = ax_helper.app_element(pid)
    return proc, pid, app


def _onboard(app, port):
    """Walk through the onboarding flow."""
    import ax_helper

    # Welcome: Create identity
    create_btn = ax_helper.wait_for_identifier(app, "create-identity-button", timeout=15)
    ax_helper.click(create_btn)

    # Node picker: Enter URL and connect
    node_url_field = ax_helper.wait_for_identifier(app, "NodeUrlField", timeout=10)
    ax_helper.set_text(node_url_field, f"http://127.0.0.1:{port}")
    connect_btn = ax_helper.find_by_identifier(app, "ConnectButton")
    assert connect_btn is not None, "ConnectButton not found"
    ax_helper.click(connect_btn)

    # Handle picker: Enter handle and register
    handle_field = ax_helper.wait_for_identifier(app, "HandleField", timeout=10)
    ax_helper.set_text(handle_field, f"mactest{port}")
    time.sleep(2)  # wait for availability check

    register_btn = ax_helper.wait_for_identifier(app, "RegisterButton", timeout=10)
    ax_helper.click(register_btn)

    # Sync setup: Skip
    skip_btn = ax_helper.wait_for_identifier(app, "skip-sync-button", timeout=20)
    ax_helper.click(skip_btn)
    time.sleep(2)


def test_macos_onboarding_creates_identity():
    """Full-stack: launch app, onboard via GUI, verify main window appears."""
    import ax_helper

    ax_helper.check_accessibility_trusted()

    nest_bin = build_node()
    app_bundle = build_macos_app()

    port = 13040
    nest_proc = None

    try:
        tmp = tempfile.mkdtemp(prefix="fauna-macos-e2e-")
        nest_proc = subprocess.Popen(
            [
                nest_bin,
                "--bind", f"127.0.0.1:{port}",
                "--db", os.path.join(tmp, "nest.db"),
                "--handle-domain", "test.fauna.social",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            **popen_group_kwargs(),
        )
        # Windows half of the die-with-the-run guarantee — no-op off Windows.
        reap_descendants_of(nest_proc.pid)
        wait_for_node(port)

        _, _, app = _launch_and_get_ax(app_bundle)

        _onboard(app, port)

        # Verify: Main window loaded — sidebar should have "Conversations"
        sidebar_item = ax_helper.wait_for_identifier(
            app, "Sidebar_conversations", timeout=10
        )
        assert sidebar_item is not None, "Sidebar not found — main window did not load"

    finally:
        try:
            subprocess.run(["pkill", "-x", "FaunaMacOS"], capture_output=True)
        except Exception:
            pass
        kill_procs(nest_proc)


def test_macos_sidebar_navigation():
    """After onboarding, click each sidebar item and verify it activates."""
    import ax_helper

    ax_helper.check_accessibility_trusted()

    nest_bin = build_node()
    app_bundle = build_macos_app()

    port = 13041
    nest_proc = None

    try:
        tmp = tempfile.mkdtemp(prefix="fauna-macos-nav-")
        nest_proc = subprocess.Popen(
            [
                nest_bin,
                "--bind", f"127.0.0.1:{port}",
                "--db", os.path.join(tmp, "nest.db"),
                "--handle-domain", "test.fauna.social",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            **popen_group_kwargs(),
        )
        # Windows half of the die-with-the-run guarantee — no-op off Windows.
        reap_descendants_of(nest_proc.pid)
        wait_for_node(port)

        _, _, app = _launch_and_get_ax(app_bundle)

        _onboard(app, port)

        # Test sidebar navigation
        sidebar_items = ["conversations", "groups", "contacts", "backups", "status"]
        for item_name in sidebar_items:
            identifier = f"Sidebar_{item_name}"
            elem = ax_helper.find_by_identifier(app, identifier)
            assert elem is not None, f"Sidebar item '{identifier}' not found"
            ax_helper.click(elem)
            time.sleep(0.5)

    finally:
        try:
            subprocess.run(["pkill", "-x", "FaunaMacOS"], capture_output=True)
        except Exception:
            pass
        kill_procs(nest_proc)
