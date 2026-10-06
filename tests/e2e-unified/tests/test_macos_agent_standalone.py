"""Standalone test: does the macOS TestAgent poll for commands and push acks?

Runs a fake HTTP bridge and launches the macOS app binary directly.
No XCUITest, no Apple bridge, no nest server — just the TestAgent protocol.

This isolates the question: does the macOS app's TestAgent correctly
fetch commands from /app/commands and POST acknowledgments to /app/state?
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from http.server import HTTPServer, BaseHTTPRequestHandler
from pathlib import Path

import pytest

pytestmark = pytest.mark.tier_2

_repo_root = Path(__file__).resolve().parent.parent.parent.parent

# The macOS debug binary built by `just mac-debug` (via swift build)
_MACOS_EXE = (
    _repo_root / "apps" / "fauna-apple" / ".build"
    / "arm64-apple-macosx" / "debug" / "FaunaMacOS"
)


# --- Fake bridge HTTP server ---

_lock = threading.Lock()


class _FakeBridgeState:
    """Shared mutable state for the fake bridge."""
    def __init__(self):
        self.command_queue: list[dict] = []
        self.received_states: list[dict] = []


def _make_handler(state: _FakeBridgeState):
    """Create a request handler class closed over `state`."""

    class FakeBridge(BaseHTTPRequestHandler):
        def do_GET(self):
            if self.path == "/app/commands":
                with _lock:
                    if state.command_queue:
                        cmd = state.command_queue.pop(0)
                        body = json.dumps(cmd).encode()
                        self.send_response(200)
                    else:
                        body = b""
                        self.send_response(204)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            elif self.path == "/app/state":
                with _lock:
                    if state.received_states:
                        body = json.dumps(state.received_states[-1]).encode()
                        self.send_response(200)
                    else:
                        body = b""
                        self.send_response(204)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            else:
                self.send_response(404)
                self.end_headers()

        def do_POST(self):
            length = int(self.headers.get("Content-Length", 0))
            body = self.rfile.read(length) if length else b"{}"
            data = json.loads(body)
            if self.path == "/app/state":
                with _lock:
                    state.received_states.append(data)
                self.send_response(200)
                resp = b'{"received":true}'
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(resp)))
                self.end_headers()
                self.wfile.write(resp)
            else:
                self.send_response(404)
                self.end_headers()

        def log_message(self, *_args):
            pass  # suppress request logging

    return FakeBridge


@pytest.fixture()
def fake_bridge():
    """Start a fake bridge HTTP server, yield (url, state), then shut down."""
    state = _FakeBridgeState()
    handler = _make_handler(state)
    server = HTTPServer(("127.0.0.1", 0), handler)
    port = server.server_address[1]
    url = f"http://127.0.0.1:{port}"
    t = threading.Thread(target=server.serve_forever, daemon=True)
    t.start()
    yield url, state
    server.shutdown()


@pytest.fixture()
def macos_app(fake_bridge, tmp_path):
    """Launch the macOS app with FAUNA_E2E_BRIDGE and yield the process."""
    if not _MACOS_EXE.exists():
        pytest.skip(f"macOS binary not found at {_MACOS_EXE} — run 'just mac-debug'")

    url, _state = fake_bridge

    # Write stderr to a temp file so we can read it without blocking.
    # Piping stdout/stderr directly can interfere with the macOS run loop.
    log_path = tmp_path / "app.log"
    log_fh = open(log_path, "w")

    env = {
        **os.environ,
        "FAUNA_E2E_BRIDGE": url,
    }

    from drivers.port_util import popen_group_kwargs, reap_descendants_of

    proc = subprocess.Popen(
        [str(_MACOS_EXE)],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=log_fh,
        **popen_group_kwargs(),
    )
    # Windows half of the die-with-the-run guarantee — no-op off Windows.
    reap_descendants_of(proc.pid)
    yield proc, log_path

    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()
    log_fh.close()


def _wait_for_states(state: _FakeBridgeState, min_count: int, timeout: float = 10.0):
    """Wait until we have at least `min_count` state pushes."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        with _lock:
            if len(state.received_states) >= min_count:
                return
        time.sleep(0.2)


def _wait_for_ack(state: _FakeBridgeState, cmd_id: str, timeout: float = 10.0) -> dict | None:
    """Wait until a state push with matching last_command_id appears."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        with _lock:
            for s in state.received_states:
                if s.get("last_command_id") == cmd_id:
                    return s
        time.sleep(0.2)
    return None


# --- Tests ---


@pytest.mark.skipif(sys.platform != "darwin", reason="macOS only")
class TestMacOSAgentProtocol:
    """Test the macOS TestAgent command/ack protocol without the full E2E stack."""

    def _app_diag(self, macos_app) -> str:
        """Collect diagnostics from the app process."""
        proc, log_path = macos_app
        exit_code = proc.poll()
        diag = ""
        if exit_code is not None:
            log = log_path.read_text()[-2000:] if log_path.exists() else "(no log)"
            diag = f" App exited with code {exit_code}. Log:\n{log}"
        elif log_path.exists():
            log = log_path.read_text()[-1000:]
            if log:
                diag = f" App log tail:\n{log}"
        return diag

    def test_initial_state_push(self, fake_bridge, macos_app):
        """TestAgent should push initial state within a few seconds of launch."""
        _url, state = fake_bridge
        _wait_for_states(state, 1, timeout=15)

        diag = self._app_diag(macos_app)
        with _lock:
            assert len(state.received_states) > 0, (
                f"TestAgent did not push any state within 15s of app launch.{diag}"
            )
            initial = state.received_states[-1]

        # Should have the protocol fields
        assert "state" in initial, f"State push missing 'state' key: {list(initial.keys())}"
        assert "ready" in initial, f"State push missing 'ready' key: {list(initial.keys())}"
        assert initial["ready"] is True, f"Initial state should be ready=true, got {initial['ready']}"

    def test_command_ack_round_trip(self, fake_bridge, macos_app):
        """TestAgent should fetch a command and ack it by pushing state with matching last_command_id."""
        _url, state = fake_bridge

        # Wait for initial state first
        _wait_for_states(state, 1, timeout=15)
        with _lock:
            assert len(state.received_states) > 0, f"No initial state push.{self._app_diag(macos_app)}"

        # Send a simple session patch command
        cmd_id = "test-cmd-session-1"
        with _lock:
            state.received_states.clear()
            state.command_queue.append({
                "id": cmd_id,
                "action": "patch",
                "state": {"session": {"authenticated": True}},
            })

        ack = _wait_for_ack(state, cmd_id, timeout=10)
        assert ack is not None, (
            f"TestAgent did not ack command {cmd_id} within 10s. "
            f"Received {len(state.received_states)} state pushes after command."
            f"{self._app_diag(macos_app)}"
        )
        assert ack.get("ready") is True, f"Ack should have ready=true, got {ack.get('ready')}"

    def test_nav_command_ack(self, fake_bridge, macos_app):
        """TestAgent should ack a navigation command."""
        _url, state = fake_bridge

        # Wait for initial state
        _wait_for_states(state, 1, timeout=15)
        with _lock:
            assert len(state.received_states) > 0, f"No initial state push.{self._app_diag(macos_app)}"

        # Send nav command
        cmd_id = "test-cmd-nav-1"
        with _lock:
            state.received_states.clear()
            state.command_queue.append({
                "id": cmd_id,
                "action": "patch",
                "state": {"nav": {"stack": [{"view": "contacts"}]}},
            })

        ack = _wait_for_ack(state, cmd_id, timeout=10)
        assert ack is not None, (
            f"TestAgent did not ack nav command {cmd_id} within 10s. "
            f"State pushes after command: {len(state.received_states)}"
            f"{self._app_diag(macos_app)}"
        )

    def test_reset_command_ack(self, fake_bridge, macos_app):
        """TestAgent should ack a reset command."""
        _url, state = fake_bridge

        # Wait for initial state
        _wait_for_states(state, 1, timeout=15)
        with _lock:
            assert len(state.received_states) > 0, f"No initial state push.{self._app_diag(macos_app)}"

        # Send reset command
        cmd_id = "test-cmd-reset-1"
        with _lock:
            state.received_states.clear()
            state.command_queue.append({
                "id": cmd_id,
                "action": "reset",
            })

        ack = _wait_for_ack(state, cmd_id, timeout=10)
        assert ack is not None, (
            f"TestAgent did not ack reset command {cmd_id} within 10s. "
            f"State pushes after command: {len(state.received_states)}"
            f"{self._app_diag(macos_app)}"
        )

    def test_multiple_commands_sequential(self, fake_bridge, macos_app):
        """TestAgent should handle multiple commands in sequence."""
        _url, state = fake_bridge

        # Wait for initial state
        _wait_for_states(state, 1, timeout=15)
        with _lock:
            assert len(state.received_states) > 0, f"No initial state push.{self._app_diag(macos_app)}"

        for i in range(5):
            cmd_id = f"test-cmd-seq-{i}"
            with _lock:
                state.received_states.clear()
                state.command_queue.append({
                    "id": cmd_id,
                    "action": "patch",
                    "state": {"nav": {"stack": [{"view": "feed"}]}},
                })

            ack = _wait_for_ack(state, cmd_id, timeout=10)
            assert ack is not None, (
                f"TestAgent failed to ack command #{i} ({cmd_id}) within 10s. "
                f"State pushes after command: {len(state.received_states)}"
                f"{self._app_diag(macos_app)}"
            )

    def test_polling_continues_after_idle(self, fake_bridge, macos_app):
        """TestAgent should still respond after an idle period (no commands for a few seconds)."""
        _url, state = fake_bridge

        # Wait for initial state
        _wait_for_states(state, 1, timeout=15)
        with _lock:
            assert len(state.received_states) > 0, f"No initial state push.{self._app_diag(macos_app)}"

        # Wait 3 seconds with no commands
        time.sleep(3)

        # Now send a command — agent should still be polling
        cmd_id = "test-cmd-after-idle"
        with _lock:
            state.received_states.clear()
            state.command_queue.append({
                "id": cmd_id,
                "action": "patch",
                "state": {"session": {"authenticated": True}},
            })

        ack = _wait_for_ack(state, cmd_id, timeout=10)
        assert ack is not None, (
            f"TestAgent stopped responding after idle period. "
            f"Command {cmd_id} not acked within 10s."
            f"{self._app_diag(macos_app)}"
        )
