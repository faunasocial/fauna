"""The principal session's client half, shared: one consent ceremony for a
third-party client, the DPoP-bound dial of ``GET /api/v1/principal/ws``, and a
minimal WS-RPC caller over that socket
(``docs/goal/architecture/transport-connection.md`` § Connection lifecycle →
*The principal session*).

Lifted out of ``tests/api/test_third_party_session.py`` so the connected-apps
journey (``tests/test_connected_apps.py``) arranges and observes a principal
the same way the API suite does, never through a second copy. A ceremony is
fixture SETUP (e2e convention 8 (b)): the approval goes over the account's own
connection, because what a caller tests is what happens to the principal
afterwards.
"""

import json
import secrets
import struct
import time
import urllib.error
import urllib.parse
import urllib.request

import cbor2
import pytest
import websocket  # type: ignore[import-untyped]  # from `websocket-client`

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.atproto_consent import HANDLE_DOMAIN
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.oauth_client import NONCE_HEADER, OAuthClient

REDIRECT_URI = "http://127.0.0.1:17774/callback"
#: What the client's metadata document declares: the one built Fauna arm, and
#: sign-in. A ceremony may request any subset (``OAuthClient.request_scope``).
DECLARED = "fauna:feed:read openid"
PATH = "/api/v1/principal/ws"
HTU = f"https://{HANDLE_DOMAIN}{PATH}"
CLOSE_AUTH_EXPIRED = 4401
#: The events doors' HTTP face (``transport.md`` § Push events → *Third-party
#: event doors*).
EVENTS_PATH = "/api/v1/events"


def alice_client(nest):
    alice = nest["user"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=alice["actor_id_bytes"],
        signing_key=bytes(alice["signing_key"]),
    )


def ceremony(nest, request_scope, redirect_uri=REDIRECT_URI, declared=DECLARED):
    """One browser-start ceremony for the shared client — or, with
    ``declared``, for the loopback client whose document declares that set (a
    different client: the set is part of its ``client_id``) — approved over the
    account's own connection. Returns the client and its token reply."""
    client = OAuthClient(
        base=nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri=redirect_uri,
        scope=declared,
        request_scope=request_scope,
    )
    request_uri = client.push_authorization_request()
    browser_code, flow_token = client.open_consent_page(request_uri)
    with alice_client(nest) as ws:
        pending = ws.call("fauna.bridges.atproto.list_pending_consents", {})["consents"]
        matches = [c for c in pending if c["code"] == browser_code]
        assert matches, f"no pending consent carries {browser_code!r}: {pending!r}"
        resolved = ws.call(
            "fauna.bridges.atproto.resolve_consent",
            {"consent_id": matches[0]["consent_id"], "approved": True},
        )
        assert resolved["resolved"], resolved
    return client, redeem(client, flow_token)


def redeem(client: OAuthClient, flow_token: str) -> dict:
    """The browser's side once the request is answered: poll the flow until it
    resolves (the poll holds while pending, so each call is the mechanism),
    then redeem the released code. Returns the token reply."""
    redirect = ""
    deadline = time.monotonic() + RPC_ROUNDTRIP_S
    while time.monotonic() < deadline:
        status, redirect = client.poll(flow_token)
        if status == "resolved":
            break
    assert redirect and "code=" in redirect, f"approval must release a code: {redirect!r}"
    tokens = client.exchange_code(redirect)
    assert tokens.get("access_token"), tokens
    return tokens


def approve_with_identity_op_grant(
    nest, client: OAuthClient, holder_pub: bytes, grant_id: bytes, op_class: str
) -> dict:
    """A browser-start ceremony for ``client`` — a third party asking for the
    ``fauna:identity:op:<op_class>`` scope and attesting ``holder_pub`` — that
    the owner approves with the keyless ``identity.op`` grant to that key
    deposited first: PAR → the browser page → mint → resolve → the code is
    redeemed. Returns the token reply. Fixture SETUP like :func:`ceremony`: a
    caller tests what the oracle then does for the principal.

    The raw ``fauna.capabilities.mint`` records no grant-log event, so this is
    for a nest with no live account runtime beside it (the API suite): a
    runtime's reconcile sweep revokes a row its log does not hold
    (``ui/nests.md`` § Trust facet — grants → *Reconcile*). A journey with an
    app signed in approves in the app instead, which records before it
    deposits."""
    import fauna_ffi

    request_uri = client.push_authorization_request()
    browser_code, flow_token = client.open_consent_page(request_uri)
    alice = nest["user"]
    now = int(time.time())
    with alice_client(nest) as ws:
        pending = ws.call("fauna.bridges.atproto.list_pending_consents", {})["consents"]
        consent = next((c for c in pending if c["code"] == browser_code), None)
        assert consent, f"no pending consent carries {browser_code!r}: {pending!r}"
        assert bytes(consent["holder_x25519"]) == holder_pub, consent
        blob = fauna_ffi.build_identity_op_grant(
            bytes(alice["signing_key"]), grant_id, holder_pub, now - 5, now + 3600, op_class
        )
        minted = ws.call("fauna.capabilities.mint", {"grant_blob": blob})
        assert minted.get("ok") is True, minted
        resolved = ws.call(
            "fauna.bridges.atproto.resolve_consent",
            {"consent_id": consent["consent_id"], "approved": True},
        )
        assert resolved["resolved"], resolved
    return redeem(client, flow_token)


def _ws_url(nest) -> str:
    parsed = urllib.parse.urlparse(nest["url"])
    scheme = "wss" if parsed.scheme == "https" else "ws"
    return f"{scheme}://{parsed.netloc}{PATH}"


def upgrade(nest, client, token, proof_signer=None, with_proof=True):
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


def refused_status(nest, client, token, **kw) -> tuple[int, dict]:
    with pytest.raises(websocket.WebSocketBadStatusException) as refused:
        upgrade(nest, client, token, **kw).close()
    headers = {k.lower(): v for k, v in (refused.value.resp_headers or {}).items()}
    return refused.value.status_code, headers


def events_poll(nest, client, token, cursor: int | None, wait: int) -> tuple[int, dict]:
    """``GET /api/v1/events`` with ``token`` and a fresh DPoP proof from
    ``client``; one ``use_dpop_nonce`` retry, the protocol's own. ``wait`` is
    how long the door may hold the poll open (``0`` answers at once)."""
    query = {"wait": wait} | ({} if cursor is None else {"cursor": cursor})
    url = f"{nest['url']}{EVENTS_PATH}?{urllib.parse.urlencode(query)}"
    htu = f"https://{HANDLE_DOMAIN}{EVENTS_PATH}"

    def send():
        request = urllib.request.Request(
            url,
            method="GET",
            headers={
                "Authorization": f"DPoP {token}",
                "DPoP": client.dpop_proof("GET", htu, access_token=token),
            },
        )
        try:
            with urllib.request.urlopen(request, timeout=wait + RPC_ROUNDTRIP_S) as resp:
                return resp.status, dict(resp.headers), json.loads(resp.read() or b"{}")
        except urllib.error.HTTPError as refused:
            raw = refused.read()
            return refused.code, dict(refused.headers), json.loads(raw) if raw else {}

    status, headers, body = send()
    lowered = {k.lower(): v for k, v in headers.items()}
    if status == 401 and "use_dpop_nonce" in lowered.get("www-authenticate", ""):
        client.nonce = lowered.get(NONCE_HEADER.lower())
        status, _, body = send()
    return status, body


class PrincipalSession:
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


def refusal_code(reply) -> str:
    return str(reply.get("code", "")) if isinstance(reply, dict) else ""
