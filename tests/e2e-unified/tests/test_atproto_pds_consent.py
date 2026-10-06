"""tier_3: the F4 OAuth **consent ceremony**, end to end across four processes.

This is D3 rung 2's definition of success (``docs/goal/behavior/atproto-pds-full.md``
§ F4 detail — *Consent ceremony wire flow*): an external ATProto app starts an
OAuth sign-in, the browser it opens shows a short **binding code**, the user's
own Fauna app shows *the same code* on an approval card, and approving it there
— over the user's own authed WS-RPC connection, so the grant is Ed25519-rooted
and the browser never holds a Fauna secret — is what releases the browser's
authorization code.

Four processes, and the wire between them is the whole point:

    external OAuth client  →  the nest's authorization server (/oauth/par,
                              /oauth/authorize — the pending consent row)
                           →  the tui app (list_pending_consents, resolve_consent)
                           →  back out through the nest's long-poll, and the
                              code redeemed at the nest's /oauth/token
                           →  the Go PDS bridge, the RESOURCE server, which
                              honours the nest-minted access token

The authorization server moved from the bridge onto the nest (TP5); the bridge's
own AS retired in the change that re-pointed its protected-resource document at
the nest's issuer (``authorization-server.md`` § The issuer). So the last hop —
a nest-minted token authenticating an XRPC call at the PDS — is the proof that
the re-point, the resource server's teaching and the retirement landed together.

**What this catches that nothing below it can.** The code comparison is the
ceremony's entire defence against a phished consent push, and it only means
anything if the two surfaces render *one* value — which is a claim about a nest
mint reaching the consent page and a Rust settings machine by two different
routes. Unit tests on either side pin their own half against their own fixture;
only this leg pins the halves against each other (the finding-13 rule: assert the
two sides AGREE, never that each equals a literal). Likewise the release: that
approving in the app is what unblocks the browser is a statement about a nest
nudge waking a held poll in another process.

**Deliberately unassigned.** The pushed request carries no ``login_hint``, so
the consent row names no account and fans out to nobody — which makes
``list_pending_consents`` the only way it is ever found (§ WS-RPC kind surface:
*"not optional"*). That is the harder path of the two and the one the poll
fallback exists for; the assigned/push path's own wiring is unit-pinned
(``Resync::for_push``).

**Landed on tui (F4 slice 6c), linux, web, macos, ios (all 2026-08-02
trickle-down), windows (2026-08-05) and android (also 2026-08-02) — all seven
apps this file can mark.** android's consent card (`atproto-consent-card` /
`-code` / `-approve` / `-deny`, matching ui.yaml exactly) is Robolectric-proven
in `AtprotoSettingsContentTest.kt` (9 tests: one card per pending request, the
raw binding code, approve/deny firing `resolveConsent`, scope description
text). `--client android` has never run against a real device
(host-emulator-gated fleet-wide per 's ruling), so
the tests below carry the `android` marker as a parse-only coverage-contract
claim, never a device-run claim — the two are different questions, and
MATRIX.md's pass/fail cell for android stays blank until that device run
actually lands.

**Budget.** The nest meters each authorization-server route in its own bucket
and bounds loopback against a separate aggregate ceiling
(`bins/fauna-nest/src/oauth_as_rate_limit.rs`), so this module's ceremonies no
longer share the bridge's old 10-per-5-minutes `ClassAuth` bucket. The rule
that bucket taught still stands: **a negative already pinned at unit level does
not belong in this file** — spend a request here only on something only four
processes can prove.
"""

import base64
import json
import ssl
import time
import urllib.error
import urllib.parse
import urllib.request

import pytest

from helpers.atproto_consent import (  # noqa: F401 — consent_nest/consent_nest_env/consent_bridge are pytest fixtures adopted by import
    HANDLE_DOMAIN,
    PERMISSION_SET_AUDIENCE,
    PERMISSION_SET_MEMBER,
    PERMISSION_SET_NSID,
    PERMISSION_SET_TITLE,
    UNRESOLVABLE_SET_NSID,
    consent_bridge,
    consent_nest,
    consent_nest_env,
)
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.oauth_client import OAuthClient

pytestmark = [pytest.mark.tier_3]


@pytest.fixture
def consent_spa_url(static_dir, consent_nest):
    """Function-scoped SPA proxy → the dedicated ``consent_nest``, so a web
    browser in ``consent_app`` can reach it without CORS.

    Mirrors ``atproto_hosted_spa_url`` (conftest.py) — the session ``spa_url``
    only proxies the SHARED ``nest_instance``. Without this override,
    ``_login_app_as``'s default ``spa_url_fixture`` points web at that shared
    nest instead, where alice (registered only on ``consent_nest``) is
    unregistered: every RPC then fails silently and the machine's snapshot
    never leaves its pre-fetch defaults (``hosted_allowed: false``), which
    reads exactly like the hosted rungs staying gated rather than an auth
    failure — found via a real run: `wait_for_depth_level("hosted_full")`
    timed out at its full budget, not on a fast retry.
    """
    from conftest import _serve_spa_proxy

    url, server = _serve_spa_proxy(static_dir, consent_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture
def consent_app(request, app, consent_nest, consent_bridge):
    """`app`, logged in as alice with the full-PDS panel — the consent card's
    home — showing.

    Depends on `consent_bridge` so the identity mint has already landed: the
    ladder was arranged over the wire in `consent_nest`, so `ensure_hosted_full`
    here is the assertion that the app really sees that level, not a second way
    of setting it.
    """
    from conftest import _login_app_as

    _login_app_as(
        app, request, consent_nest, consent_nest["user"],
        spa_url_fixture="consent_spa_url",
    )
    app.atproto_settings.ensure_hosted_full()
    return app


@pytest.mark.linux
@pytest.mark.web
@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.feature("atproto")
def test_consent_ceremony_end_to_end(consent_app, consent_bridge, consent_nest):
    """The whole ceremony: PAR at the nest → the browser's page → the app's
    card (same code) → approve in the app → the browser's poll releases an
    authorization code → the nest's token endpoint → an XRPC call the PDS
    resource server honours.
    """
    app = consent_app
    client = OAuthClient(
        base=consent_nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri="http://127.0.0.1:17771/callback",
        resource_base=consent_bridge["base"],
        resource_htu_origin=consent_bridge["htu_origin"],
    )

    # How many requests are already live. Deliberately a BASELINE rather than an
    # assertion that it is zero: every unassigned request on this nest is listed
    # to every account, so a sibling test's live row is a legitimate neighbour,
    # not contamination. Asserting an empty list would make test order
    # load-bearing and would still not prove the card below is ours — the code
    # does that.
    before = app.atproto_settings.consent_count()

    # ── The external app pushes its authorization request. ──
    request_uri = client.push_authorization_request()
    assert request_uri.startswith("urn:ietf:params:oauth:request_uri:"), request_uri

    # ── The browser lands on the consent page and is shown a binding code. ──
    browser_code, flow_token = client.open_consent_page(request_uri)
    assert browser_code, "the consent page rendered an empty binding code"

    # ── The user's own app shows the SAME request. The re-list is what finds
    # it: this request named no account, so it fanned out to nobody. ──
    #
    # THIS lookup is the assertion the whole leg exists for. The app is asked for
    # a card carrying the code the BROWSER was shown — two surfaces reached by
    # two different routes from one nest mint — so finding it at all is the
    # agreement, and it is an agreement between the two sides rather than
    # between each side and a fixture literal (the finding-13 rule).
    app.atproto_settings.navigate()
    index = app.atproto_settings.wait_for_consent_code(browser_code)
    assert index is not None, (
        "no approval card is showing the code the browser was shown. The user's "
        "whole job is comparing the two, so a divergence — or a card that never "
        f"arrives — makes the ceremony's anti-phishing check meaningless. "
        f"browser={browser_code!r} cards="
        f"{[app.atproto_settings.consent_code(i) for i in range(app.atproto_settings.consent_count())]!r} "
        f"error={app.atproto_settings.current_error_text()!r}"
    )
    assert app.atproto_settings.consent_count() == before + 1, (
        "opening one consent must add exactly one card"
    )

    # The card names who is asking, verbatim, and says what they asked for.
    card = app.atproto_settings.consent_card_text(index)
    assert "localhost" in card, f"the card must render the client_id verbatim: {card!r}"
    assert "account identity" in card.lower(), (
        "the requested `atproto` scope must render in human-readable form (the "
        f"same describe_scope wording the browser page shows): {card!r}"
    )

    # ── The user approves, in their own app, over their own authed connection. ──
    app.atproto_settings.approve_consent(index)
    assert app.atproto_settings.wait_for_consent_count(before, timeout=RPC_ROUNDTRIP_S), (
        "an answered request must stop rendering as a card. "
        f"error={app.atproto_settings.current_error_text()!r}"
    )
    assert app.atproto_settings.consent_index_for_code(browser_code) is None, (
        "the answered request specifically — not merely one card — must be gone"
    )
    assert app.atproto_settings.current_error_text() == "", (
        "a clean approval leaves no page error"
    )

    # ── …and that is what releases the browser. The poll holds while pending,
    # so this loop is not a settle-sleep: each call is the mechanism, and the
    # bound is how many holds we are willing to wait out. ──
    redirect = ""
    deadline = time.monotonic() + RPC_ROUNDTRIP_S
    while time.monotonic() < deadline:
        status, redirect = client.poll(flow_token)
        if status == "resolved":
            break
    assert redirect, (
        "approving in the app must release the browser's poll with the flow's "
        "one answer"
    )
    assert redirect.startswith("http://127.0.0.1:17771/callback?"), redirect
    assert "code=" in redirect, f"an approval must redirect with an authorization code: {redirect}"
    assert f"state={client.state}" in redirect, (
        f"the client's own state must come back verbatim (RFC 6749 §10.12): {redirect}"
    )
    query = urllib.parse.parse_qs(urllib.parse.urlsplit(redirect).query)
    assert query.get("iss") == [f"https://{HANDLE_DOMAIN}"], (
        f"RFC 9207 requires the issuer — the NEST — on the response: {redirect}"
    )
    assert "error=" not in redirect, redirect

    # ── The released code is REDEEMABLE, and what it buys works. ──
    #
    # Everything above proves the ceremony; this proves it was worth completing.
    tokens = client.exchange_code(redirect)
    assert tokens.get("token_type") == "DPoP", (
        "these tokens are DPoP-bound, so a client told `Bearer` would present one "
        f"without a proof and be refused for a reason its own code never chose: {tokens}"
    )
    assert tokens.get("access_token") and tokens.get("refresh_token"), tokens
    assert tokens.get("scope") == "atproto", (
        f"the grant must carry the scope set the user actually approved: {tokens}"
    )

    # The access token the NEST minted authenticates a real XRPC call at the
    # PDS — the resource server honours the nest's issuer and verifies against
    # the key set the nest feeds it — over the DPoP scheme with a per-request
    # proof, and resolves to the account that approved it. This is the re-point
    # seen from the outside: before it, the PDS refused a nest-minted token on
    # both its issuer and its key.
    status, session = client.authed_xrpc_get("com.atproto.server.getSession", tokens["access_token"])
    assert status == 200, (
        f"the PDS must honour an access token the nest minted: {status} {session}\n"
        + consent_bridge["tail"]()
    )
    assert session.get("did") == consent_bridge["did"], (
        "the authenticated call must resolve to the approving account, not merely "
        f"to some account: {session}"
    )

    # Rotation works, and the superseded token is dead — the nest's own
    # rotate-on-use registry, which the OAuth plane rides rather than
    # reimplementing.
    rotated = client.refresh(tokens["refresh_token"])
    assert rotated.get("access_token") and rotated.get("refresh_token"), rotated
    assert rotated["refresh_token"] != tokens["refresh_token"], (
        "a rotation must issue a NEW refresh token"
    )
    # ⚠ The refusals this journey could also check — a replayed refresh token,
    # a re-redeemed code — are deliberately NOT here: both are store-level rules
    # pinned at unit level in the nest's token plane (`oauth_as_token.rs`,
    # `oauth_as_routes.rs`). What only four processes can show is above: the
    # exchange, the call the resource server honours, and the rotation crossing
    # into the nest's own registry.


@pytest.mark.linux
@pytest.mark.web
@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.feature("atproto")
def test_the_nests_own_authorization_server_issues_and_revokes_a_grant(
    consent_app, consent_bridge, consent_nest
):
    """The grant the nest's authorization server issues: its claim set, its
    audit row, its rotation and its revocation.

    It needs four processes for one reason: **an approval by an account with no
    ACTIVE ATProto identity is refused**, so the ceremony cannot reach a code
    without the bridge that mints one. That is why the nest-side refusal suite
    lives in ``tests/api/test_oauth_issuer.py``, which spins no bridge, and why
    this — the happy path — is here. That the PDS honours the token is
    ``test_consent_ceremony_end_to_end``'s to show.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    app = consent_app
    client, tokens = _nest_ceremony(app, consent_nest)
    assert tokens.get("scope") == "atproto", (
        f"the grant must carry the scope set the user actually approved: {tokens}"
    )

    # The ratified `iss`/`aud` split, read off the wire rather than trusted.
    # An access token names the nest as issuer (the AS that minted it) and the
    # set of its readers as audience — for this ATProto-only grant the PDS
    # service DID alone, spelled as a plain string (`authorization-server.md`
    # § The issuer → *The audience is the set of readers*). Read WITHOUT
    # verifying, on purpose:
    # this asserts what the claim set SAYS, and verifying it here would only be
    # re-running the unit tests through a slower path.
    claims = _jwt_claims(tokens["access_token"])
    assert claims["iss"] == f"https://{HANDLE_DOMAIN}", (
        f"the access token must name the nest that minted it: {claims!r}"
    )
    assert claims["aud"] == f"did:web:pds.{HANDLE_DOMAIN}", (
        "an ATProto-only grant's one reader is the RESOURCE SERVER — the PDS — "
        f"not the authorization server that issued it: {claims!r}"
    )
    assert claims["cnf"]["jkt"], (
        f"a token with no confirmation claim is a bearer token in disguise: {claims!r}"
    )

    # ── The grant is an AUDIT ROW the user can see. ──
    #
    # `principles.md` forbids a capability with no audit row, so a token answered
    # before its grant was recorded would be exactly the failure the write
    # ordering exists to prevent. Read through the account's own kind, which is
    # the path the user's connected-apps surface takes.
    alice = consent_nest["user"]
    with WsRpcAdminClient(
        consent_nest["url"],
        actor_id=alice["actor_id_bytes"],
        signing_key=bytes(alice["signing_key"]),
    ) as alice_ws:
        grants = alice_ws.call("fauna.bridges.atproto.list_grants", {})["grants"]
    mine = [g for g in grants if g.get("client_id") == client.client_id]
    assert mine, (
        "the exchange must have recorded a grant the user's own app can list; "
        f"got {grants!r}"
    )

    # ── Rotation, through the nest's own registry. ──
    rotated = client.refresh(tokens["refresh_token"])
    assert rotated.get("access_token") and rotated.get("refresh_token"), rotated
    assert rotated["refresh_token"] != tokens["refresh_token"], (
        "a rotation must issue a NEW refresh token"
    )

    # ── Revocation destroys the GRANT, and the proof is what stops working. ──
    #
    # Asserted by EFFECT rather than by the response, because the response is
    # deliberately uninformative: this endpoint answers an identical empty 200 to
    # every token outcome so as not to be a validity oracle. So the evidence that
    # anything happened is that the live refresh token no longer rotates.
    status, body = client.revoke(rotated["refresh_token"])
    assert status == 200, f"revocation answered {status}: {body!r}"
    assert body == "", f"RFC 7009's success answer carries no body: {body!r}"

    status, refused = client.token_request_expecting_refusal(
        {"grant_type": "refresh_token", "refresh_token": rotated["refresh_token"]}
    )
    assert status == 400, (
        f"a revoked grant must not still rotate: {status} {refused!r}"
    )
    assert refused.get("error") == "invalid_grant", refused


def _nest_ceremony(app, consent_nest, scope="atproto", on_card=None):
    """Run the consent ceremony against the NEST's own authorization server.

    Push → consent page → approve in the user's app → poll → redeem the code
    at the nest's ``/oauth/token``. Returns the ``OAuthClient`` (its DPoP key
    is what every later request must be bound to) and the token reply. The
    ceremony is one helper because two tests need a *live* grant of their own:
    the revocation proof and the forced session-secret rotation proof both
    assert by effect — "this refresh token no longer rotates" — and one token
    cannot witness both.

    ``on_card`` runs against the rendered card's text before it is approved —
    the one moment the card exists — for a test that has something to say about
    what the user was shown.
    """
    # The nest's own AS: dialled on loopback, but proving `htu` over the apex it
    # was claimed onto — the distinction `helpers/dpop.py` exists to warn about.
    client = OAuthClient(
        base=consent_nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri="http://127.0.0.1:17772/callback",
        scope=scope,
    )

    before = app.atproto_settings.consent_count()
    request_uri = client.push_authorization_request()
    assert request_uri.startswith("urn:ietf:params:oauth:request_uri:"), request_uri

    browser_code, flow_token = client.open_consent_page(request_uri)
    assert browser_code, "the nest's consent page rendered an empty binding code"

    app.atproto_settings.navigate()
    index = app.atproto_settings.wait_for_consent_code(browser_code)
    assert index is not None, (
        "the nest-hosted ceremony must reach the user's own app with the code "
        f"the browser was shown. browser={browser_code!r} "
        f"error={app.atproto_settings.current_error_text()!r}"
    )
    if on_card is not None:
        on_card(app.atproto_settings.consent_card_text(index))
    app.atproto_settings.approve_consent(index)
    assert app.atproto_settings.wait_for_consent_count(before, timeout=RPC_ROUNDTRIP_S), (
        "an answered request must stop rendering as a card. "
        f"error={app.atproto_settings.current_error_text()!r}"
    )

    redirect = ""
    deadline = time.monotonic() + RPC_ROUNDTRIP_S
    while time.monotonic() < deadline:
        status, redirect = client.poll(flow_token)
        if status == "resolved":
            break
    assert redirect, "approving in the app must release the nest's poll"
    assert "code=" in redirect, f"an approval must redirect with a code: {redirect}"
    assert "error=" not in redirect, redirect

    # ── The code redeems at the NEST's token endpoint. ──
    tokens = client.exchange_code(redirect)
    assert tokens.get("token_type") == "DPoP", (
        f"these tokens are DPoP-bound; `Bearer` would break the client: {tokens}"
    )
    assert tokens.get("access_token") and tokens.get("refresh_token"), tokens
    return client, tokens


@pytest.mark.feature("atproto")
def test_a_permission_set_resolves_through_the_bridge_onto_the_card_and_into_the_token(
    consent_app, consent_bridge, consent_nest
):
    """The nest→bridge permission-set request call, end to end
    (``atproto-oauth-provider.md`` § Implementation status today, the
    2026-09-25 bullet).

    The nest's own ``/oauth/par`` cannot resolve an ``include:<NSID>`` — the
    chain lives on the bridge — so it pushes ``permission_set_requested`` to
    the connected PDS bridge and the bridge answers ``deliver_permission_set``
    with the verified bytes. What this asserts is the CROSSING: the scope the
    client asked for reaches the card the user is shown as the set's own title,
    and the grant the user approves carries the member the set expanded to,
    with the include's audience applied — not the ``include:`` string itself,
    which is never a grantable value.

    The document is served through the e2e-flavor fixture seam; the honest
    chain is the Go side's own tests' to prove. The unresolvable arm below is
    what shows the seam serves ONLY the mapped set.
    """
    include = f"include:{PERMISSION_SET_NSID}?aud={PERMISSION_SET_AUDIENCE}"
    seen = {}

    def on_card(text):
        seen["card"] = text

    client, tokens = _nest_ceremony(
        consent_app, consent_nest, scope=f"atproto {include}", on_card=on_card
    )

    card = seen.get("card", "")
    assert PERMISSION_SET_NSID in card and PERMISSION_SET_TITLE in card, (
        "the card must attribute the expanded members to the permission set "
        f"they came from, by NSID and title; card={card!r}"
    )

    expanded = f"rpc:{PERMISSION_SET_MEMBER}?aud={PERMISSION_SET_AUDIENCE}"
    granted = set(tokens.get("scope", "").split())
    assert {"atproto", expanded} <= granted, (
        f"the token must carry the set's expansion, got {tokens.get('scope')!r}"
    )
    assert not any(s.startswith("include:") for s in granted), (
        f"an include: is a request for an expansion, never a grant: {granted}"
    )
    assert f"rpc:com.atproto.server.deleteSession?aud={PERMISSION_SET_AUDIENCE}" not in granted, (
        "a member outside the set's own namespace is ignored by the hierarchy "
        f"constraint, never granted: {granted}"
    )


@pytest.mark.feature("atproto")
def test_an_unresolvable_permission_set_is_refused_with_the_bridge_present(
    consent_bridge, consent_nest
):
    """The closed-world answer, with a bridge connected: a set the bridge's
    chain cannot resolve refuses ``invalid_scope`` naming the set — the
    client's own string — and never an unverified expansion.

    Distinct from ``test_oauth_issuer.py``'s bridge-ABSENT refusal: here the
    push goes out, the bridge really runs its chain (``.invalid`` never
    resolves), and its delivered refusal is what answers the PAR, promptly,
    rather than the nest's deadline.
    """
    client = OAuthClient(
        base=consent_nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri="http://127.0.0.1:17772/callback",
        scope=f"atproto include:{UNRESOLVABLE_SET_NSID}",
    )
    status, body = client.push_authorization_request_expecting_refusal()
    assert status == 400, f"expected 400, got {status}: {body!r}"
    assert body.get("error") == "invalid_scope", body
    assert UNRESOLVABLE_SET_NSID in body.get("error_description", ""), (
        f"the refusal names the set and nothing else: {body!r}"
    )
    # That the refusal came from the bridge's delivery rather than the nest's
    # deadline is not a wall-clock claim this test makes (convention 14); the
    # Go handler test pins that a refusal IS delivered, and the nest's unit
    # test pins that a delivered refusal releases the waiter at once.


def test_a_forced_session_secret_rotation_kills_every_live_refresh_token(
    consent_app, consent_bridge, consent_nest
):
    """``fauna.oauth.force_rotate_session_secret`` — the refresh plane's half
    of the compromise response (``authorization-server.md`` § The issuer →
    *Two rotation arms*).

    The forced issuer-key arm drops every ES256 key in the store, but the same
    store holds the HS256 secret every OAuth refresh token is MACed under, and
    a forged refresh token would redeem for an access token signed by the
    *new* issuer key — so rotating the issuer key alone leaves the response
    half done. This proves the other half with a REAL grant: a live refresh
    token, freshly rotated through the nest's own registry (so it is the
    strongest token the client holds, not a stale one), is refused as
    ``invalid_grant`` on the very next ``/oauth/token`` read after the admin
    forces the secret — no restart, no sweep, no wait.

    Asserted by effect for the same reason the revocation test is: the door's
    reply says only when it rotated, and what an admin is buying is that
    every token minted before that instant is dead.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    client, tokens = _nest_ceremony(consent_app, consent_nest)
    rotated = client.refresh(tokens["refresh_token"])
    assert rotated.get("refresh_token"), rotated

    alice = consent_nest["user"]
    with WsRpcAdminClient(
        consent_nest["url"],
        actor_id=alice["actor_id_bytes"],
        signing_key=bytes(alice["signing_key"]),
    ) as alice_ws:
        before = alice_ws.call("fauna.bridges.atproto.list_grants", {})["grants"]
    assert [g for g in before if g.get("client_id") == client.client_id], (
        f"precondition: the ceremony's grant is listed as connected: {before!r}"
    )

    admin = consent_nest["admin"]
    with WsRpcAdminClient(
        consent_nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    ) as admin_ws:
        forced = admin_ws.call("fauna.oauth.force_rotate_session_secret", {})
    assert forced["rotated_at"] > 0, forced
    assert forced["replaced_minted_at"], (
        "a nest that has minted real refresh tokens holds a secret to replace, "
        f"and the reply must date it: {forced!r}"
    )
    assert forced.get("grants_ended", 0) >= 1, (
        "the rotation must END the grants it killed and COUNT them — leaving "
        "them listed is the state the re-point ruling condemns, reached "
        f"through a second door: {forced!r}"
    )

    status, refused = client.token_request_expecting_refusal(
        {"grant_type": "refresh_token", "refresh_token": rotated["refresh_token"]}
    )
    assert status == 400, (
        "a refresh token MACed under the replaced secret must not rotate after "
        f"the forced rotation: {status} {refused!r}"
    )
    assert refused.get("error") == "invalid_grant", refused

    # The dead token is dead at the OTHER verifier too: `/oauth/revoke` reads
    # the same secret, so it can no longer recognise the token — and per RFC
    # 7009 answers the same empty 200 it answers everything, which is why the
    # token endpoint above, not this one, carries the proof.
    status, body = client.revoke(rotated["refresh_token"])
    assert status == 200 and body == "", (status, body)

    # And the connected-apps surface agrees with the count: the grant whose
    # refresh family this rotation killed stops being listed, so the user is
    # not left looking at a live-looking connection nothing can be done with
    # (`authorization-server.md` § The issuer -> *The forced session-secret arm
    # ends the grants it kills*). Read through the account's own kind, the same
    # path the user's app takes.
    with WsRpcAdminClient(
        consent_nest["url"],
        actor_id=alice["actor_id_bytes"],
        signing_key=bytes(alice["signing_key"]),
    ) as alice_ws:
        after = alice_ws.call("fauna.bridges.atproto.list_grants", {})["grants"]
    assert not [g for g in after if g.get("client_id") == client.client_id], (
        "a grant whose refresh token the rotation killed must stop being "
        f"listed as connected: {after!r}"
    )


def _jwt_claims(token: str) -> dict:
    """The claim set of a compact JWS, decoded and NOT verified.

    Verification is the resource server's job and the unit tests'; what a
    journey test wants from a token is what it CLAIMS, so decoding is the honest
    operation here — and doing it by hand keeps the test free of a JWT library
    whose defaults could quietly reject a token this server legitimately mints.
    """
    payload = token.split(".")[1]
    payload += "=" * (-len(payload) % 4)
    return json.loads(base64.urlsafe_b64decode(payload))


@pytest.mark.feature("atproto")
def test_the_pds_sends_clients_to_the_nests_authorization_server(consent_bridge):
    """**The re-point and the retirement, from the outside, on the real bridge.**

    The PDS's protected-resource document names the NEST's issuer, and the PDS
    serves no authorization server of its own: no AS document, no JWKS, and none
    of the request-taking OAuth routes. The two halves are one fact
    (``authorization-server.md`` § The issuer → *The re-point, the teaching, and
    the bridge AS's retirement are ONE change*): a PDS that named the nest while
    still answering ``/oauth/revoke`` would let a client revoke a bridge-minted
    token at the wrong server and be told it worked.

    Every endpoint the nest's own document advertises is dialled in
    ``tests/api/test_oauth_issuer.py``.
    """
    base = consent_bridge["base"]
    ctx = ssl._create_unverified_context()

    with urllib.request.urlopen(
        f"{base}/.well-known/oauth-protected-resource", timeout=30, context=ctx
    ) as r:
        assert r.status == 200, r.status
        resource = json.loads(r.read().decode())
    assert resource["resource"] == consent_bridge["htu_origin"], resource
    assert resource["authorization_servers"] == [f"https://{HANDLE_DOMAIN}"], (
        f"the PDS must send clients to the nest's issuer: {resource}"
    )
    assert resource["bearer_methods_supported"] == ["DPoP"], (
        f"DPoP is mandatory in ATProto, so advertising bearer would invite a "
        f"presentation this server refuses: {resource}"
    )

    for path in (
        "/.well-known/oauth-authorization-server",
        "/oauth/jwks",
        "/oauth/par",
        "/oauth/authorize",
        "/oauth/token",
        "/oauth/revoke",
    ):
        req = urllib.request.Request(f"{base}{path}", method="GET")
        try:
            with urllib.request.urlopen(req, timeout=30, context=ctx) as r:
                code = r.status
        except urllib.error.HTTPError as e:
            code = e.code
        assert code == 404, (
            f"the PDS still serves {path} (HTTP {code}); the bridge's own "
            "authorization server retired with the re-point"
        )


@pytest.mark.linux
@pytest.mark.web
@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.feature("atproto")
def test_declining_fails_the_browser_cleanly(consent_app, consent_bridge, consent_nest):
    """A decline is *recorded*, not dismissed locally — so the waiting browser
    gets a clean `access_denied` refusal into its own callback instead of
    hanging until the flow times out.

    This is the half a purely local "close the card" implementation would pass
    on the app side while stranding every real client.
    """
    app = consent_app
    client = OAuthClient(
        base=consent_nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri="http://127.0.0.1:17772/callback",
    )

    request_uri = client.push_authorization_request()
    browser_code, flow_token = client.open_consent_page(request_uri)

    app.atproto_settings.navigate()
    index = app.atproto_settings.wait_for_consent_code(browser_code)
    assert index is not None, f"no card is showing this flow's code {browser_code!r}"

    app.atproto_settings.deny_consent(index)
    deadline = time.monotonic() + RPC_ROUNDTRIP_S
    while time.monotonic() < deadline:
        if app.atproto_settings.consent_index_for_code(browser_code) is None:
            break
        time.sleep(0.3)
    assert app.atproto_settings.consent_index_for_code(browser_code) is None, (
        "a declined request must stop rendering as a card"
    )

    redirect = ""
    deadline = time.monotonic() + RPC_ROUNDTRIP_S
    while time.monotonic() < deadline:
        status, redirect = client.poll(flow_token)
        if status == "resolved":
            break
    assert redirect, "a decline must release the browser rather than leave it holding"
    assert "error=access_denied" in redirect, (
        f"a decline redirects with access_denied, not a code: {redirect}"
    )
    assert "code=" not in redirect, (
        f"a declined request must never release an authorization code: {redirect}"
    )
