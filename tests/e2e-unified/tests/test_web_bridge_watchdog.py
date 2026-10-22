"""The web bridge's self-diagnosis watchdog (convention 6; convention 9's
pegged-streak bound is what a peg otherwise ends in).

The bridge server is single-threaded — Playwright's sync API is bound to the
thread that started it — so a page-bound call that never returns silences
`/health` too, and from outside a pegged bridge says nothing about why. The
watchdog thread names the request the server thread is in, that thread's
Python stack, and the bridge's descendant process tree (RSS and CPU spent
since the last look). Pure in-process: the bridge module is loaded, no
Playwright session, no app.
"""

import importlib.util
import json
import pathlib
import subprocess
import sys
import threading
import time
import urllib.request

import pytest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from drivers.port_util import popen_group_kwargs  # noqa: E402

pytestmark = pytest.mark.tier_1


def _bridge():
    server_path = (
        pathlib.Path(__file__).resolve().parents[1] / "web-bridge" / "server.py"
    )
    spec = importlib.util.spec_from_file_location("fauna_e2e_web_bridge_wd", server_path)
    bridge = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(bridge)
    return bridge


def test_a_long_request_is_named_with_the_server_stack_and_the_tree():
    bridge = _bridge()
    bridge._server_thread_id = threading.get_ident()
    now = time.monotonic()
    bridge._inflight = ("POST /execute HTTP/1.1", now - bridge._WATCHDOG_BUSY_S - 1)
    lines, _ = bridge._watchdog_tick({}, now, now)
    assert len(lines) == 1
    line = lines[0]
    assert line.startswith("[bridge-watchdog] 'POST /execute HTTP/1.1' busy")
    assert "tree:" in line
    # The stack is the server thread's — here, this test calling the tick.
    assert "test_a_long_request_is_named_with_the_server_stack_and_the_tree" in line


def test_a_short_request_writes_nothing_until_the_sample_is_due():
    bridge = _bridge()
    now = time.monotonic()
    bridge._inflight = ("GET /health HTTP/1.1", now)
    lines, last = bridge._watchdog_tick({}, now, now)
    assert lines == [] and last == now
    lines, last = bridge._watchdog_tick({}, now, now - bridge._WATCHDOG_SAMPLE_S)
    assert len(lines) == 1 and lines[0].startswith("[bridge-sample] pages 0 contexts 0;")
    assert last == now


@pytest.mark.skipif(sys.platform != "linux", reason="the tree is read from /proc")
def test_the_tree_report_counts_descendants_and_their_cpu():
    bridge = _bridge()
    # A child that spends CPU until killed, so the second report sees a delta.
    child = subprocess.Popen(
        [sys.executable, "-c", "while True: pass"], **popen_group_kwargs()
    )
    try:
        prev: dict[int, float] = {}
        bridge._tree_report(prev)
        assert child.pid in prev
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if bridge._proc_stat(child.pid)[2] > prev[child.pid]:
                break
            time.sleep(0.05)
        report = bridge._tree_report(prev)
        assert f"busiest pid {child.pid}" in report
    finally:
        child.kill()
        child.wait()


def test_the_server_marks_the_request_it_is_in_and_clears_it():
    """`parse_request` marks the in-flight request line and `handle_one_request`
    clears it once the response is out, so an idle bridge reads as idle."""
    bridge = _bridge()
    server, port = bridge.bind_fresh_port(lambda: bridge.randint(18000, 19000))
    seen: list = []
    original = bridge.BridgeHandler._respond

    def spy(self, status, data):
        seen.append(bridge._inflight[0] if bridge._inflight else None)
        original(self, status, data)

    bridge.BridgeHandler._respond = spy
    t = threading.Thread(target=server.handle_request, daemon=True)
    t.start()
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port}/health", timeout=30) as r:
            assert json.loads(r.read())["ready"] is True
        t.join(30)
    finally:
        server.server_close()
    assert seen == ["GET /health HTTP/1.1"]
    assert bridge._inflight is None
