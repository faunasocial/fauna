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
when its route intake lands.
Web's page does not list the mail app passwords yet (its wasm chunk does not
hold the mail machine), so the mail-row test stays off web.
"""

import uuid

import pytest

from actions.connected_apps import ConnectedAppsActions
from helpers.atproto_consent import (  # noqa: F401 — consent_nest/consent_nest_env/consent_bridge are pytest fixtures adopted by import
    HANDLE_DOMAIN,
    consent_bridge,
    consent_nest,
    consent_nest_env,
    consent_spa_url,
)
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

# Distinct loopback redirect ports → distinct client ids (the loopback
# client_id embeds its redirect), so each case is its own third-party app and
# no case sees another's grant or block.
_TYPED_CODE_PORT = 17791
_QUIET_PUSH_PORT = 17792
_UNBLOCK_PORT = 17793
_HANDOFF_APPROVE_PORT = 17794
_HANDOFF_DECLINE_PORT = 17795


def _client(consent_nest, port: int) -> OAuthClient:
    return OAuthClient(
        base=consent_nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri=f"http://127.0.0.1:{port}/callback",
        scope="atproto",
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
