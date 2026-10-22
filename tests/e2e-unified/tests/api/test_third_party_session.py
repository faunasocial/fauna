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

A grant-family journey holds door (c) (§ *The principal session* →
*Revocation — three doors*): two ceremonies of one client open two families of
one principal, each dials a session, and the user's per-grant revoke from their
own app (``fauna.bridges.atproto.revoke_session``) closes that family's socket
``4401`` while the sibling family's session keeps answering — and the revoked
family's still-unexpired token opens nothing more.

A second journey holds the fifth arm, ``fauna:personalization:rw``
(``docs/goal/behavior/authorization-server.md`` § Scope grammar → *The fifth
arm*): a public client attesting a holder key consents the arm, the owner
deposits its grant twin before resolving (the production
``mint_personalization_grant`` through ``fauna_ffi``), and the principal finds
both pairs on its grant, trains and reads a model, lists ``state`` and opens
the owner's registry row with the wrapped pair — and opens no row of a delegable
kind the twin does not carry, listed beside it — and, holding
``fauna:events:subscribe`` beside the arm — hears the owner's next ``state``
put on the events long-poll (``docs/goal/architecture/transport.md`` § Push
events → *Third-party event doors*) — while the ``state`` put, the model delete
and an ``ext:`` list refuse; the owner's ``fauna.principals.revoke`` then
deletes the twin (``fauna.capabilities.reconcile`` no longer lists it) and
closes the session. The confidential client's PAR refusal of the same string,
and the nest's refusal to mint the twin to a ``remote`` principal's key, sit
beside their TP4 sibling in ``test_ext_kinds.py``, whose fixture serves the
``https`` document a confidential client needs.

A third journey covers the fourth arm, ``fauna:feed:read:account``
(``docs/goal/behavior/authorization-server.md`` § Scope grammar → *The fourth
arm*): the bare arm alone reaches no custom-feed read and no viewer state; a
ceremony adding the account arm reads the account's own feed and its like
state, and never a gated post.

A fourth journey covers the author arm, ``fauna:feed:read:author`` (§ Scope
grammar → *The author arm*): the bare arm alone is refused the author read; a
ceremony consenting the author arm pages one author's public posts with no
viewer state, and one adding the account arm reads them as the account.

No wall-clock assertion anywhere (convention 14): the ``exp`` close is pinned
at tier_1 (``principal_session::tests::the_session_closes_at_its_tokens_exp``)
against tokio's paused clock.
"""

import base64
import json
import secrets
import threading
import time

import cbor2
import pytest
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ed25519, x25519

import fauna_ffi
from helpers.atproto_consent import (  # noqa: F401 — consent_nest/consent_nest_env/consent_bridge are pytest fixtures adopted by import
    HANDLE_DOMAIN,
    consent_bridge,
    consent_nest,
    consent_nest_env,
)
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.blob_upload import SEALED_MIME, upload_blob
from helpers.oauth_client import NONCE_HEADER, OAuthClient
from helpers.principal_session import (
    CLOSE_AUTH_EXPIRED,
    DECLARED,
    REDIRECT_URI,
    PrincipalSession,
    alice_client,
    ceremony,
    events_poll,
    refusal_code,
    refused_status,
    upgrade,
)

pytestmark = pytest.mark.tier_3

PERSONALIZATION = "fauna:personalization:rw"
EVENTS = "fauna:events:subscribe"
#: How long the events long-poll is asked to hold — the door's own ceiling.
#: The assertion is on what it answers, never on how long it took.
LONG_POLL_WAIT_S = 25
REGISTRY_KIND = "fauna.state.personalization"
MODEL_KIND = "fauna.personalization.model"
#: A delegable-rung kind the personalization twin does not carry.
UNGRANTED_KIND = "fauna.state.moderation"
TOPIC = "topic:" + "c4" * 16
STATE_FEED = "state-entry"


def _raw(public_key) -> bytes:
    return public_key.public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)


def _secret(private_key) -> bytes:
    return private_key.private_bytes(
        serialization.Encoding.Raw, serialization.PrivateFormat.Raw, serialization.NoEncryption()
    )


@pytest.mark.feature("connected-apps")
def test_a_principal_session_is_gated_by_ceiling_scope_and_revoke(consent_nest, consent_bridge):
    nest = consent_nest
    client, tokens = ceremony(nest, DECLARED)
    token = tokens["access_token"]

    # ── The upgrade gate. ──
    status, headers = refused_status(nest, client, token, with_proof=False)
    assert status == 401 and "invalid_token" in headers.get("www-authenticate", ""), (
        status,
        headers,
    )
    assert headers.get(NONCE_HEADER.lower()), "every refusal carries a fresh DPoP-Nonce"

    stranger = OAuthClient(
        base=nest["url"], htu_origin=f"https://{HANDLE_DOMAIN}", redirect_uri=REDIRECT_URI
    )
    stranger.nonce = client.nonce
    status, _ = refused_status(nest, client, token, proof_signer=stranger)
    assert status == 401, "a proof under a key the token is not bound to opens nothing"

    # ── The session. ──
    session = PrincipalSession(upgrade(nest, client, token))
    ok, reply = session.call("fauna.feed.trending.posts", {})
    assert ok, f"a kind inside the principal's scopes answers: {reply!r}"
    ok, reply = session.call("fauna.capabilities.fetch", {})
    assert ok and reply.get("grants") == [], (
        f"the session kind answers — empty, since the client attested no key: {reply!r}"
    )
    ok, reply = session.call("fauna.principals.list", {})
    assert not ok and refusal_code(reply).endswith("permission_denied"), (
        f"a User-only kind is above the ThirdParty ceiling: {reply!r}"
    )

    # ── The scope check, with one arm: narrow the row under the live session. ──
    narrow_client, narrow_tokens = ceremony(nest, "openid")
    assert narrow_client.client_id == client.client_id, "the same client, re-consenting"
    status, headers = refused_status(nest, narrow_client, narrow_tokens["access_token"])
    assert status == 403 and "insufficient_scope" in headers.get("www-authenticate", ""), (
        f"a token holding no Fauna scope opens no session: {status} {headers!r}"
    )

    ok, reply = session.call("fauna.feed.trending.posts", {})
    assert not ok and refusal_code(reply).endswith("permission_denied"), (
        f"the row now grants only openid, so the live session's feed read is refused: {reply!r}"
    )
    ok, reply = session.call("fauna.capabilities.fetch", {})
    assert ok, f"a session kind needs no scope and still answers: {reply!r}"

    # ── Revoke: the live socket closes 4401, and the token opens nothing more. ──
    with alice_client(nest) as ws:
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

    status, _ = refused_status(nest, client, token)
    assert status == 401, "an unexpired token of a revoked principal opens no session"


def _family(access_token: str) -> bytes:
    """The grant family an access token was minted under — its ``sid``,
    decoded: the grant id the user's app revokes by."""

    def unpad(s: str) -> bytes:
        return base64.urlsafe_b64decode(s + "=" * (-len(s) % 4))

    return unpad(json.loads(unpad(access_token.split(".")[1]))["sid"])


@pytest.mark.feature("connected-apps")
def test_revoking_one_grant_closes_only_that_familys_session(consent_nest, consent_bridge):
    nest = consent_nest
    first_client, first = ceremony(nest, DECLARED)
    second_client, second = ceremony(nest, DECLARED)
    assert first_client.client_id == second_client.client_id, "one client, one principal"
    first_family = _family(first["access_token"])
    assert first_family != _family(second["access_token"]), "two ceremonies, two families"

    revoked_session = PrincipalSession(upgrade(nest, first_client, first["access_token"]))
    sibling_session = PrincipalSession(upgrade(nest, second_client, second["access_token"]))
    for session in (revoked_session, sibling_session):
        ok, reply = session.call("fauna.capabilities.fetch", {})
        assert ok, f"both families' sessions serve: {reply!r}"

    # ── The user revokes ONE grant from their own app. ──
    with alice_client(nest) as ws:
        grants = [
            g
            for g in ws.call("fauna.bridges.atproto.list_grants", {})["grants"]
            if g["client_id"] == first_client.client_id
        ]
        assert first_family in [bytes(g["grant_id"]) for g in grants], (
            f"the token's family is a grant the user's app lists: {grants!r}"
        )
        revoked = ws.call("fauna.bridges.atproto.revoke_session", {"session_id": first_family})
    assert revoked["revoked"] is True, revoked
    assert revoked_session.close_code() == CLOSE_AUTH_EXPIRED, (
        "the revoked family's live socket closes 4401, not at its token's exp"
    )
    ok, reply = sibling_session.call("fauna.capabilities.fetch", {})
    assert ok, f"the sibling family's session keeps serving: {reply!r}"

    status, headers = refused_status(nest, first_client, first["access_token"])
    assert status == 401 and "invalid_token" in headers.get("www-authenticate", ""), (
        f"an unexpired token of an ended family opens no session: {status} {headers!r}"
    )


def _approve_personalization(nest, client, holder_pub: bytes, grant_id: bytes) -> dict:
    """PAR → the browser page → the owner deposits the arm's grant twin and
    approves → the code is redeemed: the order the approving machines run.
    Returns the token reply."""
    request_uri = client.push_authorization_request()
    browser_code, flow_token = client.open_consent_page(request_uri)
    alice = nest["user"]
    now = int(time.time())
    with alice_client(nest) as ws:
        pending = ws.call("fauna.bridges.atproto.list_pending_consents", {})["consents"]
        consent = next((c for c in pending if c["code"] == browser_code), None)
        assert consent, f"no pending consent carries {browser_code!r}: {pending!r}"
        assert bytes(consent["holder_x25519"]) == holder_pub, consent
        blob = fauna_ffi.build_personalization_grant(
            bytes(alice["signing_key"]), grant_id, holder_pub, now, now + 3600
        )
        minted = ws.call("fauna.capabilities.mint", {"grant_blob": blob})
        assert minted.get("ok") is True, minted
        resolved = ws.call(
            "fauna.bridges.atproto.resolve_consent",
            {"consent_id": consent["consent_id"], "approved": True},
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
    return tokens


@pytest.mark.feature("connected-apps")
def test_the_personalization_arm_reads_and_trains_the_accounts_models(
    consent_nest, consent_bridge
):
    nest = consent_nest
    alice = nest["user"]
    holder = x25519.X25519PrivateKey.generate()
    holder_pub = _raw(holder.public_key())
    client = OAuthClient(
        base=nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri=REDIRECT_URI,
        scope=f"{PERSONALIZATION} {EVENTS}",
        holder_x25519=holder_pub,
    )

    # The owner's registry row on the delegable scope — what one of the
    # account's own apps wrote when the factor was trained.
    owner_device = ed25519.Ed25519PrivateKey.generate()
    owner_writer = _raw(owner_device.public_key())
    item_key, envelope = fauna_ffi.account_state_seal_delegable_row(
        bytes(alice["signing_key"]), _secret(owner_device), REGISTRY_KIND, TOPIC,
        b"learn_from_engagement", 1,
    )
    state_put = {
        "scope": "state",
        "writer_id": owner_writer.hex(),
        "writer_seq": 1,
        "item_key": item_key,
        "op": "state-put",
        "entry": envelope,
    }
    with alice_client(nest) as ws:
        ws.call("fauna.account.state.put", state_put)

    # A row of a delegable kind the twin does not carry — on the same
    # listed scope, so only the grant's wraps keep the principal out of it.
    other_device = ed25519.Ed25519PrivateKey.generate()
    other_writer = _raw(other_device.public_key())
    ungranted_key, ungranted_envelope = fauna_ffi.account_state_seal_delegable_row(
        bytes(alice["signing_key"]), _secret(other_device), UNGRANTED_KIND, "labeler",
        b"hide", 1,
    )
    with alice_client(nest) as ws:
        ws.call(
            "fauna.account.state.put",
            {
                **state_put,
                "writer_id": other_writer.hex(),
                "item_key": ungranted_key,
                "entry": ungranted_envelope,
            },
        )

    grant_id = secrets.token_bytes(16)
    tokens = _approve_personalization(nest, client, holder_pub, grant_id)
    session = PrincipalSession(upgrade(nest, client, tokens["access_token"]))

    # ── The grant twin: both pairs, over the principal's own session. ──
    ok, reply = session.call("fauna.capabilities.fetch", {})
    assert ok, reply
    grants = [bytes(g) for g in reply.get("grants", [])]
    assert len(grants) == 1, f"the principal holds exactly the consent-time twin: {reply!r}"
    tuples = cbor2.loads(grants[0])["scope"]
    assert sorted((t["class"], t["kind"]) for t in tuples) == [
        ("content.read", MODEL_KIND),
        ("content.read", REGISTRY_KIND),
    ], tuples

    # ── Train and read a model, as the account. ──
    sealed_model = secrets.token_bytes(48)
    ok, reply = session.call(
        "fauna.personalization.model.put",
        {"factor": TOPIC, "sealed_blob": sealed_model, "sample_count": 7},
    )
    assert ok, f"the arm trains the account's model: {reply!r}"
    ok, reply = session.call("fauna.personalization.model.fetch", {"factor": TOPIC})
    assert ok and bytes(reply["sealed_blob"]) == sealed_model, reply
    with alice_client(nest) as ws:
        owned = ws.call("fauna.personalization.model.fetch", {"factor": TOPIC})
    assert bytes(owned["sealed_blob"]) == sealed_model, (
        f"the principal's put is the account's own model row: {owned!r}"
    )

    # ── List `state`, read only, and open the registry row with the twin. ──
    ok, reply = session.call(
        "fauna.sync.changes.list", {"scope": "state", "item_class": STATE_FEED}
    )
    assert ok, f"the arm lists the delegable scope: {reply!r}"
    rows = [c for c in reply["changes"] if c.get("path_hash") == item_key.hex()]
    assert len(rows) == 1, f"the owner's registry row is listed: {reply!r}"
    opened = fauna_ffi.personalization_open_state_row(
        grants[0], _secret(holder), owner_writer, rows[0]["origin_seq"], item_key,
        bytes(rows[0]["entry"]),
    )
    assert opened["kind"] == REGISTRY_KIND and opened["key"] == TOPIC, opened
    assert bytes(opened["value"]) == b"learn_from_engagement", opened

    # ── An ungranted kind: listed beside it, opened by no pair the twin wraps. ──
    ungranted = [c for c in reply["changes"] if c.get("path_hash") == ungranted_key.hex()]
    assert len(ungranted) == 1, f"the owner's {UNGRANTED_KIND} row is listed: {reply!r}"
    with pytest.raises(RuntimeError, match="fauna_personalization_open_state_row"):
        fauna_ffi.personalization_open_state_row(
            grants[0], _secret(holder), other_writer, ungranted[0]["origin_seq"],
            ungranted_key, bytes(ungranted[0]["entry"]),
        )

    # ── The events doors: the owner's next `state` put answers the long-poll. ──
    status, first = events_poll(nest, client, tokens["access_token"], None, wait=0)
    assert status == 200 and first["frames"] == [{"scope": "state"}], (
        f"the arm lists `state`, so the poll names it: {first!r}"
    )
    held: dict = {}

    def long_poll():
        held["reply"] = events_poll(
            nest, client, tokens["access_token"], first["cursor"], wait=LONG_POLL_WAIT_S
        )

    poller = threading.Thread(target=long_poll)
    poller.start()
    next_key, next_envelope = fauna_ffi.account_state_seal_delegable_row(
        bytes(alice["signing_key"]), _secret(owner_device), REGISTRY_KIND, TOPIC,
        b"learn_from_dwell", 2,
    )
    with alice_client(nest) as ws:
        ws.call(
            "fauna.account.state.put",
            {**state_put, "writer_seq": 2, "item_key": next_key, "entry": next_envelope},
        )
    poller.join(timeout=LONG_POLL_WAIT_S + 2 * RPC_ROUNDTRIP_S)
    assert not poller.is_alive(), "the long-poll answered"
    status, after = held["reply"]
    assert status == 200 and after["frames"] == [{"scope": "state"}], (
        f"the long-poll from the cursor names the owner's `state` put: {after!r}"
    )
    assert after["cursor"] > first["cursor"], after

    # ── What the arm never reaches. ──
    ok, reply = session.call("fauna.account.state.put", {**state_put, "writer_seq": 3})
    assert not ok and refusal_code(reply).endswith("permission_denied"), (
        f"a principal never writes the registry: {reply!r}"
    )
    ok, reply = session.call("fauna.personalization.model.delete", {"factor": TOPIC})
    assert not ok and refusal_code(reply).endswith("permission_denied"), (
        f"destruction is the owner's: {reply!r}"
    )
    ok, reply = session.call(
        "fauna.sync.changes.list",
        {"scope": "ext:ext.example.com.notes", "item_class": STATE_FEED},
    )
    assert not ok and refusal_code(reply).endswith("permission_denied"), (
        f"the list dispatches on the scope — an ext: scope is the records arm's: {reply!r}"
    )

    # ── Revoke: the twin goes with the principal (`encryption-at-rest.md` §
    # Capability tiering → *Third-party holders*, the per-kind revoke bound). ──
    with alice_client(nest) as ws:
        assert grant_id in [
            bytes(g) for g in ws.call("fauna.capabilities.reconcile", {})["grant_ids"]
        ], "precondition: the twin is on the nest"
        row = next(
            p
            for p in ws.call("fauna.principals.list", {})["principals"]
            if p["client_id"] == client.client_id
        )
        revoked = ws.call(
            "fauna.principals.revoke", {"principal_id": bytes(row["principal_id"])}
        )
        assert revoked["revoked"] is True, revoked
        owned = [bytes(g) for g in ws.call("fauna.capabilities.reconcile", {})["grant_ids"]]
    assert grant_id not in owned, "the revoke deletes the twin nest-side; no fetch serves it"
    assert session.close_code() == CLOSE_AUTH_EXPIRED, "the revoke closes the live socket 4401"
    status, _ = refused_status(nest, client, tokens["access_token"])
    assert status == 401, "an unexpired token of a revoked principal opens no session"
#: The fourth arm's client: its document declares both feed arms, so one
#: ceremony may consent the bare arm alone and a later one add the account arm.
ACCOUNT_DECLARED = "fauna:feed:read fauna:feed:read:account openid"

#: The five feed kinds a principal session may reach.
FIVE_KINDS = (
    "fauna.feed.list",
    "fauna.feed.get",
    "fauna.feed.posts",
    "fauna.feed.local.posts",
    "fauna.feed.trending.posts",
)


@pytest.mark.feature("connected-apps")
def test_the_account_arm_reads_the_accounts_feeds_over_public_posts(
    consent_nest, consent_bridge
):
    """``fauna:feed:read:account`` (``docs/goal/behavior/authorization-server.md``
    § Scope grammar → *The fourth arm*; the refinement of the two timelines,
    ``apps/bridges.md`` rule (c)): with the bare arm alone the custom-feed
    reads are refused and the local timeline carries no viewer state; after a
    ceremony adding the account arm the session reads the account's own feed,
    the public post the owner liked over its first-party connection shows
    ``viewer_liked``, and the gated post the owner can read never appears on
    any of the five kinds."""
    nest = consent_nest
    alice = nest["user"]
    secret = bytes(alice["signing_key"])

    # ── Fixture setup over the account's own connection (convention 8 (b)). ──
    gold_key = bytes(range(32))
    with alice_client(nest) as ws:
        ws.call(
            "fauna.subscriptions.tiers.create",
            {
                "name": "gold",
                "rank": 2,
                "description": None,
                "price_hint": "5 EUR / month",
                "payment_url": "https://pay.example/gold",
                "auto_approve": False,
                "encrypted_upload": fauna_ffi.build_tier_birth_upload(secret, "gold", gold_key),
            },
        )
    gated_body, sealed = fauna_ffi.build_gated_post(
        secret,
        "Restricted: accountarmbeacon",
        "Restricted: accountarmbeacon\n\nThe part you pay for.",
        "gold",
        2,
        b"\x11" * 32,
        gold_key,
    )
    upload_blob(
        nest["port"], alice["token"], sealed,
        audience_class="PeriodRestrictedPost", mime=SEALED_MIME,
    )
    with alice_client(nest) as ws:
        gated = ws.call("fauna.posts.create", {"body": gated_body})["post_id"]
        public = ws.call(
            "fauna.posts.create",
            {"body": fauna_ffi.build_post(secret, "Public: accountarmbeacon")},
        )["post_id"]
        for post in (public, gated):
            ws.call("fauna.posts.interact", {"post_id": post, "action": "like"})
        feed_id = ws.call(
            "fauna.feed.create",
            {"name": "Account arm", "rules": [], "combination": "any"},
        )["feed_id"]
        own = ws.call("fauna.feed.posts", {"feed_id": feed_id})["posts"]
    assert {gated, public} <= {p["post_id"] for p in own}, (
        f"the control: the owner's own read of its feed holds both posts: {own!r}"
    )

    def payload(kind: str) -> dict:
        return {"feed_id": feed_id} if kind in ("fauna.feed.get", "fauna.feed.posts") else {}

    # ── The bare arm alone. ──
    client, tokens = ceremony(nest, DECLARED, declared=ACCOUNT_DECLARED)
    bare = PrincipalSession(upgrade(nest, client, tokens["access_token"]))
    for kind in ("fauna.feed.list", "fauna.feed.get", "fauna.feed.posts"):
        ok, reply = bare.call(kind, payload(kind))
        assert not ok and refusal_code(reply).endswith("permission_denied"), (
            f"the bare arm never covers {kind}: {reply!r}"
        )
    ok, reply = bare.call("fauna.feed.local.posts", {})
    assert ok, reply
    rows = {p["post_id"]: p for p in reply["posts"]}
    assert public in rows and gated not in rows, (
        f"the bare arm reads public posts only: {sorted(rows)!r}"
    )
    assert not rows[public]["viewer_liked"], (
        f"the bare arm reads nothing of the account's own state: {rows[public]!r}"
    )

    # ── A ceremony adding the account arm. ──
    wide_client, wide_tokens = ceremony(nest, ACCOUNT_DECLARED, declared=ACCOUNT_DECLARED)
    assert wide_client.client_id == client.client_id, "the same client, re-consenting wider"
    session = PrincipalSession(upgrade(nest, wide_client, wide_tokens["access_token"]))
    replies = {}
    for kind in FIVE_KINDS:
        ok, reply = session.call(kind, payload(kind))
        assert ok, f"{kind} answers under the account arm: {reply!r}"
        replies[kind] = reply
    feeds = [f["feed_id"] for f in replies["fauna.feed.list"]["feeds"]]
    assert feed_id in feeds, f"the account's own feed is listed: {feeds!r}"
    assert replies["fauna.feed.get"]["feed_id"] == feed_id
    for kind in ("fauna.feed.posts", "fauna.feed.local.posts"):
        rows = {p["post_id"]: p for p in replies[kind]["posts"]}
        assert public in rows, f"{kind}: the public post: {sorted(rows)!r}"
        assert rows[public]["viewer_liked"], (
            f"{kind}: the owner's like, made over its first-party session: {rows[public]!r}"
        )
    for kind in ("fauna.feed.posts", "fauna.feed.local.posts", "fauna.feed.trending.posts"):
        assert gated not in {p["post_id"] for p in replies[kind]["posts"]}, (
            f"{kind}: a gated post the owner can read never leaves the box"
        )
    for kind in ("fauna.feed.list", "fauna.feed.get"):
        assert gated not in repr(replies[kind]), f"{kind} names no gated post"


#: The author arm's client: its document declares the bare arm, the author
#: arm and the account arm, so one ceremony may consent the bare arm alone,
#: a second the author arm, and a third add the account arm's refinement.
AUTHOR_DECLARED = "fauna:feed:read fauna:feed:read:author fauna:feed:read:account openid"


@pytest.mark.feature("connected-apps")
def test_the_author_arm_reads_one_authors_public_posts(consent_nest, consent_bridge):
    """``fauna:feed:read:author`` (``docs/goal/behavior/authorization-server.md``
    § Scope grammar → *The author arm*; ``docs/goal/ui/feed.md`` § The read
    model → *The author read*): the bare arm alone is refused the author read
    at the gate; after a ceremony consenting the author arm the session pages
    the owner's posts public-only — the gated post the owner can read never
    appears — with no viewer state, and reaches no timeline; a ceremony adding
    the account arm reads the same posts as the account, its like state
    filled."""
    nest = consent_nest
    alice = nest["user"]
    secret = bytes(alice["signing_key"])
    author = bytes(alice["actor_id_bytes"]).hex()

    # ── Fixture setup over the account's own connection (convention 8 (b)). ──
    # A tier of its own: the module-scoped nest already holds the account
    # arm's journey's tier.
    platinum_key = bytes(range(1, 33))
    with alice_client(nest) as ws:
        ws.call(
            "fauna.subscriptions.tiers.create",
            {
                "name": "platinum",
                "rank": 3,
                "description": None,
                "price_hint": "9 EUR / month",
                "payment_url": "https://pay.example/platinum",
                "auto_approve": False,
                "encrypted_upload": fauna_ffi.build_tier_birth_upload(
                    secret, "platinum", platinum_key
                ),
            },
        )
    gated_body, sealed = fauna_ffi.build_gated_post(
        secret,
        "Restricted: authorarmbeacon",
        "Restricted: authorarmbeacon\n\nThe part you pay for.",
        "platinum",
        3,
        b"\x12" * 32,
        platinum_key,
    )
    upload_blob(
        nest["port"], alice["token"], sealed,
        audience_class="PeriodRestrictedPost", mime=SEALED_MIME,
    )
    with alice_client(nest) as ws:
        gated = ws.call("fauna.posts.create", {"body": gated_body})["post_id"]
        public = ws.call(
            "fauna.posts.create",
            {"body": fauna_ffi.build_post(secret, "Public: authorarmbeacon")},
        )["post_id"]
        ws.call("fauna.posts.interact", {"post_id": public, "action": "like"})
        own = ws.call("fauna.feed.author.posts", {"author": author})["posts"]
    assert {gated, public} <= {p["post_id"] for p in own}, (
        f"the control: the owner's own author read holds both posts: {own!r}"
    )

    # ── The bare arm alone. ──
    client, tokens = ceremony(nest, DECLARED, declared=AUTHOR_DECLARED)
    bare = PrincipalSession(upgrade(nest, client, tokens["access_token"]))
    ok, reply = bare.call("fauna.feed.author.posts", {"author": author})
    assert not ok and refusal_code(reply).endswith("permission_denied"), (
        f"the bare arm never covers the author read: {reply!r}"
    )

    # ── A ceremony consenting the author arm. ──
    author_client, author_tokens = ceremony(
        nest, "fauna:feed:read:author openid", declared=AUTHOR_DECLARED
    )
    assert author_client.client_id == client.client_id, "the same client, re-consenting"
    session = PrincipalSession(upgrade(nest, author_client, author_tokens["access_token"]))
    ok, reply = session.call("fauna.feed.author.posts", {"author": author})
    assert ok, f"the author arm reaches the author read: {reply!r}"
    rows = {p["post_id"]: p for p in reply["posts"]}
    assert public in rows and gated not in rows, (
        f"the author arm reads public posts only: {sorted(rows)!r}"
    )
    assert not rows[public]["viewer_liked"], (
        f"the author arm alone reads nothing of the account's own state: {rows[public]!r}"
    )
    ok, reply = session.call("fauna.feed.local.posts", {})
    assert not ok and refusal_code(reply).endswith("permission_denied"), (
        f"the author arm covers no timeline: {reply!r}"
    )

    # ── A ceremony adding the account arm's refinement. ──
    wide_client, wide_tokens = ceremony(
        nest, "fauna:feed:read:author fauna:feed:read:account openid", declared=AUTHOR_DECLARED
    )
    wide = PrincipalSession(upgrade(nest, wide_client, wide_tokens["access_token"]))
    ok, reply = wide.call("fauna.feed.author.posts", {"author": author})
    assert ok, f"the author read under both arms: {reply!r}"
    rows = {p["post_id"]: p for p in reply["posts"]}
    assert public in rows and gated not in rows, (
        f"the account's view, public posts only: {sorted(rows)!r}"
    )
    assert rows[public]["viewer_liked"], (
        f"the account arm fills the owner's like state: {rows[public]!r}"
    )
