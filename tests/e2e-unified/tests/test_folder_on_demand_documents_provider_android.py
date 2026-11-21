"""tier_3 — on-demand files on android: the SAF documents provider, end to end
(`docs/features/files-on-demand.md` outcomes 3, 4, 5, 6, 8, 11 and 12;
`docs/features/folders.md` outcome 13).

Android's on-demand surface is not a directory: it is a SAF
``DocumentsProvider`` over a replica the app owns, shown per set by the
folder row's "Show in Files" switch (``folder-on-demand-toggle``)
(``on-demand-files-android.md`` § Android SAF DocumentsProvider binding). The
desktop witnesses of these outcomes read a FUSE mount or a cloud-files root
from another process; no host process reaches the provider, so these are their
android twins, driving it the way another app's picker does — through a
``ContentResolver`` in the app's own process, the instrumentation bridge's
``documents_*`` verbs (``drivers/android.py``, ``helpers/android_documents.py``).

**What the reads are.** A listing is the provider's own cursor
(``documents_children``: name and size). Residency — where a body rests — is
read in-process off the host the provider calls, never by opening the document
(``documents_residency``; the twin of a desktop leg's ``stat``): the **kept
root** holds an edit the nest has not recorded, the **cache root** a recorded
body, and a placeholder holds its bytes on no disk of this device (*Two roots
per set*). A read (``documents_read``) is an open, and opening a placeholder
fetches its bytes (*Hydrate on open*).

**Freeing space is the OS's, never a Fauna button** (*Freeing space is the OS's
cache reclaim — and it is never a delete*): android trims an app's cache under
storage pressure or on the user's "Clear cache", and the host observes it.
Outcomes 4 and 5 therefore free with the OS's gesture — the bridge's
``documents_reclaim_cache`` deletes the account's cache roots exactly as a trim
does, telling the app nothing — and the desktops' ``folder-location-free-space-button``
is not android's (it is declared not built there, by design).

**The journeys:**

* **A file from another device; a write that joins and uploads; the OS freeing
  its space** (outcomes 11, 12, 3, 4) — two seats of one account: a tui seat
  bound to the set (the other device) and the android phone. tui drops a file;
  the phone's set lists it by itself, at its size, as a placeholder; opening it
  serves tui's bytes and leaves it hydrated. The phone creates a file and
  changes the first one through the provider; tui's directory receives both.
  The OS then trims the cache: the file stays listed at its size as a
  placeholder, tui still holds it, and opening it fetches its bytes from the
  nest again.
* **A change the nest has not recorded cannot have its space freed** (outcome
  5) — the nest stops; a change written through the provider rests in the kept
  root; a trim leaves it there and a read returns it; the nest comes back, the
  change is recorded and demoted, and the next trim frees it.
* **Several sets at once** (outcome 6) — two shown sets; switching one off
  takes it out of the provider's root and leaves the other serving reads and
  recording writes; switching it back on presents it again with its file.
* **Switching a set off keeps an unsent change** (outcome 8) — the nest stops,
  a change is written, the set is switched off; the nest returns and the set is
  switched on: the change is recorded and the nest serves it back.
* **Showing a set enrols this phone, and the row shows it** (folders 13) — a
  folder no device holds a place in; the switch's ON is the enrol gesture
  (*Auto-appear default-ON*), which writes one default place, and the row's
  place editor repaints it without a collapse.

The MUTATIONS a user makes are the app's switch and the picker's writes; the
nest's stop is the harness's (the outage, never a user act); every verdict is
the provider's cursor, the host's residency, the peer's directory or the nest's
roster.
"""

from __future__ import annotations

import secrets
import sys

import pytest

from common.nest import start_nest_in_place, stop_nest
from helpers import sync_seats
from helpers.android_documents import (
    await_residency,
    kept,
    listed,
    open_row,
    placeholder,
    presented_set,
    recorded,
    set_hidden,
    set_shown,
    show_in_files,
)
from helpers.folder_content import atomic_write
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.android,
    # The emulator is driven over adb from the linux box (`seat_modes.PHONE_APPS`).
    pytest.mark.host_platform("linux"),
]

# Ceilings a green run never pays (convention 14).
_PEER_WINDOW = 180.0
_ROSTER_WINDOW = 30.0
_ROW_WINDOW = 30.0

# seat a: the other device (tui, bound to the set); seat b: the phone.
_PHONE_PAIR = ("tui", sync_seats.PHONE_MODE)
_PHONE_PAIRS = (
    [pytest.param(_PHONE_PAIR, id=sync_seats.seat_set_param_id(_PHONE_PAIR))]
    if sync_seats.phone_app_for_platform(sys.platform)
    else []
)


def _new_shown_folder(app) -> tuple[str, str]:
    """A fresh owner folder made on the Folders page (UI), shown in Files —
    returns its name and its set's document id in the provider's root."""
    name = f"odp-{secrets.token_hex(4)}"
    b = app.backups
    b.navigate_folders()
    b.create_folder_via_wizard(name)
    show_in_files(app, name)
    return name, presented_set(app.driver, name)


def _create(drv, set_doc: str, name: str, body: bytes) -> str:
    """A picker creates ``name`` in the set and writes ``body`` through it;
    the new document's id."""
    doc = drv.documents_create(set_doc, name)
    drv.documents_write(doc, body)
    return doc


def _reads(drv, doc: str, body: bytes, what: str) -> None:
    got = drv.documents_read(doc)
    assert got == body, f"{what}: the provider served {got!r}, want {body!r}"


@pytest.mark.parametrize("seat_set", _PHONE_PAIRS)
@pytest.mark.timeout(3600)
@pytest.mark.feature("files-on-demand")
def test_android_a_file_from_another_device_hydrates_on_open_writes_reach_it_and_the_os_frees_it(
    seat_set, nest_mode, tmp_path_factory, request
):
    """Outcomes 11, 12, 3 and 4 on the phone, with a tui seat as the other device."""
    from tests.test_filesync_seats import WINDOW, _seat_run, _set_name_for

    set_name = _set_name_for(len(seat_set))
    with _seat_run(seat_set, nest_mode, tmp_path_factory, request) as run:
        peer, phone = run.seats
        assert (peer.app_name, phone.app_name) == ("tui", "android"), (
            f"seat order drifted: {[s.app_name for s in run.seats]}"
        )
        app = phone.app
        drv = app.driver
        show_in_files(app, set_name)
        set_doc = presented_set(drv, set_name)

        # ── 12: a file another device adds shows up by itself … ──
        name = f"from-peer-{secrets.token_hex(3)}.txt"
        body = f"written on the other device {secrets.token_hex(8)}\n".encode()
        atomic_write(peer.path / name, body)
        row = listed(drv, set_doc, name)
        assert row.get("size") == len(body), f"the set lists {name!r} at its size: {row!r}"

        # ── 11: … taking no space until it is opened, which downloads it. ──
        doc = row["id"]
        before = drv.documents_residency(doc)
        assert placeholder(before), (
            f"a file this device has not opened holds its bytes on no disk of it: {before!r}"
        )
        _reads(drv, doc, body, "opening the placeholder")
        after = drv.documents_residency(doc)
        assert after.get("hydrated") and after.get("root") == "cache", (
            f"an opened file is held, in the cache root: {after!r}"
        )

        # ── 3: a file put in the set joins it; a changed file uploads. ──
        new_name = f"from-phone-{secrets.token_hex(3)}.txt"
        new_body = f"created through the provider {secrets.token_hex(8)}\n".encode()
        new_doc = _create(drv, set_doc, new_name, new_body)
        changed = body + f"changed on the phone {secrets.token_hex(8)}\n".encode()
        drv.documents_write(doc, changed)
        for path, want in ((peer.path / new_name, new_body), (peer.path / name, changed)):
            wait_until(
                lambda p=path, w=want: p.is_file() and p.read_bytes() == w,
                max(WINDOW, _PEER_WINDOW), interval=2.0,
                diagnose=lambda p=path: (
                    f"the other device never received the phone's write {p.name!r}; "
                    f"phone parks {drv.documents_parks()!r} error={app.error_text()!r}\n"
                    f"{peer.diagnostics()}"
                ),
            )
        await_residency(drv, set_doc, new_doc, recorded, "recorded (cache root)")
        await_residency(drv, set_doc, doc, recorded, "recorded (cache root)")

        # ── 4: the OS frees the space; the file stays listed and is never deleted. ──
        assert drv.documents_reclaim_cache() >= 2, "the trim found the two recorded bodies"
        freed = await_residency(drv, set_doc, doc, placeholder, "a placeholder after the trim")
        assert freed.get("size") == len(changed), f"freed, the file keeps its size: {freed!r}"
        assert listed(drv, set_doc, name).get("size") == len(changed), (
            "freeing keeps the file listed where it was, at its size"
        )
        assert (peer.path / name).read_bytes() == changed, (
            "freeing on the phone never deletes the file from another device"
        )
        # The phone held no bytes, so these came from the nest: never deleted there.
        _reads(drv, doc, changed, "opening the freed file")
        assert not app.has_error(), f"the journey surfaced an error: {app.error_text()!r}"


@pytest.mark.timeout(1800)
@pytest.mark.feature("files-on-demand")
def test_android_an_unrecorded_change_survives_the_os_freeing_space(logged_in_app, nest_instance):
    """Outcome 5: a change the nest has not recorded rests in the kept root,
    which the OS's trim never reaches."""
    app = logged_in_app
    drv = app.driver
    _, set_doc = _new_shown_folder(app)
    name = f"kept-{secrets.token_hex(3)}.txt"
    body = f"first version {secrets.token_hex(8)}\n".encode()
    doc = _create(drv, set_doc, name, body)
    await_residency(drv, set_doc, doc, recorded, "recorded (cache root)")
    changed = body + f"changed with the nest down {secrets.token_hex(8)}\n".encode()

    stop_nest(nest_instance, graceful=True)
    try:
        drv.documents_write(doc, changed)
        await_residency(drv, set_doc, doc, kept, "the kept root (the nest cannot record it)")

        # The OS frees space: the unrecorded change is the only copy, and stays.
        drv.documents_reclaim_cache()
        still = await_residency(drv, set_doc, doc, kept, "the kept root after the trim")
        assert still.get("body_bytes") == len(changed), f"the change's bytes are all still here: {still!r}"
        _reads(drv, doc, changed, "with the nest down, the change can only be local")
    finally:
        start_nest_in_place(nest_instance)

    # The nest is back on the same data: the change is recorded, and then it frees.
    await_residency(drv, set_doc, doc, recorded, "recorded once the nest is back")
    drv.documents_reclaim_cache()
    await_residency(drv, set_doc, doc, placeholder, "a placeholder after the trim")
    _reads(drv, doc, changed, "the freed change, fetched back from the nest")
    assert not app.has_error(), f"the journey surfaced an error: {app.error_text()!r}"


@pytest.mark.timeout(1800)
@pytest.mark.feature("files-on-demand")
def test_android_several_sets_on_demand_each_switch_leaves_the_others_working(logged_in_app):
    """Outcome 6: two sets shown at once; switching one off and on again
    leaves the other serving reads and recording writes."""
    app = logged_in_app
    drv = app.driver
    first, first_doc = _new_shown_folder(app)
    second, second_doc = _new_shown_folder(app)
    bodies = {}
    docs = {}
    for folder, set_doc in ((first, first_doc), (second, second_doc)):
        bodies[folder] = f"{folder} {secrets.token_hex(8)}\n".encode()
        docs[folder] = _create(drv, set_doc, f"{folder}.txt", bodies[folder])
        await_residency(drv, set_doc, docs[folder], recorded, f"recorded in {folder}")
    assert presented_set(drv, first) == first_doc

    # MUTATION (UI): switch the first set off.
    set_shown(app, first, False)
    set_hidden(drv, first)
    assert presented_set(drv, second) == second_doc, "the other set is still presented, as it was"
    drv.documents_reclaim_cache()
    await_residency(drv, second_doc, docs[second], placeholder, "a placeholder after the trim")
    _reads(drv, docs[second], bodies[second], "the other set still fetches from the nest")
    extra = f"written while the first set was off {secrets.token_hex(8)}\n".encode()
    extra_doc = _create(drv, second_doc, f"{second}-extra.txt", extra)
    await_residency(drv, second_doc, extra_doc, recorded, "a new write in the other set recorded")

    # MUTATION (UI): switch the first set back on.
    set_shown(app, first, True)
    assert presented_set(drv, first) == first_doc, "the set comes back under the same id"
    row = listed(drv, first_doc, f"{first}.txt")
    _reads(drv, row["id"], bodies[first], "the switched-back set serves its file")
    _reads(drv, docs[second], bodies[second], "the other set is untouched by the switch")
    assert not app.has_error(), f"the journey surfaced an error: {app.error_text()!r}"


@pytest.mark.timeout(1800)
@pytest.mark.feature("files-on-demand")
def test_android_switching_a_set_off_keeps_an_unsent_change(logged_in_app, nest_instance):
    """Outcome 8: hiding a set never destroys a change that had not uploaded —
    the kept root survives the switch, and the change is recorded later."""
    app = logged_in_app
    drv = app.driver
    folder, set_doc = _new_shown_folder(app)
    name = f"unsent-{secrets.token_hex(3)}.txt"
    body = f"first version {secrets.token_hex(8)}\n".encode()
    doc = _create(drv, set_doc, name, body)
    await_residency(drv, set_doc, doc, recorded, "recorded (cache root)")
    changed = body + f"changed with the nest down {secrets.token_hex(8)}\n".encode()

    stop_nest(nest_instance, graceful=True)
    try:
        drv.documents_write(doc, changed)
        await_residency(drv, set_doc, doc, kept, "the kept root (the nest cannot record it)")
        # MUTATION (UI): switch the set off while its change is unsent.
        set_shown(app, folder, False)
        set_hidden(drv, folder)
    finally:
        start_nest_in_place(nest_instance)

    # MUTATION (UI): show it again; the host's start sweep re-drives the kept body.
    set_shown(app, folder, True)
    assert presented_set(drv, folder) == set_doc
    await_residency(drv, set_doc, doc, recorded, "the unsent change recorded after the switch")
    _reads(drv, doc, changed, "the change survived the switch")
    drv.documents_reclaim_cache()
    await_residency(drv, set_doc, doc, placeholder, "a placeholder after the trim")
    _reads(drv, doc, changed, "the nest serves the change back")
    assert not app.has_error(), f"the journey surfaced an error: {app.error_text()!r}"


@pytest.mark.timeout(900)
@pytest.mark.feature("folders")
def test_android_showing_a_set_enrols_this_phone_and_the_row_shows_it(
    logged_in_app, nest_instance, test_user
):
    """folders outcome 13: the phone's local presence is the shown set, so
    showing it is the gesture that adds the phone to the folder's devices —
    one default place, painted on the row straight away (`file-sync.md` § 4,
    *A local presence writes the place it needs*)."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from helpers.set_names import addressed

    app = logged_in_app
    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(test_user["signing_key"].verify_key),
        signing_key=bytes(test_user["signing_key"]),
    )

    def roster(name: str) -> list[dict]:
        # A sealed set rests no plaintext name: address it by its hash.
        with client:
            return client.call("fauna.folders.members.list", addressed(name))["members"]

    name = f"odp-seat-{secrets.token_hex(4)}"
    b = app.backups
    b.navigate_folders()
    b.create_folder_via_wizard(name)  # no device checked
    assert roster(name) == [], "precondition: the folder holds no place"

    b = open_row(app, name)
    assert b.on_demand_toggle_state() == "on", "a set this phone may show reads ON by default"
    # MUTATION (UI): the gesture — off, then on; the ON enrols.
    assert b.toggle_on_demand() == "off"
    assert b.toggle_on_demand() == "on"

    wait_until(
        lambda: len(roster(name)) == 1, _ROSTER_WINDOW,
        diagnose=lambda: f"the ON gesture never enrolled this phone; roster={roster(name)!r} "
        f"error={app.error_text()!r}",
    )
    (seat,) = roster(name)
    assert seat["flags"] == {"originates": True, "accepts": True, "applies_deletes": True}, seat

    # The row repaints nest truth with no collapse/re-expand.
    app.driver.wait_for("folder-place-row", timeout=_ROW_WINDOW)
    scope = "folder-place-row[0]"
    for box in ("folder-place-originates", "folder-place-accepts", "folder-place-applies-deletes"):
        assert app.driver.get_attr(box, "state", scope=scope) == "on", (
            f"{box} on the repainted row; {app.driver.diagnose(box)}"
        )
    assert not app.has_error(), f"the enrol raised an error: {app.error_text()!r}"
