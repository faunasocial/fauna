"""tier_3: the Settings → **Connected apps** page, end to end
(``docs/goal/ui/connected-apps.md``; the third-party chain's S6).

The page is the one roster of everything acting for the user from outside the
seven apps, plus the two app halves of the polled consent starts
(``docs/goal/behavior/authorization-server.md`` § Consent → *How the two polled
starts are built*):

- **Connect an app** — a device app shows the user an RFC 8628 ``user_code``;
  typing it here claims the request (``fauna.oauth.consent.lookup_code``) and
  opens the built consent card; approving releases the device app's poll.
- **Requests** — a quiet push (``/oauth/bc-authorize`` by handle) from a client
  the user never approved raises no notification, so the tray is where it is
  found; *Never show requests from this app* blocks the client.
- **Revoke** — one per row, the verb chosen by the shared machine; revoking a
  consent-minted principal ends its grant families, so the client's refresh is
  refused.
- **Mail app passwords** — rows of the same roster, each with its exact login,
  its kind, and reveal / copy for its secret; they no longer render on Mail &
  Calendar, which keeps Add password and Rotate keys.
- **Blocked apps** — a blocked client is listed while it is blocked, and
  Unblock lifts the block so its next request is shown again.
- **The oracle record** — a principal the oracle served (here, a service-auth
  token the bridge minted for it) shows on its row what the nest did with the
  user's keys for it: the operations in the current window and the newest
  operation, its detail verbatim (``connected-apps.md`` § Layout & flow,
  region 3). tui only until the other apps' trickle-down; the per-operation
  record's own controls await rule-A approval.
- **The same-device handoff** — a device app pushes a PAR and opens
  ``fauna://consent/<request_uri>``; the user's app takes the route onto this
  page (``fauna.oauth.consent.open_handoff`` through the shared machine) and
  the card lands in the tray; the device polls with
  ``urn:fauna:params:grant-type:handoff`` (§ Consent → *How the same-device
  handoff is built*). The route reaches the app through the ``open_route``
  automation command, the same door its launch argument takes
  (``docs/goal/architecture/apps/tui.md`` § System integration → *In-app
  routes*).

Every mutation goes through the app (convention 8). The fake third-party
client is ``helpers/oauth_client.py`` — the same one the API suite drives the
two starts with (``tests/api/test_oauth_consent_starts.py``) — and the nest +
the real PDS bridge are ``helpers/atproto_consent.py``'s: a grant is refused
unless the approving account's ATProto identity is active, and only the bridge
mints that.

**All seven apps**: tui is the lead app; the others rode the batched
trickle-down rows, each adding its marker as its page landed. The
same-device handoff pair runs on tui, macos, ios and windows (the Apple pair's
intake is ``onOpenURL``'s one FaunaKit door; windows' is the argv leg's
``App.ApplyRoute``, fed by ``open_route``) and gains each other app's marker
when its route intake lands. The mail-row test runs on all seven: web's chunk
builds its own mail machine for the app-password rows.
"""

import sqlite3
import uuid

import pytest
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ed25519, x25519

from actions.connected_apps import ConnectedAppsActions
from helpers.atproto_consent import (  # noqa: F401 — consent_nest/consent_nest_env/consent_bridge are pytest fixtures adopted by import
    HANDLE_DOMAIN,
    PERMISSION_SET_AUDIENCE,
    PERMISSION_SET_NSID,
    PERMISSION_SET_TITLE,
    consent_bridge,
    consent_nest,
    consent_spa_url,
    open_first_emit_gate,
)
from helpers.client_metadata_server import (
    DOCUMENT_PATH,
    ClientMetadataServer,
    client_document,
    manifest_payload,
    sign_manifest,
)
from helpers.xrpc_client import xrpc_post
from helpers.oauth_client import (
    BC_AUTHORIZE_PATH,
    DEVICE_AUTHORIZATION_PATH,
    GRANT_TYPE_DEVICE_CODE,
    GRANT_TYPE_HANDOFF,
    OAuthClient,
)
from clients.ws_rpc_admin_client import WsRpcAdminClient
from i18n.strings import S

pytestmark = [pytest.mark.tier_3]

# The oracle case's client is a publisher's (an https `client_id` whose signed
# manifest declares the one `service_auth` pair the oracle may mint for), so
# this module's nest resolves that publisher's document from a loopback
# metadata server (`tests/api/test_atproto_oracle.py`'s arrangement). Every
# other case's client is a loopback client, which the nest never fetches.
_ORACLE_PUBLISHER = "example.com"
_ORACLE_SCOPE = "fauna:identity:op:atproto.service_auth"
_ORACLE_REDIRECT_URI = "http://127.0.0.1:17798/callback"
_ORACLE_AUD = "did:web:feed.example.com#bsky_fg"
_ORACLE_LXM = "app.bsky.feed.getFeedSkeleton"
_ORACLE_APPS = ("web", "linux", "windows", "macos", "ios", "android", "tui")


def _oracle_document_path(app_name: str) -> str:
    return f"/client-metadata-{app_name}.json"


@pytest.fixture(scope="module")
def metadata_server(tmp_path_factory):
    server = ClientMetadataServer(_ORACLE_PUBLISHER, tmp_path_factory.mktemp("client-metadata"))
    publisher_key = ed25519.Ed25519PrivateKey.generate()
    payload = manifest_payload(_ORACLE_PUBLISHER, publisher_key, ["notes"])
    payload["service_auth"] = [{"aud": _ORACLE_AUD, "lxm": [_ORACLE_LXM]}]
    # One client document per app: the nest, its principals and this module's
    # metadata server outlive a case, so a shared client id would hand the next
    # app a row an earlier app's operation already served.
    for path in (DOCUMENT_PATH, *map(_oracle_document_path, _ORACLE_APPS)):
        server.documents[path] = client_document(
            server.client_id(path),
            _ORACLE_REDIRECT_URI,
            _ORACLE_SCOPE,
            manifest_jws=sign_manifest(publisher_key, payload),
        )
    yield server
    server.close()


@pytest.fixture(scope="module")
def consent_nest_binary(bluesky_nest_binary):
    """The `bluesky` build: the oracle-door test below makes repo writes, whose
    nest kind (`ingest_external_write`) only that build registers. A superset of
    the default build for every other test here."""
    return bluesky_nest_binary


@pytest.fixture(scope="module")
def consent_nest_env(metadata_server):
    """The consent nest's metadata fetcher pointed at ``metadata_server``."""
    return metadata_server.nest_env()


# Distinct loopback redirect ports → distinct client ids (the loopback
# client_id embeds its redirect), so each case is its own third-party app and
# no case sees another's grant or block.
_TYPED_CODE_PORT = 17791
_QUIET_PUSH_PORT = 17792
_UNBLOCK_PORT = 17793
_HANDOFF_APPROVE_PORT = 17794
_HANDOFF_DECLINE_PORT = 17795
_PERMISSION_SET_PORT = 17797
_LAST_USED_PORT = 17798
_BAD_CODE_PORT = 17799


def _client(consent_nest, port: int, scope: str = "atproto") -> OAuthClient:
    return OAuthClient(
        base=consent_nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri=f"http://127.0.0.1:{port}/callback",
        scope=scope,
    )


@pytest.fixture
def connected_app(request, app, consent_nest, consent_bridge):
    """`app`, logged in as alice on the consent nest, on the Connected apps page."""
    from conftest import _login_app_as

    _login_app_as(
        app, request, consent_nest, consent_nest["user"], spa_url_fixture="consent_spa_url"
    )
    ca = app.connected_apps
    ca.navigate()
    assert ca.is_page_visible(), (
        f"the Connected apps page did not render. error: {ca.current_error_text()!r}"
    )
    return app


def _scope_lines(text: str) -> list[str]:
    return [line.strip()[2:] for line in text.splitlines() if line.strip().startswith("• ")]


@pytest.mark.tui
@pytest.mark.android
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("atproto", "connected-apps")
def test_a_typed_code_connects_the_app_and_revoke_ends_it(connected_app, consent_nest):
    """(a) + (b): a device app's typed code → the card → approve → the app's
    poll yields tokens and the roster shows it, scopes in words; Revoke on that
    row → the app's refresh is refused and the row is gone on re-read."""
    ca: ConnectedAppsActions = connected_app.connected_apps
    client = _client(consent_nest, _TYPED_CODE_PORT)

    status, started = client.start_polled(DEVICE_AUTHORIZATION_PATH, {})
    assert status == 200, f"device authorization start refused: {status} {started!r}"

    ca.connect_with_code(started["user_code"])
    index = ca.wait_for_request_from(client.client_id)
    assert index is not None, (
        "typing a live code must open its card in the tray. "
        f"error: {ca.current_error_text()!r}"
    )
    assert client.client_id in ca.request_text(index), "the client id renders verbatim"

    ca.approve_request(index)
    status, tokens = client.poll_token(
        GRANT_TYPE_DEVICE_CODE, "device_code", started["device_code"]
    )
    assert status == 200 and tokens.get("refresh_token"), (
        f"approving in the app must release the device app's poll: {status} {tokens!r}"
    )
    assert ca.current_error_text() == ""

    row = ca.wait_for_item_containing(client.client_id)
    assert row is not None, "the new connected app must appear on the roster"
    scopes = _scope_lines(ca.item_text(row))
    assert scopes, f"the row lists what it may reach: {ca.item_text(row)!r}"
    assert "atproto" not in scopes, (
        f"scopes render in words, never as the token string: {scopes!r}"
    )

    ca.revoke_item(row)
    assert ca.wait_for_no_item_containing(client.client_id), (
        "a revoked row must be gone on re-read. "
        f"error: {ca.current_error_text()!r}"
    )
    status, body = client.token_request_expecting_refusal(
        {"grant_type": "refresh_token", "refresh_token": tokens["refresh_token"]}
    )
    assert status == 400 and body.get("error") == "invalid_grant", (
        f"after Revoke the app's refresh must be refused: {status} {body!r}"
    )


@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.feature("atproto")
def test_a_connected_apps_row_names_the_permission_set_it_came_from(
    connected_app, consent_nest
):
    """A connected app's row says which permission set its permissions came
    from, as the approval card said it (``atproto-oauth-provider.md`` §
    Implementation status today): the grant's frozen sets are composed into the
    roster row by the card's own function, so the set's NSID and title render
    on the row after the permission list. tui leads; web, linux and android
    carry it from the first batched lift, macos, ios and windows follow."""
    ca: ConnectedAppsActions = connected_app.connected_apps
    include = f"include:{PERMISSION_SET_NSID}?aud={PERMISSION_SET_AUDIENCE}"
    client = _client(consent_nest, _PERMISSION_SET_PORT, scope=f"atproto {include}")

    status, started = client.start_polled(DEVICE_AUTHORIZATION_PATH, {})
    assert status == 200, f"device authorization start refused: {status} {started!r}"

    ca.connect_with_code(started["user_code"])
    index = ca.wait_for_request_from(client.client_id)
    assert index is not None, (
        "typing a live code must open its card in the tray. "
        f"error: {ca.current_error_text()!r}"
    )
    ca.approve_request(index)
    status, tokens = client.poll_token(
        GRANT_TYPE_DEVICE_CODE, "device_code", started["device_code"]
    )
    assert status == 200 and tokens.get("refresh_token"), (
        f"approving in the app must release the device app's poll: {status} {tokens!r}"
    )

    row = ca.wait_for_item_containing(client.client_id)
    assert row is not None, "the new connected app must appear on the roster"
    text = ca.item_text(row)
    assert PERMISSION_SET_NSID in text and PERMISSION_SET_TITLE in text, (
        "the row must name the permission set its permissions came from, by "
        f"NSID and title, as the card did: {text!r}"
    )

    ca.revoke_item(row)
    assert ca.wait_for_no_item_containing(client.client_id), (
        f"a revoked row must be gone on re-read. error: {ca.current_error_text()!r}"
    )


@pytest.mark.tui
@pytest.mark.android
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("connected-apps")
def test_a_quiet_push_lands_in_the_tray_and_never_show_blocks_the_app(
    connected_app, consent_nest
):
    """(c): a quiet push from a never-approved client is found in the Requests
    tray; *Never show requests from this app* removes it, blocks the client
    nest-side, and a later push from it renders nothing."""
    ca: ConnectedAppsActions = connected_app.connected_apps
    stranger = _client(consent_nest, _QUIET_PUSH_PORT)

    status, pushed = stranger.start_polled(BC_AUTHORIZE_PATH, {"login_hint": "alice"})
    assert status == 200 and pushed.get("auth_req_id"), f"push refused: {status} {pushed!r}"

    index = ca.wait_for_request_from(stranger.client_id)
    assert index is not None, (
        "a quiet push must be findable in the Requests tray. "
        f"error: {ca.current_error_text()!r}"
    )

    ca.block_request(index)
    assert ca.visit(), f"the page must re-read. error: {ca.current_error_text()!r}"
    assert ca.request_index_for_client(stranger.client_id) is None, (
        "a blocked app's request must leave the tray"
    )

    alice = consent_nest["user"]
    with WsRpcAdminClient(
        consent_nest["url"],
        actor_id=alice["actor_id_bytes"],
        signing_key=bytes(alice["signing_key"]),
    ) as ws:
        blocked = ws.call("fauna.oauth.consent.list_blocked_clients", {})["clients"]
        assert stranger.client_id in [b["client_id"] for b in blocked], (
            f"the block is nest state: {blocked!r}"
        )
        try:
            # A second push from the blocked client opens nothing.
            status, _ = stranger.start_polled(BC_AUTHORIZE_PATH, {"login_hint": "alice"})
            assert status == 200, "a blocked push is answered like a real one (no oracle)"
            assert ca.visit(), f"the page must re-read. error: {ca.current_error_text()!r}"
            assert ca.request_index_for_client(stranger.client_id) is None, (
                "no future request from a blocked app renders"
            )
        finally:
            ws.call(
                "fauna.oauth.consent.block_client",
                {"client_id": stranger.client_id, "blocked": False},
            )


@pytest.mark.tui
@pytest.mark.android
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("connected-apps")
def test_the_lifted_rows_render_here_and_no_longer_on_their_old_pages(connected_app):
    """(d): the consent card and the connected-app rows left the atproto page,
    and the NIP-46 connections left the Nostr page — their old IDs are ABSENT
    there, asserted rather than assumed (a lift, never a duplication). The
    names are retired from ui.yaml (2026-10-03) and survive here only as the
    legacy ids a regression would re-paint."""
    app = connected_app
    app.atproto_settings.navigate()
    assert app.atproto_settings.is_page_visible()
    for gone in (
        "atproto-consent-card",
        "atproto-connected-app-item",
        "atproto-connected-app-revoke",
    ):
        assert app.driver.count(gone) == 0, f"{gone} still renders on the atproto page"

    app.nostr.navigate()
    assert app.nostr.is_page_visible()
    for gone in ("nostr-bunker-app-item", "nostr-bunker-app-revoke"):
        assert app.driver.count(gone) == 0, f"{gone} still renders on the Nostr page"


def _blocked_client_ids(consent_nest) -> list[str]:
    alice = consent_nest["user"]
    with WsRpcAdminClient(
        consent_nest["url"],
        actor_id=alice["actor_id_bytes"],
        signing_key=bytes(alice["signing_key"]),
    ) as ws:
        blocked = ws.call("fauna.oauth.consent.list_blocked_clients", {})["clients"]
    return [b["client_id"] for b in blocked]


@pytest.mark.tui
@pytest.mark.android
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("connected-apps")
def test_a_blocked_app_is_listed_and_unblock_lifts_the_block(connected_app, consent_nest):
    """(e): a blocked app shows in the Blocked apps section with its client id
    verbatim; Unblock removes the row, lifts the block nest-side, and the app's
    next quiet push is shown in the tray again."""
    ca: ConnectedAppsActions = connected_app.connected_apps
    stranger = _client(consent_nest, _UNBLOCK_PORT)

    status, pushed = stranger.start_polled(BC_AUTHORIZE_PATH, {"login_hint": "alice"})
    assert status == 200 and pushed.get("auth_req_id"), f"push refused: {status} {pushed!r}"
    index = ca.wait_for_request_from(stranger.client_id)
    assert index is not None, (
        f"the push must land in the tray. error: {ca.current_error_text()!r}"
    )
    ca.block_request(index)

    row = ca.wait_for_blocked(stranger.client_id)
    assert row is not None, (
        "a blocked app must be listed under Blocked apps. "
        f"error: {ca.current_error_text()!r}"
    )

    ca.unblock(row)
    assert ca.wait_for_not_blocked(stranger.client_id), (
        "an unblocked app must leave the Blocked apps section. "
        f"error: {ca.current_error_text()!r}"
    )
    assert stranger.client_id not in _blocked_client_ids(consent_nest), (
        "Unblock must lift the block on the nest"
    )

    # The block is gone, so the app's next request is shown again.
    status, _ = stranger.start_polled(BC_AUTHORIZE_PATH, {"login_hint": "alice"})
    assert status == 200
    again = ca.wait_for_request_from(stranger.client_id)
    assert again is not None, (
        "after Unblock the app's next request must be shown in the tray. "
        f"error: {ca.current_error_text()!r}"
    )
    ca.decline_request(again)  # leave the tray as it was


@pytest.mark.tui
@pytest.mark.android
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.web
@pytest.mark.feature("connected-apps")
def test_a_mail_app_password_is_a_roster_row_and_no_longer_on_the_mail_page(logged_in_app):
    """(f): a mail app password made on Mail & Calendar is a row of this roster
    — its name, the app-password badge, its kind, the exact login a mail app
    uses, and its secret on demand — and Revoke on that row removes it. Mail &
    Calendar no longer lists the passwords; it keeps Add password and Rotate
    keys."""
    app = logged_in_app
    mail = app.mail_settings
    ca: ConnectedAppsActions = app.connected_apps
    mail.navigate()
    mail.ensure_mail_enabled()

    name = f"Laptop {uuid.uuid4().hex[:4]}"
    chosen = "RosterRowPassword01Aa"
    mail.add_credential_plain(name, chosen)

    row = ca.wait_for_item_containing(name)
    assert row is not None, (
        "a new mail app password must appear on the Connected apps roster. "
        f"error: {ca.current_error_text()!r}"
    )
    assert S.connected_apps.class_app_password in ca.item_text(row)
    mail_rows = ca.mail_row_items()
    assert row in mail_rows, "the row must carry the mail app-password leaves"
    n = mail_rows.index(row)
    assert ca.mail_name(n) == name
    assert ca.mail_kind(n) == S.settings.mail.kind_password
    login = ca.mail_login(n)
    credential_id = name.lower().replace(" ", "-")
    assert login.split("@")[0].endswith(f"+{credential_id}") and "{" not in login, (
        f"the row must show the exact mail-app login; reads {login!r}"
    )
    assert ca.reveal_mail_secret(n) == chosen, (
        "revealing the row's secret must return the password that was typed. "
        f"error: {ca.current_error_text()!r}"
    )

    # The lift is a move: Mail & Calendar lists no password, and keeps the two
    # controls that stay with it.
    mail.navigate()
    assert mail.is_page_visible()
    app.driver.wait_for("mail-settings-add-credential-button", timeout=10.0)
    app.driver.wait_for("mail-settings-rotate-keys-button", timeout=10.0)
    for gone in (
        "mail-settings-credential-item",
        "mail-settings-credential-item-name",
        "mail-settings-credential-item-username",
        "mail-settings-credential-item-secret",
        "mail-settings-credential-item-revoke-button",
    ):
        assert app.driver.count(gone) == 0, f"{gone} still renders on Mail & Calendar"

    assert ca.visit(), f"the page must re-read. error: {ca.current_error_text()!r}"
    row = ca.item_index_containing(name)
    assert row is not None
    ca.revoke_item(row)
    assert ca.wait_for_no_item_containing(name), (
        "a revoked app password must be gone on re-read. "
        f"error: {ca.current_error_text()!r}"
    )


@pytest.fixture
def handoff_app(request, app, consent_nest, consent_bridge):
    """`app`, logged in as alice on the consent nest and left where login lands
    it — NOT on Connected apps, so the route itself must navigate there. The
    consent nest is a dedicated nest, so web's login goes through ITS proxy
    (`consent_spa_url`, as `connected_app` passes) — on the shared proxy the
    browser reaches a nest where alice is unregistered and the transport never
    comes online (`_login_app_as`'s docstring)."""
    from conftest import _login_app_as

    _login_app_as(
        app, request, consent_nest, consent_nest["user"], spa_url_fixture="consent_spa_url"
    )
    return app


def _poll_handoff(client: OAuthClient, request_uri: str):
    return client.poll_token(
        GRANT_TYPE_HANDOFF, "request_uri", request_uri, {"code_verifier": client.verifier}
    )


def _open_handoff_card(ca: ConnectedAppsActions, client: OAuthClient, request_uri: str) -> int:
    """Open the route and return the card's index. No re-navigation: the
    command acks once the open has landed, so the card must already be painted
    by the route itself, not by a later visit's re-read."""
    ca.open_handoff_route(request_uri)
    assert ca.is_page_visible(), (
        f"the consent route must land on Connected apps. error: {ca.current_error_text()!r}"
    )
    index = ca.request_index_for_client(client.client_id)
    assert index is not None, (
        "the route must open its card in the tray. "
        f"error: {ca.current_error_text()!r}"
    )
    assert ca.current_error_text() == ""
    return index


@pytest.mark.feature("connected-apps")
@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.linux
@pytest.mark.web  # declared absence — open_handoff_route declares it (apps/web.md § Implementation status today)
def test_a_handoff_link_opens_the_card_and_approving_connects_the_app(
    handoff_app, consent_nest
):
    """(f): a device app's PAR handle, handed to the app as
    ``fauna://consent/<request_uri>``, opens the card on Connected apps;
    approving there turns the device's handoff poll into tokens and the roster
    shows the app. The handle is single-use: opening the same link again shows
    the one expired-link message."""
    ca: ConnectedAppsActions = handoff_app.connected_apps
    device = _client(consent_nest, _HANDOFF_APPROVE_PORT)
    request_uri = device.push_authorization_request()

    status, body = _poll_handoff(device, request_uri)
    assert (status, body.get("error")) == (400, "authorization_pending"), (status, body)

    index = _open_handoff_card(ca, device, request_uri)
    assert device.client_id in ca.request_text(index), "the client id renders verbatim"

    ca.approve_request(index)
    status, tokens = _poll_handoff(device, request_uri)
    assert status == 200 and tokens.get("refresh_token"), (
        f"approving in the app must release the device app's poll: {status} {tokens!r}"
    )
    row = ca.wait_for_item_containing(device.client_id)
    assert row is not None, "the new connected app must appear on the roster"

    ca.open_handoff_route(request_uri)
    assert ca.current_error_text() == S.connected_apps.error_handoff_expired, (
        "a spent link reads as the one expired-link message: "
        f"{ca.current_error_text()!r}"
    )


@pytest.mark.feature("connected-apps")
@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.linux
@pytest.mark.web  # declared absence — open_handoff_route declares it (apps/web.md § Implementation status today)
def test_a_declined_handoff_tells_the_app_no(handoff_app, consent_nest):
    """(g): declining the card a handoff link opened answers the device's
    poll with ``access_denied`` and connects nothing."""
    ca: ConnectedAppsActions = handoff_app.connected_apps
    device = _client(consent_nest, _HANDOFF_DECLINE_PORT)
    request_uri = device.push_authorization_request()

    index = _open_handoff_card(ca, device, request_uri)
    ca.decline_request(index)

    status, body = _poll_handoff(device, request_uri)
    assert (status, body.get("error")) == (400, "access_denied"), (status, body)
    assert ca.visit(), f"the page must re-read. error: {ca.current_error_text()!r}"
    assert ca.item_index_containing(device.client_id) is None, (
        "a declined request connects nothing"
    )
    assert ca.request_index_for_client(device.client_id) is None, (
        "a declined request leaves the tray"
    )


# ── The account's external-apps switch (``atproto-pds-full.md`` § Detailed
# design → F1 detail, the kill-switch bullet): OFF binds the account's
# third-party principals too — the live session closes ``4401``, a fresh dial
# is ``401`` — and the roster says the app is paused rather than gone; ON
# restores it with nothing redone.
_SWITCH_PORT = 17796


@pytest.mark.tui
@pytest.mark.android
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("connected-apps")
def test_the_external_apps_switch_pauses_a_connected_app_and_on_restores_it(
    connected_app, consent_nest
):
    """With a principal connected and its session live, flip the switch OFF
    from the app: the live socket closes ``4401``, a fresh dial is refused
    ``401``, and the roster row reads paused, in words and as its ``status``.
    Flip ON: the row reads connected and a fresh dial answers a call.

    The principal's consent is fixture setup over the account's own connection
    (convention 8 (b)); the flips — the behavior under test — go through the
    app's own toggle."""
    from helpers.principal_session import (
        CLOSE_AUTH_EXPIRED,
        DECLARED,
        PrincipalSession,
        ceremony,
        refused_status,
        upgrade,
    )

    app = connected_app
    ca: ConnectedAppsActions = app.connected_apps
    bs = app.atproto_settings
    client, tokens = ceremony(
        consent_nest, DECLARED, redirect_uri=f"http://127.0.0.1:{_SWITCH_PORT}/callback"
    )
    token = tokens["access_token"]
    session = PrincipalSession(upgrade(consent_nest, client, token))
    ok, reply = session.call("fauna.feed.trending.posts", {})
    assert ok, f"the connected app's session answers before the flip: {reply!r}"

    row = ca.wait_for_item_status(client.client_id, "connected")
    assert row is not None, (
        "the connected app must be on the roster, connected. "
        f"error: {ca.current_error_text()!r}"
    )

    bs.navigate()
    try:
        bs.set_external_apps_enabled(False)
        assert bs.wait_for_external_apps_state("off"), (
            f"the switch must flip off. error: {bs.page_error_text()!r}"
        )
        assert session.close_code() == CLOSE_AUTH_EXPIRED, (
            "turning external apps off closes the app's live socket with code 4401"
        )
        status, _ = refused_status(consent_nest, client, token)
        assert status == 401, f"while off, a fresh dial is refused: {status}"

        ca.navigate()
        row = ca.wait_for_item_status(client.client_id, "paused")
        assert row is not None, (
            "while off, the app's row must read paused. "
            f"row: {ca.item_text(ca.item_index_containing(client.client_id) or 0)!r} "
            f"error: {ca.current_error_text()!r}"
        )
        text = ca.item_text(row)
        assert S.connected_apps.paused in text, f"paused, in words: {text!r}"
        assert S.connected_apps.not_connected not in text, (
            f"a paused app says why, never the bare 'not connected': {text!r}"
        )
    finally:
        bs.navigate()
        bs.set_external_apps_enabled(True)
    assert bs.wait_for_external_apps_state("on"), (
        f"the switch must flip back on. error: {bs.page_error_text()!r}"
    )

    session = PrincipalSession(upgrade(consent_nest, client, token))
    ok, reply = session.call("fauna.feed.trending.posts", {})
    assert ok, f"back on, a fresh dial answers with nothing redone: {reply!r}"
    session.ws.close()

    ca.navigate()
    assert ca.wait_for_item_status(client.client_id, "connected") is not None, (
        "back on, the app's row reads connected. "
        f"error: {ca.current_error_text()!r}"
    )


@pytest.mark.tui
@pytest.mark.web
@pytest.mark.android
@pytest.mark.linux
@pytest.mark.feature("connected-apps")
def test_a_principal_the_oracle_served_shows_what_the_nest_did_on_its_row(
    request, connected_app, consent_nest, metadata_server
):
    """A publisher's app holding ``fauna:identity:op:atproto.service_auth``
    asks the oracle for a service-auth token for its declared pair; the bridge
    mints it with the user's repo key. On the roster, that app's row then
    carries the oracle summary — the window's count as its ``oracle_ops``
    value and the newest operation with its detail (``<aud> <lxm>``) verbatim —
    while a row the oracle never served carries none.

    The consent is approved on the app's own request card, which mints its
    keyless ``identity.op`` grant the product's way — the ``Mint`` recorded in
    the grant log before the deposit — so the account runtime's reconcile sweep
    keeps it (a raw ``fauna.capabilities.mint`` beside a live runtime is
    revoked as a row the log never held). The operation is the app's own call.
    Asserts what the roster read says, never the count's value: the count is
    the rate window's position, which a slow box may see roll over."""
    from helpers.principal_session import PrincipalSession, redeem, upgrade

    ca: ConnectedAppsActions = connected_app.connected_apps
    holder_pub = x25519.X25519PrivateKey.generate().public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    )
    client = OAuthClient(
        base=consent_nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri=_ORACLE_REDIRECT_URI,
        scope=_ORACLE_SCOPE,
        holder_x25519=holder_pub,
        client_id_url=metadata_server.client_id(
            _oracle_document_path(request.node.callspec.params["app"])
        ),
    )
    request_uri = client.push_authorization_request()
    browser_code, flow_token = client.open_consent_page(request_uri)
    index = ca.wait_for_request_code(browser_code)
    assert index is not None, (
        "the request must open its card in the tray. "
        f"error: {ca.current_error_text()!r}"
    )
    ca.approve_request(index)
    tokens = redeem(client, flow_token)

    connected = ca.wait_for_item_containing(client.client_id)
    assert connected is not None, (
        "the approved app must be on the roster. "
        f"error: {ca.current_error_text()!r}"
    )
    assert ca.item_oracle_ops(connected) is None, (
        "before the oracle has served it, the row carries no oracle summary. "
        f"row: {ca.item_text(connected)!r}"
    )

    session = PrincipalSession(upgrade(consent_nest, client, tokens["access_token"]))
    try:
        ok, reply = session.call(
            "fauna.atproto.oracle.service_auth", {"aud": _ORACLE_AUD, "lxm": _ORACLE_LXM}
        )
        assert ok, f"the declared pair must mint: {reply!r}"
    finally:
        session.ws.close()

    detail = f"{_ORACLE_AUD} {_ORACLE_LXM}"
    row = ca.wait_for_oracle_item_containing(client.client_id)
    assert row is not None, (
        "the served app's row must carry the oracle summary. "
        f"row: {ca.item_text(ca.item_index_containing(client.client_id) or 0)!r} "
        f"error: {ca.current_error_text()!r}"
    )
    ops = ca.item_oracle_ops(row)
    assert ops is not None and ops.isdigit(), f"the window's count, as a value: {ops!r}"
    text = ca.item_text(row)
    assert detail in text, f"the newest operation's detail, verbatim: {text!r}"


def _app_password_rows(ca: ConnectedAppsActions) -> list[int]:
    """The roster rows that are ATProto app-password SESSIONS: the app-password
    badge without the mail leaves a mail app password adds (the two share the
    badge)."""
    mail = set(ca.mail_row_items())
    return [
        i
        for i in range(ca.item_count())
        if S.connected_apps.class_app_password in ca.item_text(i) and i not in mail
    ]


def _sign_in_with_app_password(app, consent_bridge) -> tuple[dict, int]:
    """Mint an app password on the atproto page (through the app), then sign in
    with it against the bridge's PDS the way a third-party Bluesky app does.
    Returns ``(createSession reply, the minted credential's row index)``."""
    bs = app.atproto_settings
    bs.ensure_hosted_full()
    assert bs.wait_for_fullpds_visible(), (
        f"the full-PDS panel must be present to mint. error: {bs.page_error_text()!r}"
    )
    index = bs.credential_count()
    bs.mint_credential()
    assert bs.wait_for_credential_count(index + 1), (
        f"minting should add one credential row. error: {bs.page_error_text()!r}"
    )
    secret = bs.reveal_credential_secret(index)
    assert secret and secret != "Reveal", f"mint must reveal the secret, got {secret!r}"
    status, session = xrpc_post(
        consent_bridge["base"],
        "com.atproto.server.createSession",
        {"identifier": "alice", "password": secret},
    )
    assert status == 200, (
        f"createSession refused: {status} {session!r}\n{consent_bridge['tail']()}"
    )
    return session, index


@pytest.mark.tui
@pytest.mark.feature("connected-apps")
def test_a_nostr_signer_app_has_its_row_and_disconnecting_it_ends_the_connection(
    connected_app, consent_nest
):
    """(h): a Nostr app the nest signs for is a roster row here, with the
    signer badge, and Revoke on that row ends the connection — gone on re-read
    and gone from the nest's own list of signer connections. The connection is
    minted on the Nostr page (that page keeps only the invite start;
    ``test_nostr_bunker.py`` drives it there and speaks for the Nostr page's
    columns, not for this one)."""
    app = connected_app
    ca: ConnectedAppsActions = app.connected_apps
    nostr = app.nostr
    nostr.navigate()
    assert nostr.is_page_visible(), f"nostr page not reachable. error: {app.error_text()!r}"
    assert nostr.ensure_linked(), f"could not link. error: {nostr.page_error_text()!r}"
    assert nostr.wait_for_connect_section(), "the signer-invite section must render"
    nostr.clear_bunker_apps()

    nostr.connect_app()
    assert nostr.wait_for_connect_string(), (
        f"minting an invite should reveal the connect string. error: {nostr.page_error_text()!r}"
    )

    assert ca.visit(), f"the page must read. error: {ca.current_error_text()!r}"
    badge = S.connected_apps.class_signer
    rows = [i for i in range(ca.item_count()) if badge in ca.item_text(i)]
    assert len(rows) == 1, (
        f"the signer connection must be one roster row with the signer badge: {rows!r}"
    )

    ca.revoke_item(rows[0])
    gone = ca._poll(
        lambda: True
        if not any(badge in ca.item_text(i) for i in range(ca.item_count()))
        else None,
        10.0,
        renavigate=True,
    )
    assert gone, (
        f"a revoked signer row must be gone on re-read. error: {ca.current_error_text()!r}"
    )
    alice = consent_nest["user"]
    with WsRpcAdminClient(
        consent_nest["url"],
        actor_id=alice["actor_id_bytes"],
        signing_key=bytes(alice["signing_key"]),
    ) as ws:
        listed = ws.call("fauna.nostr.bunker.list", {})["apps"]
    assert not listed, f"the nest must hold no signer connection after Revoke: {listed!r}"


@pytest.mark.tui
@pytest.mark.feature("connected-apps")
def test_an_app_signed_in_with_an_app_password_has_its_row(connected_app, consent_bridge):
    """(i): an app that signs in with one of the user's Bluesky app passwords is
    a roster row here, badged as an app-password sign-in; Revoke ends it."""
    app = connected_app
    ca: ConnectedAppsActions = app.connected_apps
    assert ca.visit(), f"the page must read. error: {ca.current_error_text()!r}"
    before = len(_app_password_rows(ca))

    _, cred_index = _sign_in_with_app_password(app, consent_bridge)
    try:
        assert ca.visit(), f"the page must read. error: {ca.current_error_text()!r}"
        found = ca._poll(
            lambda: True if len(_app_password_rows(ca)) == before + 1 else None,
            10.0,
            renavigate=True,
        )
        assert found, (
            "the app-password sign-in must be one new roster row. "
            f"error: {ca.current_error_text()!r}"
        )

        ca.revoke_item(_app_password_rows(ca)[-1])
        ended = ca._poll(
            lambda: True if len(_app_password_rows(ca)) == before else None,
            10.0,
            renavigate=True,
        )
        assert ended, f"a revoked row must be gone on re-read. error: {ca.current_error_text()!r}"
    finally:
        app.atproto_settings.navigate()
        app.atproto_settings.revoke_credential(cred_index)


@pytest.mark.tui
@pytest.mark.feature("connected-apps")
def test_each_row_says_when_the_app_was_last_used_and_how_long_it_lasts(
    connected_app, consent_nest, consent_bridge
):
    """(j): a consent-minted app's row carries a last-used and a lasts-until
    fact; an app-password sign-in's row reads *Never used* until the app
    refreshes its session, then *Last used …* — the state change, not a clock
    reading, so nothing here waits on time."""
    ca: ConnectedAppsActions = connected_app.connected_apps
    last_used = S.connected_apps.last_used(time="").strip()
    until = S.connected_apps.lasts_until(time="").strip()

    client = _client(consent_nest, _LAST_USED_PORT)
    status, started = client.start_polled(DEVICE_AUTHORIZATION_PATH, {})
    assert status == 200, f"device authorization start refused: {status} {started!r}"
    ca.connect_with_code(started["user_code"])
    index = ca.wait_for_request_from(client.client_id)
    assert index is not None, f"the card must open. error: {ca.current_error_text()!r}"
    ca.approve_request(index)
    status, tokens = client.poll_token(
        GRANT_TYPE_DEVICE_CODE, "device_code", started["device_code"]
    )
    assert status == 200, f"approval must release the poll: {status} {tokens!r}"
    row = ca.wait_for_item_containing(client.client_id)
    assert row is not None, "the new connected app must appear on the roster"
    text = ca.item_text(row)
    assert last_used in text or S.connected_apps.never_used in text, (
        f"the row says when the app was last used: {text!r}"
    )
    assert until in text or S.connected_apps.open_ended in text, (
        f"the row says how long its access lasts: {text!r}"
    )
    ca.revoke_item(row)

    session, cred_index = _sign_in_with_app_password(connected_app, consent_bridge)
    try:
        assert ca.visit(), f"the page must read. error: {ca.current_error_text()!r}"
        rows = _app_password_rows(ca)
        assert rows, "the app-password sign-in must be on the roster"
        text = ca.item_text(rows[-1])
        assert S.connected_apps.never_used in text and last_used not in text, (
            f"before any refresh the row reads never used: {text!r}"
        )
        assert until in text, f"the row says how long the sign-in lasts: {text!r}"

        status, refreshed = xrpc_post(
            consent_bridge["base"],
            "com.atproto.server.refreshSession",
            None,
            bearer=session["refreshJwt"],
        )
        assert status == 200, f"refreshSession refused: {status} {refreshed!r}"
        changed = ca._poll(
            lambda: True
            if (rs := _app_password_rows(ca)) and last_used in ca.item_text(rs[-1])
            else None,
            10.0,
            renavigate=True,
        )
        assert changed, (
            "after the app refreshes its session the row reads last used. "
            f"row: {ca.item_text(_app_password_rows(ca)[-1])!r}"
        )
        assert S.connected_apps.never_used not in ca.item_text(_app_password_rows(ca)[-1])
    finally:
        ca.visit()
        for i in reversed(_app_password_rows(ca)):
            ca.revoke_item(i)
        connected_app.atproto_settings.navigate()
        connected_app.atproto_settings.revoke_credential(cred_index)


@pytest.mark.tui
@pytest.mark.feature("connected-apps")
def test_a_code_nobody_issued_gets_one_plain_message_and_opens_no_card(
    connected_app, consent_nest
):
    """(k): a well-formed code the nest never issued (a live code with its last
    character changed) shows the one expired-or-unknown message and opens no
    request card."""
    ca: ConnectedAppsActions = connected_app.connected_apps
    client = _client(consent_nest, _BAD_CODE_PORT)
    status, started = client.start_polled(DEVICE_AUTHORIZATION_PATH, {})
    assert status == 200, f"device authorization start refused: {status} {started!r}"
    live = started["user_code"]
    other = next(c for c in dict.fromkeys(live) if c != live[-1] and c.isalnum())
    unissued = live[:-1] + other

    before = ca.request_count()
    ca.connect_with_code(unissued)
    shown = ca._poll(lambda: ca.current_error_text() or None, 10.0, renavigate=False)
    assert shown == S.connected_apps.error_code_expired, (
        f"an unissued code reads as the one expired-or-unknown message: {shown!r}"
    )
    assert ca.request_count() == before, "an unissued code must open no request card"
    assert ca.request_index_for_client(client.client_id) is None


# ── The oracle at the `atproto.commit` door ────────────────────────────────
#
# `key-material-hierarchy.md` § *The oracle* → *The ATProto classes ride the
# protocol's own door*: the PDS write funnel is the door, the bridge forwards
# the caller's session on its one nest round trip, and the nest checks the
# session live, ceilings it and records each write.

_ORACLE_WRITE_PORT = 17800
# `fauna_core::identity_op::RateWindow`: 60 operations per 60 s per session.
_CEILING = 60
_LIST_RECORD = {
    "$type": "app.bsky.graph.list",
    "name": "mates",
    "purpose": "app.bsky.graph.defs#curatelist",
    "createdAt": "2026-10-12T12:00:00.000Z",
}


def _recorded_writes(consent_nest) -> int:
    """Rows in the per-operation record, read off the nest's own store (the
    owner's app reading it is a later slice)."""
    conn = sqlite3.connect(consent_nest["db_path"], timeout=10.0)
    try:
        return conn.execute("SELECT COUNT(*) FROM atproto_external_write_ops").fetchone()[0]
    finally:
        conn.close()


@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.feature("atproto", "connected-apps")
def test_a_revoke_bites_at_the_very_next_write_and_each_session_is_ceilinged(
    connected_app, consent_nest, consent_bridge
):
    """Over a real OAuth session against the real PDS bridge: 61 writes in one
    window are `RateLimitExceeded` while 60 land, each recorded against the
    session; and after the owner revokes the app on this page, the very next
    write on the SAME, still-unexpired access token is refused — not up to an
    hour later at refresh."""
    ca: ConnectedAppsActions = connected_app.connected_apps
    # A write needs alice's identity resolvable (the bridge's first-emit gate)
    # — a precondition, not the property under test.
    open_first_emit_gate(consent_bridge)
    client = OAuthClient(
        base=consent_nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri=f"http://127.0.0.1:{_ORACLE_WRITE_PORT}/callback",
        scope="atproto transition:generic",
        resource_base=consent_bridge["base"],
        resource_htu_origin=consent_bridge["htu_origin"],
    )
    status, started = client.start_polled(DEVICE_AUTHORIZATION_PATH, {})
    assert status == 200, f"device authorization start refused: {status} {started!r}"
    ca.connect_with_code(started["user_code"])
    index = ca.wait_for_request_from(client.client_id)
    assert index is not None, f"the card must open. error: {ca.current_error_text()!r}"
    ca.approve_request(index)
    status, tokens = client.poll_token(
        GRANT_TYPE_DEVICE_CODE, "device_code", started["device_code"]
    )
    assert status == 200 and tokens.get("access_token"), f"{status} {tokens!r}"
    access = tokens["access_token"]
    did = consent_bridge["did"]

    def batch(n: int) -> tuple[int, dict]:
        return client.authed_xrpc_post(
            "com.atproto.repo.applyWrites",
            access,
            {
                "repo": did,
                "writes": [
                    {
                        "$type": "com.atproto.repo.applyWrites#create",
                        "collection": "app.bsky.graph.list",
                        "value": _LIST_RECORD,
                    }
                    for _ in range(n)
                ],
            },
        )

    # The ceiling's two sides as single batches, so neither rests on how fast
    # this box makes two calls (convention 14): one batch can never span two
    # windows, and a refused batch consumes none of the fresh session's.
    #
    # 1. Sixty-one writes inside one window are RateLimitExceeded, whole.
    before = _recorded_writes(consent_nest)
    status, body = batch(_CEILING + 1)
    assert status == 429 and body.get("error") == "RateLimitExceeded", (
        f"61 writes in one window must be RateLimitExceeded: {status} {body!r}\n"
        f"{consent_bridge['tail']()}"
    )
    assert _recorded_writes(consent_nest) == before, "a refused batch records nothing"

    # 2. Sixty land — the 60th included — and each is recorded against the
    # session, one row per write.
    status, body = batch(_CEILING)
    assert status == 200, (
        f"the 60th write must land: {status} {body!r}\n{consent_bridge['tail']()}"
    )
    assert _recorded_writes(consent_nest) == before + _CEILING, (
        "every applied write leaves exactly one row in the per-operation record"
    )

    # 3. The owner reads the record on the app's row: the oracle summary,
    # with the newest write — `<action> <collection>/<rkey>`, the key it
    # landed at — in the row's words. (The window's count is asserted as a
    # value, never as 60: the window may have rolled by the read.)
    newest_rkey = body["results"][-1]["uri"].rsplit("/", 1)[-1]
    newest = f"create app.bsky.graph.list/{newest_rkey}"
    row = ca.wait_for_oracle_item_containing(client.client_id)
    assert row is not None, (
        "the writing app's row must carry the oracle summary. "
        f"row: {ca.item_text(ca.item_index_containing(client.client_id) or 0)!r} "
        f"error: {ca.current_error_text()!r}"
    )
    ops = ca.item_oracle_ops(row)
    assert ops is not None and ops.isdigit(), f"the window's count, as a value: {ops!r}"
    text = ca.item_text(row)
    assert newest in text, f"the newest write, verbatim: {newest!r} in {text!r}"

    # 4. The owner revokes the app on this page; the very next write on the
    # same access token — whose own lifetime has most of an hour left — is
    # refused. (The live check precedes the ceiling, so this answer does not
    # depend on whether the window above has rolled.)
    ca.revoke_item(row)
    assert ca.wait_for_no_item_containing(client.client_id), (
        f"the revoked row must be gone on re-read. error: {ca.current_error_text()!r}"
    )
    status, body = client.authed_xrpc_post(
        "com.atproto.repo.createRecord",
        access,
        {"repo": did, "collection": "app.bsky.graph.list", "record": _LIST_RECORD},
    )
    assert status == 401 and body.get("error") == "AuthenticationRequired", (
        f"a revoked session's next write must be refused at once: {status} {body!r}"
    )
