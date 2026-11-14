"""tier_1: an agent a fixture starts on a launch's behalf runs in that launch's environment.

``serving_agent`` (``helpers/windows_sync_agent.py``) starts a launch's sync agent
when a fixture needs it serving before login, which is before the app has spawned
it. The app's own spawn (``WindowsDetachedSpawner``,
``libs/fauna-client-sync/src/agent_spawner.rs``) inherits the app's whole
environment, and with it the launch's isolation overrides (e2e-conventions.md
convention 10): the relocated ``LOCALAPPDATA`` the account store hangs off, and the
credential dir and namespace that hold the account's keys. An agent spawned on the
bare pytest environment found neither. Its account host never assembled ("the
account runtime is not running"), custody stayed unreadable, and every bound set's
engine was withheld, so
``test_sync_live_apply.py::test_sync_location_map_edits_apply_live[windows]`` read
``running: False`` on every run.

So the launch environment is a required argument: a caller states whose agent it
starts, and an agent a fixture starts is the agent the app would have started.
"""

from __future__ import annotations

import subprocess
import sys
from types import SimpleNamespace

import pytest

pytestmark = [
    pytest.mark.skipif(sys.platform != "win32", reason="Windows named-pipe agent helper"),
    pytest.mark.tier_1,
]

from helpers import windows_sync_agent as wsa  # noqa: E402

_PIPE = r"\\.\pipe\fauna-sync-test-launch-env"
_PID = 4242


class _FakeAgent:
    """The spawned agent: records the environment it was started with."""

    started: list[dict] = []

    def __init__(self, argv, *, env, **_kwargs):
        self.argv = argv
        self.pid = _PID
        _FakeAgent.started.append(dict(env))

    def terminate(self):
        pass

    def wait(self, timeout=None):
        return 0

    def kill(self):
        pass


@pytest.fixture
def nothing_serving(monkeypatch):
    """No agent serves the pipe until our spawn does, and the spawn is a fake."""
    _FakeAgent.started = []
    probes = {"n": 0}

    def wait_for_pipe(pipe_path, timeout):
        probes["n"] += 1
        if probes["n"] == 1:
            raise TimeoutError(pipe_path)

    monkeypatch.setattr(
        wsa,
        "ipc",
        SimpleNamespace(
            wait_for_pipe=wait_for_pipe,
            pipe_server_pid=lambda pipe_path, timeout: _PID,
        ),
    )
    monkeypatch.setattr(
        wsa,
        "subprocess",
        SimpleNamespace(
            Popen=_FakeAgent,
            STDOUT=subprocess.STDOUT,
            TimeoutExpired=subprocess.TimeoutExpired,
        ),
    )
    monkeypatch.setattr(wsa, "popen_group_kwargs", lambda: {})
    monkeypatch.setattr(wsa, "reap_descendants_of", lambda pid: None)
    return _FakeAgent.started


def test_a_spawned_agent_holds_the_launchs_store_root_and_credentials(nothing_serving, tmp_path):
    data_dir = tmp_path / "agent"
    data_dir.mkdir()
    launch_env = {
        "LOCALAPPDATA": str(tmp_path / "localappdata"),
        "FAUNA_E2E_CREDENTIAL_DIR": str(tmp_path / "launch-creds"),
        "FAUNA_KEYRING_APP": "fauna-windows",
    }

    with wsa.serving_agent(
        "fauna-sync-agent.exe", _PIPE, data_dir, tmp_path / "spawn.log", launch_env=launch_env
    ) as spawned:
        assert spawned is not None, "nothing served the pipe, so this call started the agent"

    [env] = nothing_serving
    for name, value in launch_env.items():
        assert env.get(name) == value, (
            f"the agent this fixture started reads {name}={env.get(name)!r}, but the launch "
            f"set {value!r}: the app's own spawn inherits the launch's environment, so an "
            f"agent started in its place must too, or it opens another account store and "
            f"credential set than the app's"
        )


def test_with_no_launch_the_agent_keeps_a_private_credential_store(nothing_serving, tmp_path):
    data_dir = tmp_path / "agent"
    data_dir.mkdir()

    with wsa.serving_agent(
        "fauna-sync-agent.exe", _PIPE, data_dir, tmp_path / "spawn.log", launch_env=None
    ):
        pass

    [env] = nothing_serving
    assert env["FAUNA_E2E_CREDENTIAL_DIR"] == str(data_dir / "credentials"), (
        "an agent no launch owns still never reaches the box's real credential store"
    )


def test_a_caller_must_state_the_launch_whose_agent_it_starts(tmp_path):
    with pytest.raises(TypeError):
        wsa.serving_agent("fauna-sync-agent.exe", _PIPE, tmp_path, tmp_path / "spawn.log")
