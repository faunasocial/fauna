"""tier_3 e2e: **third-party deposit adoption** — the owner's seat opens a
parked deposit and makes it a file like any other
(``docs/goal/behavior/folder-deposit.md``, the
adoption paragraph; the write door, § Content-Addressed Storage → *Write-path
containment*).

One journey, two tui seats of the same account (the owner's laptop and
tablet), over a real consent ceremony:

- the owner's folder is bound on both seats; the owner enables mail on seat
  a's own Mail settings page, which mints the MSEK custody adoption opens with
  and publishes the recipient key the nest seals deposits to;
- a connected app holding ``fauna:folder:deposit:<folder>`` deposits one file
  through S9a's door (``fauna.folders.deposit`` on its principal session);
  the second case deposits several MiB over the HTTP door in one streamed
  POST, which the nest cuts into sealed parts, and the seats adopt it by
  fetching every part (``folder-deposit.md`` § Deposits larger than one
  message);
- the seats' next catch-up adopts it: the file lands on BOTH seats' disks
  under its own name, exactly once — one seat adopts, the other finds it
  landed (or both land the same bytes, which converge to one file);
- the inbox segment drains, and the file's bytes rest nowhere in clear on the
  nest's disk: the adopted copy is sealed under the folder's own scheme;
- the third case adopts into a **bound** folder: the owner shares it with a
  second account first, the owner's seat seals the adopted file under the
  set's M2 content key (the ordinary upload's seal), and the member's own seat
  receives it as any shared-folder change.

The deposit door itself is ``tests/api/test_folder_deposit.py``'s; this
journey reuses its consent helpers rather than copying them. Only an owner's
seat can open a parked item (it is sealed to the owner's recipient key), so a
member never adopts: in the bound case the member's seat witnesses the
ordinary shared-folder path the adopted file then travels.
"""

from __future__ import annotations

import contextlib
import shutil
import sqlite3
import tempfile
from pathlib import Path

import pytest
from cryptography.hazmat.primitives.asymmetric import x25519

from common.accounts import actor_id_hex
from common.auth import UNBINDING_MAX_DEVICES, set_tier_caps, user_create_folder
from helpers import sync_seats
from helpers.atproto_consent import (  # noqa: F401 — pytest fixtures adopted by import
    HANDLE_DOMAIN,
    consent_bridge,
    consent_nest,
)
from helpers.client_metadata_server import client_document
from helpers.oauth_client import OAuthClient
from helpers.waiting import wait_until
from tests.api.test_folder_deposit import (  # noqa: F401 — fixtures adopted by import
    _approve_with_grant,
    _everything_on_disk,
    _http_deposit,
    _inbox,
    _large_body,
    _raw,
    consent_nest_env,
    metadata_server,
)
from helpers.principal_session import PrincipalSession, upgrade

pytestmark = [pytest.mark.tier_3, pytest.mark.tui]

DOCUMENT = "/adoption-client.json"
REDIRECT_URI = "http://127.0.0.1:17777/callback"
NAME = "deposited-report-e1.txt"
MARKER = b"adopted-deposit-plaintext-4b9d"

# Ceilings a green run never pays (convention 14).
STARTUP_WINDOW = 180.0
WINDOW = 180.0


@contextlib.contextmanager
def _owner_seats(nest, request, folder: str):
    """The consent nest's owner on two tui seats bound to ``folder`` — the same
    account, two devices. Yields ``(seat_a, seat_b)``."""
    from tests.test_filesync_seats import harness_sign_in

    alice = nest["user"]
    secret_key = bytes(alice["signing_key"]).hex()
    actor_id = actor_id_hex(secret_key)
    set_tier_caps(
        nest["port"], admin_signing_key=nest["admin"]["signing_key"],
        max_devices=UNBINDING_MAX_DEVICES, base_url=nest["url"],
    )
    run_token = sync_seats.new_run_token(seats=2)
    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))
    try:
        with contextlib.ExitStack() as stack:
            seats = [
                stack.enter_context(
                    sync_seats.make_seat(
                        "tui",
                        name=name,
                        run_token=run_token,
                        root=tmp / name,
                        node_url=nest["url"],
                        node_port=nest["port"],
                        folder=folder,
                        sign_in=harness_sign_in(
                            nest["url"], secret_key, actor_id, f"alice.{HANDLE_DOMAIN}"
                        ),
                        request=request,
                    )
                )
                for name in ("a", "b")
            ]
            for seat in seats:
                seat.await_ready(STARTUP_WINDOW)
            yield seats[0], seats[1]
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def _user_files(seat) -> list[str]:
    """The names in the seat's folder — the engine's own dot-dirs aside."""
    return sorted(p.name for p in seat.path.iterdir() if not p.name.startswith("."))


def _await_landed(seat, what: str, name: str = NAME, body: bytes = MARKER) -> None:
    target = seat.path / name

    def landed() -> bool:
        try:
            return target.read_bytes() == body
        except OSError:
            return False

    wait_until(
        landed, WINDOW, interval=1.0,
        diagnose=lambda: (
            f"{what}: the deposit never landed at {target}; folder holds "
            f"{_user_files(seat)!r}\n{seat.diagnostics()}"
        ),
    )


def _depositor(nest, metadata_server, folder_id: int):
    """A connected app holding ``fauna:folder:deposit:<folder_id>`` through a
    real consent ceremony — one client document per folder, since the nest
    caches a client's document by its URL and the module shares the nest.
    Returns ``(client, access token)``."""
    scope = f"fauna:folder:deposit:{folder_id}"
    document = DOCUMENT.replace(".json", f"-{folder_id}.json")
    metadata_server.documents[document] = client_document(
        metadata_server.client_id(document), REDIRECT_URI, scope
    )
    holder = x25519.X25519PrivateKey.generate()
    holder_pub = _raw(holder.public_key())
    client = OAuthClient(
        base=nest["url"],
        htu_origin=f"https://{HANDLE_DOMAIN}",
        redirect_uri=REDIRECT_URI,
        scope=scope,
        holder_x25519=holder_pub,
        client_id_url=metadata_server.client_id(document),
    )
    token = _approve_with_grant(
        nest, client, holder_pub, folder_id, scope, choose=False
    )["access_token"]
    return client, token


def _parts_at_rest(nest) -> int:
    """Every sealed part resting on the nest, whichever item it belongs to —
    an adopted item's retire takes its parts with it."""
    conn = sqlite3.connect(f"file:{nest['db_path']}?mode=ro", uri=True, timeout=10.0)
    try:
        return conn.execute("SELECT COUNT(*) FROM folder_deposit_parts").fetchone()[0]
    finally:
        conn.close()


def _await_drained(nest, folder_id: int, seats) -> None:
    wait_until(
        lambda: _inbox(nest, folder_id) == [], WINDOW, interval=1.0,
        diagnose=lambda: (
            f"the inbox never drained: {len(_inbox(nest, folder_id))} item(s) "
            "still parked\n" + "\n".join(seat.diagnostics() for seat in seats)
        ),
    )


@pytest.mark.timeout(int(2 * STARTUP_WINDOW + 3 * WINDOW + 240))
@pytest.mark.feature("connected-apps")
def test_an_owner_seat_adopts_a_parked_deposit_once(
    consent_nest, consent_bridge, metadata_server, request
):
    nest = consent_nest
    alice = nest["user"]
    folder = f"drop-{sync_seats.new_run_token(seats=1)[-8:]}"
    folder_id = user_create_folder(
        nest["port"], folder, secret_key=bytes(alice["signing_key"]).hex(),
        base_url=nest["url"],
    )["id"]

    with _owner_seats(nest, request, folder) as (seat_a, seat_b):
        # The owner's mailbox, enabled the way a user enables one: the MSEK
        # custody both seats' engines open deposits with, and the recipient
        # key the nest seals them to.
        mail = seat_a.app.mail_settings
        mail.navigate()
        mail.ensure_mail_enabled()

        client, token = _depositor(nest, metadata_server, folder_id)
        session = PrincipalSession(upgrade(nest, client, token))
        try:
            ok, reply = session.call(
                "fauna.folders.deposit",
                {"folder_id": folder_id, "name": NAME, "content_type": "text/plain",
                 "body": MARKER},
            )
        finally:
            session.ws.close()
        assert ok and reply == {"accepted": True}, f"the deposit door refused: {reply!r}"

        # ── Adoption: on both seats, once. ──
        _await_landed(seat_a, "seat a")
        _await_landed(seat_b, "seat b")
        _await_drained(nest, folder_id, (seat_a, seat_b))
        for seat in (seat_a, seat_b):
            assert _user_files(seat) == [NAME], (
                f"[{seat.name}] one adopted file, never a duplicate: {_user_files(seat)!r}"
            )

    # ── At rest: sealed under the folder's scheme, nothing in clear. ──
    for path, raw in _everything_on_disk(nest):
        assert MARKER not in raw, (
            f"the adopted file's bytes rest in cleartext in {path} — adoption must "
            "re-seal under the folder's own scheme (folder-deposit.md § The "
            "deposit)"
        )


LARGE_NAME = "deposited-scan-7f.bin"


@pytest.mark.timeout(int(2 * STARTUP_WINDOW + 3 * WINDOW + 240))
@pytest.mark.feature("connected-apps")
def test_an_owner_seat_adopts_a_deposit_larger_than_one_message(
    consent_nest, consent_bridge, metadata_server, request
):
    """Several MiB over the HTTP door in one streamed POST — cut into sealed
    parts nest-side — adopted by the owner's seats: each fetches and opens
    every part and lands the file once, with exactly the deposited bytes; the
    inbox drains with every part, and no part rests in clear on the nest's
    disk (``folder-deposit.md`` § Deposits larger than one message)."""
    nest = consent_nest
    alice = nest["user"]
    folder = f"drop-{sync_seats.new_run_token(seats=1)[-8:]}"
    folder_id = user_create_folder(
        nest["port"], folder, secret_key=bytes(alice["signing_key"]).hex(),
        base_url=nest["url"],
    )["id"]
    body, markers = _large_body(b"adopt-large")

    with _owner_seats(nest, request, folder) as (seat_a, seat_b):
        mail = seat_a.app.mail_settings
        mail.navigate()
        mail.ensure_mail_enabled()

        client, token = _depositor(nest, metadata_server, folder_id)
        status, reply = _http_deposit(nest, client, token, folder_id, LARGE_NAME, body)
        assert status == 202 and reply == {"accepted": True}, (
            f"the HTTP door refused a body larger than one message: {status} {reply!r}"
        )

        # ── Adoption: every part fetched, the file on both seats, once. ──
        _await_landed(seat_a, "seat a", LARGE_NAME, body)
        _await_landed(seat_b, "seat b", LARGE_NAME, body)
        _await_drained(nest, folder_id, (seat_a, seat_b))
        assert _parts_at_rest(nest) == 0, (
            "the adopted item's retire takes its sealed parts with it"
        )
        for seat in (seat_a, seat_b):
            assert _user_files(seat) == [LARGE_NAME], (
                f"[{seat.name}] one adopted file, never a duplicate: {_user_files(seat)!r}"
            )

    # ── At rest: sealed under the folder's scheme, no part in clear. ──
    for path, raw in _everything_on_disk(nest):
        for marker in [*markers, LARGE_NAME.encode()]:
            assert marker not in raw, (
                f"{marker!r} rests in cleartext in {path} — adoption re-seals the "
                "reassembled file under the folder's own scheme"
            )


BOUND_NAME = "deposited-minutes-b3.txt"
BOUND_MARKER = b"adopted-into-a-bound-folder-plaintext-c81e"

# The member's KeyPackage publish and the shared folder reaching their Folders
# page (convention 14: ceilings a green run never pays).
KEYPACKAGE_WINDOW = 30.0
MEMBER_ROW_WINDOW = 90.0
# A member's engine hydrates a change it never wrote — a pull, a content-key
# open and a chunk walk after the owner's upload: the bound journeys' budget.
MEMBER_WINDOW = 360.0


def _enter(stack: contextlib.ExitStack, seat):
    """The first yield of a seat generator, its teardown on ``stack``."""
    value = next(seat)
    stack.callback(seat.close)
    return value


@pytest.mark.timeout(1500)
@pytest.mark.real_conversations
@pytest.mark.feature("connected-apps")
def test_an_owner_seat_adopts_into_a_bound_folder_and_a_member_receives_it(
    consent_nest, consent_bridge, metadata_server, request, tmp_path
):
    """Adoption into a **bound** folder: the owner's seat re-seals the parked
    item under the set's M2 content key — the ordinary upload's seal; adoption
    has none of its own — and a member's seat receives it as any shared-folder
    change (``folder-deposit.md`` § The deposit, the adoption paragraph).

    Only the owner's recipient secret opens a parked item, so the member never
    adopts: a member's engine is never armed for it, and the file reaches their
    disk only through the pull every member makes. The folder is shared before
    the deposit, so the member receives a live change, not history on join.

    Two tui apps on the consent nest: the owner (``alice``, the account the
    connected-app ceremony approves for) with a live conversations session to
    share from, and a member who accepts the share and binds a folder of their
    own as a writer — the bound recipient's shape in
    ``test_folder_member_media_decrypt.py``'s second-share journey.
    """
    from common.auth import sync_changes_list
    from conftest import _folder_share_owner_seat, _folder_share_recipient_seat
    from helpers.folder_content import agent_diagnosis, bind_location_under_set
    from helpers.folder_share import roster_index, share_through_owner_ui
    from tests.api import conv_api
    from tests.test_folder_agent_content_sync import _await_file_content
    from tests.test_folder_member_media_decrypt import _path_hash

    nest = consent_nest
    alice = nest["user"]
    folder = f"bound-drop-{sync_seats.new_run_token(seats=1)[-8:]}"
    # Created custody-first before either app launches, so the owner's
    # Folders page lists it on its first read.
    folder_id = user_create_folder(
        nest["port"], folder, secret_key=bytes(alice["signing_key"]).hex(),
        base_url=nest["url"],
    )["id"]

    with contextlib.ExitStack() as stack:
        owner_app, _ = _enter(
            stack, _folder_share_owner_seat("tui", nest, request, owner=alice)
        )
        member_app, member = _enter(
            stack,
            _folder_share_recipient_seat(
                "tui", nest, request, handle_prefix="member", domain=HANDLE_DOMAIN,
                screenshot="teardown-deposit-member",
            ),
        )
        request.addfinalizer(lambda: print(agent_diagnosis(owner_app, "owner")))
        request.addfinalizer(lambda: print(agent_diagnosis(member_app, "member")))

        # ── The owner shares the folder; the member accepts and binds it. ──
        wait_until(
            lambda: conv_api.keypackage_count(nest["port"], member, member["actor_id_hex"]) > 0,
            KEYPACKAGE_WINDOW,
            diagnose=lambda: "the member never published a KeyPackage the share can admit",
        )
        ob = owner_app.backups
        row = share_through_owner_ui(owner_app, folder, member, want=1)
        ob.set_member_access("writer", roster_index(ob, member["handle"], row=row), row=row)
        wait_until(
            lambda: ob.member_access(roster_index(ob, member["handle"], row=row), row=row)
            == "writer",
            15.0,
            diagnose=lambda: f"the writer grant never persisted; error={owner_app.error_text()!r}",
        )
        owner_folder = tmp_path / "owner-bound"
        owner_folder.mkdir()
        bind_location_under_set(owner_app, folder, owner_folder, seat="owner")

        mb = member_app.backups
        mb.navigate_folders()
        assert mb.wait_for_pending_shares(1) == 1, (
            f"[member] the share should wait as one pending knock; error={member_app.error_text()!r}"
        )
        mb.accept_pending_share(0)

        def _member_row_listed() -> bool:
            if any(folder in mb.folder_title(k) for k in range(mb.folder_count())):
                return True
            mb.navigate_devices()
            mb.navigate_folders()
            return False

        wait_until(
            _member_row_listed, MEMBER_ROW_WINDOW,
            diagnose=lambda: f"[member] the folder never appeared; error={member_app.error_text()!r}",
        )
        member_folder = tmp_path / "member-bound"
        member_folder.mkdir()
        bind_location_under_set(member_app, folder, member_folder, seat="member")

        # ── The owner's mailbox: the custody adoption opens with. ──
        mail = owner_app.mail_settings
        mail.navigate()
        mail.ensure_mail_enabled()

        # ── A connected app deposits into the shared folder. ──
        client, token = _depositor(nest, metadata_server, folder_id)
        session = PrincipalSession(upgrade(nest, client, token))
        try:
            ok, reply = session.call(
                "fauna.folders.deposit",
                {"folder_id": folder_id, "name": BOUND_NAME,
                 "content_type": "text/plain", "body": BOUND_MARKER},
            )
        finally:
            session.ws.close()
        assert ok and reply == {"accepted": True}, f"the deposit door refused: {reply!r}"

        # ── The owner's seat adopts, under the set's content key. ──
        _await_file_content(
            owner_folder / BOUND_NAME, BOUND_MARKER.decode(), seat="owner",
            app=owner_app, window=WINDOW,
        )
        wait_until(
            lambda: _inbox(nest, folder_id) == [], WINDOW, interval=1.0,
            diagnose=lambda: (
                f"the inbox never drained: {len(_inbox(nest, folder_id))} item(s) "
                "still parked\n" + agent_diagnosis(owner_app, "owner")
            ),
        )
        changes = sync_changes_list(
            nest["port"], secret_key=bytes(alice["signing_key"]).hex(),
            folder=folder, base_url=nest["url"],
        ).get("changes", [])
        generations = {
            c.get("path_hash"): c.get("content_key_version") for c in changes
        }
        assert generations.get(_path_hash(BOUND_NAME)) is not None, (
            "the adopted file must be sealed under the set's M2 content key — a "
            "change with no content-key generation was sealed under the owner's "
            f"own BackupKey, which no member can open: {generations!r}"
        )

        # ── …and the member's seat receives it as any shared change. ──
        _await_file_content(
            member_folder / BOUND_NAME, BOUND_MARKER.decode(), seat="member",
            app=member_app, window=MEMBER_WINDOW,
        )
        for who, path in (("owner", owner_folder), ("member", member_folder)):
            files = sorted(p.name for p in path.iterdir() if not p.name.startswith("."))
            assert files == [BOUND_NAME], (
                f"[{who}] one adopted file, never a duplicate: {files!r}"
            )

    # ── At rest: sealed under the set's content key, nothing in clear. ──
    for path, raw in _everything_on_disk(nest):
        assert BOUND_MARKER not in raw, (
            f"the adopted file's bytes rest in cleartext in {path} — adoption "
            "re-seals under the folder's own scheme, the M2 content key for a "
            "bound one (folder-deposit.md § The deposit)"
        )
