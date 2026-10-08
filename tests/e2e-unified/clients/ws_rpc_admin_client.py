"""Python WS-RPC client for an authenticated (bearer) caller class.

This is the test-side analogue of
``bins/fauna-bridges/internal/wsrpc/client.go`` (Go) and
``libs/fauna-client/src/ws_adapter.rs`` (Rust). It exists so e2e tests
can drive nest's Admin-class WS-RPC handlers — the ``provision_*_blob``
kinds, plus the put_<substruct>_policy / provision_recipient_mls_pubkey
kinds — over the same canonical wire surface production
uses.

The envelope/framing/``call()`` plumbing lives in the shared
``clients._ws_rpc_core`` base (one canonical wire surface for every caller
class); this module adds only the authenticated handshake (silent-challenge
over the anonymous WS → bearer subprotocol → ``/api/v1/ws/<actor_hex>``) and the
Admin-only typed convenience methods. The pre-identity sibling that needs no
bearer — and which this client borrows for its ``fauna.auth.challenge`` /
``fauna.auth.verify`` bearer mint — is
:class:`clients.ws_rpc_anon_client.WsRpcAnonClient`.

Wire reference (do not duplicate the contract here; defer to these):
* Envelope shapes — ``bins/fauna-bridges/internal/wsrpc/envelope.go``
* Server-side envelope/dispatch — ``libs/fauna-protocol/src/envelope.rs``
  and ``bins/fauna-nest/src/routes.rs::dispatch_request``.
* Auth flow — ``bins/fauna-bridges/internal/wsrpc/auth.go`` and its
  Rust counterparts ``bins/fauna-nest/src/auth_handlers.rs`` +
  ``auth_core.rs`` (the `fauna.auth.*` kinds; the HTTP twins were deleted).

This file deliberately replaces the original proposal of
"HTTP admin twin routes". The Admin caller class is reached over the
same WS-RPC surface as every other class — one canonical wire surface,
no duplicate ``/api/admin/...`` routes, no deprecation debt later
(rationale tracked internally).

Usage::

    from clients.ws_rpc_admin_client import WsRpcAdminClient

    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )
    with client:
        client.provision_tls_cert_blob(sealed_bytes)            # typed
        client.provision_wrapped_submission_token(sealed_bytes) # typed
        reply = client.call("fauna.bridges.put_spam_policy", {...})  # generic

The ``call()`` method (inherited from the shared base) is the extension
hook for any kind the typed methods don't cover. It returns the decoded
reply payload on ``ok=true`` and raises ``RpcCallError`` carrying ``code``
/ ``message`` / ``details`` on ``ok=false``.

Threading model: synchronous. One in-flight Request at a time per
client instance (the test code path doesn't need pipelining; the Go
bridge does, the Python tests don't). The instance is not safe to share
across threads.
"""

from __future__ import annotations

import dataclasses
import time
from typing import Optional

import websocket  # type: ignore[import-untyped]  # from `websocket-client`
from nacl.signing import SigningKey

# Re-exported for backward compatibility: many tests do
# `from clients.ws_rpc_admin_client import RpcCallError` / `_build_rpc_call_error`.
from ._ws_rpc_core import (  # noqa: F401
    _DEFAULT_HTTP_TIMEOUT,
    _DEFAULT_REPLY_TIMEOUT,
    PushFrame,
    RpcCallError,
    _build_rpc_call_error,
    _WsRpcClientBase,
)


@dataclasses.dataclass(frozen=True)
class _BearerToken:
    token: str
    expires_at: float  # unix seconds, monotonic-clock-agnostic
    #: The nest's short session id for this mint (`VerifyReply.token_id`), so a
    #: test reading `fauna.sessions.list` can subtract the harness's own row.
    token_id: str = ""


class WsRpcAdminClient(_WsRpcClientBase):
    """Synchronous WS-RPC client driven by a single Ed25519 actor identity.

    The class is not Admin-specific in its plumbing — every nest WS-RPC
    caller class shares the same envelope/auth flow. It carries the
    Admin name only because an internal investigation
    spawned it for the Admin-only ``provision_*`` kinds, and the typed
    convenience methods (`provision_tls_cert_blob`, …) only target
    Admin kinds. The generic ``.call(kind, payload)`` surface works for
    any caller class — pass a User-class actor's keypair and you have a
    User-class client.

    Args:
        nest_base_url: e.g. ``"http://127.0.0.1:13030"`` (no trailing
            slash, no path). The ``http(s)://`` → ``ws(s)://`` mapping
            mirrors ``buildWSURL`` in ``client.go``.
        actor_id: 32-byte Ed25519 public key. The bearer is bound to
            this actor by nest; the WS URL contains it as hex.
        signing_key: Ed25519 private key. Accepts either:
            * 32-byte seed (PyNaCl ``bytes(SigningKey(...))`` format), or
            * 64-byte expanded private key (rare; we only need the seed
              for ed25519, but cryptography lib outputs 32-byte seeds
              and PyNaCl matches).
        reply_timeout: per-call ``ws.recv()`` budget. Defaults to 20s.
        http_timeout: per-HTTP-roundtrip budget for challenge/verify.
    """

    def __init__(
        self,
        nest_base_url: str,
        actor_id: bytes,
        signing_key: bytes,
        reply_timeout: float = _DEFAULT_REPLY_TIMEOUT,
        http_timeout: float = _DEFAULT_HTTP_TIMEOUT,
    ):
        if not isinstance(actor_id, (bytes, bytearray)) or len(actor_id) != 32:
            raise ValueError(f"actor_id must be 32 bytes, got {len(actor_id)}")
        if not isinstance(signing_key, (bytes, bytearray)) or len(signing_key) not in (32, 64):
            raise ValueError(
                f"signing_key must be 32 (seed) or 64 (expanded) bytes, "
                f"got {len(signing_key)}"
            )
        super().__init__(nest_base_url, reply_timeout, http_timeout)
        self._actor_id = bytes(actor_id)
        self._actor_id_hex = self._actor_id.hex()
        # PyNaCl's SigningKey accepts a 32-byte seed. If the caller passed
        # a 64-byte expanded form, the seed is the first 32 bytes.
        seed = signing_key[:32]
        self._signing_key = SigningKey(bytes(seed))
        self._bearer: Optional[_BearerToken] = None

    # ── Typed Admin-class convenience methods ─────────────────────────

    # `fauna.admin.users.list` clamps `limit` to 1..=500 nest-side; paging at the
    # ceiling reads a nest of up to 500 accounts in one round trip.
    _USERS_LIST_MAX_LIMIT = 500

    def users_list_all(self) -> list[dict]:
        """Every account on the nest, newest first — the Python twin of the shared
        ``fauna_client_admin::users_list_all`` every admin actor picker reads
        (``admin.md`` § 2 → *What identifies a user in an admin picker*).

        Wire kind: ``fauna.admin.users.list``, which answers one page (50 by
        default, newest first), so a caller reading only its first reply loses
        the OLDEST accounts — the box claimer first — once a nest holds more than
        a page. Pages at the nest's 500-row ceiling until ``total``, skipping a
        repeated actor id, and stops early on an empty page or on a page that
        adds no new account (a nest ignoring ``offset`` would otherwise loop).
        """
        users: list[dict] = []
        seen: set[bytes] = set()
        offset = 0
        while True:
            reply = self.call(
                "fauna.admin.users.list",
                {"limit": self._USERS_LIST_MAX_LIMIT, "offset": offset},
            )
            page = reply.get("users", [])
            fresh = 0
            for user in page:
                actor_id = bytes(user["actor_id"])
                if actor_id not in seen:
                    seen.add(actor_id)
                    users.append(user)
                    fresh += 1
            offset += len(page)
            if not page or fresh == 0 or offset >= reply.get("total", 0):
                return users

    def provision_tls_cert_blob(self, sealed_bytes: bytes) -> None:
        """Provision a wrapped TLS-cert blob — Admin only.

        Wire kind: ``fauna.bridges.provision_tls_cert_blob``.
        Body shape mirrors
        ``libs/fauna-protocol/src/wrapped_blob.rs::ProvisionTlsCertBlobRequest``
        — a string-keyed CBOR map with one key ``"blob"`` whose value is
        the canonical-CBOR-encoded ``TlsCertBlob`` ciphertext bytes.
        Per-handler size cap enforced by nest.

        Raises ``RpcCallError`` on ``ok=false``. The full seal pipeline
        that produces ``sealed_bytes`` lives in the sibling seal-helper
        (E.3.2); this client only owns the wire-side.
        """
        self.call(
            "fauna.bridges.provision_tls_cert_blob",
            {"blob": bytes(sealed_bytes)},
        )

    def force_rotate_dkim(self, domain: str) -> None:
        """Emergency DKIM rotation — flip a domain's **active** selector to its
        newest-provisioned one, then push ``config_changed`` — Admin only.

        Wire kind: ``fauna.bridges.force_rotate_dkim``. Body shape per
        ``ForceRotateDkimRequest``: a single ``"domain"`` key. Nest flips
        ``mail_domains.dkim_selector`` to the domain's newest selector (the
        nest-held keys, ordered by ``created_at``), skipping the scheduled
        24 h peer-cache wait (`mail-multidomain.md` § Rotation — "Emergency
        rotation"), then pushes ``config_changed``; the nest signs the next
        outbound ``s=<newest>`` at the hand-out, with no bridge restart.
        Nest errors ``no_dkim_selector_to_rotate`` when nothing newer exists. DKIM is
        automatic (no manual selector knob), so this is the only public surface
        that mutates the active selector.
        """
        self.call(
            "fauna.bridges.force_rotate_dkim",
            {"domain": domain},
        )

    def provision_wrapped_submission_token(
        self, sealed_bytes: bytes, credential_id: str = "default"
    ) -> None:
        """Provision a wrapped MTA submission token — Admin only.

        Wire kind: ``fauna.bridges.provision_wrapped_submission_token``.
        Body shape ``{"blob": <bytes>, "credential_id": <str>}`` per
        ``ProvisionWrappedSubmissionTokenRequest``. ``credential_id`` keys the
        per-credential row (matching the wrapped-MSEK blob + fetch/revoke sides);
        it must equal the credential the token was sealed under ("default" for the
        single-credential test fixtures). Required field — omitting it makes
        nest's strict CBOR decode reject the call as ``fauna.protocol.malformed``.
        """
        self.call(
            "fauna.bridges.provision_wrapped_submission_token",
            {"blob": bytes(sealed_bytes), "credential_id": credential_id},
        )

    # ── Authenticated handshake ───────────────────────────────────────

    def _connect(self) -> None:
        """Acquire a bearer token and open the WebSocket.

        A bearer the nest refuses at the upgrade with ``401`` is dropped,
        re-minted **once**, and the dial retried — the harness twin of every
        app's recovery (`transport-connection.md` § Connection lifecycle →
        *Upgrade-time auth rejection*). The nest's token store is in memory,
        so this is what a nest restart does to a cached client. A fresh bearer
        refused again is a real refusal and raises; so does any other status,
        because a new bearer for the same actor cannot fix a ``403`` binding
        refusal.
        """
        try:
            self._ws = self._dial(self._acquire_bearer())
        except websocket.WebSocketBadStatusException as refused:
            if refused.status_code != 401:
                raise
            self._bearer = None
            self._ws = self._dial(self._acquire_bearer())

    def _dial(self, token: _BearerToken):
        """Open ``/api/v1/ws/<actor_hex>`` presenting ``token``."""
        # Per `routes.rs::parse_subprotocol`, nest accepts both the
        # comma-joined and the two-value Sec-WebSocket-Protocol header
        # forms. websocket-client emits one header value per subprotocol,
        # which lands in the second branch (matched by the regression
        # comment "websocket-client/Python" on line 736 of routes.rs).
        return websocket.create_connection(
            self._build_ws_url(),
            subprotocols=["fauna.v1", f"bearer.{token.token}"],
            timeout=self._http_timeout,
            sslopt=self._ssl_opt(),
        )

    def _build_ws_url(self) -> str:
        """``http(s)://nest/`` → ``ws(s)://nest/api/v1/ws/<actor_hex>``."""
        return f"{self._ws_base()}/api/v1/ws/{self._actor_id_hex}"

    def _acquire_bearer(self) -> _BearerToken:
        """The cached bearer while it has more than 30 s left to live, else a
        fresh one from :meth:`_mint_bearer`.

        The TTL is only half of a bearer's life. The nest's token store is in
        memory, so a nest restart forgets every bearer long before it expires,
        and `tests/api/ws_api.py` keeps one client per actor for the whole
        pytest process — across tests that restart the nest on purpose.
        :meth:`_connect` is where a forgotten bearer is recovered.
        """
        if self._bearer is not None and self._bearer.expires_at > time.time() + 30:
            return self._bearer
        self._bearer = self._mint_bearer()
        return self._bearer

    def _mint_bearer(self) -> _BearerToken:
        """Silent-challenge → sign → verify → bearer, over the pre-identity
        (anonymous) WS connection — the production launch path.

        The `POST /api/v1/auth/{challenge,verify}` HTTP twins were deleted in
        the WS-RPC-everywhere rip-out; bearer minting now rides the
        `fauna.auth.challenge` / `fauna.auth.verify` kinds on a transient
        anonymous WS (`WsRpcAnonClient`), exactly as every app's silent
        sign-in does.
        """
        # Local import to avoid a module-load cycle (the anon client imports the
        # shared core too, not this module — but keep the dependency edge light).
        from .ws_rpc_anon_client import WsRpcAnonClient

        with WsRpcAnonClient(
            self._base_url,
            reply_timeout=self._reply_timeout,
            http_timeout=self._http_timeout,
        ) as anon:
            # Step 0: the identity this login binds — read off THIS connection
            # before anything is signed (`login.md` § Binding the nest).
            from common.nest_identity import read_nest_identity

            nest_id = read_nest_identity(anon)

            # Step 1: fauna.auth.challenge → server-issued nonce (hex String).
            challenge_data = anon.call(
                "fauna.auth.challenge", {"actor_id": self._actor_id_hex}
            )
            nonce_hex = challenge_data["nonce"]
            nonce_bytes = bytes.fromhex(nonce_hex)
            if len(nonce_bytes) != 32:
                raise RuntimeError(
                    f"WS-RPC auth: server nonce was {len(nonce_bytes)} bytes, "
                    f"expected 32 (server response: {challenge_data!r})"
                )

            # Step 2: sign the tagged, nest-bound verify message (no timestamp
            # — clock-skew immune); common.sig_domain twins the Rust builder.
            from common.sig_domain import challenge_verify_signed_message

            sig = self._signing_key.sign(
                challenge_verify_signed_message(self._actor_id, nonce_bytes, nest_id)
            ).signature

            # Step 3: fauna.auth.verify → bearer + cached handle/domain/tier.
            verify_data = anon.call(
                "fauna.auth.verify",
                {
                    "actor_id": self._actor_id_hex,
                    "nonce": nonce_hex,
                    "signature": sig.hex(),
                    "nest_id": nest_id.hex(),
                },
            )

        token = verify_data.get("token")
        if not token:
            raise RuntimeError(
                f"WS-RPC auth: fauna.auth.verify returned no token: "
                f"{verify_data!r}"
            )
        # The deadline is `expires_in` anchored on THIS clock at receipt — the
        # same rule every Rust holder applies (`login.md` § Token lifetime on
        # the client's clock). `expires_in` is required: a reply without it is
        # a malformed nest, never a tolerated older one (the fallback retired
        # 2026-09-24 with the compat-remnant sweep).
        if "expires_in" not in verify_data:
            raise RuntimeError(
                f"WS-RPC auth: fauna.auth.verify returned no expires_in: {verify_data!r}"
            )
        expires_at = time.time() + float(verify_data["expires_in"])

        return _BearerToken(
            token=token, expires_at=expires_at, token_id=str(verify_data.get("token_id") or "")
        )

    @property
    def own_token_id(self) -> str:
        """The session id of THIS client's bearer (minting one if none is held)
        — what a `fauna.sessions.list` reader subtracts to see only the sessions
        the subject under test minted."""
        return self._acquire_bearer().token_id
