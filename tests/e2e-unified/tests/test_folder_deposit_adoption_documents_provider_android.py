"""tier_3: a third-party deposit adopted by the android documents provider's refresh.

Android keeps no synced local folder, so the two-seat witnesses of
``test_folder_deposit_adoption.py`` take no android seat; an android phone
reaches a deposit only as a capability host — the SAF ``DocumentsProvider``
over the replica the app owns, whose host's ``Refresh`` arm runs the shared
adoption pass after its fold (``docs/goal/behavior/folder-deposit.md``
§ Implementation status today, *Adoption on a capability host*: "the apple
File Provider face and android's provider, from the host's ``Refresh`` arm";
the provider, ``on-demand-files-android.md`` § Android SAF DocumentsProvider
binding). The iOS twin is
``platform/ios/test_folder_deposit_adoption_file_provider_ios.py``; this
module is its android arm over the bridge's documents verbs
(``drivers/android.py``'s ``documents_*``, the instrumentation's
``DocumentsOps``), which call the provider through a ``ContentResolver``
exactly as another app's picker does.

The first journey, one android seat of the folder's owner:

  1. The owner's folder exists; on the seat's Folders page the owner turns
     its "Show in Files" toggle on — the enrol gesture that writes this
     device's place, after which the provider presents the set.
  2. The owner enables mail on the seat's Mail settings page (the MSEK custody
     the host opens the deposit with, the recipient key the nest seals it to)
     and approves a connected app's deposit request on the seat's own
     Connected apps card; the app deposits one file through the deposit door.
  3. A picker's re-query of the set's directory is the browser's refresh: it
     drives ``host.refresh()``, whose adoption pass lands the deposit. The
     provider lists the file under its own name, once, and serves the
     deposited bytes; the inbox drains; nothing rests in clear on the nest's
     disk.

The second journey adopts into a folder the owner SHARES: an owner tui seat
shares the set with a member's tui seat, who accepts and binds it; the owner's
tui seat is then closed (a live owner root would adopt first), the android seat
presents the set, and the provider host's refresh re-seals the adopted item
under the set's M2 content key — its change carries a content-key generation —
and the member's seat receives it once, as any shared-folder change.

Mutation carve-out (testing conventions point 8): the folder's creation is
fixture setup through the nest's own folder call; every other mutation goes
through the apps' UI. The action under test is the capability host's adoption,
driven by a picker's re-query, the provider's own refresh trigger.
"""

from __future__ import annotations

import contextlib
import secrets
import shutil
import tempfile
from pathlib import Path

import pytest

from common.accounts import actor_id_hex
from common.auth import UNBINDING_MAX_DEVICES, set_tier_caps, user_create_folder
from helpers.atproto_consent import (  # noqa: F401 — pytest fixtures adopted by import
    HANDLE_DOMAIN,
    consent_bridge,
    consent_nest,
)
from helpers.android_documents import presented_set as _presented_set
from helpers.android_documents import show_in_files as _show_in_files
from helpers.principal_session import PrincipalSession, upgrade
from helpers.sync_seats import PHONE_MODE, make_seat, new_run_token
from helpers.waiting import wait_until
from tests.api.test_folder_deposit import (  # noqa: F401 — fixtures adopted by import
    _everything_on_disk,
    _inbox,
    _signed,
    consent_nest_env,
    metadata_server,
)
from tests.test_folder_deposit_adoption import _depositor

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.android,
    # The emulator is driven over adb from the linux box (`seat_modes.PHONE_APPS`).
    pytest.mark.host_platform("linux"),
]

NAME = "deposited-report-a3.txt"
MARKER = b"android-provider-adopted-deposit-plaintext-5e1c"
SHARED_NAME = "deposited-agenda-a4.txt"
SHARED_MARKER = b"android-provider-adopted-into-a-shared-folder-plaintext-2b7f"

# Ceilings a green run never pays (convention 14).
STARTUP_WINDOW = 180.0
PRESENT_WINDOW = 90.0
ADOPT_WINDOW = 180.0
DRAIN_WINDOW = 60.0
KEYPACKAGE_WINDOW = 30.0
MEMBER_ROW_WINDOW = 90.0
MEMBER_WINDOW = 360.0


@contextlib.contextmanager
def _android_owner_seat(nest, request, folder: str):
    """The consent nest's owner (``alice``, the account the connected-app
    ceremony approves for) on one android phone seat — a device of the
    account, signed in and Connected. Yields the seat."""
    from tests.test_filesync_seats import harness_sign_in

    alice = nest["user"]
    secret_key = bytes(alice["signing_key"]).hex()
    set_tier_caps(
        nest["port"], admin_signing_key=nest["admin"]["signing_key"],
        max_devices=UNBINDING_MAX_DEVICES, base_url=nest["url"],
    )
    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))
    try:
        with make_seat(
            PHONE_MODE,
            name="a",
            run_token=new_run_token(seats=1),
            root=tmp / "a",
            node_url=nest["url"],
            node_port=nest["port"],
            folder=folder,
            sign_in=harness_sign_in(
                nest["url"], secret_key, actor_id_hex(secret_key), f"alice.{HANDLE_DOMAIN}"
            ),
            request=request,
        ) as seat:
            seat.await_ready(STARTUP_WINDOW)
            yield seat
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def _await_adopted(drv, nest, folder_id: int, set_doc: str, name: str, body: bytes) -> None:
    """Re-query the set's directory — the browser's refresh, which drives the
    host's ``refresh`` and its adoption pass — until the provider lists
    ``name`` once and serves exactly ``body`` for it."""

    def adopted() -> bool:
        rows = [c for c in drv.documents_children(set_doc) if c.get("name") == name]
        return len(rows) == 1 and drv.documents_read(rows[0]["id"]) == body

    wait_until(
        adopted, ADOPT_WINDOW, interval=2.0,
        diagnose=lambda: (
            f"{name} never read back with the deposited bytes through the provider — "
            f"its refresh did not adopt the parked deposit; "
            f"{len(_inbox(nest, folder_id))} item(s) still parked; the set lists "
            f"{drv.documents_children(set_doc)!r}; parks {drv.documents_parks()!r}"
        ),
    )
    names = [c.get("name") for c in drv.documents_children(set_doc)]
    assert names.count(name) == 1, f"one adopted file, never a duplicate: {names!r}"


def _deposit(nest, metadata_server, folder_id: int, app, name: str, body: bytes) -> None:
    """The owner approves the connected app's deposit request on ``app``'s own
    card (its ``Mint`` logged first, so no reconcile sweep revokes it), and the
    app deposits one signed file through the door on its principal session."""
    client, token = _depositor(nest, metadata_server, folder_id, app)
    session = PrincipalSession(upgrade(nest, client, token))
    try:
        ok, reply = session.call(
            "fauna.folders.deposit",
            _signed(client, {"folder_id": folder_id, "name": name,
                             "content_type": "text/plain", "body": body}),
        )
    finally:
        session.ws.close()
    assert ok and reply == {"accepted": True}, f"the deposit door refused: {reply!r}"
    assert len(_inbox(nest, folder_id)) == 1, "the deposit is parked before the host adopts it"


def _await_drained(nest, folder_id: int) -> None:
    wait_until(
        lambda: _inbox(nest, folder_id) == [], DRAIN_WINDOW, interval=1.0,
        diagnose=lambda: (
            f"adoption left the deposit parked: {len(_inbox(nest, folder_id))} item(s) listed"
        ),
    )


def _nothing_in_clear(nest, name: str, body: bytes) -> None:
    for path, raw in _everything_on_disk(nest):
        for marker in (body, name.encode()):
            assert marker not in raw, (
                f"{marker!r} rests in cleartext in {path} — adoption re-seals under the "
                "folder's own scheme (folder-deposit.md § The deposit)"
            )


@pytest.mark.timeout(int(STARTUP_WINDOW + PRESENT_WINDOW + ADOPT_WINDOW + DRAIN_WINDOW + 600))
@pytest.mark.feature("connected-apps")
def test_a_deposit_is_adopted_by_the_android_documents_provider_refresh(
    consent_nest, consent_bridge, metadata_server, request
):
    nest = consent_nest
    alice = nest["user"]
    folder = f"dp-drop-{secrets.token_hex(4)}"
    folder_id = user_create_folder(
        nest["port"], folder, secret_key=bytes(alice["signing_key"]).hex(),
        base_url=nest["url"],
    )["id"]

    with _android_owner_seat(nest, request, folder) as seat:
        app = seat.app
        drv = app.driver
        _show_in_files(app, folder)
        set_doc = _presented_set(drv, folder)

        mail = app.mail_settings
        mail.navigate()
        mail.ensure_mail_enabled()

        _deposit(nest, metadata_server, folder_id, app, NAME, MARKER)

        # ── The action under test: the provider's refresh adopts the deposit. ──
        _await_adopted(drv, nest, folder_id, set_doc, NAME, MARKER)
        _await_drained(nest, folder_id)
        assert not app.has_error(), f"the journey surfaced an error: {app.error_text()!r}"

    _nothing_in_clear(nest, NAME, MARKER)


@pytest.mark.timeout(3600)
@pytest.mark.real_conversations
@pytest.mark.feature("connected-apps")
def test_the_android_documents_provider_adopts_into_a_shared_folder_and_a_member_receives_it(
    consent_nest, consent_bridge, metadata_server, request, tmp_path
):
    """Adoption into a folder the owner **shares**, by the android capability
    host: the provider host's refresh re-seals the parked item under the set's
    M2 content key — the ordinary upload's seal (``on-demand-files-shared-sets.md``,
    decision 1′) — and the member's own seat receives it as any shared-folder
    change (``folder-deposit.md`` § The deposit, the adoption paragraph).

    The owner's tui seat shares the folder and is CLOSED before the android
    seat comes up: an owner root adopts after its pull, so a live tui owner
    could adopt first and the witness would no longer be the provider's. The
    member's tui seat stays up; a member never adopts (only the owner's
    recipient secret opens a parked item)."""
    from common.auth import sync_changes_list
    from conftest import _folder_share_owner_seat, _folder_share_recipient_seat
    from helpers.folder_content import agent_diagnosis, bind_location_under_set
    from helpers.folder_share import roster_index, share_through_owner_ui
    from tests.api import conv_api
    from tests.test_folder_agent_content_sync import _await_file_content
    from tests.test_folder_member_media_decrypt import _path_hash

    nest = consent_nest
    alice = nest["user"]
    folder = f"dp-shared-drop-{secrets.token_hex(4)}"
    # Created before any app launches, so the owner's Folders page lists it
    # on its first read.
    folder_id = user_create_folder(
        nest["port"], folder, secret_key=bytes(alice["signing_key"]).hex(),
        base_url=nest["url"],
    )["id"]

    with contextlib.ExitStack() as stack:
        owner_seat = _folder_share_owner_seat("tui", nest, request, owner=alice)
        owner_app, _ = next(owner_seat)
        stack.callback(owner_seat.close)
        member_seat = _folder_share_recipient_seat(
            "tui", nest, request, handle_prefix="member", domain=HANDLE_DOMAIN,
            screenshot="teardown-dp-deposit-member",
        )
        member_app, member = next(member_seat)
        stack.callback(member_seat.close)
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

        mb = member_app.backups
        mb.navigate_folders()
        assert mb.wait_for_pending_shares(1) == 1, (
            f"[member] the share should wait as one pending knock; error={member_app.error_text()!r}"
        )
        mb.accept_pending_share(0)

        def _member_row_listed() -> bool:
            if any(folder in t for t in mb.folder_titles()):
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

        # ── The owner's tui seat closes: no owner root but the provider's. ──
        owner_seat.close()

        seat = stack.enter_context(_android_owner_seat(nest, request, folder))
        app = seat.app
        drv = app.driver
        _show_in_files(app, folder)
        set_doc = _presented_set(drv, folder)

        mail = app.mail_settings
        mail.navigate()
        mail.ensure_mail_enabled()

        _deposit(nest, metadata_server, folder_id, app, SHARED_NAME, SHARED_MARKER)

        # ── The action under test: the provider's refresh adopts the deposit. ──
        _await_adopted(drv, nest, folder_id, set_doc, SHARED_NAME, SHARED_MARKER)
        _await_drained(nest, folder_id)
        changes = sync_changes_list(
            nest["port"], secret_key=bytes(alice["signing_key"]).hex(), folder=folder,
            base_url=nest["url"],
        ).get("changes", [])
        generations = {c.get("path_hash"): c.get("content_key_version") for c in changes}
        assert generations.get(_path_hash(SHARED_NAME)) is not None, (
            "the host must seal the adopted file under the set's M2 content key — a "
            "change with no content-key generation was sealed under the owner's own "
            f"BackupKey, which no member can open: {generations!r}"
        )

        # ── …and the member's seat receives it as any shared change, once. ──
        _await_file_content(
            member_folder / SHARED_NAME, SHARED_MARKER.decode(), seat="member",
            app=member_app, window=MEMBER_WINDOW,
        )
        files = sorted(p.name for p in member_folder.iterdir() if not p.name.startswith("."))
        assert files == [SHARED_NAME], f"[member] one adopted file, never a duplicate: {files!r}"

    _nothing_in_clear(nest, SHARED_NAME, SHARED_MARKER)
