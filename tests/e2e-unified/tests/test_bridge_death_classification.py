"""tier_1 unit tests for HttpBridgeDriver's timeout-vs-death classification —
no driver launch, no app, no nest; the "bridge" is an in-test stdlib HTTP
server on loopback.

Regression guard for the false-"[BRIDGE DEAD ... (setup)]" family: `_post`/
`_get` used to funnel every OSError into `_mark_dead`, but a socket timeout is
proof the server ACCEPTED the connection and is still working — evidence of
life under machine load, not death. Conflating the two turned one slow
response into a whole-session skip, and no per-call-site timeout bump could
converge because after a navigation the boot cost lands on whichever cheap
call runs next. The fix (http_bridge._timeout_verdict): on a timeout,
deadline-poll /health under a named budget — the bridge servers are
single-threaded, so the abandoned request must drain before /health can
answer, which is exactly why the old one-shot 5s probe observed the same
stall twice and called it death — and raise plain TimeoutError when the
bridge proves alive (the conftest `app` fixture retries reset() on exactly
that). Death is concluded ONLY from a refused/reset connection — the peer
actively signalling there is nothing there. Silence at budget expiry still
classifies as alive: every silent probe's connect was ACCEPTED, so the
listener (hence the process) exists; the bridge thread is pegged — typically
inside a page-bound Playwright call waiting out an SPA wasm reboot under
machine load, which can outlast any polite health budget (observed >210s,
2026-08-02).

The fake server here is deliberately a plain single-threaded HTTPServer, the
same shape as web-bridge/server.py, so the drain semantics under test are the
real ones.

Listed in conftest's _CLIENT_INDEPENDENT_FILES so it never acquires a client
parametrization.
"""

import os
import socket
import sys
import threading
import time
import urllib.error
from http.server import BaseHTTPRequestHandler, HTTPServer

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from drivers import http_bridge  # noqa: E402
from drivers.http_bridge import BridgeDead, HttpBridgeDriver  # noqa: E402
from drivers.port_util import find_free_port  # noqa: E402

pytestmark = pytest.mark.tier_1

# The fake bridge's controlled latency for its "slow" routes. Client calls use
# a 0.5s socket timeout, so the timeout fires long before this responds (12x
# margin); the death-confirm budget (90s default) then has a 15x margin to see
# /health answer once the handler drains. Both relations hold under any
# non-pathological scheduling delay (testing.md § point 14).
_SLOW_ROUTE_DELAY_S = 6.0
_CLIENT_TIMEOUT_S = 0.5


class _FakeBridgeHandler(BaseHTTPRequestHandler):
    """Single-threaded fake bridge: /health always ready, /execute and
    GET /slow optionally delayed via server.delay_s (consumed once)."""

    def _respond(self, payload: bytes = b'{"result": null}') -> None:
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def _maybe_delay(self) -> None:
        delay = getattr(self.server, "delay_s", 0.0)
        self.server.delay_s = 0.0
        if delay:
            # The fake bridge's controlled latency — the mechanism under test
            # (a response slower than the caller's socket timeout), never a
            # settle-wait.
            time.sleep(delay)  # sleep-ok: the fake latency under test itself

    def do_GET(self):  # noqa: N802
        if self.path.startswith("/health"):
            self._respond(b'{"ready": true}')
            return
        self._maybe_delay()
        self._respond(b'{"text": "ok"}')

    def do_POST(self):  # noqa: N802
        length = int(self.headers.get("Content-Length") or 0)
        if length:
            self.rfile.read(length)
        self._maybe_delay()
        self._respond()

    def log_message(self, *args):  # silence per-request stderr noise
        pass


class _FakeBridgeServer(HTTPServer):
    delay_s = 0.0

    def handle_error(self, request, client_address):
        # The client abandons timed-out requests, so the handler's late write
        # hits a closed socket — expected here, not worth a traceback.
        pass


@pytest.fixture
def fake_bridge():
    server = _FakeBridgeServer(("127.0.0.1", 0), _FakeBridgeHandler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    yield server
    server.shutdown()


def _driver_for(server) -> HttpBridgeDriver:
    return HttpBridgeDriver(f"http://127.0.0.1:{server.server_address[1]}")


def test_slow_but_alive_bridge_is_not_marked_dead(fake_bridge):
    """A response slower than the RPC socket timeout raises plain
    TimeoutError — never BridgeDead — and leaves the driver usable: the
    death-confirm poll sees /health answer once the single-threaded server
    drains the abandoned request."""
    driver = _driver_for(fake_bridge)
    fake_bridge.delay_s = _SLOW_ROUTE_DELAY_S
    with pytest.raises(TimeoutError):
        driver._post("/execute", {}, timeout=_CLIENT_TIMEOUT_S)
    assert driver._bridge_dead is False
    # The very next RPC succeeds with no recover() step in between.
    assert driver._post("/execute", {}) == {"result": None}


def test_slow_get_is_classified_alive_too(fake_bridge, monkeypatch):
    """_get shares the classification (it has no timeout parameter — the
    shared default is resolved at call time, which is what lets this shrink
    it)."""
    monkeypatch.setattr(http_bridge, "BRIDGE_RPC_TIMEOUT_S", _CLIENT_TIMEOUT_S)
    driver = _driver_for(fake_bridge)
    fake_bridge.delay_s = _SLOW_ROUTE_DELAY_S
    with pytest.raises(TimeoutError):
        driver._get("/element/text", {"id": "x"})
    assert driver._bridge_dead is False
    assert driver._get("/element/text", {"id": "x"})["text"] == "ok"


def test_silent_but_listening_bridge_is_classified_alive(monkeypatch):
    """A server that accepts connections but never answers anything —
    /health included — is STALLED, not dead: every silent probe's connect
    was accepted, so the listener (hence the process) exists. The verdict
    at budget expiry is a bounded plain TimeoutError (conventions 9 + 14),
    never BridgeDead — this is exactly the bridge-thread-pegged-by-an-SPA-
    boot shape that outlasts any polite health budget under machine load."""
    monkeypatch.setattr(http_bridge, "BRIDGE_DEATH_CONFIRM_BUDGET_S", 3.0)
    monkeypatch.setattr(http_bridge, "BRIDGE_HEALTH_PROBE_TIMEOUT_S", 1.0)
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.bind(("127.0.0.1", 0))
    listener.listen(5)  # connects complete via the backlog; nothing ever answers
    try:
        driver = HttpBridgeDriver(f"http://127.0.0.1:{listener.getsockname()[1]}")
        with pytest.raises(TimeoutError):
            driver._post("/execute", {}, timeout=_CLIENT_TIMEOUT_S)
        assert driver._bridge_dead is False
    finally:
        listener.close()


def test_listener_vanishing_during_confirm_poll_is_death(monkeypatch):
    """If the process dies while the confirm poll runs, the next probe is
    refused and death is concluded immediately — the refused connect is the
    one true death signal."""
    monkeypatch.setattr(http_bridge, "BRIDGE_DEATH_CONFIRM_BUDGET_S", 30.0)
    # Generous on purpose: a refusal is an EVENT, not a wait, so this bound is
    # never spent on the passing path — but it must outlast a refused loopback
    # connect's own latency. Windows retries the SYN before reporting
    # WSAECONNREFUSED, and under load that outran a 1 s probe: the probe read
    # "silent" and the test red'd against a correct classifier (measured on
    # win 2026-09-22, the tier_1 gate's confirmation run).
    monkeypatch.setattr(http_bridge, "BRIDGE_HEALTH_PROBE_TIMEOUT_S", 10.0)
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.bind(("127.0.0.1", 0))
    listener.listen(5)
    # The process dies mid-poll: the listener closes as the confirm poll's
    # FIRST probe starts — an ordering, not a timer (convention 14), so the
    # probe that follows is refused on every box at every load.
    driver = HttpBridgeDriver(f"http://127.0.0.1:{listener.getsockname()[1]}")
    real_probe = driver._probe_health

    def dying_probe(timeout=None):
        listener.close()
        return real_probe(timeout)

    monkeypatch.setattr(driver, "_probe_health", dying_probe)
    try:
        with pytest.raises(BridgeDead):
            driver._post("/execute", {}, timeout=_CLIENT_TIMEOUT_S)
        assert driver._bridge_dead is True
    finally:
        listener.close()


def test_connection_refused_is_immediate_death(monkeypatch):
    """Refused means the peer actively signalled 'nothing here': immediate
    BridgeDead with zero health-poll dawdle."""
    driver = HttpBridgeDriver(f"http://127.0.0.1:{find_free_port()}")
    probes = []
    monkeypatch.setattr(
        driver, "_health_raw", lambda timeout=None: probes.append(1) or False
    )
    with pytest.raises(BridgeDead):
        driver._post("/execute", {}, timeout=_CLIENT_TIMEOUT_S)
    assert driver._bridge_dead is True
    assert probes == []


def test_is_timeout_classification():
    """The classifier itself: raw and URLError-wrapped timeouts are timeouts;
    refused/reset (raw or wrapped) are not."""
    assert HttpBridgeDriver._is_timeout(TimeoutError())
    assert HttpBridgeDriver._is_timeout(socket.timeout())  # alias, pre-3.10 spelling
    assert HttpBridgeDriver._is_timeout(urllib.error.URLError(TimeoutError()))
    assert not HttpBridgeDriver._is_timeout(ConnectionRefusedError())
    assert not HttpBridgeDriver._is_timeout(ConnectionResetError())
    assert not HttpBridgeDriver._is_timeout(
        urllib.error.URLError(ConnectionRefusedError())
    )
    assert not HttpBridgeDriver._is_timeout(urllib.error.URLError("no host given"))


@pytest.fixture
def silent_listener():
    """A listener that accepts connects and never answers — the pegged bridge."""
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.bind(("127.0.0.1", 0))
    listener.listen(16)
    yield listener
    listener.close()


def _shrink_confirm_budgets(monkeypatch) -> None:
    monkeypatch.setattr(http_bridge, "BRIDGE_DEATH_CONFIRM_BUDGET_S", 1.0)
    monkeypatch.setattr(http_bridge, "BRIDGE_HEALTH_PROBE_TIMEOUT_S", 0.3)


def test_a_pegged_streak_aborts_the_run(silent_listener, monkeypatch):
    """Each pegged verdict alone is a retryable TimeoutError, but
    BRIDGE_PEGGED_STREAK_ABORT of them in a row — the bridge answering nothing
    between — ends the RUN with `pytest.exit`: a thread that never frees would
    otherwise make every later test pay the full boot ceiling plus the confirm
    wait (measured: nine web tests in a row at ~490 s each, the run at 59% after
    13 h, holding the one-wide e2e_other lane)."""
    _shrink_confirm_budgets(monkeypatch)
    driver = HttpBridgeDriver(f"http://127.0.0.1:{silent_listener.getsockname()[1]}")
    for _ in range(http_bridge.BRIDGE_PEGGED_STREAK_ABORT - 1):
        with pytest.raises(TimeoutError):
            driver._post("/execute", {}, timeout=_CLIENT_TIMEOUT_S)
    with pytest.raises(pytest.exit.Exception) as aborted:
        driver._post("/execute", {}, timeout=_CLIENT_TIMEOUT_S)
    assert aborted.value.returncode == 3
    assert "[BRIDGE PEGGED STREAK]" in aborted.value.msg
    assert "POST /execute" in aborted.value.msg


def test_any_bridge_answer_between_pegged_verdicts_restarts_the_count(
    silent_listener, fake_bridge, monkeypatch
):
    """The bound is on a bridge that answers NOTHING: one successful RPC between
    two pegged verdicts — the SPA boot that finally finished — restarts the
    count, so a slow-but-recovering run is never aborted."""
    _shrink_confirm_budgets(monkeypatch)
    pegged_url = f"http://127.0.0.1:{silent_listener.getsockname()[1]}"
    live_url = f"http://127.0.0.1:{fake_bridge.server_address[1]}"
    driver = HttpBridgeDriver(pegged_url)
    for _ in range(2):
        for _ in range(http_bridge.BRIDGE_PEGGED_STREAK_ABORT - 1):
            with pytest.raises(TimeoutError):
                driver._post("/execute", {}, timeout=_CLIENT_TIMEOUT_S)
        driver._url = live_url
        assert driver._post("/execute", {}) == {"result": None}
        driver._url = pegged_url
