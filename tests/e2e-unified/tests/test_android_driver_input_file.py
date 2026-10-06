"""tier_1: android's `set_input_files` carries the picked file's BYTES to the device.

Authority: `docs/goal/architecture/testing.md` § Default app and nest mode →
*Android's run venue* (venue A2: pytest on Linux, the device on the emulator host behind a
tunnel — so no host path is ever a device path) and
`docs/goal/ui/conversations.md` § the attachments-outbound android cell (the
agent reads ``compose.file`` on the device's own filesystem).

WHY THIS FILE EXISTS. Every other bridge-backed driver
runs its app on the pytest host, so the shared ``set_input_files`` hands the
agent the host path and the agent opens it. android's agent (``TestAgent.kt``'s
``compose.file`` arms) opens ``java.io.File(path)`` ON THE DEVICE, where that
path does not exist on any venue. The android driver therefore crosses the
boundary the way ``seed_credentials`` already does: the bytes ride the bridge's
HTTP connection (``POST /input-file``), the bridge — same uid as the app —
writes them into the app's own cache dir and answers with that DEVICE path, and
only that path goes into the ``compose`` patch. Venue-independent by
construction: no ``adb push``, no question of what an untrusted app may read
outside its sandbox.

The instrument is a real loopback HTTP server standing in for the on-device
bridge's one route, so what is pinned is the wire the Kotlin route reads: the
exact bytes, the ``name`` query parameter, a binary content type. The
``compose`` patch is recorded instead of sent (there is no app to ack it).
Assertions are over recorded state only (convention 14).

What this file cannot pin, and where that lives: the Kotlin half
(``BridgeHttpServer``'s ``/input-file`` route, ``AppLauncher.writeInputFile``)
runs only on a device, so its first run is the device verification android's
run venue unblocks.
"""

import http.server
import json
import os
import sys
import threading
from urllib.parse import parse_qs, urlparse

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from drivers.android import AndroidBridgeDriver  # noqa: E402

pytestmark = pytest.mark.tier_1

DEVICE_DIR = "/data/user/0/social.fauna.fauna/cache/e2e-input/1"


class _FakeBridge(http.server.BaseHTTPRequestHandler):
    """The on-device bridge's ``POST /input-file``, recorded."""

    requests: list = []
    reply: dict | None = None

    def do_POST(self):  # noqa: N802 - http.server's naming
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length)
        url = urlparse(self.path)
        type(self).requests.append({
            "path": url.path,
            "query": parse_qs(url.query),
            "content_type": self.headers.get("Content-Type"),
            "body": body,
        })
        name = parse_qs(url.query).get("name", [""])[0]
        reply = type(self).reply
        if reply is None:
            reply = {"path": f"{DEVICE_DIR}/{name}"}
        out = json.dumps(reply).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)

    def log_message(self, *args):  # silence the per-request stderr line
        pass


@pytest.fixture
def bridge(monkeypatch):
    """A loopback fake bridge; yields its recorded requests and the patch list.

    Yields lists, never a driver — see `test_android_driver_adb.py`'s
    `bridge_posts` docstring: a driver in `funcargs` makes convention 17's frame
    probe issue a real `GET /app/state` against a bridge that has no app.
    """
    _FakeBridge.requests = []
    _FakeBridge.reply = None
    server = http.server.HTTPServer(("127.0.0.1", 0), _FakeBridge)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    patches: list = []
    monkeypatch.setattr(
        AndroidBridgeDriver, "set_state",
        lambda self, state, timeout=None, *, wait_ready=True: patches.append(state),
    )
    try:
        yield {
            "url": f"http://127.0.0.1:{server.server_address[1]}",
            "requests": _FakeBridge.requests,
            "patches": patches,
        }
    finally:
        server.shutdown()
        server.server_close()


def _driver(url: str) -> AndroidBridgeDriver:
    d = AndroidBridgeDriver()
    d._url = url
    return d


def test_the_bytes_cross_and_the_patch_names_the_device_path(bridge, tmp_path):
    payload = bytes(range(256)) * 9  # every byte value: a text decode would mangle it
    host = tmp_path / "photo.jpg"
    host.write_bytes(payload)

    _driver(bridge["url"]).set_input_files("attachment-button", str(host))

    [req] = bridge["requests"]
    assert req["path"] == "/input-file"
    assert req["query"] == {"name": ["photo.jpg"]}
    assert req["content_type"] == "application/octet-stream"
    assert req["body"] == payload
    assert bridge["patches"] == [
        {"compose": {"file": f"{DEVICE_DIR}/photo.jpg", "target": "attachment-button"}},
    ]


def test_the_host_path_never_reaches_the_agent(bridge, tmp_path):
    host = tmp_path / "doc.pdf"
    host.write_bytes(b"%PDF-1.4")

    _driver(bridge["url"]).set_input_files("compose-file", [str(host)])

    sent = json.dumps(bridge["patches"])
    assert str(tmp_path) not in sent
    assert bridge["patches"][0]["compose"]["target"] == "compose-file"


def test_a_name_with_spaces_survives_the_query(bridge, tmp_path):
    host = tmp_path / "my holiday photo.png"
    host.write_bytes(b"\x89PNG")

    _driver(bridge["url"]).set_input_files("attachment-button", str(host))

    assert bridge["requests"][0]["query"] == {"name": ["my holiday photo.png"]}
    assert bridge["patches"][0]["compose"]["file"] == f"{DEVICE_DIR}/my holiday photo.png"


def test_an_empty_file_still_crosses(bridge, tmp_path):
    host = tmp_path / "empty.txt"
    host.write_bytes(b"")

    _driver(bridge["url"]).set_input_files("attachment-button", str(host))

    assert bridge["requests"][0]["body"] == b""
    assert bridge["patches"][0]["compose"]["file"] == f"{DEVICE_DIR}/empty.txt"


def test_a_reply_without_a_device_path_raises_and_sends_no_patch(bridge, tmp_path):
    _FakeBridge.reply = {"written": True}
    host = tmp_path / "photo.jpg"
    host.write_bytes(b"x")

    with pytest.raises(RuntimeError, match="device path"):
        _driver(bridge["url"]).set_input_files("attachment-button", str(host))

    assert bridge["patches"] == []


def test_a_missing_host_file_fails_before_the_bridge_is_touched(bridge, tmp_path):
    with pytest.raises(FileNotFoundError):
        _driver(bridge["url"]).set_input_files("attachment-button", str(tmp_path / "absent.jpg"))

    assert bridge["requests"] == []
    assert bridge["patches"] == []
