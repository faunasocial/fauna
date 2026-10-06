"""The publicly-synced follow's UI journey — follow a public folder, then
browse it in Media as a read-only scope.

Owner docs: ``docs/goal/ui/folders.md`` § Following a public folder (the
gesture + the followed row), ``docs/goal/ui/media.md`` § Followed public
folders (the browse scope: identity-addressed, keyless, read-only),
``docs/goal/behavior/folders.md`` § Publicly-synced follow (the behavior).

What this file adds over everything below it: the **user's own path** through
the follow, end to end on a real binary — every layer beneath is already
pinned elsewhere (the wire + bytes cross-nest:
``tests/api/test_public_folder_follow_cross_nest.py``; the client stack:
``bins/fauna-nest/tests/conformance_public_follow_client.rs``; the machine
scope + tui render: ``fauna-media-machine``'s and tui's own test suites), but
no test had ever pressed ``folder-follow-button``. The mutations under test —
the follow and the browse — run through the app UI (rule 8); the *owner's*
folder (create, declassify, content) is fixture setup, the same API carve-out
``test_public_folder_fetch.py`` documents.

This is the **same-nest** arm (two actors, one deployment — the follow's
``home_nest_url`` is empty). The relay arm's wire is deliberately not
re-proven here; it needs two nests and already has its own file.
"""

import secrets

import pytest

import fauna_ffi

from common.auth import create_actor_and_register
from helpers.app_surface import skip_unbuilt
from helpers.waiting import wait_until
from i18n.strings import S

from tests.api.test_public_folder_fetch import DEVICE_ID, _record, _set_audience
from tests.api.test_web_paywall_folder import _actor_client

pytestmark = pytest.mark.tier_3


@pytest.mark.feature("follow-a-public-folder")
def test_follow_a_public_folder_and_browse_it_in_media(logged_in_app, nest_instance):
    app = logged_in_app
    if not (
        app.driver.is_tui()
        or app.driver.is_web()
        or app.driver.is_linux()
        # macos + ios, 2026-08-28: one shared FaunaKit leg covers both — the
        # `FollowedFoldersSection` on Folders and the followed options on
        # `media-folder-filter`, over the same `follow_ops` recipe and the same
        # `wire_{devices,media}_followed_folders` seams every other app uses.
        or app.driver.is_macos()
        or app.driver.is_ios()
        # windows, 2026-08-29: the sixth app. Same two seams every other app
        # wires (`wire_devices_followed_folders` on the Folders page,
        # `wire_media_followed_folders` on Media) over the same `follow_ops`
        # recipe; its followed options need no value→label side map, because a
        # `ComboBoxItem` carries the minted label and the drive-key on separate
        # properties — the apple shape, not the GTK/Compose one.
        or app.driver.is_windows()
    ):
        # Declared debt, not a bare skip (convention 7): the whole surface is
        # shared Rust — each remaining app owes only the follow-flow render and
        # the Media followed options.
        skip_unbuilt(
            app.driver,
            surface="folder-follow-button / media-folder-filter followed options",
            detail="the follow UX + Media followed browse scope (tui built 2026-08-19)",
            tracked="",
        )

    url = nest_instance["url"]
    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]

    # ── Fixture: a second actor publishes a folder with one real file. ──
    folder = f"pub-{secrets.token_hex(3)}"
    owner = create_actor_and_register(port, admin_signing_key=admin_sk)
    owner_hex = bytes(owner["actor_id_bytes"]).hex()
    with _actor_client(url, owner) as ws:
        fauna_ffi.harness_create_set(
            url, bytes(owner["signing_key"]),
            {"name": folder},
        )
        ws.call(
            "fauna.sync.register",
            {"device_id": DEVICE_ID.hex(), "label": "seed", "capabilities": "read,write"},
        )
    _set_audience(url, owner, folder, "public")
    _record(url, port, owner, folder, "sunset.jpg", b"not really a jpeg, and that is fine")

    # The pinned id the follow will address — read through the public plane
    # itself, exactly as the app's first fetch pins it.
    with _actor_client(url, owner) as ws:
        reply = ws.call(
            "fauna.folders.public.fetch",
            {"owner_actor_id": owner_hex, "folder_name": folder, "since": 0},
        )
    folder_id = reply["folder_id"]
    assert folder_id > 0

    # ── The follow, through the UI. ──
    app.backups.navigate_folders()
    app.driver.click("folder-follow-button")
    app.driver.wait_for("folder-follow-name-input")
    # The owner half reuses the shared recipient picker, which accepts a bare
    # 64-hex actor id (ui/folders.md § Following a public folder).
    app.driver.type_text("recipient-picker-input", owner_hex)
    app.driver.type_text("folder-follow-name-input", folder)
    app.driver.click("folder-follow-confirm")

    app.driver.wait_for("folder-followed-item", timeout=15.0)
    # Containment, not equality: `folder-followed-item` is the ROW — the scope
    # its status/badge/unfollow children resolve under (the scoped read just
    # below depends on exactly that) — and the element models differ in what a
    # row's own "text" then is. tui's row label carries only the display name,
    # while a DOM row reports `textContent`, which concatenates every
    # descendant's text. The name being present is the cross-app claim; the
    # precise per-field reads are the scoped ones.
    assert folder in app.driver.get_text("folder-followed-item", 0)
    assert (
        app.driver.get_text("folder-followed-status", 0, scope="folder-followed-item[0]")
        == S.devices.followed_status_following
    )

    # ── The browse, through Media. ──
    m = app.media
    m.navigate()
    # The machine mints the scope's opaque select value; its shape is pinned by
    # the machine's own tests, and the same-nest arm's home_nest_url is empty.
    scope_value = f"followed:{folder_id}@"
    m.set_filter(scope_value)

    # A barrier on the NAME, never on a count of 1: this journey shares its
    # actor with the rest of the session, so the ordinary all-media list can
    # already hold exactly one item of a neighbour's (`test_devices_conflicts`
    # leaves a `report.txt`), and a count of 1 is then true before the scope's
    # own list has swapped in (`MediaActions.wait_for_item_count` says when an
    # exact count is a sound barrier: only when the outgoing collection cannot
    # have that size).
    wait_until(
        lambda: m.item_names() == ["sunset.jpg"],
        10.0,
        diagnose=lambda: f"media items read {m.item_names()!r}, want ['sunset.jpg']",
    )
    assert m.item_name(0) == "sunset.jpg"
    # The scope is read-only: the upload affordance is absent entirely, never
    # painted-but-inert (ui/media.md § Followed public folders).
    assert app.driver.count("upload-button") == 0, (
        "a followed browse scope must offer no upload affordance"
    )
    assert app.driver.count("file-upload") == 0

    # ── Leaving the scope restores the ordinary browse. ──
    m.set_filter()  # the all-media default
    wait_until(
        lambda: app.driver.count("upload-button") == 1,
        10.0,
        diagnose=lambda: f"upload-button count={app.driver.count('upload-button')}",
    )
