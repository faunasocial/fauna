import time
from pathlib import Path

import pytest

from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]

# A real 960×720 PNG — above the producer's 300×300 thumbnail threshold
# (`libs/fauna-media/src/process.rs`), so an upload of it exercises the on-device
# `process_media` thumbnail producer. Shared with test_feed.py's image fixture.
FIXTURE_IMAGE = Path(__file__).parent.parent / "fixtures" / "test-image.png"


def test_media_page_loads(logged_in_app):
    """Navigate to media page and verify upload form is visible."""
    logged_in_app.media.navigate()
    assert logged_in_app.driver.is_visible("file-upload"), (
        "media page should show the file-upload control: "
        f"{logged_in_app.driver.diagnose('file-upload')}"
    )
    assert logged_in_app.driver.is_visible("upload-button"), (
        "media page should show the upload button: "
        f"{logged_in_app.driver.diagnose('upload-button')}"
    )


@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.web
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.feature("media")
def test_media_explorer_chrome(empty_media_app):
    """The Media page renders the Windows-Explorer-style cross-set browser chrome
    off the shared observer-driven ``MediaMachine`` (``docs/goal/ui/media.md``
    § Layout & flow; spec 2026-06-28-sync-file-set-ui-unification § 4): the
    view-toggle, the sort + per-set-filter selects, the upload affordance, the
    empty state, and the view-toggle gesture round-tripping through the machine.

    Rides ``empty_media_app`` (a dedicated fresh actor), NOT the shared session
    ``test_user``: the empty-media precondition below is ESTABLISHED by the
    fixture rather than inherited from suite order. It rode ``test_user`` until
    2026-08-18, and any earlier module that gave that shared user media broke
    this test's empty-state half (in-suite failure mode).

    Cross-set aggregation + sort/filter run in shared Rust
    (``fauna-client-media`` / ``fauna-media-machine``, unit-tested there); this
    e2e proves the client renders the chrome and the gestures reach the machine.

    ``@pytest.mark.linux`` + ``@pytest.mark.windows`` — linux leads the explorer
    shape; windows joined 2026-06-30 (its MediaPage
    chrome matches the shared ids, incl. the ``__all__`` filter sentinel) and as of
    the 2026-06-30 MediaMachine lift renders ENTIRELY off the shared cross-set
    ``MediaMachine`` (``libs/fauna-media-machine`` via UniFFI), same as linux — no
    per-app ``MediaViewModel`` remains. apple (macos+ios) joined 2026-06-30 — the
    shared FaunaKit ``MediaExplorerContent`` renders off the same ``MediaMachine`` on
    both apple apps. web/android add their own
    ``@pytest.mark.<client>`` as they migrate (the same per-app window as
    ``BackupsActions._folders_in_settings``).
    """
    app = empty_media_app
    d = app.driver
    app.media.navigate()
    app.media.ensure_loaded()

    # Explorer chrome is present.
    for el in (
        "page-heading",
        "media-view-toggle",
        "media-sort-select",
        "media-folder-filter",
        "file-upload",
        "upload-button",
    ):
        assert d.is_visible(el), f"explorer chrome missing {el!r}: {d.diagnose(el)}"

    # Empty state: no media seeded → no items, no page error.
    assert app.media.item_count() == 0, (
        f"expected an empty media list, got {app.media.item_count()}; "
        f"error={app.error_text()!r}"
    )
    assert not app.has_error(), f"unexpected media error on load: {app.error_text()!r}"

    # The view-toggle flips list ↔ grid (its label reflects the active mode, so
    # the flip is observable) — proving the gesture reaches the machine and the
    # page re-renders off the new snapshot.
    before = app.media.view_label()
    app.media.toggle_view()
    after = app.media.wait_for_view_label_change(before)
    assert after != before, f"media-view-toggle did not flip the view (stayed {before!r})"

    # Sort + filter selects operate without raising (the ordering logic itself is
    # shared-Rust unit-tested; here we only assert the selects are wired).
    for key in ("size", "date", "name"):
        app.media.set_sort(key)
    app.media.set_filter()  # the all-media default
    assert not app.has_error(), f"sort/filter raised a page error: {app.error_text()!r}"


# tui leads (the lead app); the other apps join through the cross-app lift row
# minted with this witness, each once its run of this ORDERING assertion passed.
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.tui
@pytest.mark.feature("media")
def test_media_browse_switches_list_and_grid_and_sorts_by_name_size_and_date(
    sortable_media_app,
):
    """The browse switches between a list and a grid, and sorts by name, size or
    date (``docs/goal/ui/media.md`` § Layout & flow — ``media-view-toggle``,
    ``media-sort-select``).

    The ordering half ``test_media_explorer_chrome`` deliberately leaves out: that
    test rides an EMPTY actor (its empty-state half needs one), so its sort
    gestures have nothing to order. This one rides ``sortable_media_app`` — three
    files whose name, size and date orders all differ, and none of which but name
    matches the shared ``(folder, path)`` tiebreak — so a key that did not really
    sort (or a date sort over tied stamps) shows the wrong order instead of
    passing by coincidence. Each order is read back whole, by name, from the page
    the user sees.

    Ascending only, on purpose: the outcome is the three keys, and
    ``media-sort-direction`` is a separate control several apps have not built
    yet (media.md § Implementation status today) — asserting it here would redden
    this outcome on those apps for a promise it does not make.
    """
    app, _set_name, rows = sortable_media_app
    app.media.navigate()
    app.media.ensure_loaded()

    by_name = sorted(name for name, _ in rows)
    by_size = [name for name, _ in sorted(rows, key=lambda r: r[1])]
    by_date = [name for name, _ in rows]  # the fixture records oldest first
    assert len({tuple(by_name), tuple(by_size), tuple(by_date)}) == 3, (
        f"the seed must give three different orders; name={by_name} "
        f"size={by_size} date={by_date}"
    )

    # Size and date first: name is the default order, so starting there would let
    # the first barrier pass before any gesture landed.
    for key, expected in (("size", by_size), ("date", by_date), ("name", by_name)):
        app.media.set_sort(key)
        shown = app.media.wait_for_item_names(expected)
        assert shown == expected, (
            f"sorting by {key} should list {expected}; the page shows {shown}; "
            f"error={app.error_text()!r}"
        )

    # List ↔ grid: the toggle names the mode it is in, and every item stays listed
    # in the active order across the switch and back.
    before = app.media.view_label()
    app.media.toggle_view()
    after = app.media.wait_for_view_label_change(before)
    assert {before, after} == {S.media.view_list, S.media.view_grid}, (
        f"media-view-toggle should switch between the list and the grid; it read "
        f"{before!r} then {after!r}"
    )
    shown = app.media.wait_for_item_names(by_name)
    assert shown == by_name, (
        f"the {after!r} view should still show every item in name order; got {shown}"
    )
    app.media.toggle_view()
    back = app.media.wait_for_view_label_change(after)
    assert back == before, f"a second switch should return to {before!r}, got {back!r}"
    assert not app.has_error(), f"unexpected media error: {app.error_text()!r}"


@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.windows
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.feature("media")
def test_media_empty_state_marks_a_loaded_page_not_a_loading_one(empty_media_app):
    """A fresh account's Media page finishes loading and SAYS it is empty, under
    ``media-empty-state`` (``docs/goal/ui/media.md`` § Default view: cross-set
    all-media — the three-state table; ui.yaml `media` page, user-approved
    2026-08-05).

    Why this element exists at all: ``items`` is empty both before the first
    ``fauna.media.list`` returns and after one that found nothing, so the page
    used to announce "No media yet" over media that was about to appear, and the
    multiseat harness could not tell an empty set from an unloaded one
    (``helpers/multiseat_config.py::settle_listing``, which now consumes this via
    :meth:`MediaActions.has_loaded`). ``MediaPageSnapshot.loaded`` is the second
    painting condition.

    Scope split, deliberately: the NEGATIVE half — that a page still loading
    paints no empty state — is a race at this level, so it is pinned
    deterministically one tier down (``fauna-media-machine``'s
    ``a_page_that_has_never_refreshed_is_not_loaded`` and tui's
    ``a_page_that_has_not_finished_loading_paints_no_empty_state``, both of which
    fail if the ``loaded`` gate is removed). This test proves the wiring: the
    element reaches the real UI tree of each app that paints it, and it means
    "loaded AND empty" rather than merely "empty".

    Marked for the six apps that paint it AND can run e2e today. As of
    2026-08-06 all seven paint it — the trickle-down is finished, so nothing is
    logged as missing in any ``ui-actual-*.yaml``'s ``missing_from_client.media``
    any more; media.md § Implementation status today carries the live matrix.

    **android paints it as of 2026-08-05 but is deliberately NOT marked** — the
    same standing infrastructure reason ``test_labeler_catalog.py``'s module
    docstring records: no android e2e test has ever run against a real device
    fleet-wide (the android bridge implements no ``/element/attr`` route and
    emulator access is emulator-host-gated). A
    mark that cannot run proves nothing; add it with the first real device run,
    not before. android's gate is covered meanwhile by its Robolectric pair
    ``MediaContentTest.emptyStateShownWhenNoMedia`` /
    ``…WithheldWhileTheFirstReadIsStillInFlight``.

    Rides ``empty_media_app`` so "a fresh account" is literal — the emptiness is
    established by a dedicated actor, not inherited from suite order (see
    ``test_media_explorer_chrome``'s docstring).
    """
    app = empty_media_app
    app.media.navigate()
    app.media.ensure_loaded()

    loaded = app.media.wait_for_loaded()
    assert loaded, (
        "the Media page never reported itself loaded within "
        f"{app.media.LOAD_BUDGET_S:.0f}s: no media-item rows and no "
        f"media-empty-state. error={app.error_text()!r}; "
        f"{app.driver.diagnose('media-empty-state')}"
    )

    assert app.media.item_count() == 0, (
        f"fixture account should hold no media, got {app.media.item_count()}"
    )
    assert app.driver.is_visible("media-empty-state"), (
        "a loaded, media-less page must paint media-empty-state: "
        f"{app.driver.diagnose('media-empty-state')}"
    )
    assert not app.has_error(), (
        "the empty state must not be accompanied by a page error — that "
        f"combination means the read FAILED, not that the set is empty: "
        f"{app.error_text()!r}"
    )


@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.web
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.feature("media")
def test_all_media_cross_set(seeded_media_app):
    """The default all-media view aggregates media across EVERY readable folder,
    and the per-set filter narrows to one set (``docs/goal/ui/media.md`` § State &
    data shape + § Default view: cross-set all-media; spec
    2026-06-28-sync-file-set-ui-unification § 4) — proven end-to-end: a dedicated
    actor's two pre-seeded sets reach the client through ``fauna.media.list`` and
    render as ``media-item``s.

    Seeded off the SHARED ``seeded_media_app`` fixture, which uses the real
    production ``fauna.sync.changes.record`` RPC (the path the ``fauna-sync`` daemon
    itself uses) — no spawned sync daemon (heavy/FlaUI-flake-prone) and no test
    backdoor. Platform-agnostic; ``@pytest.mark.linux`` + ``@pytest.mark.windows``
    are the apps that have shipped the explorer (mirrors
    ``test_media_explorer_chrome``); apple (macos+ios) joined 2026-06-30; web/android
    add their mark as they migrate.

    The proof is count-based and rendering-format-independent: the all-media count
    equals the SUM across both sets (so a single-set view would fail it), and each
    per-set filter count equals exactly that set's subset (so narrowing is real).
    """
    app, plan = seeded_media_app
    total = sum(len(paths) for paths in plan.values())
    assert total >= 2 and len(plan) >= 2, "fixture must seed ≥2 items across ≥2 sets"

    app.media.navigate()
    app.media.ensure_loaded()

    # (1) Cross-set: the all-media default (filter == __all__ on load) shows the
    # union of BOTH seeded sets. A single-set render would settle at one set's
    # size, not the sum — so count == total proves aggregation.
    count = app.media.wait_for_item_count(total)
    assert count == total, (
        f"all-media should show all {total} seeded items across {list(plan)}, "
        f"got {count}; error={app.error_text()!r}"
    )
    assert not app.has_error(), f"unexpected media error on all-media load: {app.error_text()!r}"

    # (2) The per-set filter narrows the view to exactly that set's subset.
    for set_name, paths in plan.items():
        app.media.set_filter(set_name)
        narrowed = app.media.wait_for_item_count(len(paths))
        assert narrowed == len(paths), (
            f"filter {set_name!r} should narrow to its {len(paths)} item(s), "
            f"got {narrowed}; error={app.error_text()!r}"
        )

    # (3) Back to all-media: the filter is reversible and lands on the full union.
    app.media.set_filter()  # __all__
    restored = app.media.wait_for_item_count(total)
    assert restored == total, (
        f"clearing the filter should restore all {total} items, got {restored}"
    )
    assert not app.has_error(), f"unexpected media error after filtering: {app.error_text()!r}"


@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.web
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.feature("media")
def test_media_upload_no_set_error(empty_media_app, tmp_path):
    """Clicking ``upload-button`` with a file chosen but no folder available runs
    the shared ``MediaMachine::upload_selected`` gesture, which surfaces the
    "select a folder" page error (``media.error_no_set``) — proving the upload
    button is WIRED to the shared gesture (no longer the inert reload) and that the
    shared "which set" target policy (the filter-selected set, else the first
    upload-target set, else this error) fires (``docs/goal/ui/media.md``
    § Layout & flow / § Where logic lives; spec
    2026-06-28-sync-file-set-ui-unification § 4).

    **The premise narrowed on 2026-08-03; since 2026-08-18 the fixture
    ESTABLISHES it.** It used to rest on "no media"; the target now comes from
    the control-plane set list, so what makes this error reachable is that the
    actor has **no folder at all** — an *empty* set is a perfectly
    good target and no longer lands here
    (``test_upload_into_a_freshly_created_empty_folder`` is that case). So this
    test is specifically the no-set-whatsoever branch. It rode the shared
    session ``test_user`` until 2026-08-18, "honest only while nothing gives
    test_user a file set" — and the suite falsified that premise: any
    filesync/webdav module running first gives ``test_user`` a Sync folder,
    this upload then SUCCEEDS into it (red), and the planted item broke the
    sibling empty-state tests too (in-suite failure mode).
    ``empty_media_app`` (a dedicated fresh actor) makes no-set-whatsoever true
    by construction, in any suite order.

    No seeding + no device registration needed: the dedicated actor has no
    folders, so the all-media view has no set to default the upload target to →
    the gesture sets the no-set error rather than uploading.
    The success round-trip (a real blob lands + the list shows it) is
    ``test_media_upload_into_selected_set`` below, gated on the logged-in client's
    sync device being registered write-capable.

    ``@pytest.mark.linux`` + ``@pytest.mark.windows`` — linux leads the explorer +
    upload shape; windows wired ``upload-button`` to the
    shared ``upload_selected`` 2026-06-30; apple
    (macos+ios) wired the native uploader + joined 2026-06-30; web/android add their
    mark as they wire it.
    """
    app = empty_media_app
    app.media.navigate()
    app.media.ensure_loaded()

    # Pre-state: empty media, no error (the all-media default has no set).
    assert app.media.item_count() == 0, (
        f"expected empty media, got {app.media.item_count()}; error={app.error_text()!r}"
    )

    # A real file so the client-side read succeeds and the gesture reaches the
    # machine's target resolution (the file is never uploaded — there's no set).
    picked = tmp_path / "snapshot.jpg"
    picked.write_bytes(b"\xff\xd8\xff\xe0 not a real jpeg, just bytes")
    app.media.upload_file(str(picked))

    assert app.media.wait_for_error(), (
        "upload-button with no set should surface a page error; "
        f"got none (item_count={app.media.item_count()})"
    )
    assert app.error_text() == S.media.error_no_set, (
        f"upload with no set should surface the no-set error, got {app.error_text()!r}"
    )


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.tui
@pytest.mark.feature("media")
def test_media_upload_with_no_file_chosen_says_so(seeded_media_app):
    """Pressing upload with no file chosen says so on ``error-message`` instead of
    doing nothing (``docs/goal/ui/media.md`` § User actions — "never a silent
    no-op, which presents as a dead button").

    Rides ``seeded_media_app`` so a folder to upload INTO exists: the only thing
    missing is the file, so the answer can only be the no-file one — not
    ``test_media_upload_no_set_error``'s no-folder answer, which an empty actor
    would reach first on an app that checks the folder before the file. The
    listing must come out of it unchanged.
    """
    app, plan = seeded_media_app
    total = sum(len(paths) for paths in plan.values())
    app.media.navigate()
    app.media.ensure_loaded()
    assert app.media.wait_for_item_count(total) == total
    assert not app.has_error(), f"unexpected media error before the press: {app.error_text()!r}"

    app.media.press_upload_with_no_file()

    assert app.media.wait_for_error(), (
        "pressing upload with no file chosen must say so on error-message; nothing "
        f"appeared (item_count={app.media.item_count()})"
    )
    expected = app.media.no_file_chosen_text()
    assert app.error_text() == expected, (
        f"the no-file press should say {expected!r}, got {app.error_text()!r}"
    )
    assert app.media.item_count() == total, (
        f"a press with no file must upload nothing: {total} → {app.media.item_count()}"
    )


@pytest.mark.linux
@pytest.mark.web
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.feature("media")
def test_file_version_history_and_restore(seeded_media_app, tmp_path):
    """Per-file version history + restore, end-to-end through the client UI
    (``docs/goal/behavior/file-sync.md`` § File Versions / § Restore;
    ``docs/goal/ui/media.md`` § Element IDs — the ``media-item-detail`` /
    ``file-version-history`` surface approved 2026-07-09).

    Every recorded change IS a version (the nest projects ``sync_changes``), so
    two uploads of the SAME member path (= two records of that path) yield two
    listable versions. Restore is a re-point: an ordinary ``modify`` carrying
    the historical manifest — no byte re-upload — that becomes the new head AND
    a new version (reversible), and the page's item size flips back to v1's,
    proving the head actually moved.

    All mutations ride the UI (e2e rule 8): the uploads via ``upload-button``
    (the shared ``upload_selected`` gesture), the restore via the per-row
    ``file-version-restore-button`` + the lightweight confirm modal, driven by
    the shared ``MediaMachine::{file_versions, restore_version}``.

    ``@pytest.mark.linux`` + ``@pytest.mark.web`` + ``@pytest.mark.windows`` +
    ``@pytest.mark.macos`` + ``@pytest.mark.ios`` — linux leads the detail/version
    surface (the explorer-LEAD precedent), web is the slice-3b adopter
    (``routes/media/+page.svelte``), windows lifted it 2026-07-10 (``MediaPage.xaml``
    inline-Border sheet), apple lifted it 2026-07-12 (shared FaunaKit
    ``MediaItemDetailView``, an inline ``@State``-driven overlay); android adds its
    mark as it lifts the pattern.
    """
    app, plan = seeded_media_app
    target_set, seeded_paths = next(
        ((name, paths) for name, paths in plan.items() if len(paths) == 1),
        (None, None),
    )
    assert target_set is not None, f"fixture should seed a single-folder; plan={plan!r}"
    seeded = len(seeded_paths)

    app.media.navigate()
    app.media.ensure_loaded()
    app.media.set_filter(target_set)
    assert app.media.wait_for_item_count(seeded) == seeded, (
        f"seeded set {target_set!r} should settle at {seeded}; error={app.error_text()!r}"
    )

    # v1: upload a fresh member (the recorded path is the picked basename).
    picked = tmp_path / "versioned.bin"
    picked.write_bytes(b"v" * 1200)
    app.media.upload_file(str(picked))
    count = app.media.wait_for_item_count(seeded + 1)
    assert count == seeded + 1, (
        f"v1 upload should add one item ({seeded} → {seeded + 1}), got {count}; "
        f"error={app.error_text()!r}"
    )
    idx = app.media.item_names().index("versioned.bin")
    size_v1 = app.media.item_size(idx)

    # v2: SAME basename, different (larger) content — an ordinary re-record of
    # the same path. The item count stays put (latest-per-path listing); the
    # item's rendered size flipping is the signal the new head landed.
    picked.write_bytes(b"w" * 48_000)
    app.media.upload_file(str(picked))
    size_v2 = app.media.wait_for_item_size_change(idx, size_v1)
    assert size_v2 != size_v1, (
        f"v2 upload of the same path should change the head size ({size_v1!r}); "
        f"error={app.error_text()!r}"
    )

    # media-item tap/open → the detail surface listing both versions,
    # oldest→newest (version rows render the same size formatting as items).
    app.media.open_item_detail(idx)
    assert app.media.detail_name() == "versioned.bin", (
        f"detail should name the opened file, got {app.media.detail_name()!r}"
    )
    versions = app.media.wait_for_version_count(2)
    assert versions == 2, (
        f"two records of the path should list two versions, got {versions}; "
        f"error={app.error_text()!r}"
    )
    assert app.media.version_size(0) == size_v1, (
        f"v1 row should show the original size {size_v1!r}, got "
        f"{app.media.version_size(0)!r}"
    )
    assert app.media.version_size(1) == size_v2

    # `file-version-author` (attribution — file-sync.md § Multi-writer shared
    # sets): both versions were recorded by the same (only) actor, so both
    # rows render a non-empty author label off the nest-stamped
    # `author_actor_id`.
    assert app.media.has_version_author(0), (
        f"v1 row should render an author label; error={app.error_text()!r}"
    )
    assert app.media.version_author(0) != "", "v1 author label should be non-empty"
    assert app.media.has_version_author(1), "v2 row should render an author label"
    assert app.media.version_author(1) == app.media.version_author(0), (
        "same single actor recorded both versions — author label should match"
    )

    # Restore v1 (restore-button → confirm modal → confirm). The restore is a
    # NEW version (2 → 3, append-only history) whose content is v1's.
    app.media.restore_version(0)
    versions = app.media.wait_for_version_count(3)
    assert versions == 3, (
        f"restore should append a version (2 → 3), got {versions}; "
        f"error={app.error_text()!r}"
    )
    assert app.media.version_size(2) == size_v1, (
        "the restored head should carry v1's size, got "
        f"{app.media.version_size(2)!r}"
    )
    assert not app.has_error(), f"unexpected page error after restore: {app.error_text()!r}"

    # Close the detail; the page's item size flipped back to v1's — the head
    # really re-pointed (fauna.media.list reads the latest live record).
    app.media.close_detail()
    settled = app.media.wait_for_item_size_change(idx, size_v2)
    assert settled == size_v1, (
        f"after restore the item should show v1's size {size_v1!r}, got {settled!r}"
    )


@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.web
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.feature("media")
def test_media_upload_into_selected_set(seeded_media_app):
    """``upload-button`` uploads the chosen file into the selected set via the
    shared ``MediaMachine::upload_selected`` gesture (seal under the owner BackupKey
    → POST the blob → record the manifest member → refresh), then the list shows it
    (``docs/goal/ui/media.md`` § Layout & flow / § User actions; the success
    definition of the filesets-explorer upload track).

    The upload is a real >300×300 image, so the SAME gesture also exercises the
    on-device thumbnail PRODUCER (``docs/goal/ui/media.md`` § Thumbnails,
    § Implementation status "Producer remaining (1)"): the curated ``process_media``
    pipeline generates + POSTs a companion thumbnail blob and records its hash, and
    web paints it (asserted below). On web this is the only leg that exercises the
    ``image`` decoder in the real **wasm runtime** (the shared producer + fetch are
    unit-tested natively). Other apps run the count round-trip only — they verify
    the paint via per-app unit tests (``painted_thumbnail_count`` returns ``None``
    off web, so the paint assertion self-skips).

    The recording device self-heals its write registration, so the e2e needs NO
    explicit device-registration step: the gesture records the new member via
    ``fauna.sync.changes.record``, whose nest handler requires the device registered
    write-capable, and a folder-less client never mapped a folder so never registered
    one — so the shared Media write seam (``fauna-media-machine``
    ``nest_api::ws_rpc::record_self_healing``) registers it on the nest's dedicated
    ``fauna.sync.device_unregistered`` rejection and retries once
    (``docs/goal/ui/media.md`` § Implementation status, "Upload precondition (closed
    2026-06-30 — self-healing registration)"; ``docs/goal/behavior/file-sync.md``
    § Device Registration).

    Seeded off the dedicated-actor ``seeded_media_app`` fixture (NOT the shared
    session ``test_user``): a fresh actor whose owned sets the upload mutates can't
    pollute the empty-state ``test_media_explorer_chrome`` /
    ``test_media_upload_no_set_error``, even on a mid-test failure — the same
    isolation rationale as ``test_all_media_cross_set`` (and no fragile teardown).
    Its single-file ``media-seed-clips`` set gives the clean 1 → 2.

    The logged-in client's device (the e2e session ``device_id``,
    ``_E2E_LOGIN_DEVICE_ID``) is NOT the seeding device, so it was never registered
    write-capable — exactly the self-heal case: the first ``changes.record`` is
    rejected ``device_unregistered`` and the shared write seam registers + retries
    (the same path on linux + windows).

    ``@pytest.mark.linux`` + ``@pytest.mark.windows`` — linux leads the explorer +
    upload shape; windows wired ``upload-button`` to the
    shared ``upload_selected`` 2026-06-30; apple
    (macos+ios) wired the native uploader + joined 2026-06-30; web joined 2026-07-02
    with the wasm blob-upload coordinator (LEG B — ``WasmBlobUploader`` injected in
    the wasm ``build_media_machine``); android adds its mark as it wires it.
    """
    app, plan = seeded_media_app

    # The single-file seeded Sync set → a clean 1 → 2 after one upload. Selecting it
    # in the filter both narrows the count to that set AND pins the upload target
    # (``upload_selected`` defaults to the filter-selected set).
    target_set, seeded_paths = next(
        ((name, paths) for name, paths in plan.items() if len(paths) == 1),
        (None, None),
    )
    assert target_set is not None, f"fixture should seed a single-folder; plan={plan!r}"
    seeded = len(seeded_paths)

    app.media.navigate()
    app.media.ensure_loaded()
    app.media.set_filter(target_set)
    pre = app.media.wait_for_item_count(seeded)
    assert pre == seeded, (
        f"the seeded set {target_set!r} should show its {seeded} item before upload, "
        f"got {pre}; error={app.error_text()!r}"
    )
    # The seeded item is a non-image (`.mp4`), so the producer makes no thumbnail —
    # nothing paints yet (web reads the DOM; other apps self-skip, returning None).
    painted_before = app.media.wait_for_painted_thumbnails(0, timeout=3.0)
    if painted_before is not None:
        assert painted_before == 0, (
            f"the seeded non-image item must not paint a thumbnail, got {painted_before}"
        )

    # Upload a real >300×300 image with a distinct basename (the recorded member path
    # is the picked file's basename — apps/fauna-linux/src/views/media/mod.rs — so a
    # new name records a new member, not an update). upload_selected seals the bytes,
    # POSTs the blob, and records the member into the selected set; the recording
    # device self-registers write-capable on the first record. An image (not fake
    # bytes) so the same gesture drives the thumbnail producer, asserted below.
    assert FIXTURE_IMAGE.exists(), f"missing image fixture: {FIXTURE_IMAGE}"
    app.media.upload_file(str(FIXTURE_IMAGE))

    # The selected set's item count goes 1 → 2 once fauna.media.list reflects the
    # newly recorded member — proving the round-trip (seal → POST → record → list)
    # AND the device self-heal (an unregistered device's record would otherwise be
    # rejected and the count would stay at 1). The companion thumbnail is a separate
    # blob, not a member, so it does not change this count.
    after = app.media.wait_for_item_count(seeded + 1)
    assert after == seeded + 1, (
        f"upload should add one item to {target_set!r} ({seeded} → {seeded + 1}), "
        f"got {after}; error={app.error_text()!r}"
    )
    assert not app.has_error(), f"unexpected page error after upload: {app.error_text()!r}"

    # The uploaded image's thumbnail is PRODUCED + painted: on web the curated
    # `process_media` wasm producer (the last web producer leg — media.md § Impl
    # status, Producer remaining (1)) generates the thumbnail, records its hash, and
    # the Slice-5 on-appear fetch → owner-key decrypt paints exactly one
    # `media-thumbnail` `<img>` (the seeded non-image paints none). Other apps
    # self-skip (None) — they verify the paint via per-app unit tests.
    painted = app.media.wait_for_painted_thumbnails(1)
    if painted is not None:
        assert painted == 1, (
            f"the uploaded image should produce + paint exactly one thumbnail, got "
            f"{painted}; error={app.error_text()!r}. A 0 means the wasm producer did not "
            f"generate a thumbnail_hash (the process_media flip) or the paint/fetch failed."
        )


# Marked only where `MediaActions.thumbnail_kind` can tell a picture from a
# placeholder (tui's text, web's DOM, linux's live `gtk::Image`, macOS's and
# iOS's decoded card image, windows' Image Source as HelpText): the other GUI
# apps paint into native image views the action layer does not read yet, so
# marking them would record a pass that asserted nothing. They join as their
# driver gains the read (the cross-app lift row minted with this).
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.tui
@pytest.mark.feature("media")
def test_media_item_shows_its_picture_or_a_placeholder(seeded_media_app):
    """Each item shows a picture of itself, or a placeholder when there is none
    (``docs/goal/ui/media.md`` § Layout & flow — ``media-thumbnail`` on every
    ``media-item``; § Thumbnails).

    Both halves on one page: the seeded ``.mp4`` has no thumbnail, and a real
    >300×300 image uploaded through the page gets one from the on-device producer.
    Each row is found by NAME — the sort decides positions — and each must carry a
    ``media-thumbnail``: a picture for the image, the placeholder for the clip.
    """
    app, plan = seeded_media_app
    target_set, seeded_paths = next(
        ((name, paths) for name, paths in plan.items() if len(paths) == 1),
        (None, None),
    )
    assert target_set is not None, f"fixture should seed a single-file folder; plan={plan!r}"
    clip = seeded_paths[0].rsplit("/", 1)[-1]

    app.media.navigate()
    app.media.ensure_loaded()
    app.media.set_filter(target_set)
    assert app.media.wait_for_item_count(1) == 1

    # Before the upload: the clip, with nothing to picture, shows the placeholder.
    kind = app.media.thumbnail_kind(app.media.index_of(clip))
    assert kind == "placeholder", (
        f"{clip!r} has no thumbnail, so its media-thumbnail should be the "
        f"placeholder; got {kind!r}"
    )

    assert FIXTURE_IMAGE.exists(), f"missing image fixture: {FIXTURE_IMAGE}"
    app.media.upload_file(str(FIXTURE_IMAGE))
    after = app.media.wait_for_item_count(2)
    assert after == 2, f"the upload should add one item, got {after}; error={app.error_text()!r}"
    # The picture arrives several async hops after the row (produce → record →
    # list → fetch → decrypt → paint), so wait for the paint itself.
    painted = app.media.wait_for_painted_thumbnails(1)
    assert painted == 1, (
        f"the uploaded image should paint its picture; painted={painted!r}, "
        f"error={app.error_text()!r}"
    )

    image = FIXTURE_IMAGE.name
    kinds = {
        name: app.media.thumbnail_kind(app.media.index_of(name))
        for name in (image, clip)
    }
    assert kinds == {image: "picture", clip: "placeholder"}, (
        f"every item shows a picture of itself or a placeholder: the image should "
        f"show its picture and the clip the placeholder; got {kinds!r}"
    )


@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.tui
# apple joined 2026-08-05 (macOS + iOS from one shared FaunaKit
# `MediaItemDetailView` change): the IDs had been specced since 2026-07-16 while
# apple had only a `deleteItem` call site with no button — the "affordance-less
# capability" `media.md` § Implementation status flagged.
@pytest.mark.macos
@pytest.mark.ios
# web joined: the shared wasm `MediaMachine::delete`
# binding (`libs/fauna-wasm-media`) was already built; only the Svelte
# affordance (media-delete-button + its confirm modal, routes/media/+page.svelte)
# was missing — the same "affordance-less capability" shape apple hit above.
@pytest.mark.web
@pytest.mark.android
@pytest.mark.feature("media")
def test_media_delete_removes_the_item(seeded_media_app):
    """``media-delete-button`` + its single confirm deletes the opened file, and the
    explorer stops listing it (``docs/goal/ui/media.md`` § Element IDs — the
    ``media-delete-button`` family user-approved 2026-07-16; tombstone semantics owned
    by ``docs/goal/behavior/file-sync.md`` § File Versions).

    **Why this test exists:** the shared ``MediaMachine::delete`` →
    ``fauna.sync.delete_member`` path was built and unit-proven, but until 2026-07-16
    NO client exposed an affordance for it and NO e2e drove it — a finished capability
    with no button, against the *"user always controls their data … delete affordances
    live in the user's client"* product invariant. This is that gap's regression proof.

    The delete records a **tombstone**; ``fauna.media.list`` is tombstone-excluding, so
    the row disappears. All mutations ride the UI (e2e rule 8): the delete goes through
    ``media-delete-button`` → ``media-delete-confirm-modal`` → confirm, never a raw RPC.

    linux led (the explorer/detail-surface LEAD precedent); windows lifted the pattern
    2026-07-22 and tui 2026-07-24; the other apps add their mark as they lift it.
    """
    app, plan = seeded_media_app
    app.media.navigate()
    app.media.ensure_loaded()

    target_set = "media-seed-photos"
    app.media.set_filter(target_set)
    seeded = len(plan[target_set])
    assert app.media.wait_for_item_count(seeded) == seeded, (
        f"precondition: {target_set!r} should list its {seeded} seeded files, got "
        f"{app.media.item_count()}; error={app.error_text()!r}"
    )

    doomed = app.media.item_name(0)
    app.media.open_item_detail(0)
    assert app.media.detail_name() == doomed, (
        "precondition: the detail surface should name the item we opened"
    )

    app.media.delete_open_item()

    after = app.media.wait_for_item_count(seeded - 1)
    assert after == seeded - 1, (
        f"deleting {doomed!r} should drop {target_set!r} from {seeded} to {seeded - 1} "
        f"items, got {after}; error={app.error_text()!r}. A count that never drops means "
        f"the confirm did not reach MediaMachine::delete, or the tombstone is not "
        f"excluded from fauna.media.list."
    )
    assert doomed not in app.media.item_names(), (
        f"the deleted file {doomed!r} should be gone from the explorer, still listed "
        f"among {app.media.item_names()!r}"
    )
    assert not app.has_error(), f"unexpected page error after delete: {app.error_text()!r}"


@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.web
@pytest.mark.android
@pytest.mark.feature("media")
def test_media_delete_cancel_is_a_no_op(seeded_media_app):
    """Cancelling the delete confirm is a **pure no-op** — no mutation, no error.

    The same contract the sibling confirm modals on this surface hold
    (``file-version-restore-cancel-button``, ``account-activate-reauth-cancel-button``):
    cancel closes with no side effect. Pinning it stops a future refactor from wiring
    cancel to the delete path — a silent data-loss bug the happy-path test cannot see.
    """
    app, plan = seeded_media_app
    app.media.navigate()
    app.media.ensure_loaded()

    target_set = "media-seed-photos"
    app.media.set_filter(target_set)
    seeded = len(plan[target_set])
    assert app.media.wait_for_item_count(seeded) == seeded, (
        f"precondition: {target_set!r} should list its {seeded} seeded files; "
        f"error={app.error_text()!r}"
    )
    before = app.media.item_names()

    app.media.open_item_detail(0)
    app.media.cancel_delete_open_item()
    app.media.close_detail()

    assert app.media.item_count() == seeded, (
        f"cancelling the delete confirm must not delete anything: {target_set!r} should "
        f"still list {seeded} items, got {app.media.item_count()}; "
        f"error={app.error_text()!r}"
    )
    assert app.media.item_names() == before, (
        f"cancelling must leave the listing untouched: {before!r} → "
        f"{app.media.item_names()!r}"
    )
    assert not app.has_error(), (
        f"cancelling a delete must surface no error, got {app.error_text()!r}"
    )


def _wait_for_gone(path, timeout=60.0) -> bool:
    """Poll until ``path`` leaves the disk; return whether it did."""
    deadline = time.monotonic() + timeout
    while path.exists() and time.monotonic() < deadline:
        time.sleep(0.5)
    return not path.exists()


@pytest.mark.linux
@pytest.mark.windows
# tui joined 2026-07-31, once the agent-lifecycle bug that blocked it was found
# and fixed. The ordering symptom recorded here — run `test_sync_live_apply.py`
# first and this test's fixture reaches `running: True` with its folder listed
# while the engine never uploads — was NOT the bind→unbind→bind cycle the note
# blamed, and the content-key suspicion it carried was wrong. The cause was that
# `fauna-sync-agent`'s engine host FREEZES its account identity (`actor_id` /
# `BackupKey` / device id / nest url) into its `FolderEngineSpec` at first build
# and nothing ever rebuilt it — so the first test's shared actor built the host,
# and this fixture's DEDICATED actor then had its engine started on it, dialling
# the previous actor's WS path with the new actor's bearer. The unbind was
# incidental; the trigger is simply "a host built for actor A survives to serve
# actor B", which is also why a prior login alone looked innocent (with nothing
# ever bound, no host existed yet). Fixed in `engine_driver.rs::reconcile_engines`
# (rebuild the host when the provisioned identity changes), pinned by tier_1
# `engine_driver::tests::a_host_built_for_one_actor_is_rebuilt_when_another_actor_provisions`
# plus its symmetric `a_bearer_rotation_..._reuse_the_running_host` guard, both
# mutation-verified. It was never tui-specific: windows drives the same agent and
# had the same defect, unobserved because nobody ran these two suites in order.
@pytest.mark.tui
# macos joined 2026-08-05 — the third agent-driven desktop. Its two missing
# pieces were pure observability, exactly as tui's were: `data.sync`
# `{running, locations}` and the `sync_add_location`/`sync_remove_location` commands
# (`FaunaMacApp.swift`, over the same `LocationsModel` the `folder-location-*` UI
# writes). The R2 (account-data-plane.md § The ratified decisions) agent-identity defect this test's ordering exposes on tui was
# already fixed fleet-wide, so macOS inherited the fix, never the bug.
@pytest.mark.macos
@pytest.mark.real_sync_agent
@pytest.mark.isolated_sync_agent
@pytest.mark.feature("media")
def test_media_delete_removes_the_file_from_disk(bound_location_media_app):
    """A UI delete must remove the file from the **disk**, not just the explorer
    (``docs/goal/behavior/file-sync.md`` § Files Appear Automatically — *"the disk,
    not the device id, decides whether [a delete] still has work to do"*).

    **Why this test exists — it is the exact test whose absence let the bug ship.**
    A user deleted a file from the Media page on 2026-07-16 and it vanished from the
    explorer while the bytes stayed on disk forever: the nest tombstoned the path,
    the tombstone-excluding ``fauna.media.list`` stopped listing it, and the local
    file remained — a three-way divergence the user could neither see nor undo
    (fixed, shared engine → every app). The sibling
    ``test_media_delete_removes_the_item`` passed **honestly** throughout: it rides
    ``seeded_media_app``, which fabricates manifests, uploads no bytes and maps no
    folder, so no engine runs and it structurally *cannot* observe the disk. It
    asserts the one layer that was already correct. This test is the other half, and
    it needs a real bound folder + a live engine to exist at all.

    The mechanism it guards: ``changes.list`` is fetched with ``device_id = None``, so
    a tombstone echoes back to the device that recorded it. Two actors record a delete
    under **one** device id and only one has touched the disk — the engine's
    ``handle_delete`` removes the file *before* recording (its echo finds nothing),
    while this UI delete records the same self-echo having never touched the disk. On
    Linux both share one id (``crate::sync::device_id()``), so the delete arm skipping
    every self-echo stranded the file permanently: ``set_anchor`` advances past the
    tombstone whether or not it was applied, so a declined delete never returns.

    Red probe (a green that never went red proves nothing): restore the ``is_self_echo``
    skip in ``libs/fauna-sync-engine/src/engine.rs``'s delete arm, rebuild, rerun — the
    disk assertion below must fail while every other media test stays green.
    """
    app, doomed, folder = bound_location_media_app

    # The fixture already parked a real engine-uploaded file on disk. Navigating now
    # (not before the upload) is what makes the count settle: the page refreshes off
    # WS-RPC on becoming visible and never re-polls. No set_filter — this actor is
    # dedicated and owns exactly one set, so the all-media default already shows only
    # this file, and `media-folder-filter` lists only sets that HAVE media (filtering
    # too early 404s into a LookupError that reads like a product bug).
    app.media.navigate()
    app.media.ensure_loaded()
    count = app.media.wait_for_item_count(1, timeout=30)
    assert count == 1, (
        f"precondition: the engine-uploaded file in {folder!r} should be listed, got "
        f"{count} items; error={app.error_text()!r}"
    )
    assert doomed.exists(), (
        f"precondition: {doomed} must be on disk before the delete — otherwise the "
        f"assertion below is vacuous"
    )

    app.media.open_item_detail(0)
    app.media.delete_open_item()

    # The explorer half (what the pre-fix code already got right).
    after = app.media.wait_for_item_count(0, timeout=30)
    assert after == 0, (
        f"the deleted file should leave the explorer, still listing {after} items; "
        f"error={app.error_text()!r}"
    )

    # The disk half — the whole point. The engine must apply its own UI's tombstone.
    assert _wait_for_gone(doomed), (
        f"THE BUG: {doomed} is STILL ON DISK after a UI delete that removed it from "
        f"the explorer and tombstoned it on the nest — the three-way divergence "
        f"file-sync.md § Files Appear Automatically forbids, and it is permanent: the "
        f"pull anchor has advanced past the tombstone, so the delete never comes back. "
        f"The engine skipped the self-echoed tombstone instead of letting the disk "
        f"decide (engine.rs delete arm; fixed)."
    )
    assert not app.has_error(), f"unexpected page error after delete: {app.error_text()!r}"


@pytest.mark.tui
@pytest.mark.feature("media")
def test_upload_into_a_freshly_created_empty_folder(empty_media_app, tmp_path):
    """The **first-use journey**: create a folder in Settings → Folders, then
    upload into it from Media — the exact path a live user walked on tui on
    2026-08-03 and found impossible.

    The defect (all 7 apps, shared-Rust root): the Media filter options AND the
    upload-target default were both derived from the media *items*
    (``MediaSnapshot::folders`` — "so the filter only ever lists sets that
    actually have media", its own doc comment). A brand-new set has no items, so
    it appeared in neither — and the one action that would have given it media
    was the action it blocked. The options now come from the control plane
    (``fauna.folders.list``), which knows a set exists before it holds
    anything (``docs/goal/ui/media.md`` § Layout & flow → *Where the offerable
    sets come from*).

    **This test deliberately does NOT select the filter before uploading, and
    that is the whole design.** The first version of it did, and passed against
    the un-fixed code — because ``driver.select`` writes the value straight
    through to the machine without checking it against the options the client
    actually rendered (tui: ``SelectTarget::MediaFolderFilter`` → ``SetFilter``,
    no membership test). Selecting an option the UI never offered is not
    something a user can do, so the assertion proved nothing. Driving the
    **default** target instead — the fresh set is this actor's only set, so
    "the set the upload defaults to" is exactly the thing that was broken —
    goes red without the fix and needs no unreachable input. (Convention 8: test
    the way a user would; the general lesson is captured for the harness.)

    ``@pytest.mark.tui`` — tui is the lead app and where the defect was found;
    the other apps ride the trickle-down (this test is app-agnostic, so each
    adds its mark as it verifies).
    """
    app = empty_media_app
    fresh_set = f"empty-target-{int(time.time())}"

    # 1. Create the set through the real Settings → Folders wizard. It holds
    #    nothing, and it is this actor's only set — that is the whole point.
    app.backups.navigate_folders()
    app.backups.create_folder_via_wizard(fresh_set)

    # 2. Media, on its all-media default view — no filter touched.
    app.media.navigate()
    app.media.ensure_loaded()
    assert app.media.item_count() == 0, (
        f"a fresh actor's Media starts empty; got {app.media.item_count()}"
    )

    # 3. Upload. With no filter selected the shared gesture must default the
    #    target to the one set that exists — which, before the fix, it could not
    #    see at all (it reported `media.error_no_set` and uploaded nothing). The
    #    device self-heals its write registration, so no explicit registration
    #    step is needed (see `test_media_upload_into_selected_set`).
    picked = tmp_path / "first.png"
    picked.write_bytes(FIXTURE_IMAGE.read_bytes())
    app.media.upload_file(str(picked))

    assert app.media.wait_for_item_count(1) == 1, (
        "upload with only an EMPTY folder available should default into it and "
        f"produce one item; error={app.error_text()!r}"
    )
    assert not app.has_error(), (
        f"upload into an empty set must not error: {app.error_text()!r}"
    )
    assert app.media.item_name(0) == "first.png", (
        f"the uploaded file should be the item, got {app.media.item_name(0)!r}"
    )

    # 4. And the set is a real filter target: scoping to it still shows the file.
    app.media.set_filter(fresh_set)
    assert app.media.wait_for_item_count(1) == 1, (
        f"scoping the browse to {fresh_set} should show its one file; "
        f"error={app.error_text()!r}"
    )


@pytest.mark.tui
@pytest.mark.feature("media")
def test_upload_into_a_metadata_only_folder_is_refused_with_its_reason(
    empty_media_app, tmp_path
):
    """The Media upload door keeps no copy, so a **metadata-only** folder (its
    content stays on the user's devices) is refused with a reason on
    ``error-message`` and nothing lands (``docs/goal/behavior/file-sync.md``
    § Relay serving → *A write door that keeps no body refuses a metadata-only
    folder*; ``docs/goal/ui/media.md`` § User actions, the ``upload-button``
    row). Without the refusal the bytes rest on the nest against the folder's
    promise.

    Every mutation rides the UI (convention 8): the folder is created through
    the Settings → Folders wizard and flipped metadata-only through the
    residency control and its confirm. It is this actor's only folder, so the
    upload defaults into it with no filter touched — which also shows the folder
    stays an upload target (a refusal with a reason, never a missing option).
    """
    app = empty_media_app
    name = f"local-only-{int(time.time())}"

    b = app.backups
    b.navigate_folders()
    b.create_folder_via_wizard(name)
    b.find_and_expand_folder(name)
    app.driver.wait_for("folder-nest-residency-select", timeout=15.0)
    b.set_residency("metadata_only")
    wait_until(
        b.residency_confirm_visible,
        15.0,
        diagnose=lambda: f"residency confirm not armed; error={app.error_text()!r}",
    )
    b.confirm_residency()
    wait_until(
        lambda: b.residency_current_value() == "metadata_only",
        15.0,
        diagnose=lambda: f"residency never read metadata_only; error={app.error_text()!r}",
    )

    app.media.navigate()
    app.media.ensure_loaded()
    picked = tmp_path / "refused.png"
    picked.write_bytes(FIXTURE_IMAGE.read_bytes())
    app.media.upload_file(str(picked))

    assert app.media.wait_for_error(), (
        "upload into a metadata-only folder must surface a refusal, got no error "
        f"(item_count={app.media.item_count()})"
    )
    assert app.error_text() == S.media.error_metadata_only_folder, (
        f"expected the metadata-only refusal, got {app.error_text()!r}"
    )
    assert app.media.item_count() == 0, (
        "the refused upload must land nothing, got "
        f"{app.media.item_count()} item(s)"
    )


@pytest.mark.feature("media")
def test_media_page_shows_a_file_recorded_while_it_is_open(live_media_app):
    """A file that lands in a synced set while the user is **watching Media**
    appears there, with no navigation and no manual reload.

    This is the Media twin of the Events staleness a live user reported and the
    quick-appearance work fixed (``docs/goal/ui/events.md`` § Implementation
    status today). Media had no live-refresh mechanism on any app: its only
    trigger was page-becomes-visible (linux ``content.connect_map``, tui
    ``media::nav_enter_op`` on tab-enter — whose own comment names linux's), so
    a file uploaded by the local sync engine, a second device, or a collaborator
    stayed invisible until the user navigated away and back.

    The rail it should ride already exists and is already universal: the nest
    fires ``PushEvent::SyncChanged`` (``fauna.sync.changed``) at **every**
    connected participant of the set — for an owner-only set, at the owner's own
    connected devices, the originating one included
    (``bins/fauna-nest/src/sync_handlers.rs::notify_sync_changed``) — and every
    app already handles that push for the sync engine's pull and the Folders
    page's device-activity render (``docs/goal/behavior/file-sync.md``
    § Remote-change nudge; § Implementation status today). The Media page was
    simply the one user-facing surface over set *contents* the nudge never
    reached. Push is the latency path; the periodic reconcile stays the
    correctness backstop, so this asserts within ``PUSH_REFRESH_S``.

    **The page stays MOUNTED throughout — no ``navigate()``, no ``reenter()``,
    no reload.** That is the whole assertion: a client with no push arm wired to
    the media machine never converges here, at any timeout.
    """
    from helpers.budgets import PUSH_REFRESH_S
    from helpers.waiting import wait_until

    app, _set_name, record_file = live_media_app

    app.media.navigate()
    app.media.ensure_loaded()
    assert app.media.item_count() == 0, (
        f"a fresh actor's Media starts empty; got {app.media.item_count()} "
        f"({app.media.item_names()})"
    )

    record_file("arrived-while-watching.jpg")
    wait_until(
        lambda: app.media.item_count() == 1,
        PUSH_REFRESH_S,
        diagnose=lambda: (
            f"item_count={app.media.item_count()} names={app.media.item_names()} "
            f"error={app.error_text() if app.has_error() else '(none)'}"
        ),
    )
    assert "arrived-while-watching.jpg" in " ".join(app.media.item_names()), (
        f"the recorded file should be the one that appeared; got "
        f"{app.media.item_names()}"
    )

    # A SECOND arrival converges too — the arm must re-fire, not latch once.
    record_file("second-arrival.png")
    wait_until(
        lambda: app.media.item_count() == 2,
        PUSH_REFRESH_S,
        diagnose=lambda: (
            f"item_count={app.media.item_count()} names={app.media.item_names()} "
            f"error={app.error_text() if app.has_error() else '(none)'}"
        ),
    )
    assert not app.has_error(), (
        f"live media refresh surfaced an unexpected error: {app.error_text()!r}"
    )
