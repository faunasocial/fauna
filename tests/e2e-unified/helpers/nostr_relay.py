"""The Nostr relay protocol from outside a nest — a NIP-01 client, and a fake
relay a nest can be pointed at.

Two halves, one wire:

* **The client** (:func:`publish`, :func:`query`) speaks to a nest's own
  ``/nostr`` endpoint as any Nostr app does — anonymously, over one WebSocket,
  ``EVENT`` → ``OK`` and ``REQ`` → events → ``EOSE``. It is what the nest-side
  witnesses of ``docs/features/nostr.md`` observe the nest's relay through, and
  what an app journey uses to drop a far user's gift wrap on it.
* **The fake relay** (:class:`FakeRelay`) is a relay the NEST dials: the user's
  publish list, or a followed author's relay hint. It records every event the
  nest sends it (so a test can assert what was published, and — anchored on a
  later control event — what was not), serves the events a test seeds to the
  nest's subscriptions, and pushes a newly seeded event live to every open
  subscription that matches, as a real relay does after ``EOSE``.

A nest refuses to dial a loopback relay in production (`nest/network-exposure.md`
§ Rulings F7); a nest that must dial a :class:`FakeRelay` runs with the
``test-hooks`` escape ``FAUNA_TEST_NOSTR_ALLOW_LOOPBACK`` set
(`bins/fauna-nest/src/nostr/relays.rs::relay_dial_policy`), the same double gate
as ActivityPub's loopback exemption. :data:`ALLOW_LOOPBACK_ENV` is that env.

The fake is a hand-rolled RFC 6455 server over the stdlib, the same shape as
`tests/test_ws_rpc_liveness_reconnect.py`'s: the harness carries a WebSocket
client package and no server one, and a relay needs nothing a few frames of
socket code cannot say.
"""

from __future__ import annotations

import base64
import hashlib
import json
import socket
import struct
import threading
import time
import urllib.parse
from typing import Callable, Optional

import websocket

from helpers.budgets import RPC_ROUNDTRIP_S

#: The env that lets a `test-hooks` nest dial a loopback relay — and nothing
#: wider (`relays.rs::relay_dial_policy`).
ALLOW_LOOPBACK_ENV = {"FAUNA_TEST_NOSTR_ALLOW_LOOPBACK": "1"}


# ── The client: a Nostr app's view of a nest's own relay ─────────────────────


def nest_relay_url(nest) -> str:
    """The ``ws(s)://<authority>/nostr`` relay URL of ``nest``'s own relay,
    from the URL the run's provider published for it."""
    parsed = urllib.parse.urlparse(nest["url"])
    scheme = "wss" if parsed.scheme == "https" else "ws"
    return f"{scheme}://{parsed.netloc}/nostr"


def _connect(relay_url: str):
    options = {"sslopt": {"cert_reqs": 0}} if relay_url.startswith("wss://") else {}
    return websocket.create_connection(relay_url, timeout=RPC_ROUNDTRIP_S, **options)


def publish(relay_url: str, event: dict) -> tuple[bool, str]:
    """``EVENT`` ``event`` to the relay at ``relay_url`` as an anonymous client;
    returns the relay's ``OK`` verdict as ``(accepted, message)``."""
    ws = _connect(relay_url)
    try:
        ws.send(json.dumps(["EVENT", event]))
        while True:
            frame = json.loads(ws.recv())
            if frame[0] == "OK" and frame[1] == event["id"]:
                return bool(frame[2]), str(frame[3] if len(frame) > 3 else "")
    finally:
        ws.close()


def subscribe(
    relay_url: str,
    *filters: dict,
    auth: Optional[tuple[str, Callable[[dict], dict]]] = None,
) -> tuple[list[dict], Optional[str]]:
    """``REQ`` ``filters`` from the relay at ``relay_url``; returns the stored
    events it serves before ``EOSE``, and the reason it gave if it ``CLOSED``
    the subscription instead (``None`` when it did not).

    Anonymous by default. ``auth=(relay_tag_url, sign)`` first answers the
    relay's NIP-42 ``AUTH`` challenge as a Nostr app holding a key does: a
    kind-22242 event naming ``relay_tag_url`` (the relay's URL as the relay
    knows itself) and the challenge, signed by ``sign`` — a callable taking the
    unsigned event and returning it signed."""
    ws = _connect(relay_url)
    try:
        if auth is not None:
            relay_tag_url, sign = auth
            challenge = None
            while challenge is None:
                frame = json.loads(ws.recv())
                if frame[0] == "AUTH":
                    challenge = frame[1]
            event = sign({
                "kind": 22242, "created_at": int(time.time()), "content": "",
                "tags": [["relay", relay_tag_url], ["challenge", challenge]],
            })
            ws.send(json.dumps(["AUTH", event]))
            while True:
                frame = json.loads(ws.recv())
                if frame[0] == "OK" and frame[1] == event["id"]:
                    assert frame[2] is True, f"the relay refused the AUTH: {frame!r}"
                    break
        ws.send(json.dumps(["REQ", "witness", *filters]))
        events = []
        while True:
            frame = json.loads(ws.recv())
            if frame[0] == "EVENT" and frame[1] == "witness":
                events.append(frame[2])
            elif frame[0] == "EOSE" and frame[1] == "witness":
                return events, None
            elif frame[0] == "CLOSED" and frame[1] == "witness":
                return events, str(frame[2] if len(frame) > 2 else "")
    finally:
        ws.close()


def query(relay_url: str, *filters: dict) -> list[dict]:
    """``REQ`` ``filters`` from the relay at ``relay_url`` as an anonymous
    client; returns the stored events it serves before ``EOSE``. A ``CLOSED``
    (the relay refusing the subscription) raises, naming its reason."""
    events, closed = subscribe(relay_url, *filters)
    if closed is not None:
        raise AssertionError(f"the relay closed the subscription: {closed!r}")
    return events


def tag_values(event: dict, name: str) -> list[list[str]]:
    """Every ``name`` tag of ``event``, each as its value list (the tag name
    dropped)."""
    return [t[1:] for t in event.get("tags", []) if t and t[0] == name]


# ── The fake relay: a relay the nest dials ───────────────────────────────────


_WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
_OP_TEXT = 0x1
_OP_CLOSE = 0x8
_OP_PING = 0x9
_OP_PONG = 0xA


def _matches(event: dict, flt: dict) -> bool:
    """NIP-01 filter semantics: every dimension the filter names must match."""
    if "ids" in flt and event["id"] not in flt["ids"]:
        return False
    if "authors" in flt and event["pubkey"] not in flt["authors"]:
        return False
    if "kinds" in flt and event["kind"] not in flt["kinds"]:
        return False
    if "since" in flt and event["created_at"] < flt["since"]:
        return False
    if "until" in flt and event["created_at"] > flt["until"]:
        return False
    for key, wanted in flt.items():
        if key.startswith("#") and len(key) == 2:
            values = {v[0] for v in tag_values(event, key[1]) if v}
            if not values & set(wanted):
                return False
    return True


class FakeRelay:
    """A NIP-01 relay on ``127.0.0.1`` for a nest to dial.

    ``received`` is every event a client sent it, in arrival order — what the
    nest PUBLISHED here. ``seed(event)`` stores an event the relay will serve
    (a far author's note or deletion), and pushes it at once to every open
    subscription it matches. ``requests`` records each ``REQ``'s filters, so a
    test can see the nest subscribe.
    """

    def __init__(self) -> None:
        self._sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._sock.bind(("127.0.0.1", 0))
        self._sock.listen(16)
        self.port = self._sock.getsockname()[1]
        self._lock = threading.Lock()
        self._stored: list[dict] = []
        self._subs: list[tuple[socket.socket, str, list[dict]]] = []
        self.received: list[dict] = []
        self.requests: list[list[dict]] = []
        self._closed = threading.Event()
        threading.Thread(target=self._accept_loop, daemon=True).start()

    @property
    def url(self) -> str:
        return f"ws://127.0.0.1:{self.port}"

    def seed(self, event: dict) -> None:
        """Store ``event`` for future ``REQ``s and push it live to every open
        subscription whose filters it matches."""
        with self._lock:
            self._stored.append(event)
            live = [(c, sid) for c, sid, flts in self._subs if any(_matches(event, f) for f in flts)]
        for conn, sid in live:
            self._send(conn, ["EVENT", sid, event])

    def received_by(self, pubkey: str, kind: Optional[int] = None) -> list[dict]:
        """The events ``pubkey`` published here (of ``kind``, when given)."""
        with self._lock:
            return [
                e for e in self.received
                if e["pubkey"] == pubkey and (kind is None or e["kind"] == kind)
            ]

    def close(self) -> None:
        self._closed.set()
        try:
            self._sock.close()
        except OSError:
            pass

    # ── the socket half ──

    def _accept_loop(self) -> None:
        while not self._closed.is_set():
            try:
                conn, _ = self._sock.accept()
            except OSError:
                return
            threading.Thread(target=self._serve, args=(conn,), daemon=True).start()

    def _serve(self, conn: socket.socket) -> None:
        try:
            if not self._handshake(conn):
                return
            while True:
                frame = self._read_frame(conn)
                if frame is None:
                    return
                opcode, payload = frame
                if opcode == _OP_CLOSE:
                    return
                if opcode == _OP_PING:
                    self._write_frame(conn, _OP_PONG, payload)
                    continue
                if opcode != _OP_TEXT:
                    continue
                self._handle(conn, json.loads(payload.decode()))
        except (OSError, ValueError):
            return
        finally:
            with self._lock:
                self._subs = [s for s in self._subs if s[0] is not conn]
            try:
                conn.close()
            except OSError:
                pass

    def _handle(self, conn: socket.socket, msg: list) -> None:
        verb = msg[0]
        if verb == "EVENT":
            event = msg[1]
            with self._lock:
                self.received.append(event)
            self._send(conn, ["OK", event["id"], True, ""])
        elif verb == "REQ":
            sid, filters = msg[1], list(msg[2:])
            with self._lock:
                self.requests.append(filters)
                self._subs = [s for s in self._subs if not (s[0] is conn and s[1] == sid)]
                self._subs.append((conn, sid, filters))
                stored = list(self._stored)
            for event in stored:
                if any(_matches(event, f) for f in filters):
                    self._send(conn, ["EVENT", sid, event])
            self._send(conn, ["EOSE", sid])
        elif verb == "CLOSE":
            with self._lock:
                self._subs = [s for s in self._subs if not (s[0] is conn and s[1] == msg[1])]
        elif verb == "AUTH":
            self._send(conn, ["OK", msg[1].get("id", ""), True, ""])

    def _send(self, conn: socket.socket, msg: list) -> None:
        try:
            self._write_frame(conn, _OP_TEXT, json.dumps(msg).encode())
        except OSError:
            pass

    @staticmethod
    def _handshake(conn: socket.socket) -> bool:
        request = b""
        while b"\r\n\r\n" not in request:
            chunk = conn.recv(4096)
            if not chunk:
                return False
            request += chunk
        key = next(
            (
                line.split(b":", 1)[1].strip().decode()
                for line in request.split(b"\r\n")
                if line.lower().startswith(b"sec-websocket-key:")
            ),
            None,
        )
        if key is None:
            return False
        accept = base64.b64encode(hashlib.sha1((key + _WS_GUID).encode()).digest()).decode()
        conn.sendall(
            b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n"
            b"Connection: Upgrade\r\nSec-WebSocket-Accept: " + accept.encode() + b"\r\n\r\n"
        )
        return True

    @staticmethod
    def _recv_exactly(conn: socket.socket, n: int) -> Optional[bytes]:
        buf = b""
        while len(buf) < n:
            chunk = conn.recv(n - len(buf))
            if not chunk:
                return None
            buf += chunk
        return buf

    def _read_frame(self, conn: socket.socket) -> Optional[tuple[int, bytes]]:
        """One client→server frame as ``(opcode, payload)``; continuation
        frames are folded into their message. Client frames are masked."""
        opcode, message = None, b""
        while True:
            header = self._recv_exactly(conn, 2)
            if header is None:
                return None
            fin, op = header[0] & 0x80, header[0] & 0x0F
            length = header[1] & 0x7F
            if length == 126:
                ext = self._recv_exactly(conn, 2)
                if ext is None:
                    return None
                (length,) = struct.unpack("!H", ext)
            elif length == 127:
                ext = self._recv_exactly(conn, 8)
                if ext is None:
                    return None
                (length,) = struct.unpack("!Q", ext)
            mask = self._recv_exactly(conn, 4) if header[1] & 0x80 else b"\0\0\0\0"
            payload = self._recv_exactly(conn, length) if length else b""
            if mask is None or payload is None:
                return None
            payload = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
            if op >= 0x8:  # a control frame may interleave a fragmented message
                return op, payload
            if op != 0:
                opcode = op
            message += payload
            if fin:
                return opcode, message

    def _write_frame(self, conn: socket.socket, opcode: int, payload: bytes) -> None:
        n = len(payload)
        if n < 126:
            header = struct.pack("!BB", 0x80 | opcode, n)
        elif n < 65536:
            header = struct.pack("!BBH", 0x80 | opcode, 126, n)
        else:
            header = struct.pack("!BBQ", 0x80 | opcode, 127, n)
        with self._lock:
            conn.sendall(header + payload)


def await_received(
    relay: FakeRelay,
    predicate: Callable[[dict], bool],
    budget_s: float,
    what: str,
) -> dict:
    """The first event ``relay`` received that satisfies ``predicate``, waiting
    up to ``budget_s`` for it; the failure names everything that DID arrive."""
    from helpers.waiting import wait_until

    return wait_until(
        lambda: next((e for e in list(relay.received) if predicate(e)), None),
        budget_s,
        diagnose=lambda: (
            f"{what} never reached the relay; it received "
            f"{[(e['kind'], e['content'][:60]) for e in relay.received]!r}"
        ),
    )
