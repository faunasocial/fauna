"""tier_3: what a third-party app signed in to a Fauna-hosted PDS can NOT do.

Three ``[nest]`` outcomes of the atproto feature page, each a limit on an app
the user let in, and each observable only where the app stands — at the PDS's
XRPC surface, across the nest and the real ``fauna-atproto-bridge``:

- **Credentials and the account stay Fauna's** (``atproto-pds-full.md`` § Problem
  4, D6's *fauna-surface* refusals): an app signed in as the user — on either
  credential plane — can never create, list or revoke app passwords, or delete
  the account, and the refusal says where those things live.
- **A grant keeps the permissions its card showed** (``atproto-oauth-provider.md``
  § F4 detail, *Expansion is FROZEN at the ceremony*): when a permission set's
  publisher changes the set after the user approved it, the standing grant does
  not move — while a NEW ceremony sees the change, which is what keeps the
  first half from passing vacuously.
- **Revoking external posting keeps what was posted** (``atproto-pds-full.md``
  § D10, *Revocation*): a post an app published under the authoring delegation
  is still served after the user revokes it, while the next write is refused.

**No app drives any of it.** Every approval here goes over alice's own authed
WS-RPC connection — the exact ``list_pending_consents`` / ``resolve_consent``
calls a Fauna app makes — because the subject is the nest and the bridge, not
the approval card (whose own journeys live in ``test_atproto_pds_consent.py``).
Credentials and the delegation are arranged the same way (e2e convention 8's
fixture-setup carve-out): the behaviour under test is the external protocol,
which by construction has no Fauna app UI.
"""

import base64
import json
import time

import pytest

from helpers.atproto_consent import (  # noqa: F401 — consent_nest/consent_nest_env/consent_bridge are pytest fixtures adopted by import
    HANDLE_DOMAIN,
    PERMISSION_SET_AUDIENCE,
    PERMISSION_SET_MEMBER,
    PERMISSION_SET_NSID,
    PERMISSION_SET_TITLE,
    consent_bridge,
    consent_nest,
    consent_nest_env,
    open_first_emit_gate,
)
from helpers.atproto_fakes import dag_cbor
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.oauth_client import OAuthClient
from helpers.xrpc_client import (
    load_app_credential_fixture,
    xrpc_get,
    xrpc_post,
)

pytestmark = pytest.mark.tier_3

#: The four methods outcome 29 names, plus ``requestAccountDelete`` — the first
#: step of the ecosystem's two-step account delete, so refusing only the second
#: would leave the first half-served. (method, HTTP verb, body).
ACCOUNT_AND_CREDENTIAL_METHODS = [
    ("com.atproto.server.createAppPassword", "POST", {"name": "escalated"}),
    ("com.atproto.server.listAppPasswords", "GET", None),
    ("com.atproto.server.revokeAppPassword", "POST", {"name": "ivory"}),
    ("com.atproto.server.requestAccountDelete", "POST", {}),
    ("com.atproto.server.deleteAccount", "POST", {
        "did": "did:plc:unused", "password": "unused", "token": "unused",
    }),
]

#: The set's publisher widening it after the user approved: the member the
#: card showed plus one the card never did.
WIDENED_MEMBER = "com.example.calendar.sync.pull"
WIDENED_SET_DOCUMENT = dag_cbor({
    "lexicon": 1,
    "id": PERMISSION_SET_NSID,
    "defs": {
        "main": {
            "type": "permission-set",
            "title": PERMISSION_SET_TITLE,
            "permissions": [
                {"resource": "rpc", "lxm": PERMISSION_SET_MEMBER, "inheritAud": True},
                {"resource": "rpc", "lxm": WIDENED_MEMBER, "inheritAud": True},
            ],
        }
    },
})

POST_TEXT = "posted from a third-party app before its posting was revoked"
AFTER_REVOKE_TEXT = "this post was written after the revoke and must never exist"


@pytest.fixture(scope="module")
def consent_nest_binary(bluesky_nest_binary):
    """The `bluesky` build: the outcome-35 test makes a repo write, whose nest
    kind (`ingest_external_write`) only that build registers. A superset of the
    default build for every other test here."""
    return bluesky_nest_binary


def _alice_ws(consent_nest):
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    alice = consent_nest["user"]
    return WsRpcAdminClient(
        consent_nest["url"],
        actor_id=alice["actor_id_bytes"],
        signing_key=bytes(alice["signing_key"]),
    )


def _app_password_session(consent_bridge, consent_nest, credential_id: str) -> tuple[str, str]:
    """Provision an app password the way a Fauna app mints one, then sign in
    with it the way a third-party app does. Returns (accessJwt, did)."""
    secret, verifier = load_app_credential_fixture()
    with _alice_ws(consent_nest) as ws:
        reply = ws.call(
            "fauna.bridges.atproto.provision_app_credential",
            {"credential_id": credential_id, "label": credential_id,
             "verifier": verifier, "dm_allowed": False},
        )
    assert reply.get("ok") is True, reply
    status, sess = xrpc_post(
        consent_bridge["base"], "com.atproto.server.createSession",
        {"identifier": "alice", "password": secret},
    )
    assert status == 200, f"createSession failed: {status} {sess}\n{consent_bridge['tail']()}"
    return sess["accessJwt"], sess["did"]


def _approve_without_an_app(consent_nest, client) -> tuple[dict, dict]:
    """One whole ceremony, approved over alice's own WS-RPC connection.

    Push → the browser's consent page → the card row alice's apps would render
    (found by the code the browser was shown — the request names no account, so
    listing is the only way it is ever found) → approve → the browser's poll
    releases a code → redeemed at the nest's token endpoint. Returns
    ``(card_row, tokens)``.
    """
    request_uri = client.push_authorization_request()
    browser_code, flow_token = client.open_consent_page(request_uri)
    assert browser_code, "the consent page rendered an empty binding code"

    with _alice_ws(consent_nest) as ws:
        rows = ws.call("fauna.bridges.atproto.list_pending_consents", {})["consents"]
        mine = [row for row in rows if row["code"] == browser_code]
        assert len(mine) == 1, (
            f"exactly one card must carry the browser's code {browser_code!r}: {rows!r}"
        )
        card = mine[0]
        resolved = ws.call(
            "fauna.bridges.atproto.resolve_consent",
            {"consent_id": card["consent_id"], "approved": True},
        )
    assert resolved["resolved"], f"the approval did not resolve: {resolved!r}"

    # The poll holds while pending, so each call IS the mechanism; the bound is
    # how many holds this test will wait out (convention 14).
    redirect = ""
    deadline = time.monotonic() + RPC_ROUNDTRIP_S
    while time.monotonic() < deadline:
        status, redirect = client.poll(flow_token)
        if status == "resolved":
            break
    assert "code=" in redirect and "error=" not in redirect, (
        f"approving must release the browser's poll with a code: {redirect!r}"
    )
    tokens = client.exchange_code(redirect)
    assert tokens.get("access_token") and tokens.get("refresh_token"), tokens
    return card, tokens


def _jwt_claims(token: str) -> dict:
    """A compact JWS's claim set, decoded and NOT verified — what the token
    SAYS; verifying it is the resource server's job and the unit tests'."""
    payload = token.split(".")[1]
    payload += "=" * (-len(payload) % 4)
    return json.loads(base64.urlsafe_b64decode(payload))


def _assert_fauna_surface_refusal(plane: str, nsid: str, status: int, body: dict) -> None:
    assert status != 200, (
        f"{plane}: {nsid} must be refused to an app signed in as the user, "
        f"and it answered {status} {body!r}"
    )
    assert body.get("error") != "MethodNotImplemented", (
        f"{plane}: {nsid} answered the DEFERRED flavour ('not implemented'), but "
        "D6 refuses it as fauna-surface — the capability exists, only in Fauna "
        f"apps, and the refusal must say so: {status} {body!r}"
    )
    assert "fauna app" in body.get("message", "").lower(), (
        f"{plane}: {nsid}'s refusal must tell the app where account and credential "
        f"management lives: {status} {body!r}"
    )


@pytest.mark.feature("atproto")
def test_an_app_signed_in_as_you_cannot_manage_app_passwords_or_delete_the_account(
    consent_bridge, consent_nest
):
    """Outcome 29, on both credential planes an app can sign in with.

    The OAuth grant asked for is the widest the PDS issues to an app
    (``transition:generic`` is the ecosystem's app-password-equivalent grant),
    so "never" is witnessed against the most an app can hold, not against a
    grant too narrow to reach anything.

    Then the effect, read through the account's own kind: the one app password
    the user minted is still the only one, so nothing was minted or revoked
    behind the refusals.
    """
    access, _did = _app_password_session(consent_bridge, consent_nest, "ivory")

    client = OAuthClient(
        base=consent_nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri="http://127.0.0.1:17791/callback",
        scope="atproto transition:generic",
        resource_base=consent_bridge["base"],
        resource_htu_origin=consent_bridge["htu_origin"],
    )
    _card, tokens = _approve_without_an_app(consent_nest, client)

    # Control: both sessions are genuinely signed in, so the refusals below
    # cannot be an authentication failure wearing a policy refusal's status.
    status, got = xrpc_get(consent_bridge["base"], "com.atproto.server.getSession", access)
    assert status == 200, f"app-password session: {status} {got}"
    status, got = client.authed_xrpc_get("com.atproto.server.getSession", tokens["access_token"])
    assert status == 200, f"OAuth session: {status} {got}\n{consent_bridge['tail']()}"

    for nsid, verb, body in ACCOUNT_AND_CREDENTIAL_METHODS:
        if verb == "GET":
            status, reply = xrpc_get(consent_bridge["base"], nsid, access)
        else:
            status, reply = xrpc_post(consent_bridge["base"], nsid, body, access)
        _assert_fauna_surface_refusal("app password", nsid, status, reply)

        if verb == "GET":
            status, reply = client.authed_xrpc_get(nsid, tokens["access_token"])
        else:
            status, reply = client.authed_xrpc_post(nsid, tokens["access_token"], body)
        _assert_fauna_surface_refusal("OAuth", nsid, status, reply)

    with _alice_ws(consent_nest) as ws:
        listed = ws.call("fauna.bridges.atproto.list_app_credentials", {})
    ids = sorted(c["credential_id"] for c in listed["credentials"])
    assert ids == ["ivory"], (
        "the user's own app passwords must be exactly the one they minted — "
        f"no app minted or revoked one behind the refusals: {listed!r}"
    )


@pytest.mark.feature("atproto")
def test_posts_published_through_another_app_stay_up_after_posting_is_revoked(
    consent_bridge, consent_nest
):
    """Outcome 35 (D10 § Revocation): revoke destroys the sub-key, and
    already-published posts stay valid.

    The SAME session token is used throughout, so what changes between the
    accepted write and the refused one is the delegation alone — the refusal
    is the control that the revoke really happened, which is what keeps "the
    post is still there" from passing vacuously.
    """
    from fauna_ffi import build_authoring_delegation_cert

    open_first_emit_gate(consent_bridge)
    access, did = _app_password_session(consent_bridge, consent_nest, "poster")
    base = consent_bridge["base"]

    def create(text: str):
        return xrpc_post(base, "com.atproto.repo.createRecord", {
            "repo": did,
            "collection": "app.bsky.feed.post",
            "record": {
                "$type": "app.bsky.feed.post",
                "text": text,
                "createdAt": time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime()),
            },
        }, access)

    # The user turns external posting ON: the D10 ceremony, minted by the same
    # shared-Rust minter all seven apps use.
    alice_secret = bytes(consent_nest["user"]["signing_key"])
    with _alice_ws(consent_nest) as ws:
        k_pub = bytes(ws.call("fauna.bridges.atproto.fetch_authoring_key", {})["k_pub"])
        now_micros = int(time.time() * 1_000_000)
        cert = build_authoring_delegation_cert(
            alice_secret, k_pub, ["Post"], created_at_micros=now_micros,
            expires_at_micros=now_micros + 3_600_000_000,  # +1 h
        )
        provisioned = ws.call(
            "fauna.bridges.atproto.provision_authoring_delegation", {"cert": cert}
        )
    assert provisioned.get("ok") is True, provisioned

    status, created = create(POST_TEXT)
    assert status == 200, f"createRecord failed: {status} {created}\n{consent_bridge['tail']()}"
    post_uri = created["uri"]
    rkey = post_uri.rsplit("/", 1)[-1]
    with _alice_ws(consent_nest) as ws:
        fauna_post_id = _await_feed_post(ws, POST_TEXT)

    # ── The user turns external posting OFF. ──
    with _alice_ws(consent_nest) as ws:
        revoked = ws.call("fauna.bridges.atproto.revoke_authoring_delegation", {})
    assert revoked.get("revoked") is True, revoked

    status, refused = create(AFTER_REVOKE_TEXT)
    assert status >= 400, (
        "with the delegation revoked, the next post must be refused — a 200 "
        f"means the revoke never took effect: {status} {refused}"
    )

    # ── …and what was already published stays up, on both faces. ──
    status, got = xrpc_get(
        base, f"com.atproto.repo.getRecord?repo={did}"
        f"&collection=app.bsky.feed.post&rkey={rkey}",
    )
    assert status == 200 and got.get("uri") == post_uri, (
        f"the PDS must still serve the post after the revoke: {status} {got}"
    )
    assert got.get("value", {}).get("text") == POST_TEXT, got
    with _alice_ws(consent_nest) as ws:
        posts = ws.call("fauna.feed.local.posts", {"limit": 100})["posts"]
    assert fauna_post_id in {p["post_id"] for p in posts}, (
        "the post must still be on the account's own Fauna feed after the "
        f"revoke: {[p['body'] for p in posts]!r}"
    )
    assert AFTER_REVOKE_TEXT not in {p["body"] for p in posts}, posts


def _await_feed_post(ws, body: str, budget_s: float = 60) -> str:
    """The Fauna post id of `body` once the account's own feed — the wire all
    seven apps render from — serves it. A deadline poll on observable state
    (convention 14); a green run returns on the first pass."""
    deadline = time.monotonic() + budget_s
    seen: list[str] = []
    while time.monotonic() < deadline:
        posts = ws.call("fauna.feed.local.posts", {"limit": 100})["posts"]
        seen = [p["body"] for p in posts]
        for p in posts:
            if p["body"] == body:
                return p["post_id"]
        time.sleep(1)  # sleep-ok: poll interval inside a deadline poll on observable state (convention 14), not a settle-sleep
    pytest.fail(f"{body!r} never reached the account's own feed; saw {seen!r}")


@pytest.mark.feature("atproto")
def test_an_approved_app_keeps_the_permissions_its_card_showed_after_the_set_changes(
    consent_bridge, consent_nest
):
    """Outcome 33: expansion is frozen at the ceremony; a standing grant never
    re-resolves.

    The set's publisher WIDENS it after the user approved — the direction that
    matters, since a widening is a permission the user never saw. The bridge
    reads its set documents once at start, so the change is a bridge restart
    serving the new document. Then, in order:

    1. a FRESH ceremony for the same set sees the new member — the change is
       live, so the next assertion is not passing because nothing changed;
    2. the standing grant, refreshed, still carries exactly what its card
       showed, and the new member is in neither the reply nor the token.

    Last in the module, and it puts the original document back, because the
    bridge is module-scoped.
    """
    include = f"include:{PERMISSION_SET_NSID}?aud={PERMISSION_SET_AUDIENCE}"
    shown = f"rpc:{PERMISSION_SET_MEMBER}?aud={PERMISSION_SET_AUDIENCE}"
    widened = f"rpc:{WIDENED_MEMBER}?aud={PERMISSION_SET_AUDIENCE}"

    def client(port: int) -> OAuthClient:
        return OAuthClient(
            base=consent_nest["url"],
            htu_origin=f"https://{HANDLE_DOMAIN}",
            redirect_uri=f"http://127.0.0.1:{port}/callback",
            scope=f"atproto {include}",
        )

    approved = client(17792)
    card, tokens = _approve_without_an_app(consent_nest, approved)
    granted = set(tokens["scope"].split())
    assert {"atproto", shown} <= granted and widened not in granted, (
        f"precondition: the grant carries the set as first published: {tokens['scope']!r}"
    )
    card_scopes = set(card.get("scopes") or [])
    assert granted == card_scopes, (
        "the grant must be exactly the permissions the card showed: "
        f"card={card!r} grant={tokens['scope']!r}"
    )

    try:
        consent_bridge["restart"](WIDENED_SET_DOCUMENT)

        _fresh_card, fresh = _approve_without_an_app(consent_nest, client(17793))
        assert widened in set(fresh["scope"].split()), (
            "a NEW ceremony must see the publisher's change — otherwise the "
            "standing grant's assertion below would pass because nothing "
            f"changed: {fresh['scope']!r}\n{consent_bridge['tail']()}"
        )

        rotated = approved.refresh(tokens["refresh_token"])
        assert rotated.get("access_token") and rotated.get("refresh_token"), rotated
        if "scope" in rotated:
            assert set(rotated["scope"].split()) == granted, (
                "a refresh must not re-resolve the set: the standing grant keeps "
                f"what its card showed. before={tokens['scope']!r} "
                f"after={rotated['scope']!r}"
            )
        token_scope = set(_jwt_claims(rotated["access_token"]).get("scope", "").split())
        assert token_scope == granted, (
            "the refreshed ACCESS TOKEN must carry the frozen expansion, never "
            f"the widened set: {sorted(token_scope)!r} vs {sorted(granted)!r}"
        )
        assert widened not in token_scope, token_scope
    finally:
        consent_bridge["restart"]()
