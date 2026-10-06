"""Tests for the state protocol driver API.

These test the Python-side logic (command ID generation, polling, timeout)
against a mock bridge implemented as a simple HTTP server.
"""
import http.server
import json
import threading
import time

import pytest

pytestmark = pytest.mark.tier_1


class MockBridge(http.server.HTTPServer):
    """Minimal HTTP server that implements the 4 state relay endpoints."""

    def __init__(self):
        self.command_queue = []
        self.cached_state = {}
        super().__init__(("127.0.0.1", 0), MockHandler)
        self.port = self.server_address[1]
        self._thread = threading.Thread(target=self.serve_forever, daemon=True)
        self._thread.start()

    def shutdown(self):
        super().shutdown()


class MockHandler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(length)) if length else {}
        if self.path == "/app/commands":
            self.server.command_queue.append(body)
            self._respond(201, {"queued": True})
        elif self.path == "/app/state":
            self.server.cached_state = body
            self._respond(200, {"received": True})
        else:
            self._respond(404, {"error": "not found"})

    def do_GET(self):
        if self.path == "/app/commands":
            if not self.server.command_queue:
                self._respond(204, {})
            else:
                self._respond(200, self.server.command_queue.pop(0))
        elif self.path == "/app/state":
            if not self.server.cached_state:
                self._respond(204, {})
            else:
                self._respond(200, self.server.cached_state)
        else:
            self._respond(404, {"error": "not found"})

    def _respond(self, status, body):
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        data = json.dumps(body).encode()
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        if status != 204:
            self.wfile.write(data)

    def log_message(self, *args):
        pass


@pytest.fixture
def mock_bridge():
    bridge = MockBridge()
    yield bridge
    bridge.shutdown()


def test_set_state_posts_command(mock_bridge):
    """set_state should POST a patch command to /app/commands."""
    import sys
    from pathlib import Path
    sys.path.insert(0, str(Path(__file__).parent.parent))
    from drivers.http_bridge import HttpBridgeDriver

    driver = HttpBridgeDriver(f"http://127.0.0.1:{mock_bridge.port}")

    # Simulate the app acknowledging the command
    def ack_commands():
        time.sleep(0.1)
        if mock_bridge.command_queue:
            cmd = mock_bridge.command_queue[0]
            mock_bridge.cached_state = {
                "last_command_id": cmd["id"],
                "state": cmd.get("state", {}),
            }

    t = threading.Thread(target=ack_commands, daemon=True)
    t.start()

    driver.set_state({"session": {"authenticated": True}})

    assert len(mock_bridge.command_queue) <= 1  # consumed or still there
    assert mock_bridge.cached_state["last_command_id"].startswith("cmd_")


def test_get_state_reads_cache(mock_bridge):
    """get_state should return the cached state from the bridge."""
    import sys
    from pathlib import Path
    sys.path.insert(0, str(Path(__file__).parent.parent))
    from drivers.http_bridge import HttpBridgeDriver

    driver = HttpBridgeDriver(f"http://127.0.0.1:{mock_bridge.port}")

    mock_bridge.cached_state = {
        "last_command_id": "cmd_0",
        "state": {"session": {"authenticated": True}},
    }

    state = driver.get_state()
    assert state["session"]["authenticated"] is True


def test_get_state_returns_none_when_empty(mock_bridge):
    """get_state should return None when no state has been pushed."""
    import sys
    from pathlib import Path
    sys.path.insert(0, str(Path(__file__).parent.parent))
    from drivers.http_bridge import HttpBridgeDriver

    driver = HttpBridgeDriver(f"http://127.0.0.1:{mock_bridge.port}")

    state = driver.get_state()
    assert state is None
