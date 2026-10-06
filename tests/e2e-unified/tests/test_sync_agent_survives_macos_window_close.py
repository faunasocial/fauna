r"""tier_3 e2e (REAL agent): macOS's Desktop Residency arm — closing the app's
window is never a silent stop, because the per-user `fauna-sync-agent`
outlives the app session.

`common.md` § Desktop Residency states the cross-app promise door-neutrally:
either something stays behind and keeps working (linux/windows hide the app
in the tray) or, on macOS, the sync agent that outlives the app session does
the staying-behind. `docs/features/general-settings.md` outcome 1's five
existing witnesses (`test_tray_close_to_tray.py`) only ever drive linux; this
file is macOS's own witness of the same outcome, through macOS's own door —
there is no close-to-tray setting on macOS and none is wanted (`macos.md`
§ Sync: the app no longer hosts resident location engines in-process at all
since the A4 cutover, so nothing about "did the WINDOW stay open" is the
observable — the agent process is).

Mirrors `test_sync_agent_unprovision_macos_real.py`'s launch shape (real
`FfiChildAgentSpawner` child agent via `FAUNA_E2E_REAL_SYNC_AGENT` +
`macos_sync_agent_binary`, `seed_credentials` through the real boot machine)
but proves the OPPOSITE outcome at the OPPOSITE door: sign-out deliberately
unprovisions; a window close must NOT — the agent must keep answering
`GetServiceStatus` with `connection: "Connected"` after the window(s) close,
proven live over the same unix-socket client the sign-out test uses
(`helpers/sync_agent_ipc_unix.py`), never by trusting the now-possibly-gone
app's own UI element (`SyncAgentHealthModel` lives in the app process, which
this test does not assume survives the close — only the externally-spawned
agent process must).
"""
from __future__ import annotations

import os
import time

import pytest

from actions import ActionLayer
from common import build_registry_seed, create_actor_and_register
from conftest import _seeded_environment
from drivers import create_driver
from helpers.sync_agent_ipc_unix import SyncAgentClient

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.macos,
    pytest.mark.destructive,
    pytest.mark.real_sync_agent,
]

SYNC_AGENT_STATUS = "sync-agent-status"

# Same derivation as `test_sync_agent_unprovision_macos_real.py`: split at
# `driver.app_support_dir()` so this test never hardcodes `<home>`.
_SOCKET_LEAF = "Fauna/sync-agent.sock"


def _wait_connection(client: SyncAgentClient, want: str, timeout: float = 60.0) -> dict:
    """Poll GetServiceStatus until `connection` reads `want`. Raises with the
    last-seen status for a self-diagnosing failure (testing.md point 6) —
    identical shape to the sign-out real-agent test's own helper."""
    deadline = time.monotonic() + timeout
    last: dict = {}
    while time.monotonic() < deadline:
        try:
            last = client.service_status()
            if last.get("connection") == want:
                return last
        except OSError:
            pass  # agent mid-restart / socket momentarily unavailable
        time.sleep(0.5)
    raise AssertionError(
        f"connection never reached {want!r} within {timeout:.0f}s; last status={last!r}"
    )


def _seed_one_account(nest_instance):
    """A single registered regular-user account, seeded active — duplicated
    from the sign-out real-agent test's helper of the same shape per this
    suite's existing no-cross-import convention."""
    admin_sk = nest_instance["admin"]["signing_key"]
    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    user_actor = user["actor_id_hex"]
    user_secret = bytes(user["signing_key"]).hex()
    seed = build_registry_seed(
        [{"actor_id": user_actor, "secret_hex": user_secret,
          "nest_url": nest_instance["url"], "device_id": "real-agent-window-close",
          "handle": "user"}],
        active=user_actor,
    )
    return seed, user_actor


@pytest.mark.feature("general-settings")
def test_macos_window_close_leaves_the_real_sync_agent_serving(nest_instance, request):
    seed, user_actor = _seed_one_account(nest_instance)
    macos_app_path = request.getfixturevalue("macos_app_path")
    agent_bin = request.getfixturevalue("macos_sync_agent_binary")

    driver = create_driver("macos")
    driver.launch({
        "url": nest_instance["url"],
        "seed_credentials": seed,
        "app_path": macos_app_path,
        "environment": {
            **_seeded_environment(request, nest_instance),
            "FAUNA_E2E_REAL_SYNC_AGENT": "1",
            "FAUNA_E2E_SYNC_AGENT_BIN": agent_bin,
        },
    })
    app = ActionLayer(driver)
    socket_path = os.path.join(driver.app_support_dir(), _SOCKET_LEAF)
    client = SyncAgentClient(socket_path, timeout=10)
    try:
        driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == user_actor
            and bool(s.get("session", {}).get("authenticated")),
            timeout=45,
        )

        # Precondition: a REAL agent must be up and connected before a window
        # close can prove anything about it surviving one. Same generous
        # budget as the sign-out real-agent test's identical precondition.
        deadline = time.monotonic() + 45
        status = None
        while time.monotonic() < deadline:
            status = driver.get_text(SYNC_AGENT_STATUS)
            if status == "Running":
                break
            time.sleep(0.5)
        assert status == "Running", (
            f"the real fauna-sync-agent never reached Running (last saw {status!r}) — "
            f"this test's own window-close proof depends on a real provisioner "
            f"existing first; error={app.error_text()!r}"
        )
        _wait_connection(client, "Connected", timeout=60)

        # The real close door: every visible window's titlebar close button,
        # not `driver.teardown()` (which SIGKILLs the whole process group —
        # see `MacosInProcessDriver.teardown`'s own docstring on why that is
        # the harness's cleanup act, never a stand-in for a real close).
        driver.window_close()

        # The agent must still answer, and must still report a live
        # capability — never "Disconnected" — proving the close did not tear
        # it down. This is a positive read (the socket answers Connected
        # right now), not a timed "never happens" claim; convention 14
        # forbids the latter, not this.
        _wait_connection(client, "Connected", timeout=30)

        # Re-check after a second beat: if window close had instead started
        # tearing the provisioner down asynchronously (the sign-out path's
        # `[sync-agent] unprovisioning` sequence), a first Connected read
        # could just be catching it before the teardown lands. Polling
        # Connected again over a second window is still a positive read at
        # each poll, not a sleep-and-hope — `_wait_connection` fails loudly,
        # with the last-seen status, the moment it ever reads anything else.
        _wait_connection(client, "Connected", timeout=15)
    finally:
        # `teardown()` group-SIGKILLs the app plus its child agent — the
        # harness's own cleanup, required regardless of whether the window
        # close above already ended the app process on its own (macOS has no
        # tray to hide behind; whether closing this app's last window also
        # quits the app process is not this test's claim either way — only
        # that the AGENT survives the close is).
        driver.teardown()
