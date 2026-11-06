"""Finder's — and the Files app's — Share action, the door half
(`docs/goal/architecture/apps/macos.md` § Sync → *Finder's Share entry*, witness
part (c); the iOS Files app takes the same three outcomes, owner-ruled
2026-10-19).

The mile splits where the mechanism splits:

(a) **the activation rule** — the Share action's Info.plist predicate, evaluated
    against items projected by the same `FileProviderShareEntry` the File
    Provider appex's items carry: offered on an eligible file and on the set's
    root, never on a sub-folder, a non-public set's file, a cross-nest set or a
    multi-item selection (outcome 29, outcome 30's second half) —
    `FaunaKitTests/FaunaDeepLinkTests`, headless;
(a′) **the entry each item carries** — this file: the appex's own host (the
    `FfiFileProviderHost.appDead` constructor), built inside the app over the
    signed-in session through `file_provider_share_entries` — no domain
    registered (convention 10) — lists a public set's file with
    `faunaShare: link`, its sub-folder and a private set's file with no entry,
    and the set's root container with `faunaShare: folder` (outcome 29);
(b) **the trampoline** — the FFI-free route speller round-trips every item shape
    through the shared Rust parser — `FaunaDeepLinkTests`, headless; the
    per-item eligibility fact itself is pinned in Rust (`provider_face` tests);
(c) **the door** — this file: each route the trampoline spells, fed to the app
    through `open_route` (the same FaunaKit door `onOpenURL` reaches), lands on
    `media-item-detail` with `share-link-create-modal` open, or on the Folders
    page with that set's `folder-share-button` flow open (outcomes 28, 30).

What is left to a human is whether the action *appears* in a live Finder or
Files app — the File Provider appex surface is signing-gated (the org's Apple
Developer certificate), like the rest of it.

Mutations go through the UI (convention 8); the sets, the public flip and the
seeded file are the `test_share_links` fixture carve-out. Every wait polls
state, never a clock.
"""

import json

import pytest

from helpers.set_names import find_set
from helpers.waiting import wait_until
from tests.api.test_public_folder_fetch import _record
from tests.api.test_web_paywall_folder import _actor_client
from tests.test_share_links import BODY, FILENAME, _share_link_setup

pytestmark = pytest.mark.tier_3

ROUTE_DEADLINE_S = 30.0
ENTRIES_DEADLINE_S = 90.0
SUB = "album"
PRIVATE_FILE = "diary.txt"


@pytest.fixture
def apple_share_owner(request, app, nest_instance):
    """``app`` logged in as an owner with a public set holding the seeded
    ``FILENAME``. Returns ``(app, public_set_id)``."""
    app, user, public_set, _private_set = _share_link_setup(request, app, nest_instance)
    with _actor_client(nest_instance["url"], user) as ws:
        rows = ws.call("fauna.folders.list", {})["folders"]
    return app, find_set(rows, public_set)["id"]


@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("share-links")
def test_the_share_action_routes_open_the_create_surface_and_the_member_share_flow(
    apple_share_owner,
):
    app, folder_id = apple_share_owner
    d = app.driver

    # The route the trampoline spells for a public file (the item identifier
    # is the folder-relative path), over the file the nest already lists.
    d.call_command(
        "open_route", {"uri": f"fauna://share-link?folder={folder_id}&path={FILENAME}"}
    )
    wait_until(
        lambda: d.is_visible("media-item-detail") and d.is_visible("share-link-create-modal"),
        ROUTE_DEADLINE_S,
        diagnose=lambda: (
            f"share-link route did not open the create surface: "
            f"detail={d.is_visible('media-item-detail')} "
            f"create={d.is_visible('share-link-create-modal')} "
            f"media items={app.media.item_names()!r} error={app.error_text()!r}"
        ),
    )

    # The route the trampoline spells for the set's root container.
    d.call_command("open_route", {"uri": f"fauna://folder-share?folder={folder_id}"})
    wait_until(
        lambda: d.is_visible("folder-share-confirm"),
        ROUTE_DEADLINE_S,
        diagnose=lambda: (
            f"folder-share route did not open the member-share flow: "
            f"folder-share-button={d.is_visible('folder-share-button')} "
            f"error={app.error_text()!r}"
        ),
    )


def _share_entries(driver, folder_id: str, paths: list[str]) -> dict:
    """The ``faunaShare`` projection the appex's host gives ``paths`` of one set."""
    raw = driver.call_command(
        "file_provider_share_entries", {"folder_id": folder_id, "paths": paths}
    )
    assert raw, "file_provider_share_entries answered nothing"
    return json.loads(raw)


@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("share-links")
def test_the_provider_items_carry_share_only_where_a_link_or_member_share_can_be_made(
    request, app, nest_instance
):
    app, user, public_set, private_set = _share_link_setup(request, app, nest_instance)
    # A sub-folder of the public set: recorded while the set is public, as the
    # fixture records its file.
    _record(
        nest_instance["url"], nest_instance["port"], user, public_set, f"{SUB}/inner.txt", BODY
    )
    with _actor_client(nest_instance["url"], user) as ws:
        rows = ws.call("fauna.folders.list", {})["folders"]
    public_id = find_set(rows, public_set)["id"]
    private_id = find_set(rows, private_set)["id"]
    d = app.driver

    # The host lists what the nest holds once its first pull lands; poll until
    # every named item is listed, so an absent entry is never an unlisted item.
    public = {}
    wait_until(
        lambda: {FILENAME, SUB} <= set(
            public.update(_share_entries(d, public_id, [FILENAME, SUB])) or public["items"]
        ),
        ENTRIES_DEADLINE_S,
        diagnose=lambda: f"the public set's host listed {public!r}",
    )
    assert public["items"][FILENAME] == {"is_dir": False, "entry": "link"}, public
    assert public["items"][SUB] == {"is_dir": True, "entry": None}, public
    assert public["root"] == "folder", public

    private = {}
    wait_until(
        lambda: PRIVATE_FILE in (
            private.update(_share_entries(d, private_id, [PRIVATE_FILE])) or private["items"]
        ),
        ENTRIES_DEADLINE_S,
        diagnose=lambda: f"the private set's host listed {private!r}",
    )
    assert private["items"][PRIVATE_FILE] == {"is_dir": False, "entry": None}, private
