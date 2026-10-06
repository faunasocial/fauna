"""tier_3 e2e: per-user Windows sync agent — provision-drives-nest-url + two-pipe isolation.

Proves:
  (a) The agent's nest URL comes from the live provision call with NO device.toml.
  (b) Two per-session named pipes are isolated (no cross-talk between agents).

Process safety:
  - The agent processes (fauna-sync-agent.exe) are OUR OWN children — Popen + terminate
    is required and allowed.
  - The nest is the shared `nest_instance` fixture — never Popen/kill a nest or bridge.
"""

from __future__ import annotations

import sys
import time
from pathlib import Path

import pytest

from helpers import sync_agent_ipc as ipc
from helpers.windows_sync_agent import running_agent

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.skipif(sys.platform != "win32", reason="windows per-user sync agent (named pipes)"),
]

# `sync_agent_binary` (build fauna-sync-agent.exe once per session) and
# `running_agent` (Popen it on a test-chosen pipe) are shared with the
# `isolated_sync_agent` UI harness — see conftest.py / helpers/windows_sync_agent.py.
# One spawner, not two.


# ---------------------------------------------------------------------------
# Log polling helper  (OS file-buffer lag mitigation)
# ---------------------------------------------------------------------------

def read_log_until(log_path, needle, timeout=5.0):
    """Poll log_path until needle appears in its content or timeout expires.

    Returns the last read content (caller asserts for a useful failure message).
    """
    deadline = time.monotonic() + timeout
    content = ""
    while time.monotonic() < deadline:
        try:
            content = Path(log_path).read_text(errors="replace")
        except OSError:
            content = ""
        if needle in content:
            return content
        time.sleep(0.2)
    return content  # caller asserts; returns last read for a useful failure message


def read_file_log_until(logs_dir, needle, timeout=5.0):
    """Poll <logs_dir>/fauna.log.<date> (the unified `fauna_log` daily-rolling
    sink) until needle appears, or timeout. Returns the concatenated content of
    every ``fauna.log*`` file (or "" if the dir/files never appear).

    The agent is GUI-subsystem in production (no console at logon) → its stderr
    is discarded, so the ON-DISK file is the only diagnostics sink that survives
    a shipped deploy. Must be polled while the agent is alive: the harness hard-
    terminates the process (TerminateProcess runs no Rust destructors), so the
    WorkerGuard's drop-flush never fires — we rely on the non-blocking writer's
    own periodic flush, exactly as the stderr poll does.
    """
    deadline = time.monotonic() + timeout
    content = ""
    logs_dir = Path(logs_dir)
    while time.monotonic() < deadline:
        parts = []
        if logs_dir.is_dir():
            for f in sorted(logs_dir.glob("fauna.log*")):
                try:
                    parts.append(f.read_text(errors="replace"))
                except OSError:
                    pass
        content = "".join(parts)
        if needle in content:
            return content
        time.sleep(0.2)
    return content  # caller asserts; returns last read for a useful failure message


# ---------------------------------------------------------------------------
# Test (a): provision drives the nest URL — no device.toml
# ---------------------------------------------------------------------------

def test_provision_drives_nest_url_without_device_toml(nest_instance, sync_agent_binary, tmp_path):
    """Prove the per-user model: nest_url comes from ProvisionCapability, not device.toml."""
    data_dir = tmp_path / "agent_a"
    data_dir.mkdir()
    pipe = r"\\.\pipe\fauna-sync-test-a"
    log_path = tmp_path / "agent_a.log"
    nest_url = nest_instance["url"]

    with running_agent(sync_agent_binary, pipe, data_dir, log_path):
        ipc.provision(
            pipe, 1,
            backup_key=bytes(32),
            actor_id=bytes(32),
            nest_url=nest_url,
            device_id="dev-e2e-a",
            bearer_token="t",
            bearer_expires_at=4102444800,
        )
        status = ipc.sync_status(pipe, 2)
        assert status["connected"] is True, f"expected connected after provision, got {status}"

        log = read_log_until(log_path, nest_url, timeout=5)
        assert nest_url in log, (
            f"agent did not log the provisioned nest_url.\nLog tail:\n{log[-2000:]}"
        )

        # The shipped agent is GUI-subsystem (no console at logon) → its stderr
        # is discarded in production, so the daily-rolling ON-DISK log written by
        # the unified `fauna_log` stack is the only diagnostics that survive a
        # real deploy. Assert it exists under <data_dir>/logs and carries the
        # provisioned nest_url. (--data-dir is the override root, so the log
        # lands at <data_dir>/logs/fauna.log.<date>.) Poll while the agent is
        # alive — the harness hard-terminates it, so the guard never flushes.
        logs_dir = data_dir / "logs"
        file_log = read_file_log_until(logs_dir, nest_url, timeout=5)
        assert nest_url in file_log, (
            "agent did not persist the nest_url to the daily-rolling file log "
            f"under {logs_dir} (the only diagnostics sink in a GUI-subsystem "
            "deploy).\n"
            f"logs dir contents: "
            f"{sorted(p.name for p in logs_dir.glob('*')) if logs_dir.is_dir() else 'NO logs dir'}\n"
            f"File-log tail:\n{file_log[-2000:]}"
        )

    # No device.toml was needed anywhere in the sync data dir — proof of the migration.
    assert not (data_dir / "device.toml").exists()


# ---------------------------------------------------------------------------
# Test (b): two per-session pipes are isolated (no cross-talk)
# ---------------------------------------------------------------------------

def test_two_pipe_isolation(nest_instance, sync_agent_binary, tmp_path):
    """Two agents on separate pipes must not see each other's nest_url."""
    pipe_a = r"\\.\pipe\fauna-sync-test-iso-a"
    pipe_b = r"\\.\pipe\fauna-sync-test-iso-b"
    url_a = nest_instance["url"]
    url_b = "https://nest-b.invalid"  # a distinct sentinel; need NOT be a running nest
    da = tmp_path / "iso_a"
    da.mkdir()
    db = tmp_path / "iso_b"
    db.mkdir()
    la = tmp_path / "iso_a.log"
    lb = tmp_path / "iso_b.log"

    with running_agent(sync_agent_binary, pipe_a, da, la), \
         running_agent(sync_agent_binary, pipe_b, db, lb):
        ipc.provision(
            pipe_a, 1,
            backup_key=bytes(32),
            actor_id=bytes(32),
            nest_url=url_a,
            device_id="iso-a",
            bearer_token="t",
            bearer_expires_at=4102444800,
        )
        ipc.provision(
            pipe_b, 1,
            backup_key=bytes(32),
            actor_id=bytes(32),
            nest_url=url_b,
            device_id="iso-b",
            bearer_token="t",
            bearer_expires_at=4102444800,
        )

        assert ipc.sync_status(pipe_a, 2)["connected"] is True
        assert ipc.sync_status(pipe_b, 2)["connected"] is True

        log_a = read_log_until(la, url_a, timeout=5)
        log_b = read_log_until(lb, url_b, timeout=5)

        assert url_a in log_a and url_b not in log_a, "pipe A leaked/saw B's nest_url"
        assert url_b in log_b and url_a not in log_b, "pipe B leaked/saw A's nest_url"


# ---------------------------------------------------------------------------
# Test (c): which agent serves the pipe — the spawner never assumes it is its own
# ---------------------------------------------------------------------------
#
# An agent spawned on a pipe some other agent already serves exits as a duplicate
# (`service.rs`, the per-pipe `Local\FaunaSyncAgent.<leaf>` mutex), yet the pipe
# still answers — so "the pipe is served" never proved "MY agent serves it". Found
# live: every windows launch spawns its own isolated agent on the session pipe, and
# a fixture that spawned "its" agent after that silently talked to the launch's
# agent while reading a per-test dir nothing was written to. Each test below mints its own pipe leaf, so a sibling session's run
# can never be the other agent.

def _own_pipe(tag: str) -> str:
    import os
    return r"\\.\pipe\fauna-sync-test-owner-{}-{}".format(tag, os.getpid())


def test_a_spawn_onto_a_served_pipe_fails_naming_the_agent_that_serves_it(
    sync_agent_binary, tmp_path
):
    from helpers.windows_sync_agent import AgentNotServingError

    pipe = _own_pipe("dup")
    first_dir, second_dir = tmp_path / "first", tmp_path / "second"
    first_dir.mkdir()
    second_dir.mkdir()

    with running_agent(sync_agent_binary, pipe, first_dir, tmp_path / "first.log") as first:
        assert ipc.pipe_server_pid(pipe) == first.pid
        with pytest.raises(AgentNotServingError) as refused:
            with running_agent(sync_agent_binary, pipe, second_dir, tmp_path / "second.log"):
                pytest.fail("the second spawn was handed a pipe its own agent does not serve")
        message = str(refused.value)
        assert refused.value.server_pid == first.pid, message
        assert str(first.pid) in message and str(first_dir) in message, message
        # The first agent is untouched by the refused spawn's teardown.
        assert ipc.pipe_server_pid(pipe) == first.pid


def test_serving_agent_adopts_the_agent_already_serving_its_data_dir(
    sync_agent_binary, tmp_path
):
    from helpers.windows_sync_agent import serving_agent

    pipe = _own_pipe("adopt")
    data_dir = tmp_path / "launch-agent"
    data_dir.mkdir()

    with running_agent(sync_agent_binary, pipe, data_dir, tmp_path / "launch.log") as launch:
        with serving_agent(sync_agent_binary, pipe, data_dir, tmp_path / "fixture.log") as spawned:
            assert spawned is None, "an agent already serving the dir must be adopted, not raced"
            assert ipc.pipe_server_pid(pipe) == launch.pid
        # Adopted, so not ours to stop: the launch's agent still serves.
        assert ipc.pipe_server_pid(pipe) == launch.pid


def test_serving_agent_refuses_an_agent_serving_a_different_data_dir(
    sync_agent_binary, tmp_path
):
    from helpers.windows_sync_agent import AgentNotServingError, serving_agent

    pipe = _own_pipe("foreign")
    theirs, ours = tmp_path / "theirs", tmp_path / "ours"
    theirs.mkdir()
    ours.mkdir()

    with running_agent(sync_agent_binary, pipe, theirs, tmp_path / "theirs.log"):
        with pytest.raises(AgentNotServingError) as refused:
            with serving_agent(sync_agent_binary, pipe, ours, tmp_path / "ours.log"):
                pytest.fail("adopted an agent whose state lives in another dir")
        assert str(theirs) in str(refused.value), str(refused.value)


def test_serving_agent_spawns_when_nothing_serves_the_pipe(sync_agent_binary, tmp_path):
    from helpers.windows_sync_agent import serving_agent

    pipe = _own_pipe("fresh")
    data_dir = tmp_path / "fresh"
    data_dir.mkdir()

    with serving_agent(sync_agent_binary, pipe, data_dir, tmp_path / "fresh.log") as spawned:
        assert spawned is not None
        assert ipc.pipe_server_pid(pipe) == spawned.pid
