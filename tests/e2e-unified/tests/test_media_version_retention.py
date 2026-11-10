"""tier_3 — the version-retention prune's cancellable window, end to end through
the client UI (``docs/goal/behavior/file-versions.md`` § Retention (2), (3)).

The floor and the head arms are witnessed by
``test_version_prune_recovery.py::test_version_prune_soft_prunes_and_the_ui_recovers``
(count=1 still keeps the head and the prior version; only v1 is marked). This
module owns the remaining arm: the evaluator schedules ONE ``VersionBulkPrune``
pending action per folder with a **7-day** ``execute_after``; the folder's owner
sees it in the standing pending-actions section (``ui/settings.md`` § Pending
actions) and cancels it with one click; a cancelled prune never executes, so
every version stays listed.

Pipeline, never shortcut: three uploads through Media record three versions;
the § 8b editor bounds the history to ``count=1`` (UI, convention 8);
``POST /api/v1/test/version_prune/evaluate`` runs the GC cycle's own sweep;
the cancel rides ``pending-action-cancel-button``; then
``POST /api/v1/test/pending_actions/run_due`` makes every queued action due —
the cancelled one must not run. The 7-day window is read off the nest's own
``fauna.pending_actions.list`` projection (convention 8's verification
carve-out), and the row's ``pending-action-execute-after`` must name that day.

⚠ ``run_due`` makes EVERY queued pending action on the shared session nest due
— the blast radius ``test_version_prune_recovery.py`` documents and accepts.
The cancel lands BEFORE it, so this test's own assertion never races it.
"""

import datetime

import pytest

from helpers.budgets import RPC_ROUNDTRIP_S
from i18n.strings import S
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier2,
    pytest.mark.tier_3,
    # tui is the lead app; the other six join in the batched trickle-down —
    # linux, web, android, macOS and iOS name the pruned folder since the
    # describer lift (`ui/settings.md` § Pending actions); windows follows.
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.android,
    pytest.mark.macos,
    pytest.mark.ios,
]

SEVEN_DAYS_S = 7 * 24 * 3600


@pytest.mark.feature("media")
def test_a_scheduled_version_prune_waits_seven_days_and_cancelling_keeps_every_version(
    seeded_media_app_and_user, nest_instance, tmp_path
):
    from common.auth import _user_call
    from conftest import _bridge_admin_post, live_admin_token

    app, plan, user = seeded_media_app_and_user
    secret_key = user["signing_key"].encode().hex()
    d = app.driver

    def version_prunes() -> list[dict]:
        reply = _user_call(
            nest_instance["port"], secret_key, "fauna.pending_actions.list", {},
            nest_instance["url"],
        )
        return [
            a for a in reply.get("actions", [])
            if a.get("action_type") == "version.bulk_prune"
        ]

    target_set, seeded_paths = next(
        ((name, paths) for name, paths in plan.items() if len(paths) == 1),
        (None, None),
    )
    assert target_set is not None, f"fixture should seed a single-folder; plan={plan!r}"
    seeded = len(seeded_paths)

    # ── 1. Three versions of one member path, through Media ────────────────
    app.media.navigate()
    app.media.set_filter(target_set)
    assert app.media.wait_for_item_count(seeded) == seeded, (
        f"seeded set {target_set!r} should settle at {seeded}; error={app.error_text()!r}"
    )
    picked = tmp_path / "keepme.bin"
    picked.write_bytes(b"1" * 1_200)
    app.media.upload_file(str(picked))
    assert app.media.wait_for_item_count(seeded + 1) == seeded + 1, (
        f"v1 upload should add one item; error={app.error_text()!r}"
    )
    idx = app.media.item_names().index("keepme.bin")
    size = app.media.item_size(idx)
    for body in (b"2" * 48_000, b"3" * 2_400):
        picked.write_bytes(body)
        app.media.upload_file(str(picked))
        new_size = app.media.wait_for_item_size_change(idx, size)
        assert new_size != size, f"a version never landed; error={app.error_text()!r}"
        size = new_size

    # ── 2. Bound the history through the § 8b editor (UI, convention 8) ─────
    b = app.backups
    b.navigate_folders()
    b.find_and_expand_folder(target_set)
    b.set_version_retention(count="1")
    b.save_nest_place()

    # ── 3. The evaluator schedules one prune, seven days out ────────────────
    assert version_prunes() == [], "precondition: this fresh actor has no prune queued"
    token = live_admin_token(nest_instance)
    assert token, "the session nest is claimed; an admin bearer must mint"
    ran = _bridge_admin_post(
        nest_instance["url"], token, "/api/v1/test/version_prune/evaluate", {}
    )
    assert ran.get("scheduled", 0) >= 1, (
        f"the evaluation sweep should schedule this folder's prune: {ran}"
    )
    prunes = version_prunes()
    assert len(prunes) == 1, f"one prune per folder, got {prunes!r}"
    prune = prunes[0]
    assert prune["status"] == "pending", f"the prune waits: {prune!r}"
    assert prune["execute_after"] - prune["created_at"] == SEVEN_DAYS_S, (
        f"the prune's window is seven days, got "
        f"{prune['execute_after'] - prune['created_at']} s: {prune!r}"
    )

    # ── 4. The owner sees it and cancels it with one click ──────────────────
    app.settings._navigate_subpage("account")
    d.wait_for("pending-actions-section", timeout=RPC_ROUNDTRIP_S)

    # The row reads what will happen, naming the folder by the name the owner
    # gave it — never the wire kind or the folder's id (`ui/settings.md`
    # § Pending actions); it is found by that sentence, and its day confirms
    # the seven-day window the nest stored.
    sentence = S.settings.pending_actions.prune_versions_in(folder=target_set)
    want_day = datetime.datetime.fromtimestamp(prune["execute_after"]).strftime("%Y-%m-%d")

    def descriptions() -> list[str]:
        return [
            d.get_text("pending-action-description", index=i) or ""
            for i in range(d.count("pending-action-item"))
        ]

    def prune_row_index() -> int:
        for i, text in enumerate(descriptions()):
            if text == sentence:
                return i + 1  # 1-based so index 0 is truthy for wait_until
        return 0

    index = wait_until(
        prune_row_index,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"a pending-action row should read {sentence!r}; "
            f"section text: {d.get_text('pending-actions-section')!r}; "
            f"descriptions: {descriptions()!r}"
        ),
    ) - 1
    shown = descriptions()
    assert not any("bulk_prune" in t for t in shown), (
        f"no row may paint the raw wire kind: {shown!r}"
    )
    assert want_day in (d.get_text("pending-action-execute-after", index=index) or ""), (
        f"the prune's row applies on {want_day}: "
        f"{d.get_text('pending-action-execute-after', index=index)!r}"
    )
    rows_before = d.count("pending-action-item")
    d.click("pending-action-cancel-button", index=index)
    wait_until(
        lambda: d.count("pending-action-item") == rows_before - 1,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"section text: {d.get_text('pending-actions-section')!r}",
    )
    assert [p["status"] for p in version_prunes()] == ["cancelled"], (
        f"the nest holds the prune cancelled: {version_prunes()!r}"
    )

    # ── 5. A cancelled prune never runs: every version stays listed ─────────
    _bridge_admin_post(
        nest_instance["url"], token, "/api/v1/test/pending_actions/run_due", {}
    )
    assert [p["status"] for p in version_prunes()] == ["cancelled"], (
        f"run_due must not execute a cancelled prune: {version_prunes()!r}"
    )
    app.media.navigate()
    app.media.set_filter(target_set)
    app.media.wait_for_item_count(seeded + 1)
    idx = app.media.item_names().index("keepme.bin")
    app.media.open_item_detail(idx)
    versions = app.media.wait_for_version_count(3)
    assert versions == 3, (
        f"after the cancel every version stays live, got {versions}; "
        f"error={app.error_text()!r}"
    )
    assert not any(app.media.has_version_pruned_badge(i) for i in range(3)), (
        "no version was pruned"
    )
    assert not app.has_error(), f"unexpected page error: {app.error_text()!r}"
