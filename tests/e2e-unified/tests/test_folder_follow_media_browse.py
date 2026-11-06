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
from helpers.app_surface import require_surface
from helpers.waiting import wait_until
from i18n.strings import S

from tests.api.test_public_folder_fetch import DEVICE_ID, _record, _set_audience
from tests.api.test_web_paywall_folder import _actor_client

pytestmark = pytest.mark.tier_3


@pytest.mark.feature("follow-a-public-folder")
def test_follow_a_public_folder_and_browse_it_in_media(logged_in_app, nest_instance):
    app = logged_in_app

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


@pytest.mark.feature("follow-a-public-folder")
def test_a_followed_folders_file_on_another_nest_downloads_with_no_account_there(
    logged_in_app, nest_instance, cross_nest_foreign
):
    """``follow-a-public-folder`` outcome 10 on every app that downloads from
    Media: *a file in a folder you follow can be downloaded from the browse,
    with no account on the owner's nest* (``ui/media.md`` § Followed public
    folders, *Downloads*). The cross-app twin of tui's external-open witness
    (``test_tui_media_external_open.py``): the gesture differs (tui hands the
    file to the OS opener), the promise is the same.

    Two nests: **H** (``cross_nest_foreign``) homes the owner and the folder;
    **F** (``nest_instance``) is the follower's, and the follower holds no
    account, grant or key on H. The follow is typed as ``handle@domain``; the
    download is ``MediaActions.download_open_item``. Convention 8: the owner's
    folder is fixture setup over the API on a nest no GUI drives; the follow,
    the browse and the download go through the app UI.
    """
    from helpers.admin_wire import admit_adult

    app = logged_in_app

    home = cross_nest_foreign
    home_url, home_port = home["url"], home["port"]
    payload = b"RIFF-fake-video-payload " * 64

    owner = admit_adult(
        home, "xnpub" + secrets.token_hex(3), domain=home["authority"], token=True,
    )
    folder = f"xnest-pub-{secrets.token_hex(3)}"
    with _actor_client(home_url, owner) as ws:
        fauna_ffi.harness_create_set(home_url, bytes(owner["signing_key"]), {"name": folder})
        ws.call(
            "fauna.sync.register",
            {"device_id": DEVICE_ID.hex(), "label": "seed", "capabilities": "read,write"},
        )
    _set_audience(home_url, owner, folder, "public")
    _record(home_url, home_port, owner, folder, "clip.mp4", payload)
    with _actor_client(home_url, owner) as ws:
        folder_id = ws.call(
            "fauna.folders.public.fetch",
            {"owner_actor_id": owner["actor_id_hex"], "folder_name": folder, "since": 0},
        )["folder_id"]

    # ── The follow, by `handle@domain`, through the UI. ──
    app.backups.navigate_folders()
    app.driver.click("folder-follow-button")
    app.driver.wait_for("folder-follow-name-input")
    app.driver.type_text("recipient-picker-input", f"{owner['handle']}@{home['authority']}")
    app.driver.type_text("folder-follow-name-input", folder)
    app.driver.click("folder-follow-confirm")
    app.driver.wait_for("folder-followed-item", timeout=30.0)

    # ── The browse on the relayed scope, then the download. ──
    m = app.media
    m.navigate()
    m.set_filter(f"followed:{folder_id}@https://{home['authority']}")
    wait_until(
        lambda: m.item_names() == ["clip.mp4"],
        15.0,
        diagnose=lambda: f"media items read {m.item_names()!r}, want ['clip.mp4']",
    )
    m.open_item_detail(0)
    assert m.download_open_item(timeout=30.0) == payload, (
        "the downloaded bytes are the file published on the other nest"
    )
