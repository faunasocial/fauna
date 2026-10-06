"""tier_3 API e2e: the **principal session** — the token-bearing WS-RPC
connection a third-party principal opens on ``GET /api/v1/principal/ws``
(``docs/goal/architecture/transport-connection.md`` § Connection lifecycle →
*The principal session*; the class and its scope check,
``docs/goal/architecture/apps/bridges.md`` § Capability-allowlist enforcement →
*How the class meets the connection*).

One journey, end to end against a real nest and the real PDS bridge (the
consent's precondition, as in ``test_third_party_principals.py``): an external
client consents ``fauna:feed:read``, redeems its code, and dials the session
with its DPoP-bound access token in ``Sec-WebSocket-Protocol``. Then:

- a kind inside its scopes answers, and so does the session kind
  ``fauna.capabilities.fetch``; a ``User``-only kind is refused by the ceiling;
- the upgrade refuses a proof-less dial, a proof under another key, and a token
  carrying no Fauna scope (``403 insufficient_scope``);
- **the scope check itself, with one arm:** a second ceremony of the same
  client consenting only ``openid`` narrows the principal row, so the LIVE
  session's next feed read is refused by ``scope_covers`` while the session
  kind still answers — the row narrowing every live session at once;
- ``fauna.principals.revoke`` closes the live socket ``4401``, and a fresh
  upgrade with the still-unexpired token is refused.

No wall-clock assertion anywhere (convention 14): the ``exp`` close is pinned
at tier_1 (``principal_session::tests::the_session_closes_at_its_tokens_exp``)
against tokio's paused clock.
"""

import secrets
import struct
import time
import urllib.parse

import cbor2
import pytest
import websocket  # type: ignore[import-untyped]  # from `websocket-client`

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.atproto_consent import (  # noqa: F401 — consent_nest/consent_nest_env/consent_bridge are pytest fixtures adopted by import
    HANDLE_DOMAIN,
    consent_bridge,
    consent_nest,
    consent_nest_env,
)
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.oauth_client import NONCE_HEADER, OAuthClient

pytestmark = pytest.mark.tier_3

REDIRECT_URI = "http://127.0.0.1:17774/callback"
#: What the client's metadata document declares: the one built Fauna arm, and
#: sign-in. A ceremony may request any subset (``OAuthClient.request_scope``).
DECLARED = "fauna:feed:read openid"
PATH = "/api/v1/principal/ws"
HTU = f"https://{HANDLE_DOMAIN}{PATH}"
CLOSE_AUTH_EXPIRED = 4401


def _alice(nest):
    alice = nest["user"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=alice["actor_id_bytes"],
        signing_key=bytes(alice["signing_key"]),
    )


def _ceremony(nest, request_scope):
    """One browser-start ceremony for the shared client, approved over the
    account's own connection. Returns the client and its token reply."""
    client = OAuthClient(
        base=nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri=REDIRECT_URI,
        scope=DECLARED,
        request_scope=request_scope,
    )
    request_uri = client.push_authorization_request()
    browser_code, flow_token = client.open_consent_page(request_uri)
    with _alice(nest) as ws:
        pending = ws.call("fauna.bridges.atproto.list_pending_consents", {})["consents"]
        matches = [c for c in pending if c["code"] == browser_code]
        assert matches, f"no pending consent carries {browser_code!r}: {pending!r}"
        resolved = ws.call(
            "fauna.bridges.atproto.resolve_consent",
            {"consent_id": matches[0]["consent_id"], "approved": True},
        )
        assert resolved["resolved"], resolved
    redirect = ""
    deadline = time.monotonic() + RPC_ROUNDTRIP_S
    while time.monotonic() < deadline:
        status, redirect = client.poll(flow_token)
        if status == "resolved":
            break
    assert redirect and "code=" in redirect, f"approval must release a code: {redirect!r}"
    tokens = client.exchange_code(redirect)
    assert tokens.get("access_token"), tokens
    return client, tokens


def _ws_url(nest) -> str:
    parsed = urllib.parse.urlparse(nest["url"])
    scheme = "wss" if parsed.scheme == "https" else "ws"
    return f"{scheme}://{parsed.netloc}{PATH}"


def _upgrade(nest, client, token, proof_signer=None, with_proof=True):
    """Dial the principal session. ``proof_signer`` signs the proof (default:
    the client whose key the token is bound to). One ``use_dpop_nonce`` retry,
    the protocol's own, never any other.

    Returns the open socket, or raises ``WebSocketBadStatusException``."""
    signer = proof_signer or client

    def dial():
        protocols = ["fauna.v1", f"dpop.{token}"]
        if with_proof:
            protocols.append(f"dpop-proof.{signer.dpop_proof('GET', HTU, access_token=token)}")
        return websocket.create_connection(
            _ws_url(nest), subprotocols=protocols, timeout=RPC_ROUNDTRIP_S
        )

    try:
        return dial()
    except websocket.WebSocketBadStatusException as refused:
        headers = {k.lower(): v for k, v in (refused.resp_headers or {}).items()}
        if refused.status_code == 401 and "use_dpop_nonce" in headers.get(
            "www-authenticate", ""
        ):
            signer.nonce = headers.get(NONCE_HEADER.lower())
            return dial()
        raise


def _refused_status(nest, client, token, **kw) -> tuple[int, dict]:
    with pytest.raises(websocket.WebSocketBadStatusException) as refused:
        _upgrade(nest, client, token, **kw).close()
    headers = {k.lower(): v for k, v in (refused.value.resp_headers or {}).items()}
    return refused.value.status_code, headers


class _Session:
    """A minimal WS-RPC caller over one principal socket: one request at a
    time, and the close code surfaced rather than reconnected around."""

    def __init__(self, ws):
        self.ws = ws
        self.corr = 0
        #: Push frames read while waiting for a reply, as ``(kind, payload)``,
        #: in arrival order — kept for :meth:`next_push`, never dropped.
        self.pushes: list[tuple[str, dict]] = []

    def call(self, kind: str, payload: dict):
        """``(ok, payload)`` of the Reply to one request."""
        self.corr += 1
        self.ws.send_binary(
            cbor2.dumps(
                {0: 0, 1: self.corr, 2: kind, 3: secrets.token_bytes(16), 4: payload},
                canonical=True,
            )
        )
        while True:
            frame = cbor2.loads(self.ws.recv())
            if frame.get(0) == 1 and frame.get(1) == self.corr:
                return bool(frame.get(7)), frame.get(4)
            if frame.get(0) == 2:
                self.pushes.append((frame.get(2), frame.get(4)))

    def next_push(self) -> tuple[str, dict]:
        """The oldest push not yet taken — buffered by :meth:`call`, else the
        next one off the socket (bounded by the socket's own timeout)."""
        while not self.pushes:
            frame = cbor2.loads(self.ws.recv())
            if frame.get(0) == 2:
                self.pushes.append((frame.get(2), frame.get(4)))
        return self.pushes.pop(0)

    def close_code(self) -> int:
        """Read until the nest closes the socket; its close code."""
        while True:
            opcode, data = self.ws.recv_data(control_frame=True)
            if opcode == websocket.ABNF.OPCODE_CLOSE:
                return struct.unpack("!H", data[:2])[0] if len(data) >= 2 else 1005


def _refusal_code(reply) -> str:
    return str(reply.get("code", "")) if isinstance(reply, dict) else ""


@pytest.mark.feature("connected-apps")
def test_a_principal_session_is_gated_by_ceiling_scope_and_revoke(consent_nest, consent_bridge):
    nest = consent_nest
    client, tokens = _ceremony(nest, DECLARED)
    token = tokens["access_token"]

    # ── The upgrade gate. ──
    status, headers = _refused_status(nest, client, token, with_proof=False)
    assert status == 401 and "invalid_token" in headers.get("www-authenticate", ""), (
        status,
        headers,
    )
    assert headers.get(NONCE_HEADER.lower()), "every refusal carries a fresh DPoP-Nonce"

    stranger = OAuthClient(
        base=nest["url"], htu_origin=f"https://{HANDLE_DOMAIN}", redirect_uri=REDIRECT_URI
    )
    stranger.nonce = client.nonce
    status, _ = _refused_status(nest, client, token, proof_signer=stranger)
    assert status == 401, "a proof under a key the token is not bound to opens nothing"

    # ── The session. ──
    session = _Session(_upgrade(nest, client, token))
    ok, reply = session.call("fauna.feed.trending.posts", {})
    assert ok, f"a kind inside the principal's scopes answers: {reply!r}"
    ok, reply = session.call("fauna.capabilities.fetch", {})
    assert ok and reply.get("grants") == [], (
        f"the session kind answers — empty, since the client attested no key: {reply!r}"
    )
    ok, reply = session.call("fauna.principals.list", {})
    assert not ok and _refusal_code(reply).endswith("permission_denied"), (
        f"a User-only kind is above the ThirdParty ceiling: {reply!r}"
    )

    # ── The scope check, with one arm: narrow the row under the live session. ──
    narrow_client, narrow_tokens = _ceremony(nest, "openid")
    assert narrow_client.client_id == client.client_id, "the same client, re-consenting"
    status, headers = _refused_status(nest, narrow_client, narrow_tokens["access_token"])
    assert status == 403 and "insufficient_scope" in headers.get("www-authenticate", ""), (
        f"a token holding no Fauna scope opens no session: {status} {headers!r}"
    )

    ok, reply = session.call("fauna.feed.trending.posts", {})
    assert not ok and _refusal_code(reply).endswith("permission_denied"), (
        f"the row now grants only openid, so the live session's feed read is refused: {reply!r}"
    )
    ok, reply = session.call("fauna.capabilities.fetch", {})
    assert ok, f"a session kind needs no scope and still answers: {reply!r}"

    # ── Revoke: the live socket closes 4401, and the token opens nothing more. ──
    with _alice(nest) as ws:
        rows = [
            p
            for p in ws.call("fauna.principals.list", {})["principals"]
            if p["client_id"] == client.client_id
        ]
        assert len(rows) == 1, rows
        revoked = ws.call(
            "fauna.principals.revoke", {"principal_id": bytes(rows[0]["principal_id"])}
        )
    assert revoked["revoked"] is True, revoked
    assert session.close_code() == CLOSE_AUTH_EXPIRED, "the revoke closes the live socket 4401"

    status, _ = _refused_status(nest, client, token)
    assert status == 401, "an unexpired token of a revoked principal opens no session"
