"""Media's ``media-source-status`` — whether the folder an item lives in can be
reached right now, told apart from whether the file itself is in sync.

Owner of the dot's UX: ``docs/goal/ui/media.md`` § Source status vs. sync state —
the dot is the FOLDER's content reachability and is "distinct from the per-file
``sync-state-badge``"; both ride the ``media-item`` row and answer different
questions. Owner of what "reachable" means: ``docs/goal/behavior/file-sync.md``
§ Content reachability — the folder's content is reachable when the nest holds it
(residency full, the default) or a seat the relay read path would ask is connected.

**Why metadata-only for the flip, and who holds.** Under that rule a full
folder is reachable whenever the nest is, so it has only one value to show; the
flip lives on a **metadata-only** folder, whose bytes rest on no nest and are
reachable only while a seat holding them is connected and has announced the
folder (``file-sync.md`` § Relay serving).

**The holder is a second device of the same account: a real app with its own
sync agent**, bound to the folder (``helpers/sync_seats.make_seat``). Its agent's
engine writes the file and signs the record, and the agent announces the folder
on its own WS-RPC connection — which is what makes the folder reachable. The app
under test binds nothing: it is the device with no copy, looking at the dot. The
seat is stopped through the driver's own teardown, never a kill; that closes the
agent's connection, the announce ends with it, and the folder reads unreachable.

Each test also reads the verdict off both wire replies (``fauna.media.list`` and
``fauna.sync.status``, convention 5), so a red names the nest or the app.
"""

import secrets
import shutil
import tempfile
import time
from pathlib import Path

import pytest

from common import (
    set_folder_residency,
    sync_status,
    user_create_folder,
)
from common.auth import UNBINDING_MAX_DEVICES, _user_call, set_tier_caps
from conftest import _login_app_as, _make_user, _seed_cross_set_media
from helpers import sync_seats
from helpers.set_names import set_name_hash
from i18n.strings import S

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]

#: Generous ceilings (convention 14): a green run returns on the first matching
#: read. The holder seat's launch + bind, and the item reaching the page.
SEAT_READY_BUDGET_S = 180.0
ITEM_BUDGET_S = 60.0
#: The holder's record: a first record a just-started seat sends can be refused
#: and lands on its next reconcile tick, so this covers more than one tick.
RECORD_BUDGET_S = 180.0


def _in_folder(item: dict, folder: str) -> bool:
    """Whether a ``fauna.media.list`` item belongs to the set called ``folder``.

    By hash first: once a seat's engine stamps the sealed set name the set rests
    no plaintext name (schema 114), so the item's ``folder`` is blank beside its
    ``folder_hash`` — a plaintext-only match finds nothing although the record
    landed. A set no engine has stamped still carries its plaintext."""
    raw = item.get("folder_hash")
    if raw is not None and bytes(raw) == set_name_hash(folder):
        return True
    return item.get("folder") == folder


def _wire_verdicts(nest_instance, secret_key: str, folder: str) -> tuple:
    """``(media.list, sync.status)`` ``source_online`` for ``folder`` — the two
    replies carrying the one per-folder verdict, read straight off the nest."""
    port, url = nest_instance["port"], nest_instance["url"]
    items = _user_call(port, secret_key, "fauna.media.list", {"limit": 1000, "cursor_version": 2}, url).get(
        "items", []
    )
    listed = {item.get("source_online") for item in items if _in_folder(item, folder)}
    status = sync_status(port, secret_key=secret_key, folder=folder, base_url=url)
    return listed, status.get("source_online")


def _await_listed(nest_instance, secret_key: str, folder: str, budget: float) -> list:
    """``folder``'s ``fauna.media.list`` items once any is recorded — a deadline
    poll that returns on the first non-empty read (convention 14)."""
    port, url = nest_instance["port"], nest_instance["url"]
    deadline = time.monotonic() + budget
    while True:
        items = _user_call(
            port, secret_key, "fauna.media.list", {"limit": 1000, "cursor_version": 2}, url
        ).get("items", [])
        mine = [item for item in items if _in_folder(item, folder)]
        if mine or time.monotonic() >= deadline:
            return mine
        time.sleep(0.5)


def _await_wire_verdicts(nest_instance, secret_key: str, folder: str, want: tuple, budget: float) -> tuple:
    """Both wire verdicts once they read ``want`` — a deadline poll returning on
    the first match (convention 14); the last read otherwise, for the message."""
    deadline = time.monotonic() + budget
    while True:
        got = _wire_verdicts(nest_instance, secret_key, folder)
        if got == want or time.monotonic() >= deadline:
            return got
        time.sleep(0.5)


# tui leads. web and linux ran 2026-10-06 and failed before their app was
# asked anything, on the same red as tui's own arm: the holder DID record the
# file, but the seat's start stamps the sealed set name, after which the item's
# plaintext `folder` is blank and only `folder_hash` names the set — the
# plaintext-only match `_in_folder` replaced read the record as missing. They
# join with their own run; android joins with its venue.
@pytest.mark.tui
@pytest.mark.timeout(int(SEAT_READY_BUDGET_S + RECORD_BUDGET_S + 4 * ITEM_BUDGET_S + 180))
@pytest.mark.feature("media")
def test_media_item_says_whether_its_folder_can_be_reached_apart_from_its_sync_state(
    request, app, nest_instance
):
    """A metadata-only folder's item says its files can be reached while a device
    holding them is running, and that they cannot once that device stops — and
    through both, the file's own sync badge does not move, because the file did
    not change.

    One metadata-only folder, one file written on the holder device (a real app
    and its agent), read from the app under test, which holds no copy. The flip
    is the assertion: a dot that always read one value (the easy wrong
    implementation) fails one half or the other, and a dot that followed the
    file's sync state instead of the folder's reachability fails on the badge.
    """
    from tests.test_filesync_seats import harness_sign_in

    port, url = nest_instance["port"], nest_instance["url"]
    user = _make_user(nest_instance)
    secret_key = user["signing_key"].encode().hex()
    # The holder and the app under test are two devices of one account; never
    # let the tier cap refuse one.
    set_tier_caps(
        port, admin_signing_key=nest_instance["admin"]["signing_key"],
        max_devices=UNBINDING_MAX_DEVICES, base_url=url,
    )
    folder = f"reach-{secrets.token_hex(4)}"
    user_create_folder(port, folder, secret_key=secret_key, base_url=url)
    # Metadata-only BEFORE any engine first sees the folder, so the nest never
    # rests these bytes and only a device holding them could serve them.
    reply = set_folder_residency(
        port, folder, secret_key=secret_key, residency="metadata_only", base_url=url
    )
    assert reply.get("ok") is True, f"residency set refused: {reply!r}"
    assert _wire_verdicts(nest_instance, secret_key, folder)[1] is False, (
        "a metadata-only folder no device has announced should read unreachable "
        "before the holder starts"
    )

    media = app.media
    name = "reachable.txt"
    tmp = Path(tempfile.mkdtemp(prefix=f"fauna-{folder}-"))
    try:
        seat = sync_seats.make_seat(
            "tui",
            name="b",
            run_token=sync_seats.new_run_token(seats=1),
            root=tmp / "b",
            node_url=url,
            node_port=port,
            folder=folder,
            sign_in=harness_sign_in(url, secret_key, user["actor_id_hex"], user["handle"]),
            request=request,
        )
        with seat:
            seat.await_ready(SEAT_READY_BUDGET_S)
            (seat.path / name).write_text(
                f"a file whose folder can be reached {secrets.token_hex(8)}\n"
            )
            listed = _await_listed(nest_instance, secret_key, folder, RECORD_BUDGET_S)
            assert len(listed) == 1, (
                f"the holder's engine should have recorded {name!r} into {folder!r}; "
                f"fauna.media.list holds {listed!r} for it.\n{seat.diagnostics()}"
            )

            verdicts = _await_wire_verdicts(
                nest_instance, secret_key, folder, ({True}, True), ITEM_BUDGET_S
            )
            assert verdicts == ({True}, True), (
                "with the holder's agent running and its folder announced, both wire "
                f"replies should call the metadata-only folder reachable; got "
                f"{verdicts!r}\n{seat.diagnostics()}"
            )

            _login_app_as(app, request, nest_instance, user, verify_live_actor=True)
            media.navigate()
            media.ensure_loaded()
            assert media.wait_for_item_count(1, timeout=ITEM_BUDGET_S) == 1, (
                f"the recorded file should list as one item; listed "
                f"{media.item_names()!r}; error={app.error_text()!r}\n"
                f"{seat.diagnostics()}"
            )

            reachable = media.wait_for_source_status(name, S.media.source_online)
            assert reachable == S.media.source_online, (
                f"while a device holding its files is running, {name!r} should say "
                f"its folder can be reached ({S.media.source_online!r}); it says "
                f"{reachable!r}"
            )
            badge_while_reachable = app.driver.get_text(
                "sync-state-badge", scope=f"media-item[{media.index_of(name)}]"
            )
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

    # The holder is stopped (the driver's own teardown): its agent's connection
    # closed, the announce ended with it, and no device holds these bytes now.
    verdicts = _await_wire_verdicts(
        nest_instance, secret_key, folder, ({False}, False), ITEM_BUDGET_S
    )
    assert verdicts == ({False}, False), (
        "once the only holder has stopped, both wire replies should call the "
        f"metadata-only folder unreachable; got {verdicts!r}"
    )
    unreachable = media.wait_for_source_status(name, S.media.source_offline)
    assert unreachable == S.media.source_offline, (
        f"once the device holding its files has stopped, {name!r} should say its "
        f"folder cannot be reached ({S.media.source_offline!r}); it says "
        f"{unreachable!r}; wire={_wire_verdicts(nest_instance, secret_key, folder)!r}"
    )
    badge_after = app.driver.get_text(
        "sync-state-badge", scope=f"media-item[{media.index_of(name)}]"
    )
    assert badge_after == badge_while_reachable, (
        f"the file did not change, so its sync badge must not move with its "
        f"folder's reachability: {badge_while_reachable!r} → {badge_after!r}"
    )
    assert badge_after not in (S.media.source_online, S.media.source_offline), (
        f"the sync badge must say something about the FILE, not repeat the "
        f"folder's reachability; it reads {badge_after!r}"
    )
    assert not app.has_error(), f"unexpected media error: {app.error_text()!r}"


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.tui
@pytest.mark.feature("media")
def test_media_item_in_a_full_folder_is_reachable_with_no_seat_connected(
    request, app, nest_instance
):
    """A full-residency folder's files rest on the nest, so its item says they can
    be reached although no seat has ever connected — the common case (every
    app-sourced folder) the old single-source reading painted red for ever.

    The file is recorded over the real ``fauna.sync.changes.record`` RPC by a
    registered device that never opens the sync data plane, so no seat can be
    what makes it reachable.
    """
    user = _make_user(nest_instance)
    secret_key = user["signing_key"].encode().hex()
    folder = f"full-{secrets.token_hex(4)}"
    name = "resting.txt"
    _seed_cross_set_media(nest_instance, user, {folder: [name]})

    assert _wire_verdicts(nest_instance, secret_key, folder) == ({True}, True), (
        "a full folder's content rests on the nest, so both wire replies should "
        "call it reachable with no seat connected"
    )

    _login_app_as(app, request, nest_instance, user, verify_live_actor=True)
    media = app.media
    media.navigate()
    media.ensure_loaded()
    assert media.wait_for_item_count(1, timeout=ITEM_BUDGET_S) == 1, (
        f"the recorded file should list as one item; listed "
        f"{media.item_names()!r}; error={app.error_text()!r}"
    )
    reachable = media.wait_for_source_status(name, S.media.source_online)
    assert reachable == S.media.source_online, (
        f"{name!r} rests on the nest, so it should say its folder can be reached "
        f"({S.media.source_online!r}); it says {reachable!r}"
    )
    assert not app.has_error(), f"unexpected media error: {app.error_text()!r}"
