"""Local sync-agent process health — the `sync-agent-status` global shell
element (`sync-agent.md` § Local agent health).

linux and tui — the two apps whose e2e launch direct-spawns a REAL agent
**unconditionally, with the ordinary `logged_in_app` fixture.** tier_3: this
e2e launch direct-spawns the REAL `fauna-sync-agent` binary (A3, `sync-agent.md`
§ Packaging + lifecycle) so `sync-agent-status` reflects a real process, not a
fake — the indicator starts "Not running" and only reaches "Running" once the
poll actually reaches that real agent over its real transport.

**windows below, as its own opt-in test (2026-09-07).** This module used to
say windows had "no real-agent-spawn e2e harness — it is VM-level tested".
That was stale even when written: `test_sync_agent_unprovision_windows.py`
(landed earlier) already drives a real, ISOLATED `fauna-sync-agent.exe` via
`helpers.windows_sync_agent.running_agent` + `FAUNA_E2E_REAL_SYNC_AGENT` +
`FAUNA_E2E_SYNC_PIPE` — windows' isolation is actually the cleanest of the
three real-agent platforms (a fully private pipe + data-dir per test run,
never the machine-global per-SID pipe or launchd/systemd).
`test_sync_agent_status_reads_running_after_login_windows_real` below reuses
that exact harness for this feature's own non-destructive "Running" witness.

**The tui leg runs on Windows too** (2026-07-24 — `sync-agent.md` § Implementation
status: the shared client resolves the per-SID named pipe through
`fauna_ipc::endpoint`, so a windows fauna-tui drives the agent like any other).
The isolation differs per OS and `drivers/tui.py` owns it: unix inherits the
launch's private XDG world, windows gets a per-launch `--pipe-name` +
`--data-dir` because its agent seam is machine-global. Nothing in this file
branches on that — hence no OS marker here, only the client ones.

**macOS below, as its own opt-in test (2026-08-30).** This module used to say
macOS had "no real-agent-spawn e2e harness" because its production path is a
machine-global launchd LaunchAgent (`sync-agent.md` § Packaging + lifecycle) —
true, and rightly out of scope per testing.md § point 10. That reasoning
predates `FfiChildAgentSpawner` (2026-07-24): under the
`FAUNA_E2E_REAL_SYNC_AGENT` opt-in the macOS app spawns a *private* child agent
on this launch's own isolated socket, exactly like linux/tui — never touching
launchd. `test_sync_agent_unprovision_macos_real.py` already proves this same
"Running" precondition on the way to its (destructive) sign-out assertion;
`test_sync_agent_status_reads_running_after_login_macos_real` below isolates
that precondition as this feature's own macOS witness, non-destructively. It
needs the heavier `seed_credentials` + real-boot-machine launch (not the
`logged_in_app` fixture's `set_state()` fast-injection path, which never builds
the real provisioner) and the `real_sync_agent` marker, so it is opt-in rather
than folded into the shared `pytestmark` above.
"""

import time

import pytest

from actions import ActionLayer
from common import build_registry_seed, create_actor_and_register
from conftest import _seeded_environment
from drivers import create_driver
from helpers.windows_sync_agent import running_agent

pytestmark = [pytest.mark.tier_3]


@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.feature("local-folder-sync")
def test_sync_agent_status_reads_running_after_login(logged_in_app):
    """The real direct-spawned agent answers `GetServiceStatus` with this
    client's own build version (client + agent share one workspace version —
    `sync-agent.md` § Local agent health), so the poll settles on 'Running'
    with a non-empty version + uptime — not the 'Not running' the indicator
    starts on before the first poll lands."""
    driver = logged_in_app.driver

    # Generous: the agent is a real OS process this e2e launch direct-spawns,
    # and the client's own poll only retries every 10s (linux `app.rs`, tui
    # `sync_agent.rs::status_poll_loop`), so the deadline must comfortably clear
    # a slow first spawn under a loaded box plus at least one retry. Sized far
    # above any non-pathological delay, and green runs pay none of it — the loop
    # below exits the moment the reading settles (testing.md § point 14).
    deadline = time.monotonic() + 45
    text = None
    while time.monotonic() < deadline:
        text = driver.get_text("sync-agent-status")
        if text == "Running":
            break
        time.sleep(0.5)

    assert text == "Running", (
        f"sync-agent-status never reached 'Running' (last saw {text!r}); "
        f"error={logged_in_app.error_text()!r}"
    )

    version = driver.get_text("sync-agent-status-version")
    assert version, (
        f"sync-agent-status-version was empty while Running; "
        f"error={logged_in_app.error_text()!r}"
    )
    uptime = driver.get_text("sync-agent-status-uptime")
    assert uptime, (
        f"sync-agent-status-uptime was empty while Running; "
        f"error={logged_in_app.error_text()!r}"
    )


@pytest.mark.macos
@pytest.mark.real_sync_agent
@pytest.mark.feature("local-folder-sync")
def test_sync_agent_status_reads_running_after_login_macos_real(nest_instance, request):
    """macOS twin of `test_sync_agent_status_reads_running_after_login` above,
    real-agent-gated (see the module docstring).

    Mirrors `test_sync_agent_unprovision_macos_real.py`'s own precondition
    block: `seed_credentials` (the real boot machine, not `logged_in_app`'s
    fast `set_state()` injection) plus `FAUNA_E2E_REAL_SYNC_AGENT=1` and a real
    `fauna-sync-agent` binary, so the app spawns a real `FfiChildAgentSpawner`
    child on this launch's own isolated socket. No sign-out here — this test
    stops at the same "Running" + version/uptime proof the linux/tui test above
    makes, non-destructively.
    """
    admin_sk = nest_instance["admin"]["signing_key"]
    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    user_actor = user["actor_id_hex"]
    seed = build_registry_seed(
        [{"actor_id": user_actor, "secret_hex": bytes(user["signing_key"]).hex(),
          "nest_url": nest_instance["url"], "device_id": "sync-agent-status-real",
          "handle": "user"}],
        active=user_actor,
    )
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
    try:
        driver.wait_for_state(
            lambda s: s.get("session", {}).get("actor_id") == user_actor
            and bool(s.get("session", {}).get("authenticated")),
            timeout=45,
        )

        # Same generous, poll-to-settle budget as the linux/tui test above —
        # a real OS process spawn under a loaded box, paid only once.
        deadline = time.monotonic() + 45
        text = None
        while time.monotonic() < deadline:
            text = driver.get_text("sync-agent-status")
            if text == "Running":
                break
            time.sleep(0.5)
        assert text == "Running", (
            f"sync-agent-status never reached 'Running' (last saw {text!r}); "
            f"error={app.error_text()!r}"
        )

        version = driver.get_text("sync-agent-status-version")
        assert version, (
            f"sync-agent-status-version was empty while Running; "
            f"error={app.error_text()!r}"
        )
        uptime = driver.get_text("sync-agent-status-uptime")
        assert uptime, (
            f"sync-agent-status-uptime was empty while Running; "
            f"error={app.error_text()!r}"
        )
    finally:
        driver.teardown()


@pytest.mark.windows
@pytest.mark.real_sync_agent
@pytest.mark.isolated_sync_agent
@pytest.mark.feature("local-folder-sync")
def test_sync_agent_status_reads_running_after_login_windows_real(
    nest_instance, windows_app_path, sync_agent_binary,
    isolated_sync_agent_pipe_name, tmp_path, request,
):
    """windows twin of `test_sync_agent_status_reads_running_after_login` above
    (see the module docstring): a real, ISOLATED `fauna-sync-agent.exe` —
    exactly `test_sync_agent_unprovision_windows.py`'s spawn shape — never the
    machine-global per-SID pipe, so this never contends with the box's
    installed product or a sibling checkout's agent. No switch/sign-out here —
    this test stops at the same "Running" + version/uptime proof the
    linux/tui/macOS tests above make, non-destructively.
    """
    admin_sk = nest_instance["admin"]["signing_key"]
    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    user_actor = user["actor_id_hex"]
    seed = build_registry_seed(
        [{"actor_id": user_actor, "secret_hex": bytes(user["signing_key"]).hex(),
          "nest_url": nest_instance["url"], "device_id": "sync-agent-status-real",
          "handle": "user"}],
        active=user_actor,
    )

    pipe_path = r"\\.\pipe\{}".format(isolated_sync_agent_pipe_name)
    data_dir = tmp_path / "isolated-sync-agent-data"
    data_dir.mkdir()
    log_path = tmp_path / "isolated-sync-agent.log"

    # The agent must already be serving before the app's first hydration tick
    # (same trap `test_sync_agent_unprovision_windows.py` documents):
    # SpawnSyncAgentDetached does NOT honour FAUNA_E2E_SYNC_PIPE, so an app
    # that probes first and finds nothing would spawn onto the box's real
    # per-SID pipe instead.
    with running_agent(sync_agent_binary, pipe_path, data_dir, log_path):
        driver = create_driver("windows")
        driver.launch({
            "app_path": windows_app_path,
            "url": nest_instance["url"],
            "seed_credentials": seed,
            "environment": {
                **_seeded_environment(request, nest_instance),
                "FAUNA_E2E_REAL_SYNC_AGENT": "1",
                "FAUNA_E2E_SYNC_PIPE": isolated_sync_agent_pipe_name,
            },
        })
        app = ActionLayer(driver)
        try:
            driver.wait_for_state(
                lambda s: s.get("session", {}).get("actor_id") == user_actor
                and bool(s.get("session", {}).get("authenticated")),
                timeout=45,
            )

            # Same generous, poll-to-settle budget as the linux/tui/macOS tests
            # above — a real OS process spawn under a loaded box, paid once.
            deadline = time.monotonic() + 45
            text = None
            while time.monotonic() < deadline:
                text = driver.get_text("sync-agent-status")
                if text == "Running":
                    break
                time.sleep(0.5)
            assert text == "Running", (
                f"sync-agent-status never reached 'Running' (last saw {text!r}); "
                f"error={app.error_text()!r}"
            )

            version = driver.get_text("sync-agent-status-version")
            assert version, (
                f"sync-agent-status-version was empty while Running; "
                f"error={app.error_text()!r}"
            )
            uptime = driver.get_text("sync-agent-status-uptime")
            assert uptime, (
                f"sync-agent-status-uptime was empty while Running; "
                f"error={app.error_text()!r}"
            )
        finally:
            driver.teardown()
