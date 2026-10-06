"""The 2 MiB WS-RPC message cap, and what it means for mail.

`fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE` is 2 MiB, applied by nest to
**both** WS upgrade routes as `max_message_size` *and* `max_frame_size`
(`bins/fauna-nest/src/routes.rs`), and symmetrically by the native apps.
`docs/goal/architecture/transport-connection.md` § Abuse posture owns the constant and
states there is "no streaming/chunking at the protocol level".

Several nest handlers nevertheless guard payloads far above it — mailbox
migration's `MAX_BATCH_BYTES` (16 MiB) and `ENQUEUE_OUTBOUND_MAX_BYTES`
(50 MB) — and the mail bodies themselves ride *inline* on this socket
(`AppendMessageRequest.encrypted_body`, `IngestInboundMailRequest.encrypted_body`,
`ImportMessageItem.body`) while the SMTP perimeter accepts up to
`max_message_bytes` (production default 50 MiB).

Either the cap does not bind, or those guards are unreachable and mail above
~2 MiB has no route to nest. This module settles which, so the design pass
(§ the 2 MiB WS cap contradiction) starts from a
fact rather than a reading of constants.
"""

from __future__ import annotations

import pytest
import websocket  # type: ignore[import-untyped]  # from `websocket-client`

from clients._ws_rpc_core import RpcCallError, WsLinkDied
from clients.ws_rpc_anon_client import WsRpcAnonClient

pytestmark = pytest.mark.tier_3

#: What "the transport hung up" is allowed to look like out of `call()`.
#:
#: `WsLinkDied` is the client's own, *more specific* statement of exactly the
#: thing these tests assert — the link died with the Request already sent —
#: raised since the WS-RPC client learned to tell an in-flight death apart from
#: a socket it had merely parked past the nest's liveness window
#: (`clients/_ws_rpc_core.py`). The raw library errors stay in the tuple: a
#: send that blows up before the frame leaves still surfaces as one, and either
#: shape proves the same point here.
_TRANSPORT_FAILURES = (WsLinkDied, websocket.WebSocketException, OSError)

#: `fauna_core::transport::MAX_RPC_WS_MESSAGE_SIZE`.
MAX_RPC_WS_MESSAGE_SIZE = 2 * 1024 * 1024


def _claim_payload(handle_bytes: int) -> dict:
    """A well-formed `fauna.auth.claim_admin` request, padded via `handle`.

    The kind is pre-identity-allowlisted, so an anonymous connection may send
    it, and `nest_instance` is already claimed — so a *decoded* request always
    comes back as a typed `RpcCallError` and never mutates the nest. That makes
    "did nest decode this frame?" observable without side effects.
    """
    return {
        "claim_code": "0" * 8,
        "actor_id": "00" * 32,
        "signature": "00" * 64,
        "timestamp": 0,
        "handle": "a" * handle_bytes,
        "mail_domain": "example.test",
    }


def test_a_message_under_the_cap_is_decoded_and_answered(nest_instance):
    """Control: nest processes a large-but-legal frame rather than dropping it."""
    with WsRpcAnonClient(nest_instance["url"]) as anon:
        with pytest.raises(RpcCallError):
            # 64 KiB payload: comfortably under the cap. A typed error proves
            # the frame arrived, was decoded, and was dispatched.
            anon.call("fauna.auth.claim_admin", _claim_payload(64 * 1024))


def test_a_message_above_the_cap_never_reaches_the_dispatcher(nest_instance):
    """A >2 MiB WS-RPC message is refused at the transport, not the handler.

    If this raised `RpcCallError`, the cap would not bind and the 16 MiB /
    50 MiB handler guards would be live — good news for mail.

    The assertions are deliberately paranoid: a client-side blow-up (cbor2,
    MemoryError, a socket the test itself broke) would otherwise make this pass
    *vacuously* and let a wrong conclusion reach the goal doc. So we pin **who
    hung up** — the failure must be a websocket transport error, and the nest
    must still be serving fresh connections afterwards.
    """
    oversized = MAX_RPC_WS_MESSAGE_SIZE + 1024 * 1024  # 3 MiB of `handle`
    with WsRpcAnonClient(nest_instance["url"]) as anon:
        with pytest.raises(Exception) as excinfo:  # noqa: PT011 — see asserts below
            anon.call("fauna.auth.claim_admin", _claim_payload(oversized))

    err = excinfo.value
    assert not isinstance(err, RpcCallError), (
        "nest DECODED a WS-RPC message above MAX_RPC_WS_MESSAGE_SIZE — the cap "
        "does not bind, and this module's premise is wrong"
    )
    assert isinstance(err, _TRANSPORT_FAILURES), (
        f"expected the WS transport to fail, got {type(err).__name__}: {err!r} — "
        "this test would otherwise pass vacuously on a client-side bug"
    )

    # The nest killed that *connection*, not itself: a fresh anonymous client
    # still gets a decoded, dispatched reply. Without this, a crashed nest would
    # look identical to an enforced cap.
    with WsRpcAnonClient(nest_instance["url"]) as anon:
        with pytest.raises(RpcCallError):
            anon.call("fauna.auth.claim_admin", _claim_payload(1024))


def test_the_cap_is_what_bounds_an_inline_mail_body(nest_instance):
    """Locate the boundary: a body that fits vs. one that a 16 MiB batch allows.

    `ImportMessageItem.body` and the two bridge ingest kinds carry the raw
    message inline on this socket. So the largest importable/deliverable message
    is bounded by the WS cap, NOT by `MAX_BATCH_BYTES` (16 MiB) or the mail
    policy's `max_message_bytes` (50 MiB). Pinning both sides of the boundary is
    what makes that concrete for the design pass.
    """
    url = nest_instance["url"]

    # 1 MiB body-sized payload: crosses the wire.
    with WsRpcAnonClient(url) as anon:
        with pytest.raises(RpcCallError):
            anon.call("fauna.auth.claim_admin", _claim_payload(1024 * 1024))

    # A payload the size of a *single* legal 16 MiB batch member: cannot.
    with WsRpcAnonClient(url) as anon:
        with pytest.raises(Exception) as excinfo:  # noqa: PT011
            anon.call("fauna.auth.claim_admin", _claim_payload(16 * 1024 * 1024))
    assert not isinstance(excinfo.value, RpcCallError), (
        "a 16 MiB WS-RPC message was decoded — MAX_BATCH_BYTES is reachable "
        "after all"
    )
    assert isinstance(excinfo.value, _TRANSPORT_FAILURES), (
        f"expected a WS transport failure, got {type(excinfo.value).__name__}"
    )
