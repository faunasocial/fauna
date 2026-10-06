"""tier_1: android's `download_dir()` is a host mirror of the DEVICE's download directory.

Authority: `docs/goal/architecture/testing.md` § Default app and nest mode →
*Android's run venue* (venue A2: pytest on Linux, the device on the emulator host behind a
tunnel — so no device path is ever a host path) and
`docs/goal/behavior/mail-export.md` § Implementation status today, the android
paragraph (the app saves into its own download directory and hands the finished
file to the share sheet).

WHY THIS FILE EXISTS. Every other driver's app saves an e2e download onto the
pytest host, so `download_dir()` just names a directory and a test lists it.
android's app writes into its own cache on the device. The driver therefore
crosses the boundary the way `credential_map` already reads the credential
file: `GET /download-dir` lists the device directory, `GET /download-file`
hands one file's bytes back, and `download_dir()` makes a host directory match
— on every call, because a test polls it while the app is still writing.

The instrument is a real loopback HTTP server standing in for the on-device
bridge's two routes, over a dict the test mutates the way the app mutates its
directory. Assertions are over the mirror's contents only (convention 14).

What this file cannot pin, and where that lives: the Kotlin half
(`BridgeHttpServer`'s two routes, `AppLauncher.downloadFiles` /
`downloadFile`) runs only on a device, so its first run is the device
verification android's run venue unblocks.
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


class _FakeBridge(http.server.BaseHTTPRequestHandler):
    """The on-device bridge's `/download-dir` and `/download-file`, over a dict."""

    #: name -> (bytes, mtime_ms): the device directory, as the test sets it.
    device: dict = {}
    #: names the listing shows but the fetch 404s — a file gone in between.
    vanishing: set = set()
    #: every `/download-file` name fetched, in order.
    fetched: list = []
    #: when set, `/download-dir` answers with this instead of a listing.
    listing_override = None

    def _send(self, status: int, body: bytes, content_type: str) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):  # noqa: N802 - http.server's naming
        cls = type(self)
        url = urlparse(self.path)
        if url.path == "/download-dir":
            reply = cls.listing_override
            if reply is None:
                reply = {"files": [
                    {"name": n, "size": len(data), "mtime_ms": mtime}
                    for n, (data, mtime) in sorted(cls.device.items())
                ]}
            self._send(200, json.dumps(reply).encode(), "application/json")
            return
        if url.path == "/download-file":
            name = parse_qs(url.query).get("name", [""])[0]
            cls.fetched.append(name)
            if name in cls.vanishing or name not in cls.device:
                self._send(404, b'{"error":"no such download"}', "application/json")
                return
            self._send(200, cls.device[name][0], "application/octet-stream")
            return
        self._send(404, b'{"error":"unknown"}', "application/json")

    def log_message(self, *args):  # silence the per-request stderr line
        pass


@pytest.fixture
def bridge():
    """A loopback fake bridge; yields its URL and the class holding its state.

    Yields no driver — see `test_android_driver_adb.py`'s `bridge_posts`
    docstring: a driver in `funcargs` makes convention 17's frame probe issue a
    real `GET /app/state` against a bridge that has no app.
    """
    _FakeBridge.device = {}
    _FakeBridge.vanishing = set()
    _FakeBridge.fetched = []
    _FakeBridge.listing_override = None
    server = http.server.HTTPServer(("127.0.0.1", 0), _FakeBridge)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield {"url": f"http://127.0.0.1:{server.server_address[1]}", "state": _FakeBridge}
    finally:
        server.shutdown()
        server.server_close()


@pytest.fixture
def mirrored(bridge):
    """`(download_dir callable, fake-bridge state)`, the mirror removed afterwards."""
    d = AndroidBridgeDriver()
    d._url = bridge["url"]
    try:
        yield d.download_dir, bridge["state"]
    finally:
        mirror = d._download_mirror
        if mirror and os.path.isdir(mirror):
            for name in os.listdir(mirror):
                os.remove(os.path.join(mirror, name))
            os.rmdir(mirror)


def _contents(path: str) -> dict[str, bytes]:
    out = {}
    for name in os.listdir(path):
        with open(os.path.join(path, name), "rb") as fh:
            out[name] = fh.read()
    return out


def test_no_bridge_means_no_download_dir():
    assert AndroidBridgeDriver().download_dir() is None


def test_an_empty_device_directory_mirrors_as_an_empty_host_directory(mirrored):
    download_dir, _state = mirrored
    path = download_dir()
    assert os.path.isdir(path)
    assert os.listdir(path) == []


def test_a_saved_file_crosses_byte_for_byte(mirrored):
    download_dir, state = mirrored
    payload = bytes(range(256)) * 9  # every byte value: a text decode would mangle it
    state.device = {"fauna-export-alice-mbox-2026-10-03.zip.zst": (payload, 1000)}

    assert _contents(download_dir()) == {"fauna-export-alice-mbox-2026-10-03.zip.zst": payload}


def test_the_mirror_is_refreshed_by_each_call_and_keeps_one_path(mirrored):
    download_dir, state = mirrored
    first = download_dir()
    assert os.listdir(first) == []

    state.device = {"a.zip.zst": (b"archive", 1000)}
    second = download_dir()

    assert second == first, "a wait holds the path across calls; it must not move"
    assert _contents(second) == {"a.zip.zst": b"archive"}


def test_a_file_the_device_dropped_leaves_the_mirror(mirrored):
    # The refusal witness asserts the directory is UNCHANGED after a refused
    # download: a `.part` the sink deleted must not linger on the host.
    download_dir, state = mirrored
    state.device = {"a.zip.zst.part": (b"half", 1000), "kept.txt": (b"k", 1000)}
    assert sorted(os.listdir(download_dir())) == ["a.zip.zst.part", "kept.txt"]

    state.device = {"kept.txt": (b"k", 1000)}

    assert os.listdir(download_dir()) == ["kept.txt"]


def test_an_unchanged_file_is_not_fetched_again(mirrored):
    download_dir, state = mirrored
    state.device = {"a.zip.zst": (b"archive", 1000)}
    download_dir()
    download_dir()
    download_dir()

    assert state.fetched == ["a.zip.zst"]


def test_a_same_length_rewrite_is_fetched_again(mirrored):
    # A same-day re-download overwrites the archive under the same name.
    download_dir, state = mirrored
    state.device = {"a.zip.zst": (b"first--", 1000)}
    download_dir()

    state.device = {"a.zip.zst": (b"second-", 2000)}

    assert _contents(download_dir()) == {"a.zip.zst": b"second-"}


def test_a_file_gone_between_listing_and_fetch_is_simply_absent(mirrored):
    download_dir, state = mirrored
    state.device = {"a.zip.zst.part": (b"half", 1000)}
    state.vanishing = {"a.zip.zst.part"}

    assert os.listdir(download_dir()) == []


@pytest.mark.parametrize("name", ["../escape", "sub/file", "..", "", "back\\slash"])
def test_a_listed_name_that_is_not_a_bare_file_name_is_refused(mirrored, name):
    download_dir, state = mirrored
    state.listing_override = {"files": [{"name": name, "size": 1, "mtime_ms": 1}]}

    with pytest.raises(RuntimeError, match="non-bare file name"):
        download_dir()
    assert state.fetched == []


def test_a_reply_without_a_listing_raises_rather_than_reading_as_empty(mirrored):
    # Every caller treats an empty directory as a real answer ("nothing was saved").
    download_dir, state = mirrored
    state.listing_override = {"error": "boom"}

    with pytest.raises(RuntimeError, match="without a files list"):
        download_dir()
