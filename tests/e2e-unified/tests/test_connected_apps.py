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
- **Key replacement** — an app the user approved asks again under a different
  holder key: its card says approving ends what the earlier key was given, and
  approving there ends that key's capability grant on the nest before the app
  redeems (``third-party.md`` § The principal model → *Key replacement*).
  tui first; the other apps' markers ride their witness lift.
- **An assistant's card** — the cli app (``fauna-cli``) connects by typed code
  under its published document; its card names it *Fauna for AI assistants*
  and says that what is approved reaches whichever assistant is connected
  (``docs/goal/architecture/apps/cli.md`` § Hosting cases). The assistant's
  reach itself is the cli page's ``[nest]`` witness (``test_cli_mcp.py``),
  approved app-less; this case is the card's own claim, on the app (tui first).
- **Key continuity** — an approved app whose document is re-published signed
  by a different publisher key, or without its manifest, is refused at its
  next refresh until the user approves a new card for it (tui first).
- **The records card** — a ``fauna:records:`` card names the publisher, the
  manifest's kind count, and *read only* when no writer key was attested
  (tui first).
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
import time
import urllib.parse
import uuid

import pytest
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ed25519, x25519

from actions.connected_apps import ConnectedAppsActions
from drivers.cli_mcp import CliMcpSession
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
    CLI_CLIENT_ID,
    CLI_CLIENT_NAME,
    CLI_DOCUMENT_FILE,
    DOCUMENT_PATH,
    ClientMetadataServer,
    client_document,
    manifest_payload,
    sign_manifest,
)
from helpers.budgets import ORCHESTRATION_STEP_S, RPC_ROUNDTRIP_S
from helpers.waiting import wait_until
from helpers.xrpc_client import xrpc_post
from helpers.oauth_client import (
    BC_AUTHORIZE_PATH,
    DEVICE_AUTHORIZATION_PATH,
    GRANT_TYPE_CIBA,
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


# The key-replacement case's client is a publisher's too: a `fauna:records:`
# scope needs the document's verified kind manifest, and only a records
# consent mints the consent-time capability grant a key replacement ends.
_RECORDS_SCOPE = f"fauna:records:rw:ext.{_ORACLE_PUBLISHER}.*"
_RECORDS_REDIRECT_URI = "http://127.0.0.1:17802/callback"


def _key_replacement_document_path(app_name: str) -> str:
    return f"/client-metadata-key-replacement-{app_name}.json"


#: The cli app's published document, served beside the publisher's on the same
#: mapped server (its host is an extra name on the server's certificate), so the
#: assistant's card resolves the cli's real identity without the internet.
_CLI_URL = urllib.parse.urlsplit(CLI_CLIENT_ID)


@pytest.fixture(scope="module")
def metadata_server(tmp_path_factory):
    server = ClientMetadataServer(
        _ORACLE_PUBLISHER,
        tmp_path_factory.mktemp("client-metadata"),
        extra_hosts=(_CLI_URL.hostname,),
    )
    server.serve_file(_CLI_URL.path, CLI_DOCUMENT_FILE.read_bytes())
    assert server.client_id(_CLI_URL.path, host=_CLI_URL.hostname) == CLI_CLIENT_ID
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
    for path in map(_key_replacement_document_path, _ORACLE_APPS):
        server.documents[path] = client_document(
            server.client_id(path),
            _RECORDS_REDIRECT_URI,
            _RECORDS_SCOPE,
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
    """The consent nest's metadata fetcher pointed at ``metadata_server``, with
    positive caching of fetched documents off (``test-hooks`` only): the
    key-continuity case re-publishes a document and the very next refresh
    must read it, never a cached copy (convention 14 forbids waiting out the
    TTL)."""
    return {**metadata_server.nest_env(), "FAUNA_TEST_CLIENT_METADATA_NO_CACHE": "1"}


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
_QUIET_DECLINE_PORT = 17801


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


def _cards_seen(ca: ConnectedAppsActions) -> str:
    """The tray as painted, for a failure message (convention 6): which cards
    were there when the expected one was not."""
    return repr([ca.request_text(i) for i in range(ca.request_count())])


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
        f"error: {ca.current_error_text()!r} client_id: {client.client_id!r} "
        f"cards: {_cards_seen(ca)}"
    )
    assert client.client_id in ca.request_text(index), "the client id renders verbatim"

    ca.approve_request(index)
    status, tokens = client.poll_until_answered(
        GRANT_TYPE_DEVICE_CODE, "device_code", started["device_code"], timeout=RPC_ROUNDTRIP_S
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
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
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
        f"error: {ca.current_error_text()!r} client_id: {client.client_id!r} "
        f"cards: {_cards_seen(ca)}"
    )
    ca.approve_request(index)
    status, tokens = client.poll_until_answered(
        GRANT_TYPE_DEVICE_CODE, "device_code", started["device_code"], timeout=RPC_ROUNDTRIP_S
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
@pytest.mark.web
@pytest.mark.feature("connected-apps")
def test_declining_a_quiet_push_tells_the_app_no(connected_app, consent_nest):
    """(g'): outcome 14's second witness, through the door web has — declining
    the card a quiet push lands in the tray answers the app's poll with
    ``access_denied`` and connects nothing. (The handoff-link witness skips on
    web, whose route intake is a declared absence.)"""
    ca: ConnectedAppsActions = connected_app.connected_apps
    stranger = _client(consent_nest, _QUIET_DECLINE_PORT)

    status, pushed = stranger.start_polled(BC_AUTHORIZE_PATH, {"login_hint": "alice"})
    assert status == 200 and pushed.get("auth_req_id"), f"push refused: {status} {pushed!r}"

    index = ca.wait_for_request_from(stranger.client_id)
    assert index is not None, (
        "a quiet push must be findable in the Requests tray. "
        f"error: {ca.current_error_text()!r}"
    )

    ca.decline_request(index)

    status, body = stranger.poll_until_answered(
        GRANT_TYPE_CIBA, "auth_req_id", pushed["auth_req_id"], timeout=RPC_ROUNDTRIP_S
    )
    assert (status, body.get("error")) == (400, "access_denied"), (status, body)
    assert ca.visit(), f"the page must re-read. error: {ca.current_error_text()!r}"
    assert ca.item_index_containing(stranger.client_id) is None, (
        "a declined request connects nothing"
    )
    assert ca.request_index_for_client(stranger.client_id) is None, (
        "a declined request leaves the tray"
    )


@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.feature("connected-apps")
def test_an_assistants_card_names_it_and_says_its_grant_reaches_the_assistant(
    connected_app, consent_nest, cli_binary, tmp_path
):
    """An assistant's approval card (``apps/cli.md`` § Hosting cases;
    ``ui/connected-apps.md`` § Layout & flow): the cli app's ``connect``
    answers a typed code; typed into *Connect an app*, it opens a card naming
    the cli by its published document — *Fauna for AI assistants* — with the
    line saying that what is approved reaches whichever assistant the user
    connects; approving it connects the cli, whose next tool call is served."""
    ca: ConnectedAppsActions = connected_app.connected_apps
    cli = CliMcpSession(cli_binary, consent_nest["url"], tmp_path)
    try:
        cli.initialize()
        started = cli.call("connect", {"handle": "alice"}, timeout=RPC_ROUNDTRIP_S)
        assert not started["isError"], f"connect refused: {started!r}\n{cli.diagnostics()}"
        code = started["structuredContent"]["user_code"]

        ca.connect_with_code(code)
        index = ca.wait_for_request_code(code)
        assert index is not None, (
            f"typing the assistant's code must open its card. cards: {_cards_seen(ca)} "
            f"error: {ca.current_error_text()!r}"
        )
        card = ca.request_text(index)
        assert CLI_CLIENT_NAME in card, f"the card names the published client: {card!r}"
        assert S.connected_apps.consent_assistant_reach in card, (
            f"the assistant's card says the grant reaches the assistant: {card!r}"
        )

        ca.approve_request(index)
        assert ca.current_error_text() == "", ca.current_error_text()
        served = wait_until(
            lambda: (lambda r: r if not r["isError"] else None)(cli.call("feed_local")),
            ORCHESTRATION_STEP_S,
            interval=1.0,
            diagnose=lambda: f"{cli.call('feed_local')!r}\n{cli.diagnostics()}",
        )
        assert isinstance(served["structuredContent"].get("posts"), list), served
    finally:
        cli.close()


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


def _answered_handoff(client: OAuthClient, request_uri: str):
    """The device's poll once the answer given in the app has reached the nest —
    the click returns on dispatch, not on the nest's record (convention 14)."""
    return client.poll_until_answered(
        GRANT_TYPE_HANDOFF,
        "request_uri",
        request_uri,
        {"code_verifier": client.verifier},
        timeout=RPC_ROUNDTRIP_S,
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
        f"error: {ca.current_error_text()!r} client_id: {client.client_id!r} "
        f"cards: {_cards_seen(ca)}"
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
    status, tokens = _answered_handoff(device, request_uri)
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
def test_a_declined_handoff_tells_the_app_no(handoff_app, consent_nest):
    """(g): declining the card a handoff link opened answers the device's
    poll with ``access_denied`` and connects nothing."""
    ca: ConnectedAppsActions = handoff_app.connected_apps
    device = _client(consent_nest, _HANDOFF_DECLINE_PORT)
    request_uri = device.push_authorization_request()

    index = _open_handoff_card(ca, device, request_uri)
    ca.decline_request(index)

    status, body = _answered_handoff(device, request_uri)
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


# The switch moved to this page 2026-10-17 (owner-ruled 2026-10-16); tui led,
# web, linux, android, macos, ios and windows followed.
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.tui
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

    ca.navigate()
    try:
        ca.set_external_apps_enabled(False)
        assert ca.wait_for_external_apps_state("off"), (
            f"the switch must flip off. error: {ca.current_error_text()!r}"
        )
        assert session.close_code() == CLOSE_AUTH_EXPIRED, (
            "turning external apps off closes the app's live socket with code 4401"
        )
        status, _ = refused_status(consent_nest, client, token)
        assert status == 401, f"while off, a fresh dial is refused: {status}"

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
        ca.navigate()
        ca.set_external_apps_enabled(True)
    assert ca.wait_for_external_apps_state("on"), (
        f"the switch must flip back on. error: {ca.current_error_text()!r}"
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
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
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
        f"error: {ca.current_error_text()!r} code: {browser_code!r} "
        f"cards: {_cards_seen(ca)}"
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


def _owned_grant_ids(consent_nest) -> set[bytes]:
    """The capability grants the account holds on the nest, read owner-side."""
    from helpers.principal_session import alice_client

    with alice_client(consent_nest) as ws:
        return {bytes(g) for g in ws.call("fauna.capabilities.reconcile", {})["grant_ids"]}


def _wait_owned(consent_nest, accept, what: str) -> set[bytes]:
    """Poll the owner's grant ids until ``accept`` holds (a state, never a
    delay — convention 14); fail naming ``what`` with the last reading."""
    deadline = time.monotonic() + RPC_ROUNDTRIP_S
    owned = _owned_grant_ids(consent_nest)
    while not accept(owned) and time.monotonic() < deadline:
        time.sleep(0.5)
        owned = _owned_grant_ids(consent_nest)
    assert accept(owned), f"{what}; the account's grants on the nest: {sorted(g.hex() for g in owned)!r}"
    return owned


def _raw_public(private_key) -> bytes:
    return private_key.public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    )


@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("connected-apps")
def test_a_re_request_with_a_new_key_says_what_it_ends_and_approving_ends_it(
    request, connected_app, consent_nest, metadata_server
):
    """Outcome 43 (``connected-apps.md`` § User actions; ``third-party.md``
    § The principal model → *Key replacement*): an app the user approved asks
    again under a different holder key. Its card says approving ends what the
    earlier key was given, and approving on that card ends it — nest first,
    from the approving app, before the app has even redeemed its code.

    The first consent asks for a ``fauna:records:`` scope, so its approve on
    the card mints the consent-time capability grant to the first key: the
    grant the replacement must end. The nest ends the old key's grants on its
    own too, but only at ``/oauth/token`` when the app redeems; the assertion
    that the old grant is gone is therefore made between the approve and the
    redemption, where only the approving app's end can have removed it. The
    roster holds one row per (account, client), so the old key never shows as
    a row of its own: after the redemption the one row is still connected and
    the grant minted to the new key is still held."""
    from helpers.principal_session import redeem

    ca: ConnectedAppsActions = connected_app.connected_apps
    app_name = request.node.callspec.params["app"]
    client_id_url = metadata_server.client_id(_key_replacement_document_path(app_name))
    writer_pub = _raw_public(ed25519.Ed25519PrivateKey.generate())

    def ask(holder_pub: bytes) -> tuple[OAuthClient, str, int]:
        client = OAuthClient(
            base=consent_nest["url"],
            htu_origin=f"https://{HANDLE_DOMAIN}",
            redirect_uri=_RECORDS_REDIRECT_URI,
            scope=_RECORDS_SCOPE,
            holder_x25519=holder_pub,
            writer_ed25519=writer_pub,
            client_id_url=client_id_url,
        )
        request_uri = client.push_authorization_request()
        browser_code, flow_token = client.open_consent_page(request_uri)
        index = ca.wait_for_request_code(browser_code)
        assert index is not None, (
            "the request must open its card in the tray. "
            f"error: {ca.current_error_text()!r} code: {browser_code!r} "
            f"cards: {_cards_seen(ca)}"
        )
        return client, flow_token, index

    ends_line = S.connected_apps.consent_ends_holder

    # ── The first ceremony: nothing to end, and the approve mints a grant. ──
    before_first = _owned_grant_ids(consent_nest)
    first, first_flow, index = ask(_raw_public(x25519.X25519PrivateKey.generate()))
    assert ends_line not in ca.request_text(index), (
        "a first ceremony replaces no key, so its card says nothing ends. "
        f"card: {ca.request_text(index)!r}"
    )
    ca.approve_request(index)
    redeem(first, first_flow)
    first_grants = (
        _wait_owned(
            consent_nest,
            lambda owned: bool(owned - before_first),
            "approving a records consent on the card must mint its grant to the first key",
        )
        - before_first
    )
    assert ca.wait_for_item_containing(first.client_id) is not None, (
        "the approved app must be on the roster. "
        f"error: {ca.current_error_text()!r}"
    )

    # ── The same app asks again under a new holder key. ──
    second, second_flow, index = ask(_raw_public(x25519.X25519PrivateKey.generate()))
    assert second.client_id == first.client_id, "the same document is the same app"
    card = ca.request_text(index)
    assert ends_line in card, (
        f"the card must say approving ends what the earlier key was given: {card!r}"
    )

    ca.approve_request(index)
    after_approve = _wait_owned(
        consent_nest,
        lambda owned: not (owned & first_grants),
        "approving on the card must end the earlier key's grant before the app "
        f"redeems (old grants {sorted(g.hex() for g in first_grants)!r})",
    )
    second_grants = after_approve - before_first - first_grants
    assert second_grants, (
        "the approve must mint the new key its own grant: "
        f"{sorted(g.hex() for g in after_approve)!r}"
    )
    assert ca.current_error_text() == "", ca.current_error_text()

    redeem(second, second_flow)
    owned = _owned_grant_ids(consent_nest)
    assert second_grants <= owned, (
        "the redemption ends only the old key's grants, never the new one's: "
        f"{sorted(g.hex() for g in owned)!r}"
    )
    row = ca.wait_for_item_status(second.client_id, "connected")
    assert row is not None, (
        "the one roster row stays connected under the new key. "
        f"row: {ca.item_text(ca.item_index_containing(second.client_id) or 0)!r} "
        f"error: {ca.current_error_text()!r}"
    )


# The key-continuity and records-card cases serve their own documents per app,
# written by the case itself: key continuity re-publishes a document mid-case,
# and the module's nest re-reads it at once because positive caching is off
# for this module (`consent_nest_env`).
_CONTINUITY_REDIRECT_URI = "http://127.0.0.1:17803/callback"
_RECORDS_CARD_REDIRECT_URI = "http://127.0.0.1:17804/callback"


def _continuity_document_path(app_name: str, case: str) -> str:
    return f"/client-metadata-continuity-{case}-{app_name}.json"


def _records_card_document_path(app_name: str, case: str) -> str:
    return f"/client-metadata-records-card-{case}-{app_name}.json"


def _publish(metadata_server, path: str, redirect_uri: str, scope: str, manifest_jws) -> str:
    """Serve ``path``'s client document carrying ``manifest_jws`` (``None``:
    no ``fauna`` member) and return its client id."""
    client_id = metadata_server.client_id(path)
    metadata_server.documents[path] = client_document(
        client_id, redirect_uri, scope, manifest_jws=manifest_jws
    )
    return client_id


def _signed_manifest(publisher_key, kind_names: list[str]) -> str:
    return sign_manifest(
        publisher_key, manifest_payload(_ORACLE_PUBLISHER, publisher_key, kind_names)
    )


@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.feature("connected-apps")
@pytest.mark.parametrize("change", ["rotated-key", "dropped-manifest"])
def test_a_changed_publisher_key_or_a_vanished_manifest_refuses_until_a_new_card_is_approved(
    request, connected_app, consent_nest, metadata_server, change
):
    """Outcome 44 (``connected-apps.md`` § User actions; ``third-party.md``
    § Implementation status today, key continuity; ``third-party-kinds.md``
    § The manifest): an app the user approved from a signed document has its
    publisher key pinned on its roster row. When the document is re-published
    signed by a different publisher key — or with its manifest gone — the
    app's next refresh is refused with ``invalid_grant``
    (``oauth_as_client::manifest_key_continues``), never a silent swap; the
    app's next request opens a new card on the user's app, and approving that
    card brings the app back: its new grant refreshes.

    The re-published document is served before the refresh and the module's
    nest caches no fetched document (``FAUNA_TEST_CLIENT_METADATA_NO_CACHE``),
    so the refresh reads the changed document — the refusal's description is
    asserted, naming which of the two continuity failures it was."""
    from helpers.principal_session import redeem

    ca: ConnectedAppsActions = connected_app.connected_apps
    app_name = request.node.callspec.params["app"]
    path = _continuity_document_path(app_name, change)
    first_key = ed25519.Ed25519PrivateKey.generate()
    client_id_url = _publish(
        metadata_server,
        path,
        _CONTINUITY_REDIRECT_URI,
        "atproto",
        _signed_manifest(first_key, ["notes"]),
    )

    def consent() -> tuple[OAuthClient, dict]:
        client = OAuthClient(
            base=consent_nest["url"],
            htu_origin=f"https://{HANDLE_DOMAIN}",
            redirect_uri=_CONTINUITY_REDIRECT_URI,
            scope="atproto",
            client_id_url=client_id_url,
        )
        request_uri = client.push_authorization_request()
        browser_code, flow_token = client.open_consent_page(request_uri)
        index = ca.wait_for_request_code(browser_code)
        assert index is not None, (
            "the request must open its card in the tray. "
            f"error: {ca.current_error_text()!r} code: {browser_code!r} "
            f"cards: {_cards_seen(ca)}"
        )
        ca.approve_request(index)
        tokens = redeem(client, flow_token)
        assert tokens.get("refresh_token"), f"the approve must grant a refresh: {tokens!r}"
        return client, tokens

    # ── Approved under the first publisher key: the grant refreshes. ──
    first, tokens = consent()
    tokens = first.refresh(tokens["refresh_token"])
    assert ca.wait_for_item_containing(first.client_id) is not None, (
        f"the approved app must be on the roster. error: {ca.current_error_text()!r}"
    )

    # ── The publisher's document changes under the existing grant. ──
    if change == "rotated-key":
        manifest = _signed_manifest(ed25519.Ed25519PrivateKey.generate(), ["notes"])
        why = "signed by a different publisher key"
    else:
        manifest = None
        why = "is gone"
    _publish(metadata_server, path, _CONTINUITY_REDIRECT_URI, "atproto", manifest)

    status, body = first.token_request_expecting_refusal(
        {"grant_type": "refresh_token", "refresh_token": tokens["refresh_token"]}
    )
    assert status == 400 and body.get("error") == "invalid_grant", (
        f"a {change} must refuse the refresh until the user re-approves: {status} {body!r}"
    )
    assert why in body.get("error_description", ""), (
        f"the refusal must come from key continuity ({why!r}): {body!r}"
    )

    # ── A new card on the user's app; approving it brings the app back. ──
    second, tokens = consent()
    assert second.client_id == first.client_id, "the same document is the same app"
    second.refresh(tokens["refresh_token"])
    assert ca.current_error_text() == "", ca.current_error_text()
    row = ca.wait_for_item_status(second.client_id, "connected")
    assert row is not None, (
        "the one roster row is connected again after the new approve. "
        f"error: {ca.current_error_text()!r}"
    )


@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.feature("connected-apps")
@pytest.mark.parametrize("writer", [False, True], ids=["read-only", "with-writer-key"])
def test_a_records_consent_card_names_the_publisher_the_kind_count_and_read_only(
    request, connected_app, consent_nest, metadata_server, writer
):
    """Outcome 46 (``connected-apps.md`` § Coverage contract;
    ``third-party-kinds.md`` § Implementation status today): a
    ``fauna:records:`` consent card is worded from the request itself — the
    publisher, how many kinds of records its verified manifest declares under
    the scope, and *read only* when the request attests no writer key. One
    owner words the row for every app (``fauna_scope::records_card_row``,
    through the shared settings machine); the strings asserted are that
    function's, not i18n entries. The card is declined afterwards, so it
    leaves nothing behind for a later case."""
    ca: ConnectedAppsActions = connected_app.connected_apps
    app_name = request.node.callspec.params["app"]
    case = "writer" if writer else "read-only"
    client_id_url = _publish(
        metadata_server,
        _records_card_document_path(app_name, case),
        _RECORDS_CARD_REDIRECT_URI,
        _RECORDS_SCOPE,
        _signed_manifest(ed25519.Ed25519PrivateKey.generate(), ["notes", "tasks"]),
    )
    client = OAuthClient(
        base=consent_nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri=_RECORDS_CARD_REDIRECT_URI,
        scope=_RECORDS_SCOPE,
        holder_x25519=_raw_public(x25519.X25519PrivateKey.generate()),
        writer_ed25519=_raw_public(ed25519.Ed25519PrivateKey.generate()) if writer else None,
        client_id_url=client_id_url,
    )
    request_uri = client.push_authorization_request()
    browser_code, _flow_token = client.open_consent_page(request_uri)
    index = ca.wait_for_request_code(browser_code)
    assert index is not None, (
        "the request must open its card in the tray. "
        f"error: {ca.current_error_text()!r} code: {browser_code!r} "
        f"cards: {_cards_seen(ca)}"
    )
    card = ca.request_text(index)
    assert f"2 kinds of records published by {_ORACLE_PUBLISHER}" in card, (
        f"the card must name the publisher and the manifest's kind count: {card!r}"
    )
    if writer:
        assert "Read and write" in card and "read only" not in card, (
            f"a request attesting a writer key may write, so it is not read only: {card!r}"
        )
    else:
        assert "read only" in card, (
            f"a request attesting no writer key must say read only: {card!r}"
        )
    ca.decline_request(index)


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
@pytest.mark.android
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
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
@pytest.mark.android
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
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
@pytest.mark.android
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
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
    status, tokens = client.poll_until_answered(
        GRANT_TYPE_DEVICE_CODE, "device_code", started["device_code"], timeout=RPC_ROUNDTRIP_S
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

        try:
            status, refreshed = xrpc_post(
                consent_bridge["base"],
                "com.atproto.server.refreshSession",
                None,
                bearer=session["refreshJwt"],
            )
        except OSError as e:
            pytest.fail(f"refreshSession got no answer ({e!r}). bridge:\n{consent_bridge['tail']()}")
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
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.windows
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

    # The page's own read lands first: a re-read finishing after the submit
    # repaints the page from the nest and takes the gesture's error with it
    # (linux and web re-read on every visit, off the GUI thread).
    assert ca.visit(), f"the page must read. error: {ca.current_error_text()!r}"
    before = ca.request_count()
    ca.connect_with_code(unissued)
    shown = ca._poll(lambda: ca.current_error_text() or None, 10.0, renavigate=False)
    assert shown == S.connected_apps.error_code_expired, (
        f"an unissued code reads as the one expired-or-unknown message: {shown!r}"
    )
    assert ca.request_count() == before, "an unissued code must open no request card"
    assert ca.request_index_for_client(client.client_id) is None

    # The machine logs the miss it shows (`set_error`: target
    # `fauna_connected_apps`, the message key), and the log is the app's own:
    # on web the machine runs in the connected-apps wasm chunk, a runtime of
    # its own that the core chunk does not link, so the line reaches the core
    # chunk's ring and the console only through that chunk's `setLogSink`
    # (`observability.md` § Persistence & privacy, the Web bullet). Waited on,
    # not read once: the web console ring is fetched over the agent.
    driver = connected_app.driver
    reader = getattr(driver, "app_log_text", None) or getattr(driver, "app_stderr_text", None)
    if reader is not None:
        wait_until(
            lambda: any(
                "fauna_connected_apps" in line and "connected_apps.error_code_expired" in line
                for line in (reader() or "").splitlines()
            ),
            RPC_ROUNDTRIP_S, interval=0.5,
            diagnose=lambda: (
                "the miss logs `connected_apps.error_code_expired` (target "
                "fauna_connected_apps), so it must reach the app's own log; "
                "on web that is the console ring, through the chunk's "
                "setLogSink. tail:\n" + "\n".join((reader() or "").splitlines()[-30:])
            ),
        )


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
    status, tokens = client.poll_until_answered(
        GRANT_TYPE_DEVICE_CODE, "device_code", started["device_code"], timeout=RPC_ROUNDTRIP_S
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


_STEP_DOWN_PORT = 17805


@pytest.mark.tui
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("atproto")
def test_after_stepping_down_a_connected_app_reads_suspended_until_a_fresh_sign_in(
    connected_app, consent_nest
):
    """After you step down from hosting, each connected Bluesky app stays listed
    as suspended, with no expiry, and can still be revoked one by one; a fresh
    sign-in clears the mark (`atproto-oauth-provider.md` § Detailed design, the
    OAuth-grant suspension bullet; `ui/atproto.md`'s downward matrix: "grants
    suspended; rows kept, listed, individually revocable").

    The journey: at hosted_full an app signs in through the nest's own
    authorization server (a device code typed into Connected apps, approved
    there), so its row reads connected. The user steps down to hosted_visible
    through the selector's card: the step-down revokes the app's live session
    and leaves its grant at rest. The row must stay listed, read paused — the
    roster's one not-working-but-reversible state, the status value the
    external-apps switch already uses, worded with the step-down's own undo
    (`connected_apps.paused_suspended`) — and carry no lasts-until. Stepping
    back up and signing the app in afresh reads connected again; Revoke on the
    row ends it.

    The nest's `live_grants` counts a suspended grant (its session revoked,
    the grant not), so the shared composition derives the paused state from
    each grant's `suspended` flag (`connected-apps.md` § State & data shape)
    and every app paints that composition unchanged.
    """
    app = connected_app
    ca: ConnectedAppsActions = app.connected_apps
    bs = app.atproto_settings
    bs.ensure_hosted_full()

    # A name only this module uses: the reach scan merges private names called
    # off an attribute (`sync_seats`' `self._sign_in(...)`), so a `_sign_in`
    # here that presses the admin approve carried the admin shell onto every
    # seat journey (helpers/kind_reach.py; test_nest_mode_axis's admin pins).
    def _approve_a_device_code_sign_in() -> tuple[OAuthClient, dict]:
        client = _client(consent_nest, _STEP_DOWN_PORT)
        status, started = client.start_polled(DEVICE_AUTHORIZATION_PATH, {})
        assert status == 200, f"device authorization start refused: {status} {started!r}"
        ca.navigate()
        ca.connect_with_code(started["user_code"])
        index = ca.wait_for_request_from(client.client_id)
        assert index is not None, (
            f"typing a live code must open its card. error: {ca.current_error_text()!r} "
            f"cards: {_cards_seen(ca)}"
        )
        ca.approve_request(index)
        status, tokens = client.poll_until_answered(
            GRANT_TYPE_DEVICE_CODE, "device_code", started["device_code"], timeout=RPC_ROUNDTRIP_S
        )
        assert status == 200 and tokens.get("refresh_token"), (
            f"approving must release the app's poll: {status} {tokens!r}"
        )
        return client, tokens

    client, tokens = _approve_a_device_code_sign_in()
    assert ca.wait_for_item_status(client.client_id, "connected") is not None, (
        f"the signed-in app reads connected. error: {ca.current_error_text()!r}"
    )

    bs.navigate()
    bs.select_depth("hosted_visible")
    assert bs.wait_for_card(), "stepping down from full PDS must stage a card"
    bs.confirm_transition()
    assert bs.wait_for_depth_level("hosted_visible"), (
        f"confirm must step down. level={bs.depth_level()!r} error={bs.page_error_text()!r}"
    )
    status, body = client.token_request_expecting_refusal(
        {"grant_type": "refresh_token", "refresh_token": tokens["refresh_token"]}
    )
    assert status == 400, (
        f"precondition: the step-down ends the app's live session: {status} {body!r}"
    )

    ca.navigate()
    row = ca.wait_for_item_status(client.client_id, "paused")
    assert row is not None, (
        "after the step-down the app stays listed and reads paused, not connected. "
        f"row: {ca.item_text(ca.item_index_containing(client.client_id) or 0)!r} "
        f"error: {ca.current_error_text()!r}"
    )
    text = ca.item_text(row)
    assert S.connected_apps.paused_suspended in text, f"suspended, in words: {text!r}"
    assert S.connected_apps.lasts_until(time="").strip() not in text, (
        f"a suspended app's row carries no expiry: {text!r}"
    )

    bs.ensure_hosted_full()
    _approve_a_device_code_sign_in()
    ca.navigate()
    row = ca.wait_for_item_status(client.client_id, "connected")
    assert row is not None, (
        "a fresh sign-in clears the suspended mark. "
        f"row: {ca.item_text(ca.item_index_containing(client.client_id) or 0)!r}"
    )

    ca.revoke_item(row)
    assert ca.wait_for_no_item_containing(client.client_id), (
        f"Revoke on the row ends it. error: {ca.current_error_text()!r}"
    )
