"""tier_3 API e2e: the **third-party principal** roster — minted by the consent,
listed by ``fauna.principals.list``, ended by the one verb
``fauna.principals.revoke`` (``docs/goal/architecture/third-party.md`` § The
principal model).

A principal is the nest-side identity of one approved metadata document for one
account. These tests drive the real thing end to end: an external OAuth client
pushes its request to the nest's own authorization server (the browser start —
the typed-code start is not built yet), the account approves over its own authed
connection exactly as its app's consent card does, and the code is redeemed at
``/oauth/token``. What the roster then says, and what the revoke then kills, is
read back through the account's own kinds — the path its connected-apps surface
takes.

**Why the real PDS bridge is in the room.** An approval by an account with no
ACTIVE ATProto identity is refused post-consent, so no ceremony can reach a code
without the bridge that mints one (``helpers/atproto_consent.py`` says more).
Nothing here talks to the bridge otherwise.

**The capability arm is asserted OWNER-side, on purpose.** Revoke must end every
capability grant whose holder is the principal's key. ``fauna.capabilities.fetch``
— the holder-side read — is a principal's to call only over its own session
(``test_third_party_session.py``), which a revoked principal no longer has, so
the evidence is the owner's own ``fauna.capabilities.reconcile``: the grant id
the owner minted to the principal's key is gone from the owner's rows.
"""

import secrets
import time

import pytest

import fauna_ffi
from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.atproto_consent import (  # noqa: F401 — consent_nest/consent_nest_env/consent_bridge are pytest fixtures adopted by import
    HANDLE_DOMAIN,
    consent_bridge,
    consent_nest,
    consent_nest_env,
)
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.oauth_client import OAuthClient

pytestmark = pytest.mark.tier_3

#: One loopback client identity for every ceremony here. The loopback dev
#: client's `client_id` IS its metadata document (redirect_uri + scope in the
#: query), so two `OAuthClient`s built with the same pair are the same client —
#: which is what lets the second ceremony prove find-or-create.
REDIRECT_URI = "http://127.0.0.1:17773/callback"


def _alice(nest):
    alice = nest["user"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=alice["actor_id_bytes"],
        signing_key=bytes(alice["signing_key"]),
    )


def _ceremony(nest, holder):
    """One complete browser-start ceremony for the shared loopback client,
    approved over the account's own connection. Returns the client and its
    token reply."""
    client = OAuthClient(
        base=nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri=REDIRECT_URI,
        holder_x25519=holder,
    )
    request_uri = client.push_authorization_request()
    browser_code, flow_token = client.open_consent_page(request_uri)
    assert browser_code, "the consent page rendered an empty binding code"

    # The approval the app's consent card performs: find the request by the
    # code the browser shows — so this cannot resolve some other ceremony's
    # row — and approve it over the account's own authed connection.
    with _alice(nest) as ws:
        pending = ws.call("fauna.bridges.atproto.list_pending_consents", {})["consents"]
        matches = [c for c in pending if c["code"] == browser_code]
        assert matches, (
            f"no pending consent carries the browser's code {browser_code!r}: "
            f"{[c['code'] for c in pending]!r}"
        )
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
    assert tokens.get("refresh_token"), tokens
    return client, tokens


def _principals(nest):
    with _alice(nest) as ws:
        return ws.call("fauna.principals.list", {})["principals"]


@pytest.mark.feature("connected-apps")
def test_the_consent_mints_one_principal_and_revoke_ends_everything_it_holds(
    consent_nest, consent_bridge
):
    """TP1 end to end: one approved document → one roster row, however many
    ceremonies; revoke → the row, every grant family, and every capability
    grant held by its key are gone."""
    holder = secrets.token_bytes(32)

    # ── The first ceremony mints the principal. ──
    first_client, first_tokens = _ceremony(consent_nest, holder)
    rows = [p for p in _principals(consent_nest) if p["client_id"] == first_client.client_id]
    assert len(rows) == 1, f"one approved document is one principal: {rows!r}"
    principal = rows[0]

    with _alice(consent_nest) as ws:
        grants = ws.call("fauna.bridges.atproto.list_grants", {})["grants"]
    grant = next(g for g in grants if g["client_id"] == first_client.client_id)
    # The roster names what the card named: the label is the RESOLVED name the
    # grant row carries — asserted as agreement, never as a literal.
    assert principal["label"] == grant.get("client_name"), (principal, grant)
    assert bytes(principal["holder_x25519"]) == holder, (
        "the holder key the client attested at PAR is the one the row records"
    )
    assert principal["granted_scopes"] == grant["scopes"] == "atproto", (principal, grant)
    # A public (loopback) client runs on the user's own device; a confidential
    # client is a server of its own.
    assert principal["execution_form"] == "device", principal
    assert principal["last_used_at"] is not None, principal
    assert principal["live_grants"] == 1, principal

    # ── A second ceremony for the same document finds the row, never adds one. ──
    second_client, second_tokens = _ceremony(consent_nest, holder)
    assert second_client.client_id == first_client.client_id
    rows = [p for p in _principals(consent_nest) if p["client_id"] == first_client.client_id]
    assert len(rows) == 1, f"a re-consent must not mint a second principal: {rows!r}"
    again = rows[0]
    assert bytes(again["principal_id"]) == bytes(principal["principal_id"]), (principal, again)
    assert again["last_used_at"] > principal["last_used_at"], (
        f"the re-consent is a use, and the row must say so: {principal!r} → {again!r}"
    )
    assert again["live_grants"] == 2, (
        f"each ceremony mints its own grant family, and both hang off the row: {again!r}"
    )

    # ── The owner grants the principal's key something. ──
    alice = consent_nest["user"]
    grant_id = secrets.token_bytes(16)
    now = int(time.time())
    blob = fauna_ffi.build_post_grant(
        alice["actor_id_bytes"], grant_id, holder, None,
        now, now + 3600, "supporter", secrets.token_bytes(32),
    )
    with _alice(consent_nest) as ws:
        minted = ws.call("fauna.capabilities.mint", {"grant_blob": blob})
        assert minted.get("ok") is True, minted
        owned = ws.call("fauna.capabilities.reconcile", {})["grant_ids"]
    assert grant_id in [bytes(g) for g in owned], "precondition: the grant is on the nest"

    # ── One verb. ──
    with _alice(consent_nest) as ws:
        reply = ws.call(
            "fauna.principals.revoke", {"principal_id": bytes(principal["principal_id"])}
        )
    assert reply["revoked"] is True, reply

    # Every grant family the principal accrued is dead at the token endpoint —
    # both ceremonies', not just the latest.
    for client, tokens in ((first_client, first_tokens), (second_client, second_tokens)):
        status, refused = client.token_request_expecting_refusal(
            {"grant_type": "refresh_token", "refresh_token": tokens["refresh_token"]}
        )
        assert status == 400 and refused.get("error") == "invalid_grant", (
            f"a revoked principal's grant family must not rotate: {status} {refused!r}"
        )

    with _alice(consent_nest) as ws:
        grants = ws.call("fauna.bridges.atproto.list_grants", {})["grants"]
        owned = ws.call("fauna.capabilities.reconcile", {})["grant_ids"]
    assert not [g for g in grants if g["client_id"] == first_client.client_id], (
        f"no connected-apps grant may outlive its principal: {grants!r}"
    )
    assert grant_id not in [bytes(g) for g in owned], (
        "a capability grant held by the revoked principal's key must be gone"
    )
    assert not [
        p for p in _principals(consent_nest) if p["client_id"] == first_client.client_id
    ], "revocation is deleting the roster row"

    # A second revoke of the same id is a clean no-op, not an error.
    with _alice(consent_nest) as ws:
        again = ws.call(
            "fauna.principals.revoke", {"principal_id": bytes(principal["principal_id"])}
        )
    assert again["revoked"] is False, again


def test_a_client_that_attests_no_key_still_gets_its_roster_row(consent_nest, consent_bridge):
    """A standard ATProto client sends no Fauna parameter. It is still one
    approved document, so it is still one principal — with no holder key,
    which grants simply cannot be minted to."""
    client = OAuthClient(
        base=consent_nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri="http://127.0.0.1:17774/callback",
    )
    request_uri = client.push_authorization_request()
    browser_code, flow_token = client.open_consent_page(request_uri)
    with _alice(consent_nest) as ws:
        pending = ws.call("fauna.bridges.atproto.list_pending_consents", {})["consents"]
        consent = next(c for c in pending if c["code"] == browser_code)
        ws.call(
            "fauna.bridges.atproto.resolve_consent",
            {"consent_id": consent["consent_id"], "approved": True},
        )
    redirect = ""
    deadline = time.monotonic() + RPC_ROUNDTRIP_S
    while time.monotonic() < deadline:
        status, redirect = client.poll(flow_token)
        if status == "resolved":
            break
    client.exchange_code(redirect)

    rows = [p for p in _principals(consent_nest) if p["client_id"] == client.client_id]
    assert len(rows) == 1, rows
    assert rows[0]["holder_x25519"] is None, rows[0]


def test_a_malformed_holder_key_is_refused_at_the_push(consent_nest):
    """The attestation is 32 bytes of X25519 public key or nothing: a value
    that is neither is the client's error, answered at PAR before a consent
    is ever shown."""
    client = OAuthClient(
        base=consent_nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri=REDIRECT_URI,
        holder_x25519=b"\x01" * 31,
    )
    status, body = client.push_authorization_request_expecting_refusal()
    assert status == 400, (status, body)
    assert body.get("error") == "invalid_request", body
    assert "fauna_holder_x25519" in body.get("error_description", ""), body
