"""E2E test: Desktop app 'Join a nest' onboarding flow.

Launches a local fauna-nest with open registration, then drives the
GTK4 desktop app through the 'Join a nest' flow using AT-SPI.

Expected: registration succeeds against the local nest. The app may
fail when transitioning to the main window because it tries to
authenticate against the nest's reported domain (test.fauna.social)
over HTTPS, which doesn't resolve to the local instance.
"""

import json
import os
import signal
import subprocess
import tempfile
import time
import urllib.request

import pytest

from common import get_repo_root
from drivers.port_util import popen_group_kwargs, reap_descendants_of

pytestmark = pytest.mark.tier_3

HANDLE_DOMAIN = "test.fauna.social"


def build_desktop() -> str:
    """Build fauna-linux and return the binary path."""
    repo = get_repo_root()
    result = subprocess.run(
        ["cargo", "build", "-p", "fauna-linux", "--message-format=json"],
        capture_output=True,
        text=True,
        cwd=repo,
    )
    if result.returncode != 0:
        raise RuntimeError(f"cargo build fauna-linux failed:\n{result.stderr}")

    for line in reversed(result.stdout.strip().split("\n")):
        try:
            msg = json.loads(line)
            exe = msg.get("executable")
            if exe and (exe.endswith("/fauna-desktop") or exe.endswith("/fauna")):
                return exe
        except (json.JSONDecodeError, KeyError):
            continue
    raise RuntimeError("Could not find fauna binary in cargo output")


@pytest.fixture(scope="module")
def desktop_binary():
    return build_desktop()


@pytest.fixture()
def local_nest(request, nest_mode, tmp_path_factory):
    """A nest of this run's mode, started by its provider.

    The module used to define its own `nest_binary` shadowing conftest's and to
    pin the fixed port 13025 — the last of the twelve fixed nest ports ruling
    (1)'s port-allocation prerequisite set out to remove. Both went with the
    routing rather than needing a slice of their own (`testing.md` § Default app
    and nest mode, ruling (1))."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "desktop-join-nest")
    try:
        yield nest
    finally:
        cleanup()


@pytest.fixture()
def desktop_app(request, desktop_binary, local_nest):
    """Launch the desktop app with no stored credentials (shows onboarding),
    its escrow trust seeded for the nest it will join (`_seeded_environment`)."""
    from conftest import _seeded_environment

    # Clear any stored credentials so the app shows onboarding.
    try:
        subprocess.run(
            ["secret-tool", "clear", "application", "fauna-desktop"],
            timeout=5,
            capture_output=True,
        )
    except (FileNotFoundError, subprocess.TimeoutExpired):
        pass

    proc = subprocess.Popen(
        [desktop_binary],
        env={**os.environ, **_seeded_environment(request, local_nest)},
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        **popen_group_kwargs(),
    )
    reap_descendants_of(proc.pid)
    # Give the app time to start and register with AT-SPI.
    time.sleep(3)

    yield proc

    proc.send_signal(signal.SIGTERM)
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()


def test_desktop_join_nest(local_nest, desktop_app):
    """Drive the 'Join a nest' flow through the desktop app UI via AT-SPI."""
    import gi

    gi.require_version("Atspi", "2.0")
    from gi.repository import Atspi
    from atspi_helper import (
        click,
        dump_tree,
        type_text,
        wait_for_app,
        wait_for_widget,
    )

    nest_url = local_nest["url"]

    # 1. Find the app in the AT-SPI tree.
    try:
        app = wait_for_app("fauna", timeout=10)
    except TimeoutError:
        app = wait_for_app("social.fauna.fauna", timeout=5)

    print("\n=== AT-SPI tree after launch ===")
    dump_tree(app)

    # 2. Click "Join a nest" on the welcome page.
    join_btn = wait_for_widget(app, Atspi.Role.PUSH_BUTTON, "Join a nest", timeout=5)
    click(join_btn)

    # 3. Enter the local nest URL.
    url_entry = wait_for_widget(app, Atspi.Role.TEXT, "Nest address", timeout=5)
    type_text(url_entry, nest_url)

    # 4. Click "Connect".
    connect_btn = wait_for_widget(app, Atspi.Role.PUSH_BUTTON, "Connect", timeout=5)
    click(connect_btn)

    # 5. Wait for the register page.
    register_btn = wait_for_widget(
        app, Atspi.Role.PUSH_BUTTON, "Register", timeout=10
    )

    # 6. Type a handle.
    handle_entry = wait_for_widget(app, Atspi.Role.TEXT, "Handle", timeout=5)
    type_text(handle_entry, "e2e-testuser")
    time.sleep(2)  # Wait for debounced availability check.

    # 7. Click "Register".
    click(register_btn)
    time.sleep(5)  # Wait for registration to complete.

    print("\n=== AT-SPI tree after Register ===")
    dump_tree(app)

    # 8. Verify registration succeeded by checking handle availability via API.
    req = urllib.request.Request(f"{nest_url}/api/v1/handle-available/e2e-testuser")
    resp = urllib.request.urlopen(req)
    data = json.loads(resp.read())

    assert data.get("available") is False, (
        f"Handle 'e2e-testuser' is still available after registration. "
        f"Response: {data}"
    )

    # Double-check: resolve the handle to confirm it exists.
    req2 = urllib.request.Request(f"{nest_url}/api/v1/actor/by-handle/e2e-testuser")
    resp2 = urllib.request.urlopen(req2)
    actor_data = json.loads(resp2.read())
    assert "actor_id" in actor_data, (
        f"Handle 'e2e-testuser' not resolvable after registration. "
        f"Response: {actor_data}"
    )
