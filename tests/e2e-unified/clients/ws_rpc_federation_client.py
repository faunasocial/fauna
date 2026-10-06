"""Python WS-RPC client for the **nest↔nest federation channel**.

The peer-symmetric sibling of the actor-facing :class:`WsRpcAnonClient` /
:class:`WsRpcAdminClient`. It drives the long-lived ``fauna.federation.*`` kinds a
*verified peer nest* may invoke over the federation channel — the cross-nest
transport that replaced the deleted HTTP federation twins (``/api/v1/nest-sync/*``,
``/api/v1/forward``, ``/api/v1/welcome/*``, ``/api/v1/keypackage/*``) in Spec Y2
slice 5.

Unlike the actor clients, the federation channel carries **no bearer**: the
connection authenticates the peer once, at handshake time, via an L3
``fauna.federation.hello`` Request sent as the *first* frame (``federation.md``
§ Transport / § Peer-auth model;
``bins/fauna-nest/src/federation_channel.rs::serve_listener``). After the hello
verifies, every subsequent ``fauna.federation.<kind>`` call is attributed to the
verified peer ``nest_id`` with no per-request signature — so the request structs
drop the ``nest_id`` / ``envelope`` fields the HTTP twins carried in their bodies
(``bins/fauna-nest/src/federation_handlers.rs``).

Wire reference (defer to these; do not duplicate the contract):
* Channel + handshake — ``bins/fauna-nest/src/federation_channel.rs``
  (``FederationHello`` / ``FederationHelloReply`` / ``FederationHelloSig``,
  ``HELLO_KIND``, ``FEDERATION_SUBPROTOCOL``, ``serve_listener``).
* Serving handlers / kind allowlist — ``bins/fauna-nest/src/federation_handlers.rs``.
* Handshake signature primitive — ``bins/fauna-nest/src/federation_sig.rs``
  (``sign_payload`` = canonical dag-cbor → 36-byte CID → Ed25519; byte-identical
  to ``common.envelope.sign_dagcbor_envelope`` / ``common.helpers.sign_as_nest``).

Roles: the **initiator** is the nest that dials the channel (it signs the hello
with its identity key); the **listener** is the nest serving
``/api/v1/federation/ws`` (the one whose DB the kinds read/write). Pass both
``start_nest`` result dicts — the initiator (for its ``nest_id`` + signing key)
and the target/listener (whose ``url`` we dial and whose ``nest_id`` the hello is
addressed to).

Usage::

    from clients.ws_rpc_federation_client import FederationChannelClient

    with FederationChannelClient(initiator=private_nest, target=public_nest) as fed:
        reply = fed.call("fauna.federation.sync.pull", {
            "actor_id": actor_hex, "namespace": ns_hex, "since": 0,
        })
        assert reply["entries"] == []

Threading model: synchronous, one in-flight Request at a time (inherited from the
shared base). Not safe to share across threads.
"""

from __future__ import annotations

import secrets

import websocket  # type: ignore[import-untyped]  # from `websocket-client`

from helpers.tls_spki import observed_spki_sha256_hex

from ._ws_rpc_core import (  # noqa: F401  (RpcCallError re-exported for symmetry)
    RpcCallError,
    _WsRpcClientBase,
)

# Match `federation_channel.rs`: the only WS subprotocol the listener accepts on
# `/api/v1/federation/ws`, and the L3 kind carried by the first (handshake) frame.
_FEDERATION_SUBPROTOCOL = "fauna.federation.v1"
_HELLO_KIND = "fauna.federation.hello"


class FederationChannelClient(_WsRpcClientBase):
    """Synchronous nest↔nest federation-channel WS-RPC client.

    Opens ``/api/v1/federation/ws`` on the *target* (listener) nest under the
    ``fauna.federation.v1`` subprotocol, runs the ``fauna.federation.hello``
    handshake as the first frame (signing the bound tuple with the *initiator*
    nest's identity key), then exposes the inherited :meth:`call` for the
    ``fauna.federation.*`` kinds.

    Args:
        initiator: the ``start_nest`` result dict of the dialing nest. Supplies
            ``nest_id`` (hex) and the ``nest_deployment.key`` under ``tmp_dir`` the
            hello is signed with. On the listener side, the kinds attribute every
            request to this nest's verified ``nest_id`` (e.g. the ``is_paired``
            gate keys on it).
        target: the ``start_nest`` result dict of the listener nest — its ``url``
            is dialed and its ``nest_id`` is the hello's ``listener_nest_id``.
        reply_timeout: per-call ``ws.recv()`` budget. Defaults to 20s.
        http_timeout: WS connect-handshake budget. Defaults to 5s.
    """

    def __init__(
        self,
        initiator: dict,
        target: dict,
        reply_timeout: float | None = None,
        http_timeout: float | None = None,
    ):
        kwargs = {}
        if reply_timeout is not None:
            kwargs["reply_timeout"] = reply_timeout
        if http_timeout is not None:
            kwargs["http_timeout"] = http_timeout
        super().__init__(target["url"], **kwargs)
        self._initiator = initiator
        self._target = target
        #: The channel binding this client last signed — the SHA-256 hex of the
        #: SubjectPublicKeyInfo of the leaf the *listener* served on the open
        #: connection, or ``""`` for a plain-HTTP peer. Assigned by
        #: :meth:`_connect` (and re-assigned on every reconnect, so a rotated
        #: cert is re-observed rather than remembered); ``None`` before the
        #: first connect. The product's initiator surfaces the same value out
        #: of ``connect_federation_ws``.
        self.observed_spki_sha256 = None

    def _connect(self) -> None:
        """Open the federation WebSocket and run the hello handshake.

        Sends the signed ``fauna.federation.hello`` as the first Request (the
        listener rejects any other first frame, and any federation kind sent
        before the hello, ``serve_listener`` § 4.B). On success the channel is
        established and subsequent :meth:`call` invocations dispatch the
        ``fauna.federation.*`` kinds.
        """
        # `sign_as_nest` reads the initiator's identity key from disk; import it
        # lazily so this module loads even when `common` (repo-root `tests/`) is
        # not yet on `sys.path` at import time (the conftest adds it before
        # collection — mirrors `sign_as_nest`'s own lazy nacl import).
        from common.envelope import FEDERATION_HELLO_V1, verify_dagcbor_envelope
        from common.helpers import sign_as_nest

        ws_url = f"{self._ws_base()}/api/v1/federation/ws"
        self._ws = websocket.create_connection(
            ws_url,
            subprotocols=[_FEDERATION_SUBPROTOCOL],
            timeout=self._http_timeout,
            sslopt=self._ssl_opt(),
        )

        # The channel binding, observed off **this** connection's handshake —
        # never assumed, and never read from a second connection (which would
        # bind nothing: a MITM may answer two connections with two certs). This
        # mirrors the product initiator exactly: `connect_federation_ws` dials
        # `wss://` with the capturing rustls verifier and returns the leaf SPKI
        # it recorded *during that dial* beside the socket, or the empty string
        # for a loopback `ws://` peer that serves no cert. The listener compares
        # what we send against `served_cert_spki…unwrap_or_default()` before
        # parsing anything else (`verify_hello_and_build_reply`, check 1), so a
        # guess here is refused rather than silently downgraded.
        self.observed_spki_sha256 = observed_spki_sha256_hex(
            getattr(self._ws, "sock", None)
        )

        initiator_nest_id = self._initiator["nest_id"]
        listener_nest_id = self._target["nest_id"]
        channel_nonce = secrets.token_bytes(32).hex()

        # The signed tuple is `FederationHelloSig` — the listener reconstructs it
        # with *its own* nest_id, so a hello addressed to the wrong nest fails to
        # verify. The reply (`FederationHelloReply`) is `ok=true` with the
        # listener's matching nest_id; `call()` raises `RpcCallError` on rejection.
        hello_sig = {
            "initiator_nest_id": initiator_nest_id,
            "listener_nest_id": listener_nest_id,
            "channel_nonce": channel_nonce,
            "spki_sha256": self.observed_spki_sha256,
        }
        envelope = sign_as_nest(self._initiator, hello_sig)

        reply = self.call(
            _HELLO_KIND,
            {
                "initiator_nest_id": initiator_nest_id,
                "channel_nonce": channel_nonce,
                "spki_sha256": self.observed_spki_sha256,
                "envelope": envelope,
            },
        )
        got = reply.get("listener_nest_id") if isinstance(reply, dict) else None
        if got != listener_nest_id:
            raise RuntimeError(
                "federation hello reply listener_nest_id mismatch: "
                f"expected {listener_nest_id!r}, got {got!r}"
            )

        # D1, the reply half: the listener proves it holds `listener_nest_id` by
        # signing the **same** `FederationHelloSig` tuple with its own key
        # (`federation_channel.rs::verify_hello_and_build_reply` → `verify_reply`).
        # Re-deriving those bytes here with cbor2 rather than serde_ipld_dagcbor
        # makes this a *cross-implementation* pin that the nest's reply signs over
        # canonical dag-cbor under the FEDERATION_HELLO_V1 tag — the Rust-side
        # handshake test cannot prove it, because both of its ends share one
        # encoder and would drift together. Matching `verify_reply`, the tuple is
        # reconstructed with the listener id from the reply itself.
        reply_envelope = reply.get("envelope") if isinstance(reply, dict) else None
        if not isinstance(reply_envelope, str) or not verify_dagcbor_envelope(
            {**hello_sig, "listener_nest_id": got},
            reply_envelope,
            bytes.fromhex(listener_nest_id),
            domain_tag=FEDERATION_HELLO_V1,
        ):
            raise RuntimeError(
                "federation hello reply envelope failed canonical-dag-cbor "
                "sign-over-CID verification against the listener nest_id "
                f"{listener_nest_id!r} (D1; serialization.md § Sign-over-CID)"
            )
