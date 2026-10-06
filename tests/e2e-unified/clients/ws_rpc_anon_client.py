"""Python WS-RPC client for the anonymous (pre-identity) caller class.

The bearer-less sibling of :class:`clients.ws_rpc_admin_client.WsRpcAdminClient`.
It opens the anonymous WebSocket — ``GET /api/v1/ws`` with a bare
``Sec-WebSocket-Protocol: fauna.v1`` (no ``bearer.<token>`` element, no
``/{actor_id}`` path segment) — and drives the fixed pre-identity allowlist of
onboarding kinds (``fauna.setup.status``, ``fauna.auth.claim_admin``,
``fauna.account.invite_request.{submit,status,cancel}``,
``fauna.account.invite_code.verify``, ``fauna.account.register``,
``fauna.setup.nat_mode``). Any off-allowlist kind is answered with
``fauna.protocol.unauthenticated`` and the connection is kept open.

This is the test-side analogue of the Rust ``libs/fauna-anon-client``
(``AnonymousNestClient``) and the wasm ``fauna_rpc_wasm::AnonymousWsRpcClient``
— the connectors the onboarding/launch state machines ride. It exists so the
tier_3 Python e2e suite can drive claim-admin / invite-request / storage-mode
*before a bearer exists*, the calls that the bearer-only
:class:`WsRpcAdminClient` cannot reach (its ``_acquire_bearer`` needs an
already-claimed admin).

Wire reference (defer to these; do not duplicate the contract):
* Anonymous endpoint — ``bins/fauna-nest/src/routes.rs::ws_anonymous_handler``.
* Allowlist — ``bins/fauna-nest/src/pre_identity_allowlist.rs``.
* Handshake shape — ``docs/goal/architecture/transport.md`` § Pre-identity
  (anonymous) connection; the Rust handshake is
  ``libs/fauna-anon-client`` ``build_anon_ws_url`` / ``connect_anonymous``.

Usage::

    from clients.ws_rpc_anon_client import WsRpcAnonClient

    with WsRpcAnonClient(nest_instance["url"]) as anon:
        status = anon.call("fauna.setup.status", {})
        assert status["claimed"] in (True, False)

Threading model: synchronous, one in-flight Request at a time (inherited from
the shared base). Not safe to share across threads.
"""

from __future__ import annotations

import websocket  # type: ignore[import-untyped]  # from `websocket-client`

from ._ws_rpc_core import (  # noqa: F401  (RpcCallError re-exported for symmetry)
    RpcCallError,
    _WsRpcClientBase,
)


class WsRpcAnonClient(_WsRpcClientBase):
    """Synchronous anonymous (pre-identity) WS-RPC client.

    Carries no actor identity and performs no HTTP challenge/verify — the
    anonymous connection authenticates per-request from the *signed payload*
    of the kinds that need it (e.g. ``fauna.auth.claim_admin`` /
    ``fauna.setup.nat_mode`` sign with the admin's Ed25519 key inside the
    body), exactly as the deprecated HTTP twins did. The connection itself is
    unauthenticated.

    Args:
        nest_base_url: e.g. ``"http://127.0.0.1:13030"`` (no trailing slash,
            no path). ``http(s)://`` → ``ws(s)://`` per the shared base.
        reply_timeout: per-call ``ws.recv()`` budget. Defaults to 20s.
        http_timeout: WS connect-handshake budget. Defaults to 5s.
    """

    def _connect(self) -> None:
        """Open the bare-``fauna.v1`` anonymous WebSocket (no bearer)."""
        ws_url = self._anon_ws_url()
        # Single subprotocol value `fauna.v1` with NO `bearer.<token>` element
        # → `routes.rs::ws_anonymous_handler` accepts; the authenticated
        # `/api/v1/ws/{actor_id}` would 401 a bearer-less upgrade.
        self._ws = websocket.create_connection(
            ws_url,
            subprotocols=["fauna.v1"],
            timeout=self._http_timeout,
            sslopt=self._ssl_opt(),
        )

    def _anon_ws_url(self) -> str:
        """``http(s)://nest/`` → ``ws(s)://nest/api/v1/ws`` (no actor segment)."""
        return f"{self._ws_base()}/api/v1/ws"
