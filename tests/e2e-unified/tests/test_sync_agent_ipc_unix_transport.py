"""tier_1: `helpers/sync_agent_ipc_unix.py`'s transport, round-tripped against a
minimal in-process fake unix-socket server — no `fauna-sync-agent` binary, no
driver, no nest.

The dag-cbor *encoding* is already pinned byte-for-byte against the Rust golden
by `test_sync_agent_ipc_conformance.py` (reused here, not re-tested). What this
file proves is the transport this module adds on top: framing a request onto
the wire, reading a length-prefixed response back, and the `SyncAgentClient`
convenience methods unwrapping it — the same shape
`libs/fauna-ipc/src/unix_transport.rs::unix_socket_request_response_round_trip`
proves on the Rust side, mirrored here in Python against a fake peer so this
module's own bugs surface in milliseconds rather than a multi-minute macOS
real-agent e2e run.
"""
from __future__ import annotations

import shutil
import socket
import struct
import tempfile
import threading

import cbor2
import pytest

from helpers.sync_agent_ipc_unix import SyncAgentClient, send_request, wait_for_socket

pytestmark = pytest.mark.tier_1


def _encode_response(resp: dict) -> bytes:
    payload = cbor2.dumps(resp, canonical=True)
    return struct.pack("<I", len(payload)) + payload


def _read_request_frame(conn: socket.socket) -> dict | None:
    """Returns None for a connect-and-close probe with no data — exactly what
    `wait_for_socket` does (connect, then close without writing), which this
    fake server must tolerate without treating it as a real request."""
    len_buf = conn.recv(4, socket.MSG_WAITALL)
    if not len_buf:
        return None
    (length,) = struct.unpack("<I", len_buf)
    payload = conn.recv(length, socket.MSG_WAITALL)
    return cbor2.loads(payload)


class _FakeAgent:
    """Binds a real unix socket, accepts connections on a background thread,
    and answers each request with the next canned response from `responses`
    (one entry per expected request, in order)."""

    def __init__(self, socket_path: str, responses: list[dict]):
        self.socket_path = socket_path
        self._responses = list(responses)
        self._requests: list[dict] = []
        self._sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._sock.bind(socket_path)
        self._sock.listen(1)
        self._thread = threading.Thread(target=self._serve, daemon=True)

    def start(self) -> None:
        self._thread.start()

    def _serve(self) -> None:
        while len(self._requests) < len(self._responses):
            try:
                conn, _ = self._sock.accept()
            except OSError:
                return  # `close()` ran (fixture teardown) while accept() blocked
            with conn:
                req = _read_request_frame(conn)
                if req is None:
                    continue  # a wait_for_socket probe — not a real request
                self._requests.append(req)
                resp = self._responses[len(self._requests) - 1]
                conn.sendall(_encode_response(resp))

    def join(self, timeout: float = 5.0) -> None:
        self._thread.join(timeout)
        assert not self._thread.is_alive(), "fake agent thread did not finish"

    def close(self) -> None:
        self._sock.close()


@pytest.fixture
def short_tmp_dir():
    """A `/tmp`-rooted dir, NOT pytest's own `tmp_path` — the latter nests under
    `.../pytest-of-user/pytest-N/<full-test-name>0/`, which routinely blows
    macOS's 104-byte `sun_path` budget for a bound socket (the same class of
    bug `unix_transport.rs::check_socket_path_len`'s docstring names as the
    2026-07-24 macOS multiseat incident).

    Every test here reaches this fixture first, so it is where a box that has
    no `AF_UNIX` declares itself (e2e conventions point 7). The transport
    under test is the unix one only: the agent serves it under `#[cfg(unix)]`
    (`libs/fauna-ipc/src/lib.rs`), a Windows agent speaks the named pipe
    `helpers/sync_agent_ipc.py` drives, and the Windows CPython build ships no
    `socket.AF_UNIX` at all — neither the helper nor its fake peer can exist
    there. Keyed on the missing attribute, not the OS name, so an interpreter
    that grows the family runs the proofs instead of skipping them."""
    if not hasattr(socket, "AF_UNIX"):
        from helpers.app_surface import skip_environment

        skip_environment(
            "no socket.AF_UNIX in this Python — the unix-socket sync-agent "
            "transport is unix-only (Windows uses the named pipe)"
        )
    d = tempfile.mkdtemp(prefix="fauna-unix-ipc-test-", dir="/tmp")
    yield d
    shutil.rmtree(d, ignore_errors=True)


@pytest.fixture
def fake_agent(short_tmp_dir):
    made: list[_FakeAgent] = []

    def _make(responses: list[dict]) -> _FakeAgent:
        agent = _FakeAgent(f"{short_tmp_dir}/sync-agent.sock", responses)
        agent.start()
        made.append(agent)
        return agent

    yield _make
    for agent in made:
        agent.close()


def test_wait_for_socket_returns_once_listening(fake_agent):
    agent = fake_agent([{"id": 1, "result": {"Ok": "Empty"}}])
    wait_for_socket(agent.socket_path, timeout=2.0)


def test_wait_for_socket_times_out_when_nothing_is_listening(short_tmp_dir):
    never_bound = f"{short_tmp_dir}/nobody-home.sock"
    with pytest.raises(TimeoutError):
        wait_for_socket(never_bound, timeout=0.3)


def test_send_request_round_trips_a_service_status_response(fake_agent):
    agent = fake_agent([
        {
            "id": 1,
            "result": {
                "Ok": {
                    "ServiceStatus": {
                        "version": "0.0.0-test",
                        "uptime_secs": 42,
                        "connection": "Connected",
                        "sync": {
                            "connected": True,
                            "syncing": False,
                            "files_pending": 0,
                            "bytes_pending": 0,
                            "last_sync": None,
                        },
                    }
                }
            },
        }
    ])
    client = SyncAgentClient(agent.socket_path, timeout=2.0)
    status = client.service_status()
    assert status["connection"] == "Connected"
    assert status["version"] == "0.0.0-test"
    agent.join()


def test_service_status_reflects_disconnected_after_unprovision(fake_agent):
    """The exact transition the macOS real-agent test polls for post-sign-out:
    `connection` flips Connected -> Disconnected, never re-Connects."""
    agent = fake_agent([
        {"id": 1, "result": {"Ok": {"ServiceStatus": {
            "version": "v", "uptime_secs": 1, "connection": "Connected",
            "sync": {"connected": True, "syncing": False, "files_pending": 0,
                      "bytes_pending": 0, "last_sync": None},
        }}}},
        {"id": 2, "result": {"Ok": {"ServiceStatus": {
            "version": "v", "uptime_secs": 2, "connection": "Disconnected",
            "sync": {"connected": False, "syncing": False, "files_pending": 0,
                      "bytes_pending": 0, "last_sync": None},
        }}}},
    ])
    client = SyncAgentClient(agent.socket_path, timeout=2.0)
    assert client.service_status()["connection"] == "Connected"
    assert client.service_status()["connection"] == "Disconnected"
    agent.join()


def test_provision_raises_on_err_response(fake_agent):
    agent = fake_agent([{"id": 1, "result": {"Err": "no capability slot"}}])
    client = SyncAgentClient(agent.socket_path, timeout=2.0)
    with pytest.raises(RuntimeError, match="no capability slot"):
        client.provision(
            backup_key=b"\x00" * 32,
            actor_id=b"\x11" * 32,
            nest_url="https://example.com",
            device_id="dev-1",
            bearer_token="tok",
            bearer_expires_at=None,
        )
    agent.join()


def test_send_request_reports_a_missing_socket(short_tmp_dir):
    with pytest.raises(OSError):
        send_request(f"{short_tmp_dir}/nobody-home.sock", b"\x00\x00\x00\x00", timeout=1.0)
