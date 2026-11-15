"""The background sync helper keeps syncing after the app quits, and renews its
own sign-in over the machine's store principal — with no app running and no
identity key. (The module and test names keep "tui", the arm that led; the
macos and linux arms below are the same journey through each app's own quit
door.)

Owners: ``docs/goal/architecture/apps/sync-agent-credentials.md`` § Credential
model (the agent renews its own bearer over the principal; *A rejected
credential asks at once*) and ``docs/goal/architecture/e2e-launch-isolation.md``
convention 10 (the opted-in own-group agent the driver still owns). Feature
catalog: ``docs/features/local-folder-sync.md`` outcome 23.

**Why the launch opts in.** Under e2e tui direct-spawns its agent as a plain
child, so it sits in tui's process group — the pty's foreground group — and the
kernel's hangup on the session leader's exit kills it the moment tui quits. In
production the agent is a user unit and outlives the app. The launch therefore
passes ``sync_agent_own_group``: the agent leads a process group of its own and
records its pgid (``agent_spawner::AGENT_OWN_GROUP_ENV``), which the driver
tracks and kills at teardown, so the run still leaves no agent behind — the
teardown below asserts exactly that.

**The macos arm.** The macOS app's e2e agent is the private child
``FfiChildAgentSpawner`` builds under ``FAUNA_E2E_REAL_SYNC_AGENT`` (pinned per
launch by ``helpers.sync_seats.pin_macos_seat_sync_agent``), on a socket under
the launch's own HOME. A GUI app holds no pty, so no hangup reaches the agent
when the app exits and no opt-in is needed. The quit door is ⌘Q
(``driver.quit_app()`` → ``NSApp.terminate``), never a window close, which on
macOS is deliberately not a quit (``apps/macos.md`` § Sync). The agent is
named by the pid its socket answers with before the quit. The driver's
teardown kills the app's whole process group, the agent included
(``drivers/macos.py::_stop_app``), and the teardown check below asserts it.

**The linux arm.** The GTK app's e2e agent is the direct child its
``SystemdAgentSpawner`` spawns under e2e (``apps/fauna-linux/src/sync_agent.rs``),
on the socket under the launch's private ``XDG_RUNTIME_DIR`` — no pin, the
same "linux needs nothing" as ``helpers.sync_seats``. The app runs under
``xvfb-run`` in a session of its own with no controlling tty, so no hangup
reaches the agent when the app exits and no opt-in is needed. The quit door is
the window close (``driver.window_close()``): the launch's private bus owns no
tray host, so a close quits rather than hides (``drivers/linux.py::
_wants_private_bus``). The agent is named by the pid its socket answers with
before the quit. It stays in the launch's process group, which the driver's
teardown kills whole (``drivers/linux.py::_stop_app`` → ``terminate_tree``),
and the teardown check below asserts it.

**How the renewal is forced, with no TTL hook.** ``fauna.sessions.revoke_all``
ends every session of the account and closes their sockets ``4401``; the
agent's readers route a rejected credential to a renewal *at once* over its
store principal. With the app closed and the grant intact the renewal must
SUCCEED: a session whose ``minted_by_device`` names the principal appears in
``fauna.sessions.list`` — the positive observable that the helper signed itself
in again. A file dropped into the folder afterwards reaching the nest is the
"keeps syncing" half.

**Latency-independent** (convention 14): every step polls state — the socket's
peer, the nest's session list, the nest's media list — never a sleep. Budgets
are ceilings a green run never pays.
"""

from __future__ import annotations

import pytest

from actions import ActionLayer
from clients.ws_rpc_admin_client import WsRpcAdminClient
from conftest import (
    _bind_live_location,
    _make_user,
    _seeded_environment,
    get_available_apps,
)
from drivers import create_driver
from helpers import enrollment
from helpers.sync_agent_ipc_unix import server_pid
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    # macos joined 2026-11-09 (the arm in the module docstring). Its launch pins
    # its own agent, so the session-wide `real_sync_agent` marker is not needed.
    pytest.mark.macos,
    # linux joined 2026-11-11 (the arm in the module docstring): every linux
    # launch already runs its own agent on a private socket.
    pytest.mark.linux,
]

#: The folder's name on the nest.
FOLDER = "agent-survives-quit"

#: tui's quit: `exit-tab` → `should_quit` → the main loop ends. macOS's:
#: `NSApp.terminate` and its bounded leave-flush. linux's: the window close
#: with no tray host, then `xvfb-run` tearing its display down.
APP_EXIT_S = 30.0

#: The agent answering on its socket after the quit — it never stopped, so this
#: is a bound on one connect, generous for a loaded box.
AGENT_ANSWERS_S = 30.0

#: One reconnect plus one renewal round trip over the principal.
RENEWAL_S = 120.0

#: The engine's upload of a dropped file: the watcher sees it at once; the
#: harness's 30 s rescan (`FAUNA_E2E_RESCAN_MS`) is the backstop.
UPLOAD_S = 120.0


@pytest.fixture(params=["tui", "macos", "linux"])
def launch_client(request):
    """The app under test, in the ``--client``-filterable fixture shape."""
    if request.param not in get_available_apps():
        pytest.skip(f"{request.param} is not available on this machine")
    return request.param


def _launch(launch_client, request, nest_instance):
    """Launch the app under test with an agent that may outlive it; return the
    driver."""
    if launch_client == "tui":
        driver = create_driver("tui")
        driver.launch(
            {
                "app_path": request.getfixturevalue("tui_app_path"),
                "url": nest_instance["url"],
                # Read by the DRIVER: the agent leads a group of its own.
                "sync_agent_own_group": True,
                "environment": _seeded_environment(request, nest_instance),
            }
        )
        return driver
    from conftest import _build_app_config
    from drivers.inprocess_agent import InProcessAgentDriver
    from helpers.sync_seats import pin_macos_seat_sync_agent

    config = _build_app_config(launch_client, nest_instance, request)
    if launch_client == "macos":
        pin_macos_seat_sync_agent(
            config, agent_bin=request.getfixturevalue("macos_sync_agent_binary")
        )
    driver = create_driver(launch_client)
    driver.launch(config)
    if isinstance(driver, InProcessAgentDriver):
        try:
            driver.assert_render_ready()
        except BaseException:
            driver.teardown()
            raise
    return driver


def _quit(launch_client, driver) -> None:
    """The app's real quit door: tui's `exit-tab`, macOS's ⌘Q, linux's window
    close."""
    if launch_client == "tui":
        driver.click("exit-tab")
    elif launch_client == "linux":
        driver.window_close()
    else:
        driver.quit_app()


def _agent_pid(launch_client, driver, socket_path: str) -> int:
    """The pid of THIS launch's agent while the app still runs. tui's is the
    own-group leader the launch recorded, cross-checked against the socket's
    peer. macOS's and linux's is the socket's peer itself."""
    if launch_client == "tui":
        pgid = wait_until(
            lambda: driver.agent_pgid,
            AGENT_ANSWERS_S,
            diagnose=lambda: "the opted-in agent never recorded its pgid",
        )
        assert server_pid(socket_path) == pgid, (
            f"the agent serving {socket_path} is not the own-group agent this "
            f"launch recorded (pgid {pgid})"
        )
        return pgid
    return server_pid(socket_path, timeout=AGENT_ANSWERS_S)


def _pid_alive(pid: int) -> bool:
    import os

    try:
        os.kill(pid, 0)  # signal 0: a liveness probe, never a kill
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def _agent_alive(launch_client, driver, pid: int) -> bool:
    if launch_client == "tui":
        return driver.agent_group_alive()
    return _pid_alive(pid)


def _media_items(nest_url: str, user: dict) -> list[dict]:
    # A fresh connection per read: revoke_all closes every cached one.
    actor = bytes.fromhex(user["actor_id_hex"])
    with WsRpcAdminClient(nest_url, actor, bytes(user["signing_key"])) as client:
        return client.call(
            "fauna.media.list", {"limit": 100, "cursor_version": 2}
        ).get("items", [])


def _sessions(nest_url: str, user: dict) -> list[dict]:
    actor = bytes.fromhex(user["actor_id_hex"])
    with WsRpcAdminClient(nest_url, actor, bytes(user["signing_key"])) as client:
        return client.call("fauna.sessions.list", {}).get("sessions", [])


@pytest.mark.host_platform("linux", "macos")
@pytest.mark.feature("local-folder-sync")
def test_the_helper_keeps_syncing_and_renews_its_own_sign_in_after_tui_quits(
    launch_client, nest_instance, request, test_user, tmp_path
):
    from common.auth import user_create_folder, user_folder_ref

    nest_url = nest_instance["url"]
    # A DEDICATED actor: revoke_all ends every session of the account, which
    # must never reach the session-scoped shared user.
    user = _make_user(nest_instance)
    secret_key = user["signing_key"].encode().hex()
    watch_dir = tmp_path / "bound-folder"
    watch_dir.mkdir()
    # The folder exists on the nest before the engine starts (the
    # `_bound_location_media` rule: engine startup reads the roster once).
    user_create_folder(
        nest_instance["port"], FOLDER, secret_key=secret_key, base_url=nest_url
    )
    folder_id = user_folder_ref(
        nest_instance["port"], FOLDER, secret_key=secret_key, base_url=nest_url
    )

    driver = _launch(launch_client, request, nest_instance)
    app = ActionLayer(driver)
    agent_pid = None
    try:
        enrollment.sign_in(app, request, nest_instance, user)
        _writer, named_row, _roster = enrollment.await_enrollment(app, nest_url, user)
        principal = enrollment.principals(nest_url, user).get(named_row)
        assert principal, (
            f"the machine's named row {named_row} carries no grant after the "
            f"enrollment latch; roster {enrollment.principals(nest_url, user)}"
        )
        _bind_live_location(app, watch_dir, FOLDER, folder_id)

        # Baseline: the bound folder syncs while the app runs.
        (watch_dir / "before-quit.txt").write_bytes(b"written while the app runs\n")
        wait_until(
            lambda: len(_media_items(nest_url, user)) == 1,
            UPLOAD_S,
            interval=0.5,
            diagnose=lambda: (
                f"precondition: the file written while the app runs never reached the "
                f"nest; media={_media_items(nest_url, user)!r}; agent log tail: "
                f"{(driver.app_stderr_text() or '')[-2000:]!r}"
            ),
        )

        socket_path = driver.sync_agent_socket
        assert socket_path, f"the {launch_client} launch publishes no agent socket"
        agent_pid = _agent_pid(launch_client, driver, socket_path)

        # The real quit door.
        _quit(launch_client, driver)
        assert driver.wait_app_exit(APP_EXIT_S), (
            f"{launch_client} did not exit at its quit door; "
            f"alive={driver.is_app_alive()}"
        )

        # The SAME agent still serves its socket with the app gone.
        survivor = wait_until(
            lambda: _peer_or_none(socket_path),
            AGENT_ANSWERS_S,
            interval=0.5,
            diagnose=lambda: (
                f"nothing answers {socket_path} after {launch_client} quit; agent "
                f"{agent_pid} alive={_agent_alive(launch_client, driver, agent_pid)}"
            ),
        )
        assert survivor == agent_pid, (
            f"after the quit {socket_path} is served by pid {survivor}, not the "
            f"agent that was running (pid {agent_pid})"
        )

        # Force the renewal: every session ends, the agent's included.
        actor = bytes.fromhex(user["actor_id_hex"])
        with WsRpcAdminClient(nest_url, actor, bytes(user["signing_key"])) as client:
            # `keep_token_id` naming no session ends them all.
            client.call("fauna.sessions.revoke_all", {"keep_token_id": ""})

        seen: list = [[]]

        def renewed():
            seen[0] = _sessions(nest_url, user)
            return any(s.get("minted_by_device") == principal for s in seen[0])

        wait_until(
            renewed,
            RENEWAL_S,
            interval=1.0,
            diagnose=lambda: (
                f"no session minted by the store principal {principal} appeared "
                f"after revoke_all with {launch_client} closed (the agent should "
                f"have renewed over it at once); sessions' minted_by_device: "
                f"{[s.get('minted_by_device') for s in seen[0]]!r}; agent alive="
                f"{_agent_alive(launch_client, driver, agent_pid)}; log tail: "
                f"{(driver.app_stderr_text() or '')[-2000:]!r}"
            ),
        )

        # …and keeps syncing on that renewed sign-in.
        (watch_dir / "after-quit.txt").write_bytes(b"written while the app is closed\n")
        wait_until(
            lambda: len(_media_items(nest_url, user)) == 2,
            UPLOAD_S,
            interval=0.5,
            diagnose=lambda: (
                f"the file written after {launch_client} quit never reached the "
                f"nest; media={_media_items(nest_url, user)!r}; agent alive="
                f"{_agent_alive(launch_client, driver, agent_pid)}; log tail: "
                f"{(driver.app_stderr_text() or '')[-2000:]!r}"
            ),
        )
    finally:
        driver.teardown()
    # Convention 10: the agent outlived the app on purpose, never the launch.
    if agent_pid is not None:
        from drivers.port_util import group_alive

        left = (
            group_alive(agent_pid) if launch_client == "tui" else _pid_alive(agent_pid)
        )
        assert not left, f"teardown left the agent (pid {agent_pid}) running"


def _peer_or_none(socket_path: str) -> int | None:
    try:
        return server_pid(socket_path, timeout=2.0)
    except Exception:  # noqa: BLE001 — not answering yet reads as "not yet"
        return None
