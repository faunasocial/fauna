"""tier_3 — folders outcome 18: *any combination of a device's receive and
serve flags can be set and is kept exactly as set, and a file dropped into an
archive device's directory syncs like any other* (`docs/features/folders.md`
§ Coverage contract).

The rulings: the place-flag model's slice f made the whole flag space
expressible (all eight points writable through `fauna.folders.places.set`, no
point rounded to a neighbour), and the `originates`/backup question was ruled
2026-08-19 — an archive seat (`applies_deletes` off) still originates, so a file
dropped into its real directory syncs like any other. Both are recorded in
`docs/goal/behavior/folders.md` § Implementation status today.

Two tests, weakest-first (convention 6):

1. :func:`test_every_place_flag_combination_is_kept_exactly_as_set` walks all
   eight points of one seat with single clicks on the post-create place editor
   (`folder-place-row`, `test_folder_place_editor.py`'s surface) in Gray-code
   order, so each click moves exactly one flag, and reads the nest's
   `fauna.folders.members.list` after every click: the point that rests is the
   point the boxes show, never a neighbour. A collapse + re-expand at the end
   proves the last point is repainted from the nest, not a lingering flip.
2. :func:`test_a_file_dropped_into_an_archive_seats_directory_syncs` binds a
   real location (which enrols this device at the default point,
   `test_folder_bind_enrols_place.py`), turns the seat into the archive point
   through the editor, then drops a file into the bound directory: the real
   `fauna-sync-agent` must report uploading it.

MUTATION is UI-driven (convention 8); the first test's extra seat is fixture
setup (the documented carve-out its sibling uses). tui led; web, linux and
android joined 2026-11-08 (the archive drop on linux alone — it needs a bound
local directory); windows, macOS and iOS follow in their trickle-down.
"""

import secrets

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import PLACE_SYNC, place_flags_payload
from helpers import enrollment
from helpers.set_names import addressed
from helpers.waiting import wait_until

pytestmark = [pytest.mark.tier_3, pytest.mark.tui, pytest.mark.linux]

_BOXES = {
    "originates": "folder-place-originates",
    "accepts": "folder-place-accepts",
    "applies_deletes": "folder-place-applies-deletes",
}

# Gray-code order from the default point: each step flips exactly one flag, and
# the eight steps visit every point once (`{O, A, D}` as booleans).
_WALK = [
    "applies_deletes",  # T T F — the archive point
    "accepts",  # T F F
    "applies_deletes",  # T F T
    "originates",  # F F T
    "applies_deletes",  # F F F — every flag off
    "accepts",  # F T F — the receive-only point
    "applies_deletes",  # F T T
]

# The seat write's round trip plus the roster re-read the editor repaints from.
_WRITE_WINDOW_SECS = 20.0


def _client(url: str, actor_id: bytes, signing_key: bytes) -> WsRpcAdminClient:
    return WsRpcAdminClient(url, actor_id=actor_id, signing_key=signing_key)


def _roster(client, folder: str) -> list:
    with client:
        return client.call("fauna.folders.members.list", addressed(folder))["members"]


def _flags_of(client, folder: str, device_id: str) -> dict | None:
    for member in _roster(client, folder):
        if member["device_id"] == device_id:
            return member["flags"]
    return None


def _painted(app, scope: str) -> dict:
    return {
        flag: app.driver.get_attr(box, "state", scope=scope) == "on"
        for flag, box in _BOXES.items()
    }


# The place editor is on every app (`test_folder_place_editor.py`'s marker
# list); web, android, macos and ios drive this walk too.
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("folders")
def test_every_place_flag_combination_is_kept_exactly_as_set(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    b = app.backups
    b.navigate_folders()

    name = f"flagwalk-{secrets.token_hex(4)}"
    b.create_folder_via_wizard(name)
    # One sealed-registered seat at the default point (the sibling editor test's
    # fixture, and its reason for registering sealed).
    device_id = enrollment.register_device(nest_instance["url"], test_user, "flagwalk-seat")
    client = _client(
        nest_instance["url"],
        bytes(test_user["signing_key"].verify_key),
        bytes(test_user["signing_key"]),
    )
    with client:
        client.call(
            "fauna.folders.places.set",
            addressed(name, device_id=device_id, flags=place_flags_payload(PLACE_SYNC)),
        )

    row = b.find_and_expand_folder(name)
    app.driver.wait_for("folder-place-row", timeout=15.0)
    scope = "folder-place-row[0]"
    assert app.driver.count("folder-place-row") == 1, (
        f"precondition: the folder holds exactly the one seeded seat; "
        f"{app.driver.diagnose('folder-place-row')}"
    )

    expected = dict(PLACE_SYNC)
    assert _painted(app, scope) == expected
    visited = [tuple(expected.values())]

    for flag in _WALK:
        expected[flag] = not expected[flag]
        app.driver.click(_BOXES[flag], scope=scope)
        wait_until(
            lambda: _painted(app, scope) == expected,
            _WRITE_WINDOW_SECS,
            diagnose=lambda: (
                f"after clicking {flag!r} the boxes read {_painted(app, scope)!r}, "
                f"wanted {expected!r}; nest={_flags_of(client, name, device_id)!r} "
                f"error={app.error_text()!r}"
            ),
        )
        # The point that rests on the nest is exactly the point the boxes show.
        assert _flags_of(client, name, device_id) == expected, (
            f"after clicking {flag!r} the nest rests "
            f"{_flags_of(client, name, device_id)!r}, wanted {expected!r} — a point "
            f"rounded to a neighbour, or one flag written alone"
        )
        assert not app.has_error(), (
            f"the {expected!r} point surfaced an error: {app.error_text()!r}"
        )
        visited.append(tuple(expected.values()))

    assert len(set(visited)) == 8, f"the walk must visit all eight points: {visited!r}"

    # The last point is the nest's, not a lingering local flip: collapse and
    # re-expand, and the boxes repaint it from a roster re-read.
    b.expand_folder(row)  # collapse (the expander is a toggle)
    b.find_and_expand_folder(name)
    app.driver.wait_for("folder-place-row", timeout=15.0)
    assert _painted(app, scope) == expected, (
        f"after a re-expand the boxes read {_painted(app, scope)!r}, wanted {expected!r}"
    )


# Desktop apps only: the drop needs a bound local directory, and
# `folder-location-*` is desktop-only in ui.yaml (web, android and ios keep
# no bindable local folder), so this witness is marked for the apps that can
# drive it (`feature-catalog.md` § Cell semantics), never absence-gated.
@pytest.mark.parametrize("folder_share_owner_app", ["tui", "linux", "macos"], indirect=True)
@pytest.mark.macos
@pytest.mark.real_conversations
# The drop must reach a real agent with a bound engine.
@pytest.mark.real_sync_agent
@pytest.mark.isolated_sync_agent
# Documented-long (convention 9): one GUI app, one real sync agent, a bind, a
# place write and an upload.
@pytest.mark.timeout(900)
@pytest.mark.feature("folders")
def test_a_file_dropped_into_an_archive_seats_directory_syncs(
    folder_share_owner_app, tmp_path
):
    from helpers.folder_content import (
        atomic_write,
        await_agent_upload,
        bind_location_under_set,
    )

    app, nest, owner = folder_share_owner_app
    client = _client(nest["url"], owner["actor_id_bytes"], bytes(owner["signing_key"]))
    name = f"archive-{secrets.token_hex(4)}"

    app.backups.navigate_folders()
    app.backups.create_folder_via_wizard(name)  # no device checked
    location = tmp_path / "archive-dir"
    location.mkdir()
    bind_location_under_set(app, name, location, seat="owner")

    # The bind enrols this device at the default point.
    wait_until(
        lambda: len(_roster(client, name)) == 1,
        60.0,
        diagnose=lambda: f"roster={_roster(client, name)!r} error={app.error_text()!r}",
    )
    device_id = _roster(client, name)[0]["device_id"]

    # Make it the archive seat through the editor: peer deletes no longer apply.
    app.driver.wait_for("folder-place-row", timeout=60.0)
    scope = "folder-place-row[0]"
    app.driver.click("folder-place-applies-deletes", scope=scope)
    archive = {"originates": True, "accepts": True, "applies_deletes": False}
    wait_until(
        lambda: _flags_of(client, name, device_id) == archive,
        _WRITE_WINDOW_SECS,
        diagnose=lambda: (
            f"the seat never became the archive point: "
            f"nest={_flags_of(client, name, device_id)!r} error={app.error_text()!r}"
        ),
    )

    # A file dropped into the archive seat's own directory syncs like any other.
    filename = f"dropped-{secrets.token_hex(3)}.txt"
    atomic_write(location / filename, "a file dropped into an archive seat\n")
    await_agent_upload(app, filename, seat="owner")

    assert _flags_of(client, name, device_id) == archive, (
        "the upload must not have moved the seat off the archive point"
    )
    assert not app.has_error(), f"the archive drop surfaced an error: {app.error_text()!r}"
