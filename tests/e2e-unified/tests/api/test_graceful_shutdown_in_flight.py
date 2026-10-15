"""tier_3 API: a request in flight when the nest begins to stop is finished and
answered before the WS 1001 close.

Goal doc: ``docs/goal/architecture/transport-connection.md`` § Graceful
shutdown — on SIGTERM each connection stops reading new requests, drains the
handlers already running, flushes their Replies, and only then closes with
1001 (Going Away). The 1001 code alone is witnessed over the real image by
``tests/platform/docker/test_graceful_shutdown.py``; this pins the drain.

**How "in flight" is made a state, not an interval (convention 14).** The
request is parked at the nest's dispatch chokepoint by
``helpers/rpc_hold.hold_rpc_until_shutdown`` (``--features test-hooks``,
``rpc_hold_test_hook.rs`` § Holding until shutdown), which the stop itself
lets go — a plain hold's HTTP release could not reach a nest that has begun to
stop. The test stops the nest only once the nest's own ``holding`` counter
proves the request has arrived and is parked, so nothing here races a handler's
running time. The parked kind is a real write (``fauna.inbox.mode.set``), and
the restart read-back proves its handler ran to commit, not merely that some
frame came back.

Process safety: the nest is this test's own dedicated one, stopped through its
own ``proc`` handle (``common.nest.stop_nest``, SIGTERM on POSIX) — never a
signal to a process found by name. ``standalone_only``: the hook exists only in
a nest compiled with ``test-hooks`` (convention 15), which no docker or live
artifact is, and only a standalone nest is a process the harness can stop.
"""
from __future__ import annotations

import secrets
import struct
import threading

import cbor2
import pytest
import websocket  # from websocket-client

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register
from common.nest import start_nest_in_place, stop_nest
from helpers.rpc_hold import (
    hold_rpc_until_shutdown,
    release_rpc_hold,
    wait_for_held_rpc,
)

pytestmark = [pytest.mark.tier_3, pytest.mark.standalone_only]

_SET_KIND = "fauna.inbox.mode.set"
_GET_KIND = "fauna.inbox.mode.get"
# Not the default (`allow_knock`), so the read-back can only see it if the held
# handler committed.
_HELD_MODE = "closed"
_CORR = 1

# Envelope frame types and keys (`clients/_ws_rpc_core.py`, the one wire
# definition the harness keeps).
_FRAME_REQUEST = 0
_FRAME_REPLY = 1

#: Ceiling for the whole stop: the nest's own drain is bounded at a few
#: seconds (`ws::GRACEFUL_SHUTDOWN_TIMEOUT`), so a green run returns far inside
#: it and only a hung reader spends it.
_STOP_BUDGET_S = 60.0


@pytest.fixture()
def shutdown_nest(request, nest_mode, tmp_path_factory):
    """A nest exclusive to this test — it gets stopped and re-spawned in place."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "graceful-in-flight-nest"
    )
    yield nest
    cleanup()


def _ws_url(nest_url: str, actor_id_hex: str) -> str:
    base = nest_url.rstrip("/")
    if base.startswith("https://"):
        base = "wss://" + base[len("https://"):]
    elif base.startswith("http://"):
        base = "ws://" + base[len("http://"):]
    return f"{base}/api/v1/ws/{actor_id_hex}"


def _read_until_close(ws: "websocket.WebSocket", frames: list) -> None:
    """Record every frame the nest sends, in arrival order, until its Close
    frame or the socket's end. ``("reply", corr, ok)``, ``("push",)``,
    ``("close", code)`` or ``("dropped", repr)`` for an end with no Close."""
    ws.settimeout(_STOP_BUDGET_S)
    while True:
        try:
            frame = ws.recv_frame()
        except Exception as e:  # timeout or a dead socket: both end the record
            frames.append(("dropped", repr(e)))
            return
        if frame is None:
            continue
        if frame.opcode == websocket.ABNF.OPCODE_CLOSE:
            code = struct.unpack("!H", frame.data[:2])[0] if len(frame.data) >= 2 else None
            frames.append(("close", code))
            return
        if frame.opcode == websocket.ABNF.OPCODE_BINARY:
            env = cbor2.loads(frame.data)
            if env.get(0) == _FRAME_REPLY:
                frames.append(("reply", env.get(1), bool(env.get(7)), env.get(4)))
            else:
                frames.append(("push",))


@pytest.mark.feature("nest-hardening")
def test_a_request_in_flight_at_stop_is_answered_before_the_1001_close(shutdown_nest):
    """hold a write until shutdown → send it → see it parked → SIGTERM the
    nest → its Reply (ok) reaches the socket BEFORE the close frame, the close
    is 1001, and after a restart the write is there."""
    nest = shutdown_nest
    port = nest["port"]
    user = create_actor_and_register(
        port,
        base_url=nest["url"],
        admin_signing_key=nest["admin"]["signing_key"],
    )

    ws = websocket.create_connection(
        _ws_url(nest["url"], user["actor_id_hex"]),
        subprotocols=["fauna.v1", f"bearer.{user['token']}"],
        timeout=30,
    )
    frames: list = []
    try:
        hold_rpc_until_shutdown(port, _SET_KIND)
        try:
            ws.send_binary(cbor2.dumps(
                {
                    0: _FRAME_REQUEST,
                    1: _CORR,
                    2: _SET_KIND,
                    3: secrets.token_bytes(16),
                    4: {"mode": _HELD_MODE},
                },
                canonical=True,
            ))
            # Arrival, not a sleep: the request is parked at the nest right now.
            wait_for_held_rpc(port, _SET_KIND)
        except BaseException:
            release_rpc_hold(port, _SET_KIND)
            raise

        # Read concurrently with the stop: the nest's drain sends the Reply and
        # the Close while the process is still exiting.
        reader = threading.Thread(target=_read_until_close, args=(ws, frames), daemon=True)
        reader.start()
        stop_nest(nest, graceful=True)
        reader.join(timeout=_STOP_BUDGET_S)
        assert not reader.is_alive(), (
            f"the socket neither closed nor dropped within {_STOP_BUDGET_S:.0f}s "
            f"of the stop; frames so far: {frames!r}"
        )
    finally:
        try:
            ws.close()
        except Exception:
            pass

    replies = [i for i, f in enumerate(frames) if f[0] == "reply" and f[1] == _CORR]
    closes = [i for i, f in enumerate(frames) if f[0] == "close"]
    assert replies, (
        "the request parked when the stop began was never answered — the drain "
        f"must run it and flush its Reply before closing. Frames: {frames!r}"
    )
    assert closes, (
        "the nest ended the connection without a Close frame — a graceful stop "
        f"closes with 1001, never a bare drop. Frames: {frames!r}"
    )
    assert replies[0] < closes[0], (
        f"the Reply must reach the socket BEFORE the close frame. Frames: {frames!r}"
    )
    reply = frames[replies[0]]
    assert reply[2], f"the held write must succeed, not error: {reply!r}"
    assert frames[closes[0]][1] == 1001, (
        f"a graceful stop closes with 1001 (Going Away); got {frames[closes[0]]!r}"
    )

    # The Reply could in principle be one the nest invented on the way out; the
    # write being there after a restart proves the handler ran to commit.
    start_nest_in_place(nest)
    with WsRpcAdminClient(
        nest["url"],
        actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    ) as client:
        stored = client.call(_GET_KIND, {})
    assert stored.get("mode") == _HELD_MODE, (
        f"the held write's Reply said ok, but the nest does not hold its value "
        f"after a restart: {stored!r}"
    )
