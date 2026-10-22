"""A page that never settles no longer pegs the web bridge for good
(convention 9 — the harness is self-terminating).

The bridge server is single-threaded and `page.evaluate` takes no timeout, so
a renderer spinning or a promise that never resolves held the one server
thread forever: `/health` went silent with the listener still accepting, and
a whole tier_2/3 web run ended hours in on the pegged-streak abort. Past
`_STUCK_CALL_CEILING_S` the bridge's watchdog kills its Chromium browser, the
stuck call fails, and the next request finds the browser relaunched with the
page back where it was. A renderer that crashes under a call is the third
shape: Playwright never answers that call, so the watchdog cuts it loose as
soon as the call has been busy `_WATCHDOG_BUSY_S` with a crash recorded. Here
the ceiling is shrunk to seconds (`--stuck-ceiling`, the bridge's test-only
flag) and every shape is driven through the real bridge and a real headless
Chromium — no app, no nest.
"""

import importlib.util
import json
import os
import pathlib
import signal
import subprocess
import sys
import threading
import urllib.error
import urllib.request

import pytest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))
from drivers.port_util import popen_group_kwargs, terminate_tree  # noqa: E402

pytestmark = [
    pytest.mark.tier_2,
    pytest.mark.skipif(sys.platform != "linux", reason="the watchdog reads /proc"),
]

_SERVER = pathlib.Path(__file__).resolve().parents[1] / "web-bridge" / "server.py"
_CEILING_S = 4
# The crash case's ceiling: high enough that only the crash path can free the
# call before it (busy threshold = ceiling / 2, tick = ceiling / 4).
_CRASH_CASE_CEILING_S = 40
# Far past the shrunk ceiling plus a tick of lateness and the browser's
# teardown, far below anything the unbounded hang would ever answer in.
_RPC_TIMEOUT_S = 90


def _bridge_module():
    spec = importlib.util.spec_from_file_location("fauna_e2e_web_bridge_stuck", _SERVER)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@pytest.fixture
def bridge(request):
    ceiling = getattr(request, "param", _CEILING_S)
    proc = subprocess.Popen(
        [sys.executable, str(_SERVER), "--stuck-ceiling", str(ceiling)],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        **popen_group_kwargs(),
    )
    port = None
    for line in proc.stdout:
        if line.startswith("BRIDGE_PORT="):
            port = int(line.strip().split("=")[1])
            break
    assert port, "the bridge never reported its port"
    stderr: list[str] = []
    threading.Thread(target=lambda: stderr.extend(proc.stderr), daemon=True).start()
    threading.Thread(target=lambda: proc.stdout.read(), daemon=True).start()
    url = f"http://127.0.0.1:{port}"
    try:
        _rpc(url, "POST", "/session", {"url": "data:text/html,<title>stuck</title>"})
        yield url, stderr, proc.pid
    finally:
        terminate_tree(proc)


def _rpc(url, method, path, body=None):
    req = urllib.request.Request(
        url + path, method=method,
        data=json.dumps(body or {}).encode(),
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=_RPC_TIMEOUT_S) as resp:
            return resp.status, json.loads(resp.read())
    except urllib.error.HTTPError as err:
        return err.code, json.loads(err.read())


def _assert_page_is_back(url, stderr, why):
    # The server thread is free, and the page answers again in a relaunched
    # browser, back where it was.
    assert _rpc(url, "GET", "/health")[0] == 200
    assert _rpc(url, "POST", "/execute", {"script": "document.title"}) == (
        200, {"result": "stuck"})
    killed = [ln for ln in stderr if "killed browser pids" in ln]
    assert killed and "POST /execute" in killed[0] and why in killed[0], stderr


@pytest.mark.parametrize("script", [
    pytest.param("new Promise(() => {})", id="a-promise-that-never-resolves"),
    pytest.param("(() => { while (true) {} })()", id="a-spinning-renderer"),
])
def test_a_stuck_evaluate_is_cut_loose_and_the_page_comes_back(bridge, script):
    url, stderr, _ = bridge
    status, body = _rpc(url, "POST", "/execute", {"script": script})
    assert status == 500 and "closed" in body.get("error", ""), (status, body)
    _assert_page_is_back(url, stderr, "ceiling")


@pytest.mark.parametrize("bridge", [_CRASH_CASE_CEILING_S], indirect=True)
def test_a_call_under_a_crashed_renderer_is_cut_loose_before_the_ceiling(bridge):
    url, stderr, bridge_pid = bridge
    module = _bridge_module()
    answer: list = []
    call = threading.Thread(
        target=lambda: answer.append(
            _rpc(url, "POST", "/execute", {"script": "new Promise(() => {})"})),
        daemon=True,
    )
    call.start()
    # The renderer dies under the pending call, as an OOM kill would take it.
    renderers: list[int] = []
    while not renderers and call.is_alive():
        renderers = [
            pid for pid in module._proc_tree(bridge_pid)
            if (module._proc_stat(pid) or ("",))[0] == "renderer"
        ]
    for pid in renderers:
        os.kill(pid, signal.SIGKILL)
    call.join(_RPC_TIMEOUT_S)
    # Playwright answers a call under a crashed renderer either way, run to
    # run: "Execution context was destroyed" at once, or never (the watchdog's
    # kill then answers it "closed"). Either way the next call lands on the
    # crashed page and is the one cut loose, so the page must come back.
    assert answer and answer[0][0] == 500, answer
    _assert_page_is_back(url, stderr, "crashed")
