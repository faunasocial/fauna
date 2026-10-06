"""Shared envelope/framing plumbing for the Python WS-RPC clients.

This is the ``clients/__init__.py`` "internal helpers" both the
authenticated :class:`WsRpcAdminClient` (``ws_rpc_admin_client.py``) and the
anonymous :class:`WsRpcAnonClient` (``ws_rpc_anon_client.py``) build on. Every
nest WS-RPC caller class shares one canonical wire surface (DAG-CBOR envelopes,
correlation-id-routed Request/Reply); only the *handshake* differs — a bearer
subprotocol + ``/api/v1/ws/<actor_hex>`` for an authenticated actor, versus the
bare ``fauna.v1`` subprotocol + ``/api/v1/ws`` for the pre-identity anonymous
connection. The base class below owns everything except that handshake.

Wire reference (do not duplicate the contract here; defer to these):
* Envelope shapes — ``bins/fauna-bridges/internal/wsrpc/envelope.go``
* Server-side envelope/dispatch — ``libs/fauna-protocol/src/envelope.rs``
  and ``bins/fauna-nest/src/routes.rs::dispatch_request``.
* Anonymous endpoint — ``bins/fauna-nest/src/routes.rs::ws_anonymous_handler``
  + the pre-identity allowlist ``bins/fauna-nest/src/pre_identity_allowlist.rs``;
  the Rust client analogue is ``libs/fauna-anon-client``.

Threading model: synchronous. One in-flight Request at a time per client
instance. The instance is not safe to share across threads.

Unsolicited Push frames (envelope type 2) are *buffered*, not dropped: any
Push seen while :meth:`_WsRpcClientBase.call` waits for its Reply lands in a
per-connection queue that :meth:`_WsRpcClientBase.drain_pushes` and
:meth:`_WsRpcClientBase.wait_for_push` read back. Buffering rather than
spawning a reader thread is deliberate — a background ``recv()`` would race
``call()``'s own ``recv()``/``settimeout()`` on the same socket, since
``websocket-client`` sockets are not safe for concurrent reads.

Liveness: **an idle connection here is a dead connection, by design**
--------------------------------------------------------------------

That synchronous no-reader-thread model has one consequence worth stating
loudly, because it caused a recurring cross-app e2e failure class that read
as a product bug on whichever test happened to be holding the socket.

The nest runs the server half of the spec heartbeat: it sends a WS Ping every
``KEEPALIVE_INTERVAL`` (30 s) and closes any connection that has sent **no
inbound frame of any kind** for ``KEEPALIVE_TIMEOUT`` (60 s), logging *"WS
peer answered no heartbeat within the liveness window; closing dead link"*
(``bins/fauna-nest/src/routes.rs:1687-1694``, policy at
``bins/fauna-nest/src/ws.rs:437-457``, constants at
``libs/fauna-ws-substrate/src/adapter.rs:29-35``). That is correct and
deliberate — ``transport.md`` § Connection lifecycle explains why a Ping,
not an inbound-idle rule, is the right primitive.

A synchronous ``websocket-client`` socket can only answer that Ping from
*inside* a ``recv()``. This client is inside ``recv()`` only while a
:meth:`call` is in flight. So a connection parked between calls answers
nothing — and ``tests/api/ws_api.py`` caches one open client per
``(base_url, actor_id)`` for the **whole pytest process**, reusing it across
tests minutes apart. Solo, the gaps are short and nothing shows. Under a full
sweep the gaps routinely pass 60 s, the nest reaps the link exactly as
designed, and the *next* call on that cached client dies — historically as a
``BrokenPipeError`` raised from ``recv()``'s courtesy ``send_close()``,
because the peer was already gone.

The fix is therefore not a retry, it is **not parking a connection past the
window**: :meth:`call` reconnects *before* it sends whenever the socket has
been silent for :data:`_IDLE_RECONNECT_AFTER`. That is the
``was_in_flight: false`` case ``transport.md`` § Idempotency and
reconnect-with-resume rules on directly — "nothing was ever sent, so *every*
``request*`` method … simply waits for the supervisor to reconnect … and then
sends, regardless of ``forbid_replay``. No replay, no double-apply, no
per-call-site opt-in."

A death that happens with a request genuinely in flight is the *other* case,
and this client deliberately does **not** replay it — see :meth:`call`.
"""

from __future__ import annotations

import dataclasses
import secrets
import select
import socket
import ssl
import time
from typing import Any, Callable, Optional

import cbor2
import websocket  # type: ignore[import-untyped]  # from `websocket-client`


# Frame-type discriminants — integer key 0 of the envelope CBOR map. Match
# `bins/fauna-bridges/internal/wsrpc/envelope.go` Type{Request,Reply,
# Push,Cancel} constants; the only ones these clients emit/consume are
# Request (out) and Reply (in).
_FRAME_REQUEST = 0
_FRAME_REPLY = 1
_FRAME_PUSH = 2
_FRAME_CANCEL = 3

# Default WS receive timeout — generous because the dispatch task spawns a
# tokio task with its own per-kind deadline; we just need to outlast the
# slowest registered kind's default deadline. fauna.protocol.echo is 5s
# and the bridge-blob handlers default to the router's default. 20s is
# enough headroom under load and still fails the test promptly.
_DEFAULT_REPLY_TIMEOUT = 20.0

# Default HTTP timeout for the challenge/verify round-trips (authenticated
# client) and the WS connect handshake. 5s mirrors every other nest HTTP
# fixture in this directory.
_DEFAULT_HTTP_TIMEOUT = 5.0

# ── The nest's heartbeat, mirrored ────────────────────────────────────────
#
# Python mirrors of `fauna_ws_substrate::{KEEPALIVE_INTERVAL,
# KEEPALIVE_TIMEOUT}` (`libs/fauna-ws-substrate/src/adapter.rs:29-35`), which
# the nest's `WsHeartbeatPolicy` defaults to verbatim rather than restating
# (`bins/fauna-nest/src/ws.rs:446-457`). Mirrored, not guessed — and pinned
# against the Rust source by `test_ws_rpc_liveness_reconnect.py` so the two
# halves of one heartbeat cannot drift apart.
_NEST_PING_INTERVAL = 30.0
_NEST_LIVENESS_TIMEOUT = 60.0

# Reconnect before sending if the socket has carried no frame for this long.
#
# Sized *below* `_NEST_PING_INTERVAL`, not merely below the 60 s liveness
# window: a connection this client hands back to the nest has then never even
# had a Ping go unanswered on it, so the reaping path is not approached, it is
# not entered at all. The margin is deliberately enormous relative to what the
# check costs (one fresh handshake on a connection that was about to be
# reaped anyway), because the failure it prevents is a red test on unrelated
# product code.
#
# This is not a wall-clock assertion (convention 14): no test outcome depends
# on the value. It decides only whether to reuse a socket or open a new one,
# and both branches produce the identical observable call result.
_IDLE_RECONNECT_AFTER = 25.0


class RpcCallError(Exception):
    """Raised when nest replies ``ok=false`` to a ``call()``.

    Carries the typed ``code`` (e.g. ``fauna.bridges.permission_denied``,
    ``fauna.protocol.malformed``, ``fauna.protocol.unknown_kind``) so
    tests can assert against the wire-stable identifier. ``message`` is
    the LocalizedText (i18n key + args) the server provided; ``details``
    is the optional kind-specific payload (often a free-form text blob).
    """

    def __init__(
        self,
        code: str,
        message_key: str,
        message_args: dict[str, str],
        details: Any,
    ):
        msg = f"WS-RPC server error: {code} (message_key={message_key})"
        if details is not None:
            # The nest attaches an operator-debug string to every internal
            # error (rpc_errors.rs). Rust's client logs it — this harness's own WS-RPC client had the same
            # silent-drop bug: `details` was captured on the exception but
            # never surfaced anywhere a failing test's traceback shows it.
            msg += f" details={details!r}"
        super().__init__(msg)
        self.code = code
        self.message_key = message_key
        self.message_args = message_args
        self.details = details

    @property
    def message(self) -> dict[str, Any]:
        """The full LocalizedText wire shape, useful for diagnostics."""
        return {"key": self.message_key, "args": self.message_args}


class WsLinkDied(Exception):
    """The WS link died with a Request **genuinely in flight**.

    Deliberately not survivable, and deliberately not the same thing as the
    routine idle reconnect. Two reasons this raises instead of replaying:

    1. **Replay would not be safe for every kind.** The obvious move — "the
       frame already carries an ``idempotency_key``, so re-send it" — is
       backed by the nest's DURABLE idempotency tier only since 2026-08-13
       (``transport.md`` § Idempotency and reconnect-with-resume: the
       per-connection cache never deduplicated a cross-connection retry; the
       durable table now does, for recorded ``ok`` replies). This harness
       stays conservative anyway: against an older nest the durable tier is
       absent, and whether a re-send is safe is then the kind's ``forbid_replay``
       flag (~40 of the ~520 registered kinds are ``true``: one-time
       resources, message delivery, imports), and that decision is ruled
       "not a per-call-site opinion" — it comes from the one serving table
       (``FederationRouter::retry_safe``, ``federation_pool.rs:217-230``),
       which this Python harness cannot read. Replaying regardless would
       double-apply a forbid-replay kind; the conservative branch of a
       decision we cannot make is the one to take.
    2. **This case is the signal.** With the idle reconnect above removing
       the heartbeat-starvation cause, a death with a request in flight means
       something else dropped an established connection — which, if it is the
       nest under load, is a product bug. Absorbing it into a silent retry is
       exactly the "brittle test → widen the timeout" move this project
       forbids.

    The message names the kind, the socket's age, and the ambiguity, so the
    failure diagnoses itself without a rerun (convention 6).
    """


class _SendNeverWentOut(Exception):
    """Internal: the send itself raised, so nothing reached the nest.

    The unambiguous half of a dead socket — ``was_in_flight: false``, which
    ``transport.md`` rules is re-sent *"regardless of ``forbid_replay``. No
    replay, no double-apply, no per-call-site opt-in."*
    """


@dataclasses.dataclass(frozen=True)
class WsReconnect:
    """One reconnect this process performed, for the run summary."""

    #: ``"idle"`` — the socket was parked past :data:`_IDLE_RECONNECT_AFTER`
    #: and was replaced before anything was sent (routine).
    #: ``"peer-closed"`` — the socket already held the nest's close frame (or
    #: EOF) before we sent, so the death is *proven* to predate the Request
    #: (routine; catches a reap or a nest restart the timer would have
    #: missed).
    #: ``"send-failed"`` — the send itself raised; nothing reached the nest,
    #: so it was re-sent on a fresh connection (notable, not fatal).
    reason: str
    kind: str
    idle_seconds: float


#: Everything that means "this socket is gone". `OSError` covers
#: `BrokenPipeError` / `ConnectionResetError` — the historical presentation of
#: this bug was a `BrokenPipeError` raised from `recv()`'s courtesy
#: `send_close()` to a peer that had already left.
#: `WebSocketTimeoutException` is a `WebSocketException` but is emphatically
#: NOT a death, so every catch site handles it first.
_DEAD_SOCKET_ERRORS = (websocket.WebSocketException, OSError)


_RECONNECTS: list[WsReconnect] = []


def reconnects() -> list[WsReconnect]:
    """Every reconnect performed this process, oldest first.

    Read by ``conftest.pytest_terminal_summary`` — a reconnect that nobody
    counts is a reconnect that can absorb a systematic nest-side drop without
    anyone noticing, which is the whole hazard this surface exists to avoid.
    """
    return list(_RECONNECTS)


def reset_reconnects() -> None:
    """Test-only: clear the tally between self-test cases.

    ⚠ **Pair it with :func:`restore_reconnects`, never leave it bare.** The
    tally is accumulated for the WHOLE run and read once, by
    ``conftest.pytest_terminal_summary`` — so a self-test that clears it mid-run
    and does not put the run's real events back does not isolate itself, it
    TRUNCATES the reconnect count, and every reconnect recorded before that
    module ran disappears from the summary. Same hazard, same wording, same
    cure as ``helpers/app_surface.py::reset_unbuilt_hits``, whose own docstring
    records the measured version: a surface
    that exists so a systematic nest-side drop cannot hide is worth nothing if
    a self-test quietly edits it.
    """
    _RECONNECTS.clear()


def restore_reconnects(events) -> None:
    """Test-only: put a snapshot from :func:`reconnects` back.

    The other half of a self-test's isolation: snapshot before, clear as needed
    during, restore after — so the module's own fake-nest reconnects never reach
    the summary and the run's real ones always do.
    """
    _RECONNECTS[:] = list(events)


@dataclasses.dataclass(frozen=True)
class PushFrame:
    """A decoded server-initiated Push frame.

    Mirrors ``libs/fauna-protocol/src/envelope.rs::Push`` — envelope keys
    ``2`` (kind), ``4`` (payload), ``8`` (seq). ``seq`` is per-connection
    ascending; a gap means nest dropped Pushes on a full send queue
    (``ws.rs`` ``dropped_pushes``), which a test may assert against.
    """

    kind: str
    payload: Any
    seq: int


class _WsRpcClientBase:
    """Synchronous WS-RPC client core — envelope framing + the Request/Reply
    round-trip — shared by every caller class.

    Subclasses implement :meth:`_connect` (the only per-class difference: the
    handshake) and assign ``self._ws``. Everything else — :meth:`call`, the
    correlation-id routing, the context-manager lifecycle, the ``http→ws``
    scheme map — lives here so the wire surface is defined exactly once.
    """

    def __init__(
        self,
        nest_base_url: str,
        reply_timeout: float = _DEFAULT_REPLY_TIMEOUT,
        http_timeout: float = _DEFAULT_HTTP_TIMEOUT,
    ):
        self._base_url = nest_base_url.rstrip("/")
        self._reply_timeout = reply_timeout
        self._http_timeout = http_timeout

        self._ws: Optional[websocket.WebSocket] = None
        # Ascending per-connection correlation_id. The dispatcher routes
        # Reply → pending caller by this; we only ever have one
        # outstanding Call so a monotonic counter is sufficient.
        self._next_corr = 0
        # Unsolicited Push frames seen while waiting for a Reply, oldest
        # first. Drained by `drain_pushes()` / `wait_for_push()`.
        self._pushes: list[PushFrame] = []
        # `time.monotonic()` of the last frame that crossed this socket in
        # either direction, or None when there is no live connection. Every
        # outbound frame re-arms the nest's liveness deadline just as an
        # inbound one proves the nest is alive, so one timestamp covers both
        # directions (`routes.rs:1702-1705` re-arms on any inbound frame).
        self._last_frame_at: Optional[float] = None

    # ── Context-manager lifecycle ─────────────────────────────────────

    def __enter__(self) -> "_WsRpcClientBase":
        self._connect()
        self._last_frame_at = time.monotonic()
        return self

    def __exit__(self, exc_type, exc_val, exc_tb) -> None:
        self.close()

    def is_open(self) -> bool:
        """Whether this client currently holds a socket.

        The public form of the `self._ws is None` check `call()` guards on, so a
        module-level client CACHE can ask before handing an instance out instead
        of discovering the answer as a `RuntimeError` three frames deep.
        """
        return self._ws is not None

    def ensure_open(self) -> "_WsRpcClientBase":
        """Re-open this client if it is not currently connected; return self.

        ⚠ A client is not closed only by `close()`. :meth:`_reconnect` is
        `close()` + `_connect()`, so a reconnect whose `_connect()` RAISES —
        the nest was mid-restart, or its port had been recycled onto a
        different nest — leaves `_ws` `None` on an object nobody ever exited.
        That is fine for a `with` block, which is unwinding anyway; it is fatal
        for the `(base_url, actor_id)` caches in `tests/api/ws_api.py` and
        `tests/api/conv_api.py`, which keep the instance and hand the same dead
        client to every later caller. Each then raises
        `WsRpcAdminClient.call() outside of \\`with client:\\` context`, a
        message that sends the reader hunting for a missing `with` that was
        never missing — measured on two linux `test_search` tests in the
        2026-09-11 sweep.

        Idempotent and free on a live client: one `is None` test.
        """
        if self._ws is None:
            self._connect()
            self._last_frame_at = time.monotonic()
        return self

    def close(self) -> None:
        """Close the WebSocket. Idempotent."""
        if self._ws is not None:
            try:
                self._ws.close()
            except Exception:
                pass
            self._ws = None
        # Push `seq` is per-connection ascending, so frames buffered on the
        # old connection must not leak into a reconnected one.
        self._pushes.clear()
        # `correlation_id` is likewise per-connection (`transport.md` § Push
        # events and `seq` numbering: "a new connection is a new counter"), so
        # a reconnected socket starts its own sequence rather than continuing
        # the dead one's.
        self._next_corr = 0
        self._last_frame_at = None

    # ── Subclass hook ─────────────────────────────────────────────────

    def _connect(self) -> None:
        """Open the WebSocket and assign ``self._ws``. Subclass-specific
        handshake (bearer vs anonymous)."""
        raise NotImplementedError

    def _ws_base(self) -> str:
        """``http(s)://nest`` → ``ws(s)://nest`` (no path). Mirrors
        ``buildWSURL`` in ``client.go`` / ``build_anon_ws_url`` in Rust."""
        base = self._base_url
        if base.startswith("https://"):
            return "wss://" + base[len("https://"):]
        if base.startswith("http://"):
            return "ws://" + base[len("http://"):]
        raise ValueError(f"unexpected nest URL scheme: {self._base_url!r}")

    def _ssl_opt(self) -> Optional[dict]:
        """`sslopt` for `websocket.create_connection` over `wss://`: skip cert
        verification when the base is `https://`. A deploy nest serves a
        self-signed/LAN cert on its loopback-published port (the
        self-signed-TLS-binding posture), which a hostname-checking client
        cannot chain — same as the Go mail bridge's loopback dial. `None` for an
        `http://` base (tier_3 plain-HTTP nests — unchanged)."""
        if self._base_url.startswith("https://"):
            return {"cert_reqs": ssl.CERT_NONE}
        return None

    def _https_context(self) -> Optional[ssl.SSLContext]:
        """Unverified `SSLContext` for urllib over an `https://` base (the
        self-signed deploy cert); `None` for `http://` (verification path
        unchanged for plain nests)."""
        if self._base_url.startswith("https://"):
            return ssl._create_unverified_context()
        return None

    # ── Generic call surface ──────────────────────────────────────────

    def call(self, kind: str, payload: Any) -> Any:
        """Send a Request and return the decoded Reply payload.

        ``payload`` is encoded with ``cbor2.dumps(payload, canonical=True)``
        — pass a dict / list / scalar matching the kind's wire schema
        (string-keyed CBOR map per spec § 2). Bytes inside the payload
        are encoded as CBOR byte strings.

        On ``ok=true`` the decoded reply payload is returned (whatever
        ``cbor2.loads`` produced — typically a ``dict``). On ``ok=false``
        the reply is parsed as an ``RpcError`` and surfaced as
        ``RpcCallError`` with ``code`` / ``message`` / ``details``.

        Idempotency: a fresh 16-byte ``idempotency_key`` is generated
        per call. Replay-forbidden hints (envelope key 5) and explicit
        deadlines (key 6) are omitted — nest applies its registered
        per-kind defaults.

        Liveness (see the module docstring for why this is here at all):

        * A socket parked longer than :data:`_IDLE_RECONNECT_AFTER`, or one
          already holding the nest's close frame, is replaced **before** the
          Request goes out. Nothing was in flight, so this is invisible to
          the caller beyond a counted tally entry.
        * If the send itself raises, nothing reached the nest
          (``was_in_flight: false``) — reconnect once and send again, which
          ``transport.md`` rules is safe for every kind.
        * If the send succeeded and the link then died, the Request may have
          run: that raises :class:`WsLinkDied` rather than replaying. The
          reasoning, and why the ``idempotency_key`` does *not* make a replay
          safe, is on :class:`WsLinkDied`.

        A harness write of a shared identity's recipient seal key is refused
        here, before anything is sent (``helpers/shared_identity.py``).
        """
        from helpers.shared_identity import refuse_seal_key_write_on_shared_identity

        refuse_seal_key_write_on_shared_identity(kind, payload)
        if self._ws is None:
            raise RuntimeError(
                f"{type(self).__name__}.call() has no open socket. Either it "
                "was used outside a `with client:` block, or — the case that "
                "actually bites, because it looks identical — a `_reconnect()` "
                "closed the old socket and then failed to open a new one "
                "(nest restarting, port recycled), leaving this instance dead "
                "for good. A cache that keeps clients across tests must call "
                "`ensure_open()` before handing one out."
            )

        self._reconnect_if_dead_or_idle(kind)

        # Minted once and reused across the send-failed re-send below, so the
        # nest sees one logical request rather than two — the envelope field
        # the protocol put there for exactly this (`transport.md`
        # § Idempotency and reconnect-with-resume).
        idempotency_key = secrets.token_bytes(16)
        try:
            return self._attempt(kind, payload, idempotency_key)
        except _SendNeverWentOut as exc:
            idle = self._idle_seconds()
            _RECONNECTS.append(
                WsReconnect(reason="send-failed", kind=kind, idle_seconds=idle)
            )
            self._reconnect()
            try:
                return self._attempt(kind, payload, idempotency_key)
            except _SendNeverWentOut as second:
                raise WsLinkDied(
                    f"WS-RPC call(kind={kind!r}) could not send on a freshly "
                    f"opened connection either (first send failed after "
                    f"{idle:.1f}s idle: {exc!r}). The nest is not accepting "
                    f"this actor's frames — this is not the harness parking a "
                    f"socket, look at the nest."
                ) from second

    def call_with_key(self, kind: str, payload: Any, idempotency_key: bytes) -> Any:
        """:meth:`call`, but with a CALLER-supplied 16-byte idempotency key —
        the python twin of the Rust client's explicit-replay path
        (``request_with_key``, `transport.md` § Idempotency and
        reconnect-with-resume). For tests that must present the SAME key again
        (possibly on a fresh connection) to exercise replay semantics — e.g.
        the nest's durable idempotency tier, whose whole point is that a
        replayed key returns the original Reply instead of re-running the
        handler. Same liveness handling as :meth:`call`'s send-failed arm."""
        from helpers.shared_identity import refuse_seal_key_write_on_shared_identity

        refuse_seal_key_write_on_shared_identity(kind, payload)
        if self._ws is None:
            raise RuntimeError(
                f"{type(self).__name__}.call_with_key() has no open socket — "
                "see `call()`'s twin of this message and `ensure_open()`."
            )
        if len(idempotency_key) != 16:
            raise ValueError("idempotency_key must be exactly 16 bytes")
        self._reconnect_if_dead_or_idle(kind)
        try:
            return self._attempt(kind, payload, idempotency_key)
        except _SendNeverWentOut as exc:
            _RECONNECTS.append(
                WsReconnect(
                    reason="send-failed", kind=kind, idle_seconds=self._idle_seconds()
                )
            )
            self._reconnect()
            try:
                return self._attempt(kind, payload, idempotency_key)
            except _SendNeverWentOut as second:
                raise WsLinkDied(
                    f"WS-RPC call_with_key(kind={kind!r}) could not send on a "
                    f"freshly opened connection either (first send: {exc!r})."
                ) from second

    def _attempt(self, kind: str, payload: Any, idempotency_key: bytes) -> Any:
        """One send + reply-wait on the current socket.

        Raises :class:`_SendNeverWentOut` when the send raised (nothing
        reached the nest), :class:`WsLinkDied` when the link died *after* the
        Request went out, ``TimeoutError`` when the reply budget expires on a
        live socket, and ``RpcCallError`` on an ``ok=false`` reply.
        """
        assert self._ws is not None  # callers check; keeps the type narrow

        self._next_corr += 1
        corr = self._next_corr
        # Envelope-level keys are integers 0..8 which encode as one byte
        # each in CBOR. `canonical=True` sorts them by length-then-lex
        # but for 0..8 that's already source-order, so byte-parity holds
        # with the Go encoder (see envelope.go header doc). The *inner*
        # payload is the kind-specific body, already a CBOR value.
        request_frame = cbor2.dumps(
            {
                0: _FRAME_REQUEST,
                1: corr,
                2: kind,
                3: idempotency_key,
                4: payload,  # cbor2 encodes nested dicts/bytes as CBOR
            },
            canonical=True,
        )

        idle_before_send = self._idle_seconds()
        try:
            self._ws.send_binary(request_frame)
        except _DEAD_SOCKET_ERRORS as exc:
            raise _SendNeverWentOut(repr(exc)) from exc
        self._last_frame_at = time.monotonic()

        # The dispatcher replies asynchronously but with the *same*
        # correlation_id we sent. Our single-in-flight model means the
        # next Reply *must* match — but assert anyway so a future
        # pipelined variant fails loudly rather than misroute.
        deadline = time.monotonic() + self._reply_timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(
                    f"WS-RPC call(kind={kind!r}, corr={corr}) timed out after "
                    f"{self._reply_timeout}s without a Reply"
                )
            self._ws.settimeout(remaining)
            try:
                reply_bytes = self._ws.recv()
            except websocket.WebSocketTimeoutException as exc:
                # The reply budget, not a death — surface it as the
                # informative TimeoutError above rather than the library's
                # bare one, which names neither the kind nor the budget.
                raise TimeoutError(
                    f"WS-RPC call(kind={kind!r}, corr={corr}) timed out after "
                    f"{self._reply_timeout}s without a Reply"
                ) from exc
            except _DEAD_SOCKET_ERRORS as exc:
                raise self._link_died(kind, corr, idle_before_send, exc) from exc
            if reply_bytes is None or reply_bytes == "":
                # `websocket-client` reports some closes by returning an empty
                # string rather than raising. Treating that as "not a binary
                # frame, keep waiting" is how a dead socket used to burn the
                # full reply budget and then blame a timeout.
                raise self._link_died(kind, corr, idle_before_send, None)
            if not isinstance(reply_bytes, (bytes, bytearray)):
                # Text frames are a Spec-Y violation server-side; ignore
                # defensively so a stray non-binary frame doesn't crash
                # the loop.
                continue
            self._last_frame_at = time.monotonic()
            frame = cbor2.loads(reply_bytes)
            if not isinstance(frame, dict):
                raise RuntimeError(
                    f"WS-RPC reply not a CBOR map: {type(frame).__name__}"
                )
            frame_type = frame.get(0)
            if frame_type == _FRAME_PUSH:
                # Buffer, don't drop: nest emits Pushes (import progress,
                # mailbox state, …) on the same socket, often *before* the
                # Reply to the very call that triggered them.
                self._pushes.append(_decode_push(frame))
                continue
            if frame_type != _FRAME_REPLY:
                raise RuntimeError(
                    f"WS-RPC unexpected frame type {frame_type!r} "
                    f"(expected Reply={_FRAME_REPLY})"
                )
            if frame.get(1) != corr:
                # Stale Reply for a previous call (shouldn't happen with
                # single-in-flight, but defend the invariant). Drop and
                # keep waiting.
                continue
            ok = bool(frame.get(7))
            reply_payload = frame.get(4)
            if ok:
                # On live, an account this call created is noted for the
                # teardown reap here, where every harness dial passes, rather
                # than at each creating call site (`helpers/live_accounts.py`).
                from helpers.live_accounts import observe

                observe(kind, payload)
                return reply_payload
            raise _build_rpc_call_error(reply_payload)

    # ── Liveness plumbing ─────────────────────────────────────────────

    def _idle_seconds(self) -> float:
        """Seconds since a frame last crossed this socket (0.0 if unknown)."""
        if self._last_frame_at is None:
            return 0.0
        return max(0.0, time.monotonic() - self._last_frame_at)

    def _reconnect(self) -> None:
        """Replace the socket with a fresh one. Subclass handshake reused."""
        self.close()
        self._connect()
        self._last_frame_at = time.monotonic()

    def _peer_closed(self) -> bool:
        """Has the nest already closed this socket, right now?

        The evidence half of the pre-send check, and the reason the fix does
        not rest on :data:`_IDLE_RECONNECT_AFTER` being tuned correctly. A
        connection the nest reaped — or one whose nest was restarted between
        two tests, which no idle timer can predict — is sitting there holding
        a Close frame or an EOF. Reading that *before* sending turns an
        ambiguous mid-flight death into the unambiguous ``was_in_flight:
        false`` case, which is re-sendable for every kind.

        Costs nothing on a healthy socket: ``select`` with a zero timeout,
        and the peek only runs when something is already pending. It consumes
        nothing — ``MSG_PEEK`` leaves real frames in the buffer for the reply
        loop — and it walks every complete frame it peeked, so a Close (0x8)
        is found even when it is queued BEHIND the ordinary Pushes (0x2) that
        pend here all the time. Reading only the first byte's opcode answered
        "open" for a Push-then-Close buffer, and the Request then went out on
        a socket the nest had already closed.

        Conservative by construction: every uncertainty answers "not closed"
        and lets the ordinary path run, so this can only ever *add* a safe
        reconnect, never mask a real failure.
        """
        sock = getattr(self._ws, "sock", None)
        if sock is None:
            return False
        if isinstance(sock, ssl.SSLSocket):
            # `SSLSocket.recv` rejects non-zero flags, so `MSG_PEEK` is not
            # available; and TLS can hold decrypted bytes that `select` cannot
            # see. The idle floor still covers `wss://` nests.
            return False
        try:
            readable, _, errored = select.select([sock], [], [sock], 0)
        except (OSError, ValueError):
            return False
        if errored:
            return True
        if not readable:
            return False
        try:
            peeked = sock.recv(self._PEEK_BYTES, socket.MSG_PEEK)
        except BlockingIOError:
            return False
        except OSError:
            return True
        if peeked == b"":
            return True  # EOF: the peer is gone
        return self._buffered_frames_hold_a_close(peeked)

    #: How far ahead :meth:`_peer_closed` peeks. A Close sits behind whatever
    #: the nest sent before it, so one frame's worth is not enough; ``MSG_PEEK``
    #: copies without consuming, so a whole buffer's worth costs nothing.
    _PEEK_BYTES = 65536

    @staticmethod
    def _buffered_frames_hold_a_close(buf: bytes) -> bool:
        """Walk the complete server→client frames at the head of ``buf``.

        True as soon as one is a Close. A frame the peek cut short ends the
        walk with False: a Close beyond it cannot be read, and an unproven
        close answers "not closed" like every other uncertainty here.
        """
        i, n = 0, len(buf)
        while i + 2 <= n:
            if buf[i] & 0x0F == 0x8:
                return True
            length = buf[i + 1] & 0x7F
            header = 2
            if length == 126:
                if i + 4 > n:
                    return False
                length = int.from_bytes(buf[i + 2 : i + 4], "big")
                header = 4
            elif length == 127:
                if i + 10 > n:
                    return False
                length = int.from_bytes(buf[i + 2 : i + 10], "big")
                header = 10
            if buf[i + 1] & 0x80:
                header += 4  # a masking key (never sent server→client, but skip it)
            i += header + length
        return False

    def _reconnect_if_dead_or_idle(self, kind: str) -> None:
        """Replace a socket that is parked, or already closed, before sending.

        Called before the Request goes out, so nothing is ever in flight
        across the swap. See the module docstring for why an idle socket here
        is a socket the nest has already reaped (or is about to).
        """
        idle = self._idle_seconds()
        if self._last_frame_at is None:
            return
        if idle >= _IDLE_RECONNECT_AFTER:
            reason = "idle"
        elif self._peer_closed():
            reason = "peer-closed"
        else:
            return
        _RECONNECTS.append(
            WsReconnect(reason=reason, kind=kind, idle_seconds=idle)
        )
        self._reconnect()

    def _link_died(
        self, kind: str, corr: int, idle_before_send: float, exc: Optional[BaseException]
    ) -> WsLinkDied:
        """Build the in-flight-death error, spelling out what it is not."""
        cause = repr(exc) if exc is not None else "the peer closed the stream"
        return WsLinkDied(
            f"WS-RPC call(kind={kind!r}, corr={corr}) lost the link with the "
            f"Request already sent: {cause}. The socket had been idle "
            f"{idle_before_send:.1f}s before the send, which is under the "
            f"{_IDLE_RECONNECT_AFTER}s reconnect floor, so this is NOT the "
            f"harness parking a connection past the nest's "
            f"{_NEST_LIVENESS_TIMEOUT}s liveness window. Something dropped an "
            f"established connection; if it was the nest under load, that is a "
            f"product bug. Not replayed on purpose: the nest's idempotency "
            f"cache is per-connection and cannot deduplicate a re-send on a "
            f"fresh socket (transport.md § Idempotency and "
            f"reconnect-with-resume)."
        )

    # ── Unsolicited Push surface ──────────────────────────────────────

    def drain_pushes(self, kind: Optional[str] = None) -> list[PushFrame]:
        """Remove and return buffered Pushes, oldest first.

        Only Pushes already read off the socket by a prior :meth:`call` are
        visible — this never blocks. To wait for one that has not arrived
        yet, use :meth:`wait_for_push`. ``kind`` filters by wire kind
        (e.g. ``"fauna.bridges.push.import_progress"``); non-matching
        frames stay buffered.
        """
        if kind is None:
            drained, self._pushes = self._pushes, []
            return drained
        drained = [p for p in self._pushes if p.kind == kind]
        self._pushes = [p for p in self._pushes if p.kind != kind]
        return drained

    def wait_for_push(
        self,
        kind: Optional[str] = None,
        timeout: float = 5.0,
        predicate: Optional[Callable[[PushFrame], bool]] = None,
    ) -> PushFrame:
        """Return the first buffered-or-incoming Push matching ``kind``.

        Checks the buffer first, then reads frames off the socket until the
        deadline. Non-matching Pushes stay buffered (so a later call can
        still see them); Replies for already-returned calls are dropped.

        Raises ``TimeoutError`` naming every Push kind seen, so a failure
        diagnoses itself without a screenshot or a rerun.

        Deliberately does **not** reconnect an idle socket the way
        :meth:`call` does: Push buffers and ``seq`` are per-connection
        (``transport.md`` § Push events and `seq` numbering), so silently
        swapping the socket here would drop the very frames the caller is
        waiting for and turn a real absence into a timeout. Callers reach
        this immediately after a :meth:`call` that triggered the Push, which
        is exactly when the connection is freshest.
        """
        if self._ws is None:
            raise RuntimeError(
                f"{type(self).__name__}.wait_for_push() outside of "
                "`with client:` context — open the connection first"
            )

        def matches(p: PushFrame) -> bool:
            return (kind is None or p.kind == kind) and (
                predicate is None or predicate(p)
            )

        for i, buffered in enumerate(self._pushes):
            if matches(buffered):
                return self._pushes.pop(i)

        seen: list[str] = [p.kind for p in self._pushes]
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            self._ws.settimeout(remaining)
            try:
                frame_bytes = self._ws.recv()
            except websocket.WebSocketTimeoutException:
                break
            if not isinstance(frame_bytes, (bytes, bytearray)):
                continue
            self._last_frame_at = time.monotonic()
            frame = cbor2.loads(frame_bytes)
            if not isinstance(frame, dict) or frame.get(0) != _FRAME_PUSH:
                # A stale Reply (or a non-map frame) — not our business here.
                continue
            push = _decode_push(frame)
            seen.append(push.kind)
            if matches(push):
                return push
            self._pushes.append(push)

        raise TimeoutError(
            f"no Push matching kind={kind!r} within {timeout}s; "
            f"Push kinds seen on this connection: {seen or '(none)'}"
        )


def _decode_push(frame: dict) -> PushFrame:
    """Decode a type-2 envelope map into a :class:`PushFrame`."""
    return PushFrame(
        kind=str(frame.get(2, "")),
        payload=frame.get(4),
        seq=int(frame.get(8, 0)),
    )


def _build_rpc_call_error(reply_payload: Any) -> RpcCallError:
    """Parse an ``ok=false`` reply payload into an ``RpcCallError``.

    Wire shape per ``libs/fauna-protocol/src/error.rs::RpcError``::

        {"code": str, "message": {"key": str, "args": map}, "details"?: any}

    Robust to a payload that isn't a dict (server malfunction) — surfaces
    the raw payload as ``details`` with a synthetic ``code`` so tests can
    still see what came back.
    """
    if not isinstance(reply_payload, dict):
        return RpcCallError(
            code="fauna.client.malformed_error_payload",
            message_key="error.client.malformed_error_payload",
            message_args={},
            details=reply_payload,
        )
    code = reply_payload.get("code", "")
    message = reply_payload.get("message") or {}
    if isinstance(message, dict):
        message_key = message.get("key", "")
        message_args_raw = message.get("args") or {}
        if isinstance(message_args_raw, dict):
            message_args = {
                str(k): str(v) for k, v in message_args_raw.items()
            }
        else:
            message_args = {}
    else:
        message_key = ""
        message_args = {}
    details = reply_payload.get("details")
    return RpcCallError(
        code=str(code),
        message_key=str(message_key),
        message_args=message_args,
        details=details,
    )
