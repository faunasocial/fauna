"""Media page — ``sync-state-badge`` (this file's own sync presence).

``tests/e2e-unified/ui.yaml`` ``sync-state-badge`` (inside ``media-item``);
``docs/goal/behavior/file-sync.md`` § Per-file sync-status display. linux's,
windows' and web's Media pages are all **control-plane clients** — they list
``fauna.sync.files``, which carries no real per-file engine status, so each
renders every item ``Synced`` (the class file-sync.md sanctions for clients
with no local sync engine) via the shared
``fauna_core::format::sync_display_state_label`` (never a hand-written
string). Windows renders it in both the list view (``MediaPage.xaml:230``)
and the grid view (``MediaPage.xaml:282``), both bound to
``MediaItem.SyncStateLabel`` — verified 2026-07-18, was a stale exclusion
(the marker predates windows' Media page). web renders it via the wasm
``syncedStateBadgeLabel`` twin (``libs/fauna-wasm-media``) — web was the last
of 7 clients owing this leg; landed 2026-07-20, closing the gap this file
previously excluded.

**apple is the other class, and needs its own leg** (``file-sync.md`` § Per-file
sync-status display draws the split): macOS and iOS are *desktop-engine* apps —
the badge comes from the shared engine's own per-set ``SyncDb`` through
``FfiSyncEngineHost::file_states``, and they are the only apps rendering the
full six-state vocabulary from real engine state. A set this device does not
sync locally has **no engine row**, and that doc says in as many words what such
a file renders: ``RemoteOnly`` — on the nest only. The seed fixture below binds
nothing, so apple's honest answer there is ``RemoteOnly``, not ``Synced``.
Asserting ``Synced`` on apple would be asserting a *wrong* badge, so the apple
leg is a separate test asserting apple's own correct state, cited under the same
platform-neutral outcome (``feature-catalog.md`` § The coverage contract — one
outcome, each column's own witness).

tier_3: real nest binary + the real `fauna.sync.changes.record` seed RPC (the
same ``seeded_media_app`` fixture ``test_media.py`` uses) — no test backdoor.
"""

import secrets
import time

import pytest

from helpers.folder_content import atomic_write, await_agent_upload, bind_location_under_set
from i18n.strings import S

# The app marks are per TEST, never module-wide: marks add up, so a module-level
# `tui` mark also selected the apple-only leg below on tui, where it asserted
# apple's `RemoteOnly` against tui's correct `Synced` and reddened tui's column.
pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]


@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.web
@pytest.mark.tui
@pytest.mark.feature("media")
def test_media_item_shows_synced_badge(seeded_media_app):
    """A control-plane client's media item renders ``sync-state-badge`` ==
    the shared ``Synced`` label (linux has no local sync engine to report a
    real per-file state, so every item settles on the same class-sanctioned
    value)."""
    app, plan = seeded_media_app
    total = sum(len(paths) for paths in plan.values())
    assert total >= 1, "fixture must seed at least one item"

    app.media.navigate()
    count = app.media.wait_for_item_count(total)
    assert count == total, (
        f"expected {total} seeded media items, got {count}; "
        f"error={app.error_text()!r}"
    )

    assert app.driver.is_visible("sync-state-badge", scope="media-item[0]"), (
        "media-item should render its own sync-state-badge: "
        f"{app.driver.diagnose('sync-state-badge')}"
    )
    badge_text = app.driver.get_text("sync-state-badge", scope="media-item[0]")
    assert badge_text == S.media.status_label.synced, (
        "a control-plane client (no local sync engine) should render the "
        f"shared Synced label; got {badge_text!r}"
    )


@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("media")
def test_apple_media_item_shows_its_engine_sync_state(seeded_media_app):
    """A desktop-engine client's media item renders ``sync-state-badge`` from
    its own engine state. The fixture seeds through ``fauna.sync.changes.record``
    and binds no local folder, so every seeded set is one this device does not
    sync locally — which `file-sync.md` § Per-file sync-status display says
    renders **RemoteOnly** ("on the nest only", the placeholder case), never the
    control-plane ``Synced`` its sibling above asserts.

    The label is read against the shared ``media.status_label.*`` keys rather
    than a literal, because the text is shared Rust's
    (``sync_display_state_label``) and only the icon + color are apple's own.
    """
    app, plan = seeded_media_app
    total = sum(len(paths) for paths in plan.values())
    assert total >= 1, "fixture must seed at least one item"

    app.media.navigate()
    count = app.media.wait_for_item_count(total)
    assert count == total, (
        f"expected {total} seeded media items, got {count}; "
        f"error={app.error_text()!r}"
    )

    assert app.driver.is_visible("sync-state-badge", scope="media-item[0]"), (
        "media-item should render its own sync-state-badge: "
        f"{app.driver.diagnose('sync-state-badge')}"
    )
    badge_text = app.driver.get_text("sync-state-badge", scope="media-item[0]")
    assert badge_text == S.media.status_label.remote_only, (
        "a desktop-engine client with no engine row for this set should render "
        f"the shared Remote Only label; got {badge_text!r}"
    )


@pytest.mark.parametrize("folder_share_owner_app", ["macos"], indirect=True)
@pytest.mark.macos
@pytest.mark.real_conversations
@pytest.mark.real_sync_agent
@pytest.mark.feature("media")
def test_macos_media_item_of_an_agent_bound_set_shows_synced(
    folder_share_owner_app, tmp_path
):
    """The apple leg's other half: a set this Mac DOES sync locally, through a
    bound folder the real ``fauna-sync-agent`` hosts, renders its engine's own
    state — ``Synced`` once the agent has uploaded the file.

    This is the read the two-root fold routes to the host's own root
    (``on-demand-files.md`` § Apple File Provider binding, *state unification*):
    the Media page reads the SAME per-set ``fsid-<ref>.db`` the agent writes,
    keyed by the set's ``FolderRef`` wire string. A read keyed by anything else
    (the set name, a DB file no agent writes) finds no engine row and renders
    ``RemoteOnly`` — the failure this pins.

    The upload witness is the agent's own log (``await_agent_upload``); the
    Media page refreshes only on its nav edge, so the listing is re-entered
    after it, never polled in place.
    """
    app, _nest, _owner = folder_share_owner_app

    set_name = f"badge-{secrets.token_hex(4)}"
    b = app.backups
    b.navigate_folders()
    b.create_folder_via_wizard(set_name)

    folder = tmp_path / "bound"
    folder.mkdir()
    bind_location_under_set(app, set_name, folder, seat="owner")

    filename = f"shot-{secrets.token_hex(3)}.jpg"
    atomic_write(folder / filename, "not really a jpeg — the badge reads engine state\n")
    await_agent_upload(app, filename, seat="owner")

    deadline = time.monotonic() + 60.0
    badge_text: str | None = None
    while time.monotonic() < deadline:
        app.media.reenter()
        # The re-entered listing loads asynchronously: poll THIS visit's state
        # for the item, then read its badge (latency-independent — no fixed wait
        # decides the verdict, the deadline only bounds the loop).
        visit_deadline = min(deadline, time.monotonic() + 10.0)
        while filename not in app.media.item_names() and time.monotonic() < visit_deadline:
            time.sleep(0.5)
        if filename in app.media.item_names():
            scope = f"media-item[{app.media.index_of(filename)}]"
            if app.driver.is_visible("sync-state-badge", scope=scope):
                badge_text = app.driver.get_text("sync-state-badge", scope=scope)
                if badge_text == S.media.status_label.synced:
                    return
    pytest.fail(
        f"{filename} in agent-bound set {set_name} never rendered the Synced badge; "
        f"last badge {badge_text!r} (RemoteOnly = the Media read found no engine row: "
        f"it is not reading the fsid-<ref>.db the agent writes); "
        f"items={app.media.item_names()!r} error={app.error_text()!r}"
    )
