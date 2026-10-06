"""tier_3 API e2e: the polled consent starts, nest side (TP9).

``docs/goal/behavior/authorization-server.md`` § Consent — *one card, four
starts*. Three of them end with the client polling ``/oauth/token`` rather than
following a redirect:

* the **typed code** — ``/oauth/device_authorization`` (RFC 8628): the device
  shows a code, the user types it into their own Fauna
  (``fauna.oauth.consent.lookup_code``), approves the card, and the device's
  ``device_code`` poll turns from ``authorization_pending`` into tokens;
* the **quiet push** — ``/oauth/bc-authorize`` (CIBA, poll mode): a device that
  knows the user's handle asks for the card, under TP9's three rules — (a) a
  client this user never approved raises NO notification and lands in the
  pending list only, (b) one live request per (client, account), a repeat
  replaces, (c) a blocked client's request is answered exactly like any other
  and opens nothing;
* the **same-device handoff** — a PAR whose ``request_uri`` the device app hands
  to the user's own Fauna app (``fauna://consent/<request_uri>``), which opens
  it as the caller's row (``fauna.oauth.consent.open_handoff``); the device
  polls with ``urn:fauna:params:grant-type:handoff``, PKCE-checked.

**Why this lives beside the bridge fixtures rather than on a plain nest.** The
typed code's journey ends in tokens, and a token exchange needs an approving
account with an ACTIVE ATProto identity — which only a real PDS bridge mints
(the same reason ``test_oauth_issuer.py`` keeps its happy path out). So the
module adopts ``consent_nest`` / ``consent_bridge`` from
``helpers/atproto_consent.py``: a claimed nest, alice at ``hosted_full``, her
identity ACTIVE.

**The approving app is alice's own authed WS-RPC connection**, exactly the
calls a Fauna app makes — ``lookup_code``, ``list_pending_consents``,
``resolve_consent``. The app halves (the *Connect an app* entry and the
requests tray) are a later slice's; what is under test here is the nest.

**Rule (a) is asserted against the push stream, never a sleep** (convention
14). "No notification" is an absence, and an absence needs a causal barrier: a
second client that alice HAS approved pushes after the never-approved one, and
pushes on one connection arrive in the order the nest raised them — so when the
approved client's notification arrives, the never-approved one's would already
have.
"""

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.oauth_client import (
    BC_AUTHORIZE_PATH,
    DEVICE_AUTHORIZATION_PATH,
    GRANT_TYPE_CIBA,
    GRANT_TYPE_DEVICE_CODE,
    GRANT_TYPE_HANDOFF,
    OAuthClient,
)
from helpers.atproto_consent import (  # noqa: F401 — consent_nest/consent_nest_env/consent_bridge are pytest fixtures adopted by import
    HANDLE_DOMAIN,
    consent_bridge,
    consent_nest,
    consent_nest_env,
)

pytestmark = pytest.mark.tier_3

CONSENT_REQUESTED = "fauna.atproto.consent_requested"
#: alice's handle, as `consent_nest` registers her — what a device that knows
#: her handle sends as `login_hint`.
ALICE_HINT = "alice"


def _client(consent_nest, port):
    """A loopback development client. Each port is a distinct `client_id` —
    the redirect target is part of a loopback client's identity — and each
    instance holds its own DPoP key, which is what a public client's
    installation IS to this server."""
    return OAuthClient(
        base=consent_nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri=f"http://127.0.0.1:{port}/callback",
        scope="atproto",
    )


def _alice(consent_nest):
    alice = consent_nest["user"]
    return WsRpcAdminClient(
        consent_nest["url"],
        actor_id=alice["actor_id_bytes"],
        signing_key=bytes(alice["signing_key"]),
    )


def _pending(ws, client_id):
    rows = ws.call("fauna.bridges.atproto.list_pending_consents", {})["consents"]
    return [row for row in rows if row["client_id"] == client_id]


def _start_device(client):
    status, started = client.start_polled(DEVICE_AUTHORIZATION_PATH, {})
    assert status == 200, f"device authorization refused: {status} {started!r}"
    return started


def _poll_device(client, device_code):
    return client.poll_token(GRANT_TYPE_DEVICE_CODE, "device_code", device_code)


def _approve_typed_code(ws, user_code):
    looked_up = ws.call("fauna.oauth.consent.lookup_code", {"user_code": user_code})
    consent = looked_up.get("consent")
    assert consent, f"the typed code {user_code!r} found no request: {looked_up!r}"
    resolved = ws.call(
        "fauna.bridges.atproto.resolve_consent",
        {"consent_id": consent["consent_id"], "approved": True},
    )
    assert resolved["resolved"], f"the looked-up request did not resolve: {resolved!r}"
    return consent


def _grant_by_typed_code(consent_nest, client):
    """Run a whole typed-code ceremony for `client` and return its tokens — how
    a test gives alice a PRIOR approval of a client."""
    started = _start_device(client)
    with _alice(consent_nest) as ws:
        _approve_typed_code(ws, started["user_code"])
    status, tokens = _poll_device(client, started["device_code"])
    assert status == 200, f"an approved typed code must redeem: {status} {tokens!r}"
    return tokens


def test_a_typed_code_reaches_the_card_and_its_poll_turns_into_tokens(
    consent_bridge, consent_nest
):
    """(a) The whole typed-code start: the device's code resolves to the card
    alice's app renders, and her approval is what turns the device's poll from
    ``authorization_pending`` into a DPoP-bound grant she can see."""
    device = _client(consent_nest, 17781)
    started = _start_device(device)
    assert started["verification_uri"] == f"https://{HANDLE_DOMAIN}", started
    assert started["interval"] >= 5 and started["expires_in"] > 0, started
    user_code = started["user_code"]

    status, body = _poll_device(device, started["device_code"])
    assert (status, body.get("error")) == (400, "authorization_pending"), (status, body)

    with _alice(consent_nest) as ws:
        # A typed-code request is listed to NOBODY until someone types its
        # code — typing it is the whole of what this start proves.
        assert not [
            row
            for row in ws.call("fauna.bridges.atproto.list_pending_consents", {})["consents"]
            if row["code"] == user_code
        ], "an unclaimed typed-code request must not be on anyone's card list"

        # Typed the way a person types: lower case, no hyphen.
        consent = _approve_typed_code(ws, user_code.replace("-", "").lower())
        assert consent["code"] == user_code, (
            "the card must show the code the device displayed", consent, user_code
        )
        assert consent["client_id"] == device.client_id, consent

    status, tokens = _poll_device(device, started["device_code"])
    assert status == 200, f"the approval must turn the poll into tokens: {status} {tokens!r}"
    assert tokens.get("token_type") == "DPoP", tokens
    assert tokens.get("access_token") and tokens.get("refresh_token"), tokens
    assert tokens.get("scope") == "atproto", tokens

    status, again = _poll_device(device, started["device_code"])
    assert (status, again.get("error")) == (400, "invalid_grant"), (
        "one approval, one set of tokens — the device_code is spent", status, again
    )

    with _alice(consent_nest) as ws:
        grants = ws.call("fauna.bridges.atproto.list_grants", {})["grants"]
    assert [g for g in grants if g.get("client_id") == device.client_id], (
        f"the grant must be an audit row alice's own app can list: {grants!r}"
    )


@pytest.mark.feature("connected-apps")
def test_a_quiet_push_notifies_only_a_client_alice_has_approved(
    consent_bridge, consent_nest
):
    """(b) Rule (a): a never-approved client's quiet push opens a pending row
    and raises NO ``consent_requested`` push; an approved client's does. Plus
    rule (b): the stranger's repeat replaces its earlier request."""
    approved = _client(consent_nest, 17782)
    stranger = _client(consent_nest, 17783)
    _grant_by_typed_code(consent_nest, approved)

    with _alice(consent_nest) as ws:
        # Open the connection before either push is raised, so both would be
        # delivered to it.
        assert not _pending(ws, stranger.client_id)

        for _ in range(2):
            status, body = stranger.start_polled(BC_AUTHORIZE_PATH, {"login_hint": ALICE_HINT})
            assert status == 200 and body.get("auth_req_id"), (status, body)
        status, body = approved.start_polled(BC_AUTHORIZE_PATH, {"login_hint": ALICE_HINT})
        assert status == 200 and body.get("auth_req_id"), (status, body)

        # The causal barrier: the approved client's notification is raised
        # AFTER both of the stranger's requests, on the same connection.
        ws.wait_for_push(
            CONSENT_REQUESTED,
            timeout=RPC_ROUNDTRIP_S,
            predicate=lambda push: push.payload["consent"]["client_id"] == approved.client_id,
        )
        from_stranger = [
            push
            for push in ws.drain_pushes(CONSENT_REQUESTED)
            if push.payload["consent"]["client_id"] == stranger.client_id
        ]
        assert not from_stranger, (
            "a client alice never approved must not put a notification in front "
            f"of her: {from_stranger!r}"
        )

        pending = _pending(ws, stranger.client_id)
        assert len(pending) == 1, (
            "the never-approved request lands in the pending list, and its repeat "
            f"REPLACES it rather than stacking: {pending!r}"
        )


@pytest.mark.feature("connected-apps")
def test_a_blocked_clients_push_is_answered_uniformly_and_opens_nothing(
    consent_bridge, consent_nest
):
    """(c) Rule (c): a client alice blocked is answered exactly like a request
    naming nobody — an ``auth_req_id`` that polls ``authorization_pending`` —
    and nothing reaches her pending list. Then the block is listed, and lifted,
    from her own app."""
    blocked = _client(consent_nest, 17784)
    with _alice(consent_nest) as ws:
        reply = ws.call(
            "fauna.oauth.consent.block_client",
            {"client_id": blocked.client_id, "blocked": True},
        )
        assert reply["blocked"] is True, reply

        status, answer = blocked.start_polled(BC_AUTHORIZE_PATH, {"login_hint": ALICE_HINT})
        nobody_status, nobody = blocked.start_polled(
            BC_AUTHORIZE_PATH, {"login_hint": "nobody-by-this-name"}
        )
        assert status == nobody_status == 200, (status, answer, nobody_status, nobody)
        assert sorted(answer) == sorted(nobody), (
            "a blocked client and an unresolvable hint must get the same answer shape",
            answer,
            nobody,
        )
        assert (answer["expires_in"], answer["interval"]) == (
            nobody["expires_in"],
            nobody["interval"],
        ), (answer, nobody)

        assert not _pending(ws, blocked.client_id), (
            "a blocked client's request must open nothing"
        )

        listed = ws.call("fauna.oauth.consent.list_blocked_clients", {})["clients"]
        assert [c for c in listed if c["client_id"] == blocked.client_id], listed
        lifted = ws.call(
            "fauna.oauth.consent.block_client",
            {"client_id": blocked.client_id, "blocked": False},
        )
        assert lifted["blocked"] is False, lifted
        listed = ws.call("fauna.oauth.consent.list_blocked_clients", {})["clients"]
        assert not [c for c in listed if c["client_id"] == blocked.client_id], listed

    status, body = blocked.poll_token(GRANT_TYPE_CIBA, "auth_req_id", answer["auth_req_id"])
    assert (status, body.get("error")) == (400, "authorization_pending"), (
        "a request that opened nothing polls exactly as one nobody has answered",
        status,
        body,
    )


def _poll_handoff(client, request_uri):
    return client.poll_token(
        GRANT_TYPE_HANDOFF, "request_uri", request_uri, {"code_verifier": client.verifier}
    )


@pytest.mark.feature("connected-apps")
def test_a_same_device_handoff_opens_alices_row_and_its_poll_turns_into_tokens(
    consent_bridge, consent_nest
):
    """(d) The same-device handoff: the device pushes a PAR, alice's app opens
    the ``request_uri`` the route carried, the card is hers alone, and her
    approval turns the device's handoff poll into tokens. Before the open the
    poll waits; the handle is single-use across both doors."""
    device = _client(consent_nest, 17785)
    request_uri = device.push_authorization_request()

    status, body = _poll_handoff(device, request_uri)
    assert (status, body.get("error")) == (400, "authorization_pending"), (
        "a PAR nobody has opened yet is waited on, not refused",
        status,
        body,
    )

    with _alice(consent_nest) as ws:
        opened = ws.call("fauna.oauth.consent.open_handoff", {"request_uri": request_uri})
        consent = opened.get("consent")
        assert consent, f"the handoff found no request: {opened!r}"
        assert consent["client_id"] == device.client_id, consent
        assert [row["consent_id"] for row in _pending(ws, device.client_id)] == [
            consent["consent_id"]
        ], "the opened row is alice's ordinary pending row"

        again = ws.call("fauna.oauth.consent.open_handoff", {"request_uri": request_uri})
        assert again.get("consent") is None, (
            "the handle is single-use — a second open is the one empty answer",
            again,
        )

        resolved = ws.call(
            "fauna.bridges.atproto.resolve_consent",
            {"consent_id": consent["consent_id"], "approved": True},
        )
        assert resolved["resolved"], resolved

    status, tokens = _poll_handoff(device, request_uri)
    assert status == 200, f"the approval must turn the poll into tokens: {status} {tokens!r}"
    assert tokens.get("token_type") == "DPoP", tokens
    assert tokens.get("access_token") and tokens.get("refresh_token"), tokens

    status, again = _poll_handoff(device, request_uri)
    assert (status, again.get("error")) == (400, "invalid_grant"), (
        "one approval, one set of tokens — the handle is spent", status, again
    )


@pytest.mark.feature("connected-apps")
def test_a_declined_handoff_answers_access_denied(consent_bridge, consent_nest):
    """(e) A decline on the card answers the device's handoff poll with
    ``access_denied``, and a handle the browser door would have needed is
    already spent by the app's open."""
    device = _client(consent_nest, 17786)
    request_uri = device.push_authorization_request()
    with _alice(consent_nest) as ws:
        consent = ws.call("fauna.oauth.consent.open_handoff", {"request_uri": request_uri})[
            "consent"
        ]
        assert consent, "the handoff must open"
        resolved = ws.call(
            "fauna.bridges.atproto.resolve_consent",
            {"consent_id": consent["consent_id"], "approved": False},
        )
        assert resolved["resolved"], resolved

    status, body = _poll_handoff(device, request_uri)
    assert (status, body.get("error")) == (400, "access_denied"), (status, body)
