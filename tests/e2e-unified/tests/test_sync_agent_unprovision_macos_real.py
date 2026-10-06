r"""tier_3 e2e (REAL agent): macOS sign-out unprovisions a REAL fauna-sync-agent.

`test_sync_agent_unprovision_macos.py` proves the `onSignOut` teardown WIRING fires
(the fix for the gap a prior pass found — sign-out never ran the session-teardown checklist)
but can only ever observe `"[sync-agent] unprovision skipped"`, because
`logged_in_app`'s `set_state()` e2e-injection login path never runs the boot machine
(`completeAuthenticatedLaunch`) that builds the REAL `syncAgentProvisioner` — so
`syncAgentProvisioner` is always nil there, by construction of the fixture, not the
fix. That file's own docstring names this as real, not-yet-built follow-up work.

This test closes that gap: it launches with `seed_credentials` (routes through the
REAL boot machine, exactly like `test_account_switcher_apple.py`) plus
`FAUNA_E2E_REAL_SYNC_AGENT=1` and a real `fauna-sync-agent` binary
(`macos_sync_agent_binary`), so the app spawns a REAL `FfiChildAgentSpawner` child
agent. `sync-agent-status` reaching "Running" (the same element + poll shape as
`test_sync_agent_status.py`'s linux/tui twin) is the precondition proof that a real
provisioner exists; the sign-out log delta must then show
`"[sync-agent] unprovisioning"` (a REAL provisioner torn down), never
`"[sync-agent] unprovision skipped"` (which would mean the wiring saw no provisioner
at all — the exact ambiguity `unprovisionSyncAgent()`'s dual log lines exist to
resolve; see its doc comment in `FaunaMacApp.swift`).

**Now also proves the stronger claim** (closing the follow-up the paragraph above
used to describe): that the underlying `fauna-sync-agent` OS process actually
reacts to unprovision — a live `GetServiceStatus` poll over its unix socket
(`helpers/sync_agent_ipc_unix.py`, the linux/macOS/tui sibling of Windows'
`helpers/sync_agent_ipc.py`) must settle to `connection == "Disconnected"` and
never re-`"Connected"`, mirroring `test_sync_agent_unprovision_windows.py`'s
`_wait_connection` check byte-for-byte in meaning (same
`bins/fauna-sync-agent/src/pipe_server.rs::handle_get_service_status`: `connection`
is `Connected` iff a capability is provisioned, `Disconnected` otherwise — the
agent process itself is never killed by unprovision on any platform, so a socket
error was never the right assertion here; a *reconnect* proves the process is
still up and answering honestly). The `sync-agent-status` UI element alone cannot
prove this: `SyncAgentHealthModel.stop()` resets it to "Not running" locally the
instant teardown runs, which is a Swift/Rust-side reset, not proof the external
process reacted. Building this client also gives linux/tui the same live
post-unprovision status proof they don't have today (both currently only poll the
UI element) — captured as their own follow-up, not attempted here
(scope: this file is macOS-only).

**The second test drives the OTHER caller, and it is the one that was broken.**
`sign_out()` above is a live view-hierarchy gesture, so the teardown it runs reads
the App struct's own slots on the LIVE `self`. The harness's per-test `reset()` —
`handleTestCommand`'s `reset` arm → `resetToFactory()` → the same
`unprovisionSyncAgent()` — reaches them through the test agent's **init-time-captured
`self`** instead, and every `@State` slot reassigned after `init()` reads back nil
there. So the reset logged `"[sync-agent] unprovision skipped — no provisioner on this
self"` even with a real agent provisioned and running, and the agent kept its
`RenewBearer` capability for an identity the reset had just erased. The line reads
truthfully in the overwhelmingly common case (a default e2e launch builds no spawner
at all, so there genuinely is no provisioner), which is why it never read as a
defect. Only a launch that
really provisions — this file's — can tell the two apart.
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

UNPROVISIONING = "[sync-agent] unprovisioning"
UNPROVISION_SKIPPED = "[sync-agent] unprovision skipped"

SYNC_AGENT_STATUS = "sync-agent-status"

# `drivers/macos.py`'s own `_AGENT_SOCKET_RELPATH`, split at `driver.app_support_dir()`
# (`<home>/Library/Application Support`) so this test never hardcodes `<home>` —
# joined back together this is `<home>/Library/Application Support/Fauna/sync-agent.sock`,
# matching `fauna_ipc::unix_transport::macos_socket_path` exactly (same HOME derivation).
_SOCKET_LEAF = "Fauna/sync-agent.sock"


def _wait_connection(client: SyncAgentClient, want: str, timeout: float = 60.0) -> dict:
    """Poll GetServiceStatus until `connection` reads `want` ("Connected" /
    "Disconnected"). Raises with the last-seen status for a self-diagnosing
    failure (testing.md point 6) — the same shape as the windows twin's own
    `_wait_connection`."""
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
    """A single registered regular-user account, seeded active — mirrors
    `test_account_switcher_apple.py::_seed_one_account`, duplicated rather than
    imported per this suite's existing no-cross-import convention (see
    `test_sync_agent_unprovision_windows.py`'s identical note)."""
    admin_sk = nest_instance["admin"]["signing_key"]
    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    user_actor = user["actor_id_hex"]
    user_secret = bytes(user["signing_key"]).hex()
    seed = build_registry_seed(
        [{"actor_id": user_actor, "secret_hex": user_secret,
          "nest_url": nest_instance["url"], "device_id": "real-agent-signout",
          "handle": "user"}],
        active=user_actor,
    )
    return seed, user_actor


def _launch_with_a_real_agent(nest_instance, request):
    """Launch macOS through the REAL boot machine with a REAL child sync agent,
    and return `(driver, app, client, user_actor)` with the agent already up.

    Extracted so the two teardown gestures below (the Sign Out button and the
    harness `reset`) share ONE precondition rather than growing two drifting
    copies of it (priority #4): everything up to and including "a real
    provisioner exists and the agent answers Connected" is identical, and it is
    the expensive half. Each test still gets its OWN launch — both gestures end
    the session, so they cannot share a process.
    """
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

        # Precondition: a REAL agent must be up before either gesture can
        # prove anything about tearing one down. Same generous, poll-to-settle
        # budget as the linux/tui twin (`test_sync_agent_status.py`) — sized
        # for a real OS process spawn under a loaded box, paid only once.
        deadline = time.monotonic() + 45
        status = None
        while time.monotonic() < deadline:
            status = driver.get_text(SYNC_AGENT_STATUS)
            if status == "Running":
                break
            time.sleep(0.5)
        assert status == "Running", (
            f"the real fauna-sync-agent never reached Running (last saw {status!r}) — "
            f"every unprovision proof in this file depends on a real provisioner "
            f"existing first; error={app.error_text()!r}"
        )

        # Live-socket confirmation of the same precondition — proves the socket
        # path derivation (`app_support_dir` + `_SOCKET_LEAF`) is correct BEFORE
        # relying on it for the post-teardown proof, rather than only
        # discovering a bad path at the assertion that matters. 60s, matching
        # the windows twin's own initial `_wait_connection(..., "Connected", 60)`:
        # the shared Rust convergence loop's tick interval is 30s
        # (`fauna_ipc::convergence::DEFAULT_TICK_INTERVAL`), so a first tick that
        # raced the just-spawned agent's socket and came back `Unreachable`
        # (no push — `tick()` only re-provisions on `NoCapability`) leaves the
        # NEXT provision attempt a full 30s away; a shorter budget flakes on
        # exactly that race rather than proving anything real.
        _wait_connection(client, "Connected", timeout=60)
    except BaseException:
        # A precondition that never settles still owns the launch it made — a
        # leaked app process would outlive the run and hold the agent socket
        # the NEXT test derives from the same home.
        driver.teardown()
        raise
    return driver, app, client, user_actor


def test_macos_sign_out_unprovisions_a_real_sync_agent(nest_instance, request):
    driver, app, client, _ = _launch_with_a_real_agent(nest_instance, request)
    try:
        offset = len(driver.app_stderr_text())
        app.settings.sign_out()

        delta = driver.app_stderr_text()[offset:]
        assert UNPROVISIONING in delta and UNPROVISION_SKIPPED not in delta, (
            f"sign-out must unprovision the REAL agent (not just find nothing to "
            f"unprovision) — the sync-agent-status Running read above proves a real "
            f"provisioner existed, so only the 'unprovisioning' branch is a correct "
            f"outcome here; error={app.error_text()!r}\n"
            f"app.err delta since sign-out:\n{delta[-4000:]}"
        )

        # The stronger claim: the real OS process reacts to unprovision. Never
        # re-Connects afterward — the agent isn't killed, it just has no
        # capability, matching windows' `_wait_connection(client, "Disconnected")`.
        _wait_connection(client, "Disconnected", timeout=30)
    finally:
        driver.teardown()


def test_macos_harness_reset_unprovisions_a_real_sync_agent(nest_instance, request):
    """The same teardown, reached through `handleTestCommand`'s init-time-captured
    `self` instead of a live view closure — the path that was silently doing nothing.

    `driver.reset()` is the harness's per-test boundary and the most-executed
    teardown in the whole macOS suite, so this is not a corner: every
    `real_sync_agent` test's reset left the agent provisioned for the identity
    the reset had just erased, and the store-principal capability's unprovision
    (`sync-agent-credentials.md` § Credential model) never ran.
    The assertion is deliberately identical in shape to the sign-out test's — the
    same two log branches, the same live-socket settle — because the whole point
    is that the two call sites must be indistinguishable in effect.
    """
    driver, app, client, _ = _launch_with_a_real_agent(nest_instance, request)
    try:
        offset = len(driver.app_stderr_text())
        driver.reset()

        delta = driver.app_stderr_text()[offset:]
        assert UNPROVISIONING in delta and UNPROVISION_SKIPPED not in delta, (
            f"the harness reset must unprovision the REAL agent — the Running + "
            f"Connected preconditions above prove a provisioner existed, so the "
            f"'skipped' branch here means the teardown read a slot the test-agent's "
            f"captured `self` cannot see, not that there was nothing to do; "
            f"error={app.error_text()!r}\n"
            f"app.err delta since reset:\n{delta[-4000:]}"
        )

        _wait_connection(client, "Disconnected", timeout=30)
    finally:
        driver.teardown()
