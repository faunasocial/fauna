"""The harness WS-RPC client must survive the nest's own heartbeat.

The failure this pins was a recurring cross-app class, not one test's bad
luck. ``tests/api/ws_api.py`` caches one open ``WsRpcAdminClient`` per
``(base_url, actor_id)`` for the whole pytest process, so a socket opened in
one test is reused minutes later in another. The nest closes any WS that has
sent no inbound frame for ``KEEPALIVE_TIMEOUT`` (60 s)
— ``bins/fauna-nest/src/routes.rs:1687-1694``, exactly as ``transport.md``
§ Connection lifecycle specifies — and a synchronous ``websocket-client``
socket can only answer the nest's Ping from *inside* a ``recv()``, which this
client is only ever in while a call is in flight. So the parked connection
answers nothing, the nest reaps it, and the next call dies on whichever test
happened to be holding it. Solo the gaps are short and it never shows; under a
full sweep it presented as a ``BrokenPipeError`` inside an unrelated
product assertion.

tier_1: no nest, no driver. The server here is ~90 lines of RFC 6455 written
for this file, which is what makes the death *injectable* — the real nest
reaps on a 60 s timer, and a test that waited for one would be the very
wall-clock brittleness convention 14 forbids.
"""

from __future__ import annotations

import base64
import hashlib
import select
import socket
import struct
import threading
import time
from typing import Callable, Optional

import cbor2
import pytest
import websocket

from clients import _ws_rpc_core
from clients._ws_rpc_core import (
    _IDLE_RECONNECT_AFTER,
    _NEST_LIVENESS_TIMEOUT,
    _NEST_PING_INTERVAL,
    WsLinkDied,
    _WsRpcClientBase,
)
from clients.ws_rpc_admin_client import WsRpcAdminClient, _BearerToken

pytestmark = pytest.mark.tier_1

_WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


# ── A WebSocket server that can be told to die ───────────────────────────────


def _read_http_request(conn: socket.socket) -> bytes:
    buf = b""
    while b"\r\n\r\n" not in buf:
        chunk = conn.recv(4096)
        if not chunk:
            break
        buf += chunk
    return buf


def _accept_key(request: bytes) -> str:
    for line in request.split(b"\r\n"):
        if line.lower().startswith(b"sec-websocket-key:"):
            key = line.split(b":", 1)[1].strip().decode()
            digest = hashlib.sha1((key + _WS_GUID).encode()).digest()
            return base64.b64encode(digest).decode()
    raise AssertionError(f"no Sec-WebSocket-Key in handshake: {request!r}")


#: RFC 6455 opcodes this server distinguishes. Ignoring the opcode is a trap
#: worth naming: a Close frame's payload is a two-byte status code, and
#: ``cbor2.loads(b"\x03\xe8")`` cheerfully decodes 1000 as the integer 3 —
#: so a reader that skips the opcode reports a client disconnect as a
#: malformed Request.
_OP_BINARY = 0x2
_OP_CLOSE = 0x8


def _read_frame(conn: socket.socket) -> Optional[tuple[int, bytes]]:
    """Read one client→server frame as ``(opcode, payload)``.

    Client frames are always masked (RFC 6455 § 5.3).
    """
    header = _recv_exactly(conn, 2)
    if header is None:
        return None
    opcode = header[0] & 0x0F
    length = header[1] & 0x7F
    if length == 126:
        extended = _recv_exactly(conn, 2)
        if extended is None:
            return None
        length = struct.unpack("!H", extended)[0]
    masked = bool(header[1] & 0x80)
    mask = _recv_exactly(conn, 4) if masked else b""
    if masked and mask is None:
        return None
    payload = _recv_exactly(conn, length) if length else b""
    if payload is None:
        return None
    if masked:
        payload = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
    return opcode, payload


def _read_request(conn: socket.socket) -> Optional[dict]:
    """Read frames until a binary one arrives; decode it as a Request envelope.

    Returns ``None`` when the peer closes (a Close frame or EOF) — control
    frames are skipped rather than decoded.
    """
    while True:
        frame = _read_frame(conn)
        if frame is None:
            return None
        opcode, payload = frame
        if opcode == _OP_CLOSE:
            return None
        if opcode != _OP_BINARY:
            continue  # Ping/Pong/text — not a Request
        decoded = cbor2.loads(payload)
        assert isinstance(decoded, dict), f"not a Request envelope: {decoded!r}"
        return decoded


def _recv_exactly(conn: socket.socket, n: int) -> Optional[bytes]:
    buf = b""
    while len(buf) < n:
        chunk = conn.recv(n - len(buf))
        if not chunk:
            return None
        buf += chunk
    return buf


def _binary_frame(payload: bytes) -> bytes:
    """Server→client binary frame (unmasked, per RFC 6455)."""
    header = bytearray([0x82])
    if len(payload) < 126:
        header.append(len(payload))
    else:
        header.append(126)
        header += struct.pack("!H", len(payload))
    return bytes(header) + payload


def _close_frame(code: int = 1000) -> bytes:
    """Server→client Close frame (unmasked) carrying the status ``code``."""
    return bytes([0x80 | _OP_CLOSE, 2]) + struct.pack("!H", code)


_REASONS = {401: b"Unauthorized", 403: b"Forbidden"}


class FakeNest:
    """A single-threaded WS server whose per-connection behavior is scripted.

    ``behaviors`` is one callable per upgraded connection, in order. Each is
    handed ``(conn, requests)`` where ``requests`` is the shared list every
    decoded Request envelope is appended to — so a test can assert what
    actually crossed the wire across a reconnect, which is the only way to
    tell a replay from a re-send.

    ``upgrade_status``, when given, decides each upgrade from the raw HTTP
    request: ``101`` upgrades, anything else is refused with that status and
    never consumes a behavior — the nest's own upgrade-time bearer check
    (``routes.rs::ws_handler``), which answers a forgotten bearer ``401
    invalid token`` before any socket exists.
    """

    def __init__(
        self,
        behaviors: list[Callable[[socket.socket, list], None]],
        upgrade_status: Optional[Callable[[bytes], int]] = None,
    ):
        self._behaviors = behaviors
        self._upgrade_status = upgrade_status
        self.requests: list[dict] = []
        self.connections = 0
        self.upgraded = 0
        self.rejected_upgrades = 0
        self._sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._sock.bind(("127.0.0.1", 0))
        self._sock.listen(8)
        self.port = self._sock.getsockname()[1]
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._serve, daemon=True)
        self._thread.start()

    @property
    def url(self) -> str:
        return f"ws://127.0.0.1:{self.port}/api/v1/ws"

    def _serve(self) -> None:
        while not self._stop.is_set():
            try:
                conn, _ = self._sock.accept()
            except OSError:
                return
            self.connections += 1
            try:
                request = _read_http_request(conn)
                status = self._upgrade_status(request) if self._upgrade_status else 101
                if status != 101:
                    self.rejected_upgrades += 1
                    body = b"invalid token" if status == 401 else b"refused"
                    conn.sendall(
                        b"HTTP/1.1 %d %s\r\n" % (status, _REASONS.get(status, b"Refused"))
                        + b"Content-Type: text/plain; charset=utf-8\r\n"
                        + b"Content-Length: %d\r\n\r\n" % len(body)
                        + body
                    )
                    continue
                conn.sendall(
                    b"HTTP/1.1 101 Switching Protocols\r\n"
                    b"Upgrade: websocket\r\n"
                    b"Connection: Upgrade\r\n"
                    b"Sec-WebSocket-Accept: "
                    + _accept_key(request).encode()
                    + b"\r\nSec-WebSocket-Protocol: fauna.v1\r\n\r\n"
                )
                index = self.upgraded
                self.upgraded += 1
                behavior = self._behaviors[min(index, len(self._behaviors) - 1)]
                behavior(conn, self.requests)
            except OSError:
                pass
            finally:
                try:
                    conn.close()
                except OSError:
                    pass

    def close(self) -> None:
        self._stop.set()
        try:
            self._sock.close()
        except OSError:
            pass


def reply_ok(conn: socket.socket, requests: list) -> None:
    """Answer every Request with ``ok=true`` and an echo of its kind."""
    while True:
        frame = _read_request(conn)
        if frame is None:
            return
        requests.append(frame)
        conn.sendall(
            _binary_frame(
                cbor2.dumps(
                    {0: 1, 1: frame[1], 4: {"echoed": frame[2]}, 7: True},
                    canonical=True,
                )
            )
        )


def die_after_reading_one(conn: socket.socket, requests: list) -> None:
    """Read one Request, record it, then drop the TCP connection outright."""
    frame = _read_request(conn)
    if frame is not None:
        requests.append(frame)
    conn.close()


def die_immediately(conn: socket.socket, requests: list) -> None:
    """Close as soon as the handshake completes — nothing is ever read."""
    conn.close()


# ── A client that skips the bearer handshake ─────────────────────────────────


class _BareClient(_WsRpcClientBase):
    """``_WsRpcClientBase`` with the simplest possible ``_connect``.

    Drives the exact base-class code under test — ``call``, the idle
    reconnect, the send-failed re-send, the in-flight refusal — without the
    authenticated client's challenge/verify round-trip, which needs a real
    nest.
    """

    def __init__(self, url: str, **kwargs):
        super().__init__("http://127.0.0.1:1", **kwargs)
        self._url = url

    def _connect(self) -> None:
        self._ws = websocket.create_connection(
            self._url, subprotocols=["fauna.v1"], timeout=5.0
        )


def _readable(client: _BareClient) -> bool:
    """Is anything pending on the client's socket right now?"""
    import select

    sock = client._ws.sock  # type: ignore[union-attr]
    return bool(select.select([sock], [], [], 0)[0])


def _wait_until_closed(client: _BareClient) -> None:
    """Block until the fake nest's close has actually reached our socket.

    A deadline poll on observable state, not a settle-sleep: the assertion
    under test is about what the client does with a *known*-closed socket, so
    racing the TCP close would make the test's own precondition timing-
    dependent (convention 14).
    """
    deadline = time.monotonic() + 5.0
    while time.monotonic() < deadline:
        if client._peer_closed():
            return
        time.sleep(0.01)
    raise AssertionError("the fake nest never closed the connection")


@pytest.fixture(autouse=True)
def _clean_tally():
    """A clean slate per case — without eating the RUN's real reconnects.

    The tally is process-global and read once at session end, so clearing it
    bare (as this fixture did) deleted every reconnect a tier_3 suite earlier in
    the run had recorded, shortening the very count the axis exists to make
    visible. Snapshot, clear, restore — the shape
    `helpers/app_surface.py::reset_unbuilt_hits` was given for the identical
    hazard on the unbuilt tally.
    """
    outer = _ws_rpc_core.reconnects()
    _ws_rpc_core.reset_reconnects()
    yield
    _ws_rpc_core.restore_reconnects(outer)


# ── The constants are mirrors, and must stay mirrors ─────────────────────────


def test_the_python_heartbeat_mirrors_match_the_rust_constants():
    """The whole fix rests on knowing when the nest reaps a link.

    `_NEST_PING_INTERVAL` / `_NEST_LIVENESS_TIMEOUT` are hand-mirrors of
    `fauna_ws_substrate`'s constants, which the nest's `WsHeartbeatPolicy`
    defaults to verbatim. A silent bump on the Rust side would leave this
    client parking sockets past a window it no longer knows — so read the
    Rust and compare, rather than trusting a comment.
    """
    import re
    from pathlib import Path

    repo_root = Path(__file__).resolve().parents[3]
    source = (repo_root / "libs/fauna-ws-substrate/src/adapter.rs").read_text()

    def secs(name: str) -> float:
        match = re.search(
            rf"pub const {name}: Duration = Duration::from_secs\((\d+)\)", source
        )
        assert match, f"{name} not found in fauna-ws-substrate/src/adapter.rs"
        return float(match.group(1))

    assert secs("KEEPALIVE_INTERVAL") == _NEST_PING_INTERVAL
    assert secs("KEEPALIVE_TIMEOUT") == _NEST_LIVENESS_TIMEOUT


def test_the_reconnect_floor_sits_below_the_first_unanswered_ping():
    """Below the *ping* interval, not merely below the 60s reap window: a
    connection handed back to the nest has then never even had a Ping go
    unanswered on it."""
    assert 0 < _IDLE_RECONNECT_AFTER < _NEST_PING_INTERVAL < _NEST_LIVENESS_TIMEOUT


# ── The idle reconnect: replace the socket BEFORE anything is sent ───────────


def test_a_socket_parked_past_the_floor_is_replaced_before_the_request_goes_out():
    """The fix proper. The old client would send onto the parked socket and
    discover the death on ``recv()`` — with the Request already gone, which is
    the one case that cannot be safely re-sent."""
    server = FakeNest([reply_ok, reply_ok])
    try:
        with _BareClient(server.url) as client:
            assert client.call("fauna.protocol.echo", {}) == {
                "echoed": "fauna.protocol.echo"
            }
            assert server.connections == 1

            # Park it: the socket has carried nothing for longer than the
            # floor. Injected rather than slept — a test that waited 25s for
            # its own precondition would be the brittleness this fixes.
            client._last_frame_at = time.monotonic() - (_IDLE_RECONNECT_AFTER + 1)

            assert client.call("fauna.protocol.echo", {}) == {
                "echoed": "fauna.protocol.echo"
            }

        assert server.connections == 2, "the parked socket was reused, not replaced"
        events = _ws_rpc_core.reconnects()
        assert [e.reason for e in events] == ["idle"]
        assert events[0].kind == "fauna.protocol.echo"
        assert events[0].idle_seconds >= _IDLE_RECONNECT_AFTER
    finally:
        server.close()


def test_the_idle_reconnect_sends_the_request_exactly_once():
    """It is a reconnect, not a replay: the swap happens before the send, so
    the nest sees one Request with one idempotency key."""
    server = FakeNest([reply_ok, reply_ok])
    try:
        with _BareClient(server.url) as client:
            client.call("fauna.posts.create", {"body": b"x"})
            client._last_frame_at = time.monotonic() - (_IDLE_RECONNECT_AFTER + 1)
            client.call("fauna.posts.create", {"body": b"y"})

        bodies = [frame[4]["body"] for frame in server.requests]
        assert bodies == [b"x", b"y"], "a payload crossed the wire twice"
        keys = [frame[3] for frame in server.requests]
        assert len(set(keys)) == 2, "distinct calls must carry distinct keys"
    finally:
        server.close()


def test_a_fresh_socket_is_not_reconnected():
    """The floor must not churn connections on an ordinary busy run."""
    server = FakeNest([reply_ok])
    try:
        with _BareClient(server.url) as client:
            for _ in range(5):
                client.call("fauna.protocol.echo", {})
        assert server.connections == 1
        assert _ws_rpc_core.reconnects() == []
    finally:
        server.close()


# ── The peer-closed reconnect: proof beats a timer ───────────────────────────


def test_a_socket_the_nest_already_closed_is_replaced_before_the_request_goes_out():
    """The evidence half, and the reason the fix does not rest on the idle
    timer being tuned right.

    A local ``send()`` returning success proves nothing about delivery — the
    kernel buffers it — so a client that only reacts to a failed send
    discovers a dead peer on ``recv()``, by which point the Request has
    notionally gone and cannot be safely re-sent. Reading the close frame
    that is *already sitting in the buffer* turns that into the unambiguous
    ``was_in_flight: false`` case, which ``transport.md`` § Idempotency and
    reconnect-with-resume rules re-sendable for every kind, with no
    ``forbid_replay`` gate.

    Note the socket here is fresh — zero seconds idle — so nothing but the
    close-frame evidence can save this call.
    """
    server = FakeNest([die_immediately, reply_ok])
    try:
        with _BareClient(server.url) as client:
            _wait_until_closed(client)
            reply = client.call("fauna.protocol.echo", {})
    finally:
        server.close()

    assert reply == {"echoed": "fauna.protocol.echo"}
    events = _ws_rpc_core.reconnects()
    assert [e.reason for e in events] == ["peer-closed"]
    assert events[0].idle_seconds < _IDLE_RECONNECT_AFTER


def test_the_peer_closed_reconnect_sends_the_request_exactly_once():
    """A reconnect, not a replay: the dead server never saw a frame, and the
    live one saw exactly one."""
    server = FakeNest([die_immediately, reply_ok])
    try:
        with _BareClient(server.url) as client:
            _wait_until_closed(client)
            client.call("fauna.protocol.echo", {})
        assert len(server.requests) == 1
        assert isinstance(server.requests[0][3], bytes)
        assert len(server.requests[0][3]) == 16
    finally:
        server.close()


def test_a_pending_push_is_not_mistaken_for_a_close():
    """The peek reads the frame's opcode, not merely "is anything readable".

    Unsolicited Pushes pend on an idle socket all the time; treating one as
    a close would churn a perfectly good connection *and* drop the Push.
    """
    def push_then_reply(conn: socket.socket, requests: list) -> None:
        conn.sendall(
            _binary_frame(
                cbor2.dumps({0: 2, 2: "fauna.test.push", 4: {}, 8: 1}, canonical=True)
            )
        )
        reply_ok(conn, requests)

    server = FakeNest([push_then_reply])
    try:
        with _BareClient(server.url) as client:
            # Let the Push land in the socket buffer before the next call.
            deadline = time.monotonic() + 5.0
            while not _readable(client) and time.monotonic() < deadline:
                time.sleep(0.01)
            assert _readable(client), "the fake nest never sent its Push"

            assert client.call("fauna.protocol.echo", {}) == {
                "echoed": "fauna.protocol.echo"
            }
            # Drained inside the `with`: `close()` clears the buffer, because
            # Push `seq` is per-connection and must not leak across one.
            assert [p.kind for p in client.drain_pushes()] == ["fauna.test.push"]

        assert server.connections == 1, "a pending Push churned the connection"
        assert _ws_rpc_core.reconnects() == []
    finally:
        server.close()


def _wait_until_buffered(client: _BareClient, n: int) -> None:
    """Block until ``n`` bytes sit unread in the client's socket buffer.

    The precondition below is "the Push AND the Close are both already here",
    so it is polled on the buffer itself rather than on ``_peer_closed()``,
    which is the thing under test (convention 14: a deadline poll on
    observable state, never a settle-sleep).
    """
    sock = client._ws.sock  # type: ignore[union-attr]
    deadline = time.monotonic() + 5.0
    while time.monotonic() < deadline:
        # A zero-timeout `select` gates the peek instead of `MSG_DONTWAIT`,
        # which Windows' socket module does not have — the same shape the
        # client's own `_peer_closed` uses, so a readable socket's MSG_PEEK
        # returns what is buffered without ever blocking.
        readable, _, _ = select.select([sock], [], [], 0)
        if readable and len(sock.recv(n, socket.MSG_PEEK)) >= n:
            return
        time.sleep(0.01)
    raise AssertionError(f"the fake nest's {n} bytes never reached the client")


def test_a_close_queued_behind_a_push_is_still_read_as_a_close():
    """A Push the nest sent just before closing must not hide its Close.

    The pre-send peek used to read ONE byte: a buffer holding a Push and then
    a Close showed the Push's opcode and answered "open", so the Request went
    out on a socket the nest had already closed and ``recv()`` then met the
    Close — an in-flight ``WsLinkDied`` that the pre-send check exists to turn
    into a clean reconnect. Walking the peeked frames reads past the Push.
    """
    push = _binary_frame(
        cbor2.dumps({0: 2, 2: "fauna.test.push", 4: {}, 8: 1}, canonical=True)
    )
    close = _close_frame()

    def push_then_close(conn: socket.socket, requests: list) -> None:
        conn.sendall(push + close)

    server = FakeNest([push_then_close, reply_ok])
    try:
        with _BareClient(server.url) as client:
            _wait_until_buffered(client, len(push) + len(close))
            reply = client.call("fauna.protocol.echo", {})
    finally:
        server.close()

    assert reply == {"echoed": "fauna.protocol.echo"}
    assert [e.reason for e in _ws_rpc_core.reconnects()] == ["peer-closed"]
    assert server.upgraded == 2, "the call must have gone out on a fresh socket"


def test_a_frame_the_peek_cut_short_proves_no_close():
    """An incomplete frame ends the walk as "not closed" — a Close that might
    follow it cannot be read, and an unproven close must not churn a socket."""
    walk = _WsRpcClientBase._buffered_frames_hold_a_close
    push = _binary_frame(b"x" * 300)
    assert walk(push + _close_frame()) is True
    assert walk(push[:-1]) is False
    assert walk(push[:3]) is False
    assert walk(push) is False
    assert walk(_close_frame()) is True


def test_the_recovery_is_bounded_when_every_connection_is_dead():
    """One pre-send reconnect, then the failure surfaces — never a loop.

    The third scripted behavior would answer happily; reaching it would mean
    the client kept redialling. It must not be reached.
    """
    server = FakeNest([die_immediately, die_immediately, reply_ok])
    try:
        with _BareClient(server.url) as client:
            _wait_until_closed(client)
            with pytest.raises(WsLinkDied):
                client.call("fauna.protocol.echo", {})
        assert server.connections == 2, "the client redialled more than once"
    finally:
        server.close()


# ── The in-flight death: refuse to replay, and say why ───────────────────────


def test_a_death_with_the_request_in_flight_is_not_replayed():
    """The deliberate non-survival.

    The obvious fix — "the frame carries an ``idempotency_key``, re-send it" —
    rests on a premise ``transport.md`` refutes in as many words: the nest's
    idempotency cache is per-``RpcConnection`` and cannot deduplicate a
    re-send that lands on a fresh socket. Whether a re-send is safe is the
    kind's ``forbid_replay`` flag, which this harness cannot read. So the
    ambiguous case fails loudly instead of double-applying.
    """
    server = FakeNest([die_after_reading_one, reply_ok])
    try:
        with _BareClient(server.url) as client:
            with pytest.raises(WsLinkDied) as caught:
                client.call("fauna.conversations.send", {"body": b"once"})
        # The killer assertion: the second connection was never used to
        # re-send. A replay here would deliver the message twice.
        assert len(server.requests) == 1
    finally:
        server.close()

    message = str(caught.value)
    assert "Request already sent" in message
    assert "per-connection" in message
    assert "NOT the harness parking a connection" in message


def test_the_in_flight_death_is_not_counted_as_a_routine_reconnect():
    """It is the signal, so it must not disappear into the idle tally."""
    server = FakeNest([die_after_reading_one, reply_ok])
    try:
        with _BareClient(server.url) as client:
            with pytest.raises(WsLinkDied):
                client.call("fauna.protocol.echo", {})
    finally:
        server.close()

    assert _ws_rpc_core.reconnects() == []


# ── The tally reaches the run summary ────────────────────────────────────────


class _StubReporter:
    def __init__(self):
        self.lines: list[str] = []

    def write_sep(self, _sep, title, **_kwargs):
        self.lines.append(title)

    def write_line(self, line, **_kwargs):
        self.lines.append(line)


def test_the_run_summary_separates_routine_reconnects_from_the_signal():
    """A reconnect nobody counts can absorb a systematic nest-side drop.

    The two reasons must stay apart in the output: parking a socket is the
    harness's own housekeeping, while a send that failed on a socket the
    pre-send check had just cleared is evidence about the nest.
    """
    import conftest

    _ws_rpc_core._RECONNECTS.extend(
        [
            _ws_rpc_core.WsReconnect("idle", "fauna.feed.list", 41.0),
            _ws_rpc_core.WsReconnect("idle", "fauna.posts.create", 92.0),
            _ws_rpc_core.WsReconnect("send-failed", "fauna.posts.create", 0.2),
        ]
    )
    reporter = _StubReporter()
    conftest._report_ws_rpc_reconnects(reporter)
    blob = "\n".join(reporter.lines)

    assert "ws-rpc reconnects: 2 idle, 1 send-failed" in blob
    assert "longest park 92s" in blob
    assert "send-failed: fauna.posts.create" in blob
    assert "why established connections are dropping" in blob


def test_the_run_summary_is_silent_when_nothing_reconnected():
    """The inner loop must never see this section on a healthy run."""
    import conftest

    reporter = _StubReporter()
    conftest._report_ws_rpc_reconnects(reporter)
    assert reporter.lines == []


# ── The reply budget is still a timeout, not a death ─────────────────────────


def test_a_slow_reply_on_a_live_socket_times_out_with_the_kind_named():
    """A silent-but-open socket must not be mistaken for a dead one — and the
    timeout has to name the kind, or the failure needs a rerun to diagnose."""

    def read_and_stall(conn: socket.socket, requests: list) -> None:
        frame = _read_request(conn)
        if frame is not None:
            requests.append(frame)
        # Answer nothing, and hold the connection open by blocking on the next
        # frame — which arrives only when the client gives up and closes. A
        # causal anchor rather than a sleep: the server does not have to guess
        # how long the client's budget is, so the two cannot drift apart.
        _read_request(conn)

    server = FakeNest([read_and_stall])
    try:
        with _BareClient(server.url, reply_timeout=0.5) as client:
            with pytest.raises(TimeoutError) as caught:
                client.call("fauna.feed.list", {})
    finally:
        server.close()

    message = str(caught.value)
    assert "fauna.feed.list" in message
    assert "without a Reply" in message


# ── The poisoned client: a reconnect that could not redial ───────────────────
#
# Everything above pins the reconnect that WORKS. This is the one that does not,
# and its victim is not the call that fails — it is every later call, because
# `_reconnect()` is `close()` then `_connect()`, so a `_connect()` that raises
# leaves `_ws` `None` on a client nobody ever exited. Inside a `with` block that
# is harmless (it is unwinding anyway); in the module-level `(base_url,
# actor_id)` caches of `tests/api/ws_api.py` and `tests/api/conv_api.py` the
# dead instance STAYS, and every later caller gets a `RuntimeError` about a
# `with` block that was never missing. Measured on two linux `test_search` tests
# in the 2026-09-11 `--app linux` sweep, whose real
# subject was 55 seeded posts and not the transport at all.


class _FlakyConnectClient(_BareClient):
    """A `_BareClient` whose next `_connect()` can be made to fail on demand.

    The injectable form of "the nest was mid-restart, or its port had been
    recycled onto a different nest" — the two ways a redial legitimately loses.
    Injected rather than waited for, for the usual reason: a test that arranged
    a real restart would be timing-dependent (convention 14).
    """

    def __init__(self, url: str, **kwargs):
        super().__init__(url, **kwargs)
        self.fail_next_connect = False

    def _connect(self) -> None:
        if self.fail_next_connect:
            self.fail_next_connect = False
            raise ConnectionRefusedError("nest is mid-restart")
        super()._connect()


def test_a_failed_redial_leaves_the_client_closed_and_says_so():
    """The state after the loss, and the message the next caller gets.

    Two assertions, and the second is the one that cost the debugging time: the
    client is closed (fair — it is), and the error naming that must not blame a
    missing `with`, because the reader then goes looking for one in a test that
    never had a `with` to lose.
    """
    server = FakeNest([die_immediately, reply_ok])
    try:
        client = _FlakyConnectClient(server.url)
        client.__enter__()
        _wait_until_closed(client)

        client.fail_next_connect = True
        with pytest.raises(ConnectionRefusedError):
            client.call("fauna.protocol.echo", {})

        assert not client.is_open(), (
            "a `_reconnect()` whose `_connect()` raised has already run "
            "`close()`, so the client is left holding no socket"
        )
        with pytest.raises(RuntimeError) as caught:
            client.call("fauna.protocol.echo", {})
    finally:
        server.close()

    message = str(caught.value)
    assert "no open socket" in message
    assert "_reconnect()" in message, (
        "the message must name the cause that actually produces this state; "
        f"got {message!r}"
    )


def test_ensure_open_revives_a_client_a_failed_redial_closed():
    """The heal, which is what lets a cache keep its own contract.

    `ensure_open()` is idempotent and free on a live client, so a cache can call
    it unconditionally rather than deciding when a socket might have dropped —
    which is the decision no cache was in a position to make.
    """
    server = FakeNest([die_immediately, reply_ok])
    try:
        client = _FlakyConnectClient(server.url)
        client.__enter__()
        _wait_until_closed(client)

        client.fail_next_connect = True
        with pytest.raises(ConnectionRefusedError):
            client.call("fauna.protocol.echo", {})
        assert not client.is_open()

        client.ensure_open()
        assert client.is_open()
        # The revived socket is a working one, not merely a non-None field.
        assert client.call("fauna.protocol.echo", {}) is not None

        # Idempotent: a second call on a live client is a no-op, not a redial.
        before = server.connections
        client.ensure_open()
        assert server.connections == before
        client.close()
    finally:
        server.close()


def test_the_api_client_cache_hands_out_only_open_clients():
    """The cache-level half, pinned where the bug actually lived.

    `_client_for` promises "an open, cached WS-RPC client" in its own docstring
    and used to return whatever was in the dict. Both caches are checked: they
    are byte-identical twins, and fixing one is how this returns.
    """
    import importlib

    for module_name in ("tests.api.ws_api", "tests.api.conv_api"):
        module = importlib.import_module(module_name)
        calls: list[str] = []

        class _StubClient:
            def __init__(self, *a, **kw):
                self.open = False

            def __enter__(self):
                self.open = True
                calls.append("enter")
                return self

            def ensure_open(self):
                calls.append("ensure_open")
                self.open = True
                return self

        actor = {"actor_id_bytes": b"\x01" * 32, "signing_key": b"\x02" * 32}
        saved_cls = module.WsRpcAdminClient
        saved_cache = dict(module._CLIENTS)
        module._CLIENTS.clear()
        try:
            module.WsRpcAdminClient = _StubClient
            first = module._client_for("http://127.0.0.1:1", actor)
            assert calls == ["enter"], f"{module_name}: {calls}"

            # The second hand-out is where a dead client used to escape.
            first.open = False
            again = module._client_for("http://127.0.0.1:1", actor)
            assert again is first, f"{module_name}: the cache stopped caching"
            assert again.open, (
                f"{module_name}: `_client_for` handed out a CLOSED client — "
                "the next `call()` on it raises about a missing `with`"
            )
            assert calls == ["enter", "ensure_open"], f"{module_name}: {calls}"
        finally:
            module.WsRpcAdminClient = saved_cls
            module._CLIENTS.clear()
            module._CLIENTS.update(saved_cache)


def test_a_cache_entry_that_cannot_be_revived_is_evicted():
    """A re-open that fails must not leave the corpse for the next caller.

    Otherwise the heal merely moves the problem: the second caller gets the
    connection error, the third gets the misleading `RuntimeError` again.
    """
    import importlib

    module = importlib.import_module("tests.api.ws_api")

    class _DeadClient:
        def __init__(self, *a, **kw):
            pass

        def __enter__(self):
            return self

        def ensure_open(self):
            raise ConnectionRefusedError("nest is gone")

    actor = {"actor_id_bytes": b"\x03" * 32, "signing_key": b"\x04" * 32}
    saved_cls = module.WsRpcAdminClient
    saved_cache = dict(module._CLIENTS)
    module._CLIENTS.clear()
    try:
        module.WsRpcAdminClient = _DeadClient
        module._client_for("http://127.0.0.1:1", actor)
        with pytest.raises(ConnectionRefusedError):
            module._client_for("http://127.0.0.1:1", actor)
        assert not module._CLIENTS, "the unrevivable entry stayed in the cache"
    finally:
        module.WsRpcAdminClient = saved_cls
        module._CLIENTS.clear()
        module._CLIENTS.update(saved_cache)


# ── A bearer the nest forgot is re-minted, once ──────────────────────────────
#
# The nest's bearer store is in memory, so a nest restart forgets every bearer
# long before its TTL runs out — and the suite restarts the SESSION nest on
# purpose (`test_family.py`, `test_nest_flip_resilience.py`,
# `test_offline_gate.py`). The cached `WsRpcAdminClient` kept re-presenting its
# bearer on every redial, so the first `ws_api` call a later test made for the
# same actor died `401 invalid token` at the upgrade: four linux instances in
# each of two whole-suite sweeps, every one in whichever test happened to run
# first after a restart. Every app already recovers
# from exactly this (`transport-connection.md` § Connection lifecycle →
# *Upgrade-time auth rejection*): drop the bearer, re-mint once, retry — and a
# fresh bearer refused again is a real refusal, not a stale one.


def _bearer_of(request: bytes) -> Optional[str]:
    """The ``bearer.<token>`` subprotocol a dial presented, in either header
    form (one comma-joined value, or one header line per subprotocol)."""
    for line in request.split(b"\r\n"):
        if not line.lower().startswith(b"sec-websocket-protocol:"):
            continue
        for part in line.split(b":", 1)[1].split(b","):
            value = part.strip().decode()
            if value.startswith("bearer."):
                return value[len("bearer."):]
    return None


class _MintCountingClient(WsRpcAdminClient):
    """The real authenticated client with only the network half of its mint
    stubbed: the bearer cache and the upgrade-refusal handling under test run
    unmodified, and every mint is recorded in order."""

    def __init__(self, port: int):
        super().__init__(
            f"http://127.0.0.1:{port}",
            actor_id=b"\x05" * 32,
            signing_key=b"\x06" * 32,
        )
        self.minted: list[str] = []

    def _mint_bearer(self) -> _BearerToken:
        token = f"t{len(self.minted) + 1}"
        self.minted.append(token)
        return _BearerToken(token=token, expires_at=time.time() + 3600)


def test_a_bearer_the_nest_forgot_is_re_minted_once_and_the_call_goes_through():
    """The restart, injected: the first connection dies, and from then on the
    nest refuses every bearer minted before it — which is what an in-memory
    token store does across a restart."""
    forgotten: set[str] = set()
    server = FakeNest(
        [die_immediately, reply_ok],
        upgrade_status=lambda request: 401 if _bearer_of(request) in forgotten else 101,
    )
    try:
        client = _MintCountingClient(server.port)
        client.__enter__()
        _wait_until_closed(client)
        forgotten.update(client.minted)  # the restart

        assert client.call("fauna.protocol.echo", {}) is not None
        client.close()
    finally:
        server.close()

    assert client.minted == ["t1", "t2"], (
        "an upgrade 401 must drop the cached bearer and re-mint exactly once; "
        f"mints={client.minted!r}"
    )
    assert server.rejected_upgrades == 1
    assert len(server.requests) == 1, (
        "nothing was in flight across the refused dial, so the Request crosses "
        f"the wire exactly once; got {server.requests!r}"
    )


def test_a_fresh_bearer_the_nest_also_refuses_raises_after_one_re_mint():
    """No refresh→401 loop: a bearer minted a moment ago and refused anyway is
    a real refusal, and the caller must see it."""
    refusing = {"on": False}
    server = FakeNest(
        [die_immediately],
        upgrade_status=lambda request: 401 if refusing["on"] else 101,
    )
    try:
        client = _MintCountingClient(server.port)
        client.__enter__()
        _wait_until_closed(client)
        refusing["on"] = True

        with pytest.raises(websocket.WebSocketBadStatusException) as caught:
            client.call("fauna.protocol.echo", {})
    finally:
        server.close()

    assert caught.value.status_code == 401
    assert client.minted == ["t1", "t2"], f"mints={client.minted!r}"
    assert server.rejected_upgrades == 2


def test_a_refusal_that_is_not_a_401_is_not_answered_with_a_re_mint():
    """A 403 is the cross-connection binding refusal (a bearer for one actor
    presented on another's URL): a fresh bearer for the same actor changes
    nothing, so it surfaces as-is rather than spending a mint."""
    status = {"code": 101}
    server = FakeNest([die_immediately], upgrade_status=lambda request: status["code"])
    try:
        client = _MintCountingClient(server.port)
        client.__enter__()
        _wait_until_closed(client)
        status["code"] = 403

        with pytest.raises(websocket.WebSocketBadStatusException) as caught:
            client.call("fauna.protocol.echo", {})
    finally:
        server.close()

    assert caught.value.status_code == 403
    assert client.minted == ["t1"], f"mints={client.minted!r}"
    assert server.rejected_upgrades == 1
