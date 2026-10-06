"""tier_3 — the version-retention pipeline's recovery browse, end-to-end
through the client UI (``docs/goal/behavior/file-versions.md`` § Retention (3);
element IDs licensed by the § Retention (5)(b) approval, 2026-08-17).

The pipeline under test is the REAL one — no shortcut around any layer:

1. Three uploads of one member path record three versions (every recorded
   change IS a version).
2. The owner bounds the set's version history to ``count=1`` through the § 8b
   editor's ``folder-version-retention-count`` knob (mutations ride the UI,
   convention 8).
3. ``POST /api/v1/test/version_prune/evaluate`` runs the SAME per-folder
   ``schedule_version_auto_prune`` sweep the GC cycle runs (the hook exists
   because that cycle is 6-hourly), which schedules one 7-day cancellable
   ``VersionBulkPrune`` marking exactly v1 — the head is structurally excluded
   and ``VERSION_HARD_FLOOR = 2`` keeps the newest prior version.
4. ``POST /api/v1/test/pending_actions/run_due`` makes it due and runs the real
   executor, which SOFT-prunes (30-day ``purge_after``) — v1 leaves the live
   listing but stays recoverable.
5. The recovery browse: ``file-version-show-pruned-toggle`` re-lists with
   ``include_pruned`` (v1 reappears wearing ``file-version-pruned-badge``),
   and ``file-version-undelete-button`` recovers it; the live listing then
   carries all three versions again — read back through the nest's own
   projection, which only ever lists live rows on the default browse.

⚠ ``run_due`` makes EVERY queued pending action due on the shared session
nest, not only this test's ``VersionBulkPrune`` — the same blast radius every
existing consumer of that hook accepts. A later test asserting an action of
its own is *still pending* would be order-coupled to this one; none does
today (the delete-account/atproto consumers run on dedicated nests).
"""

import pytest

pytestmark = [
    pytest.mark.tier2,
    pytest.mark.tier_3,
    # tui is the lead app (rust-first ordering); the other six join as their
    # recovery-browse legs land in the batched trickle-down.
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.windows,  # 2026-09-09, row 323 — first green run of the whole pipeline
    # apple joins in its own catalog trickle-down pass: the shared FaunaKit
    # `MediaItemDetailView` carries the `file-version-show-pruned-toggle`, the
    # `file-version-pruned-badge` and the undelete action over
    # `MediaMachineVM.fileVersions(includePruned:)`/`undeleteVersion`, and the
    # retention-count setup control is the shared one in `FoldersContent`.
    pytest.mark.macos,
    pytest.mark.ios,
]


@pytest.mark.feature("media")
def test_version_prune_soft_prunes_and_the_ui_recovers(
    seeded_media_app, nest_instance, tmp_path
):
    from conftest import _bridge_admin_post, live_admin_token

    app, plan = seeded_media_app
    target_set, seeded_paths = next(
        ((name, paths) for name, paths in plan.items() if len(paths) == 1),
        (None, None),
    )
    assert target_set is not None, f"fixture should seed a single-folder; plan={plan!r}"
    seeded = len(seeded_paths)

    # ── 1. Three versions of one member path ────────────────────────────────
    app.media.navigate()
    app.media.set_filter(target_set)
    assert app.media.wait_for_item_count(seeded) == seeded, (
        f"seeded set {target_set!r} should settle at {seeded}; error={app.error_text()!r}"
    )

    picked = tmp_path / "prunable.bin"
    picked.write_bytes(b"1" * 1_200)
    app.media.upload_file(str(picked))
    count = app.media.wait_for_item_count(seeded + 1)
    assert count == seeded + 1, (
        f"v1 upload should add one item, got {count}; error={app.error_text()!r}"
    )
    idx = app.media.item_names().index("prunable.bin")
    size_v1 = app.media.item_size(idx)

    picked.write_bytes(b"2" * 48_000)
    app.media.upload_file(str(picked))
    size_v2 = app.media.wait_for_item_size_change(idx, size_v1)
    assert size_v2 != size_v1, f"v2 never landed; error={app.error_text()!r}"

    picked.write_bytes(b"3" * 2_400)
    app.media.upload_file(str(picked))
    size_v3 = app.media.wait_for_item_size_change(idx, size_v2)
    assert size_v3 not in (size_v1, size_v2), (
        f"v3 never landed; error={app.error_text()!r}"
    )

    # ── 2. Bound the history through the § 8b editor (UI, convention 8) ─────
    b = app.backups
    b.navigate_folders()
    b.find_and_expand_folder(target_set)
    b.set_version_retention(count="1")
    b.save_nest_place()

    # ── 3+4. The real pipeline, fast-forwarded (never shortcut) ─────────────
    token = live_admin_token(nest_instance)
    assert token, "the session nest is claimed; an admin bearer must mint"
    ran = _bridge_admin_post(
        nest_instance["url"], token, "/api/v1/test/version_prune/evaluate", {}
    )
    assert ran.get("scheduled", 0) >= 1, (
        f"the evaluation sweep should schedule this folder's prune: {ran}"
    )
    ran = _bridge_admin_post(
        nest_instance["url"], token, "/api/v1/test/pending_actions/run_due", {}
    )
    assert ran.get("executed", 0) >= 1, f"the VersionBulkPrune never executed: {ran}"

    # ── 5. The live listing lost v1; the recovery browse finds + recovers it ─
    app.media.navigate()
    app.media.set_filter(target_set)
    app.media.wait_for_item_count(seeded + 1)
    idx = app.media.item_names().index("prunable.bin")
    app.media.open_item_detail(idx)
    versions = app.media.wait_for_version_count(2)
    assert versions == 2, (
        f"the live listing should hold head + the floor-kept prior (v1 soft-"
        f"pruned), got {versions}; error={app.error_text()!r}"
    )
    assert not app.media.has_version_pruned_badge(0), (
        "a live-only listing never renders the pruned badge"
    )

    app.media.toggle_show_pruned()
    versions = app.media.wait_for_version_count(3)
    assert versions == 3, (
        f"the include_pruned browse should list all three, got {versions}; "
        f"error={app.error_text()!r}"
    )
    badges = [app.media.has_version_pruned_badge(i) for i in range(3)]
    assert badges == [True, False, False], (
        f"exactly the oldest version is soft-pruned, got {badges}"
    )
    assert app.media.version_size(0) == size_v1, (
        "the pruned row is v1 — its size says so"
    )

    app.media.undelete_version(0)
    # The re-list keeps the browse's toggle; recovery shows as the badge
    # leaving row 0 (deadline poll on latency-independent state).
    import time

    deadline = time.monotonic() + 15.0
    while app.media.has_version_pruned_badge(0) and time.monotonic() < deadline:
        time.sleep(0.3)
    assert not app.media.has_version_pruned_badge(0), (
        f"the undelete should return v1 to the live population; "
        f"error={app.error_text()!r}"
    )
    assert not app.has_error(), f"unexpected page error: {app.error_text()!r}"

    # Ground truth through the nest's own live projection: the DEFAULT browse
    # (toggle back OFF) now lists all three versions again.
    app.media.toggle_show_pruned()
    versions = app.media.wait_for_version_count(3)
    assert versions == 3, (
        f"after undelete the live listing should carry all three versions, "
        f"got {versions}; error={app.error_text()!r}"
    )
    assert app.media.version_size(0) == size_v1, "v1 is live again, size intact"
