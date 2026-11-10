"""tier_3 e2e: **the depositor signs, and the owner sees who did** — the
approval card says each dropped file is signed by the app, and a deposited
file's version history names the app that put it there
(``docs/goal/behavior/folder-deposit.md`` § The depositor signs, the owner's
device verifies; its § Implementation status today: "the consent card's
deposit row says each file is signed by the app", and ruling 3's author line,
``ConnectedAppsMachine::version_author``, rendered on tui).

One journey, two seats of the owner over a real consent ceremony (the
parked-deposit shape of ``test_folder_deposit_adoption.py``, whose seat,
landing and version-row helpers this module reuses by import). Seat ``a`` is
the app under test — it shows the card and reads the version history; the
seat pair is the app axis: two tui seats, two native seats (macOS on darwin,
the linux app on linux), or a device-only seat — the phone (iOS on darwin,
android on linux) or the web app (linux) — beside one tui seat, which then
adopts alone: a phone or a browser binds no folder and hosts no engine
(``sync_seats.DeviceSeatDriver``), and the ledger credits the device-only seat
alone for that pair:

- a connected app asks for ``fauna:folder:deposit:<folder>``; its request
  opens a card on seat ``a``'s Connected apps page by its binding code, and
  the card's deposit row says each file is signed by the app — read off the
  card, before the owner approves on it;
- the app deposits one file, signed with its attested writer key; the owner's
  seats adopt it (the adopter admits it only under that writer);
- the adopted change row carries the depositor's writer key on the wire
  (``deposited_by``, convention 5's API cross-check), and the owner opens the
  file's version history on seat ``a``: the version's author line is
  ``version_author_deposited`` — the app's name from the owner's connected-apps
  roster, then the recording device.
"""

from __future__ import annotations

import sys

import pytest
from cryptography.hazmat.primitives.asymmetric import ed25519, x25519

from common.auth import sync_changes_list, user_create_folder
from helpers import sync_seats
from helpers.atproto_consent import (  # noqa: F401 — pytest fixtures adopted by import
    HANDLE_DOMAIN,
    consent_bridge,
    consent_nest,
    consent_spa_url,
)
from helpers.client_metadata_server import client_document
from helpers.oauth_client import OAuthClient
from helpers.principal_session import PrincipalSession, redeem, upgrade
from helpers.waiting import wait_until
from i18n.strings import S
from tests.api.test_folder_deposit import (  # noqa: F401 — fixtures adopted by import
    _everything_on_disk,
    _raw,
    _signed,
    consent_nest_env,
    metadata_server,
)
from tests.test_folder_deposit_adoption import (
    REDIRECT_URI,
    STARTUP_WINDOW,
    WINDOW,
    _await_drained,
    _await_landed,
    _await_version_author,
    _owner_seats,
    _user_files,
)
from tests.test_folder_member_media_decrypt import _path_hash

pytestmark = [pytest.mark.tier_3]

# Its own client document path: the nest caches a client's document by URL and
# the module shares the consent nest with the adoption module's runs.
DOCUMENT = "/signature-client.json"
# `client_document`'s published `client_name` — the name the owner's roster
# gives the app, so the name the author line must carry.
APP_NAME = "Example Notes"
NAME = "signed-deposit-a7.txt"
MARKER = b"signed-deposit-plaintext-5e02"

# The deposit arm's card row is worded in shared Rust, not i18n: the browser's
# consent page and the apps' card render one composition
# (`fauna_bridge_atproto::fauna_scope::card_row` / `folder_card_row`, both
# forms — folder named or not — carrying this clause; `authorization-server.md`
# § the deposit arm, "Card row").
SIGNED_BY_THE_APP = "each signed by the app"

# The app axis as seat pairs (`sync_seats.make_seat` modes): seat `a` is the
# app under test. This journey's legs still owed on a platform, in
# `sync_seats.UNBUILT_MODES`' shape — the per-machine legs of the 2026-11-08
# lift; a landed leg deletes
# its entry (linux, android and web landed with that row).
OWED_SEATS: dict = {
    "win32": {"native": "the windows app's leg of the signed-deposit card and author line"},
}
_PAIRS = [("tui", "tui"), ("native", "native")]
if sync_seats.phone_app_for_platform(sys.platform):
    _PAIRS.append((sync_seats.PHONE_MODE, "tui"))
# The web app (Linux alone, `sync_seats.WEB_APPS`): a device-only seat like the
# phone — it shows the card and reads the history beside one adopting tui seat,
# and the ledger credits web alone for the pair.
if sync_seats.seat_mode_app(sync_seats.WEB_MODE, sys.platform):
    _PAIRS.append((sync_seats.WEB_MODE, "tui"))
_SEAT_PAIRS = [
    pytest.param(pair, id=sync_seats.seat_set_param_id(pair))
    for pair in sync_seats.seat_sets_for_platform(sys.platform, _PAIRS, unbuilt=OWED_SEATS)
]


def _depositor_on_card(nest, metadata_server, folder_id: int, app):
    """A connected app asking for ``fauna:folder:deposit:<folder_id>``, its
    card opened on ``app``'s Connected apps page by its binding code and read
    BEFORE the owner approves on it — the one step the adoption module's
    ``_depositor`` does not hand back. Returns ``(client, token, card text)``."""
    scope = f"fauna:folder:deposit:{folder_id}"
    document = DOCUMENT.replace(".json", f"-{folder_id}.json")
    metadata_server.documents[document] = client_document(
        metadata_server.client_id(document), REDIRECT_URI, scope
    )
    writer_key = ed25519.Ed25519PrivateKey.generate()
    client = OAuthClient(
        base=nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri=REDIRECT_URI,
        scope=scope,
        holder_x25519=_raw(x25519.X25519PrivateKey.generate().public_key()),
        # A deposit scope is never granted blind of its writer.
        writer_ed25519=_raw(writer_key.public_key()),
        client_id_url=metadata_server.client_id(document),
    )
    client.writer_key = writer_key  # `_signed` signs with it
    request_uri = client.push_authorization_request()
    browser_code, flow_token = client.open_consent_page(request_uri)
    ca = app.connected_apps
    assert ca.visit(), f"the Connected apps page must read. error: {ca.current_error_text()!r}"
    index = ca.wait_for_request_code(browser_code)
    assert index is not None, (
        "the depositor's request must open its card in the owner's tray. "
        f"error: {ca.current_error_text()!r} code: {browser_code!r}"
    )
    card = ca.request_text(index)
    ca.approve_request(index)
    assert ca.current_error_text() == "", (
        f"approving the deposit card must not error: {ca.current_error_text()!r}\ncard: {card!r}"
    )
    return client, redeem(client, flow_token)["access_token"], card


@pytest.mark.timeout(int(2 * STARTUP_WINDOW + 4 * WINDOW + 240))
@pytest.mark.parametrize("seat_pair", _SEAT_PAIRS)
@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.web
@pytest.mark.feature("connected-apps")
def test_the_card_says_files_are_signed_and_the_version_names_the_app(
    consent_nest, consent_bridge, metadata_server, request, seat_pair
):
    nest = consent_nest
    alice = nest["user"]
    secret_key = bytes(alice["signing_key"]).hex()
    # A web seat signs in through the consent nest's SPA proxy (`harness_sign_in`).
    spa_url = (
        request.getfixturevalue("consent_spa_url") if sync_seats.WEB_MODE in seat_pair else None
    )
    folder = f"signed-drop-{sync_seats.new_run_token(seats=1)[-8:]}"
    folder_id = user_create_folder(
        nest["port"], folder, secret_key=secret_key, base_url=nest["url"],
    )["id"]

    note = sync_seats.unbuilt_note(sys.platform, unbuilt=OWED_SEATS)
    if note:
        print(note, flush=True)

    with _owner_seats(nest, request, folder, seat_pair, spa_url=spa_url) as (seat_a, seat_b):
        # The seats that adopt: every seat hosting an engine — a phone or web
        # seat binds no folder, so it only shows the card and the history.
        adopters = [s for s in (seat_a, seat_b) if s.carries_peer_leg]
        # The owner's mailbox: the MSEK custody adoption opens with, and the
        # recipient key the nest seals deposits to — enabled on an adopter.
        mail = adopters[0].app.mail_settings
        mail.navigate()
        mail.ensure_mail_enabled()

        # ── The card: the deposit row says each file is signed by the app. ──
        client, token, card = _depositor_on_card(nest, metadata_server, folder_id, seat_a.app)
        assert APP_NAME in card, f"the card names the asking app: {card!r}"
        assert SIGNED_BY_THE_APP in card, (
            "the deposit row on the approval card must say each file is signed by "
            f"the app (folder-deposit.md § The depositor signs): {card!r}"
        )

        # ── The app deposits one file, signed with its writer key. ──
        session = PrincipalSession(upgrade(nest, client, token))
        try:
            ok, reply = session.call(
                "fauna.folders.deposit",
                _signed(client, {"folder_id": folder_id, "name": NAME,
                                 "content_type": "text/plain", "body": MARKER}),
            )
        finally:
            session.ws.close()
        assert ok and reply == {"accepted": True}, f"the deposit door refused: {reply!r}"

        # ── Adopted by the owner's seats, once. ──
        for seat in adopters:
            _await_landed(seat, f"seat {seat.name}", NAME, MARKER)
        _await_drained(nest, folder_id, adopters)
        for seat in adopters:
            assert _user_files(seat) == [NAME], (
                f"[{seat.name}] one adopted file, never a duplicate: {_user_files(seat)!r}"
            )

        # ── On the wire: the adopted row names its depositor (convention 5). ──
        depositor = _raw(client.writer_key.public_key()).hex()
        deposited: list = []

        def row_names_depositor() -> bool:
            changes = sync_changes_list(
                nest["port"], secret_key=secret_key, folder=folder, base_url=nest["url"],
            ).get("changes", [])
            deposited[:] = [
                c.get("deposited_by") for c in changes if c.get("path_hash") == _path_hash(NAME)
            ]
            return bool(deposited) and all(d == depositor for d in deposited)

        wait_until(
            row_names_depositor, WINDOW, interval=1.0,
            diagnose=lambda: (
                "the adopted change row must carry the depositor's writer key "
                f"(deposited_by) — expected {depositor}, rows carry {deposited!r}"
            ),
        )

        # ── In the version history: the author line names the app. ──
        # `version_author_deposited` up to its `{author}` slot — the recorder
        # (the adopting device) follows, and is whichever seat adopted.
        sentinel = "\x00"
        wanted = S.media.version_author_deposited(app=APP_NAME, author=sentinel).split(sentinel)[0]
        line = _await_version_author(seat_a.app, NAME, wanted, "owner seat a")
        assert line.startswith(wanted) and line[len(wanted):].strip(), (
            f"the author line is the deposited form, app then recorder: {line!r}"
        )
        unknown = S.media.version_author_deposited_unknown(author=sentinel).split(sentinel)[0]
        assert unknown not in line, (
            f"the owner's app names the app from its roster, never the generic wording: {line!r}"
        )

    # ── At rest: sealed under the folder's scheme, nothing in clear. ──
    for path, raw in _everything_on_disk(nest):
        assert MARKER not in raw, f"the adopted file's bytes rest in cleartext in {path}"
