"""tier_3 — what the phone shows while a backup pass runs, and making one run now.

Target state: `docs/goal/ui/folders.md` § Photo backup (apple + android), whose
`photo-backup-controls` component owns `photo-backup-sync-progress` and
`photo-backup-sync-now-button` (`folders.md:322`). This file is the witness for
`docs/features/photo-backup.md`:

  - **outcome 5** — *"While a backup pass runs, the phone shows how far it has
    got."*
  - **outcome 6** — *"You can make the backup run now instead of waiting for the
    next pass."*

**Outcome 5 needed a product fix before it could be witnessed at all
(2026-09-20).** `PhotoBackupEngine.pendingCount` was declared and never written,
so `PhotoBackupControlsView`'s `if engine.pendingCount > 0` row could not render,
and `photo-backup-sync-progress` published one fixed string —
`L.photoBackup.backupInProgress`, "Backup in progress..." — for every pass of
every size. That is a statement that *a* pass is running; the outcome promises
*how far it has got*. Writing a witness against the old surface would have meant
asserting the spinner and calling the outcome covered, which is the
authoring-rigor iron-clad's convenient excuse exactly. The engine now counts the
pass down and the element publishes the position through the catalog's own
`photo_backup.syncing` — "Syncing 3 of 5..." — a string that had no caller on any
app until this landed.

**Outcome 5's window is HELD, never raced (convention 14, "Pre-fetch windows").**
A pass over a couple of small photos is over in well under a second, so polling
for the in-flight readout is a race whose green runs are the ones where the poll
happened to land — the wall-clock dependence the convention forbids outright. The
nest's `rpc-hold` test hook parks one RPC kind's dispatch until the test releases
it, which turns the in-flight window from an interval you race into a **state you
stand in**: the pass's own `changes.record` is held, so `isBackingUp` stays true
for as long as the assertion needs, and `wait_for_held_rpc` proves the app has
actually issued the request before anything is read.

**Outcome 6's problem was attribution, not absence.** The original journey does
click Sync now, but — as its own comment records — enabling backup has already
started a pass by then, so the completion it waits for is not attributable to the
button; the test would pass unchanged with the button wired to nothing. The fix is
the pass **cycle counters** (`fauna_e2e_agent::PHOTO_BACKUP_KEY`): quiesce first
(no pass in flight), read the baseline, click, and require a NEW completed pass.
Nothing touches the photo library between the quiesce and the click, so the
foreground observer has nothing to react to and the button is the only thing that
can have started it.

**Why this module is iOS-only** — macOS runs the same engine against the
machine-global host Photos library, out of reach of an ordinary launch under
convention 10; the macOS column runs both functions from
`tests/real_session/test_photo_backup_macos.py` (convention 12's macOS arm).
"""

from __future__ import annotations

import re

import pytest

from helpers import budgets
from helpers.photo_backup import (
    a_real_png,
    a_tag,
    await_pass_after,
    await_quiet,
    enable_photo_backup,
    funnel,
    granted,
    passes,
    prepare_photos_access,
)
from helpers.rpc_hold import arm_rpc_hold, release_rpc_hold, wait_for_held_rpc
from helpers.waiting import describe_photo_backup_funnel, wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.ios,
]

#: The kind the sealed per-file ingest drives once per file — `host.ingestFile` →
#: `SyncEngine::upload_file` → `record_change`. Holding it parks the pass inside
#: its first file, which is precisely where a progress readout has something to
#: say. Held rather than, say, the resolve call, because the resolve happens once
#: per launch and before the position exists.
INGEST_KIND = "fauna.sync.changes.record"


def _backup_on_with(app, tmp_path, count: int) -> list[str]:
    """Photos granted, `count` photos in the library, backup on and quiesced.

    Returns the per-photo tags. Enabling is itself a trigger, so this leaves one
    completed pass behind and `await_quiet` is the latency-independent statement
    that it is over — the precondition both tests need before they can attribute
    a pass to anything (convention 14).
    """
    prepare_photos_access(app)
    tags = [a_tag() for _ in range(count)]
    for tag in tags:
        app.driver.add_photo_to_library(str(a_real_png(tmp_path, tag=tag)))
    enable_photo_backup(app)
    granted(app)
    await_quiet(app.driver)
    return tags


@pytest.mark.feature("photo-backup")
def test_a_running_pass_shows_how_far_it_has_got(logged_in_app, tmp_path,
                                                 nest_instance):
    """Stand inside a running pass and read the progress the user would read.

    The assertion is that `photo-backup-sync-progress` names a POSITION — how many
    of how many — and not merely that something is happening. The old fixed
    "Backup in progress..." string fails it, which is the point: a witness that
    the spinner appears would have declared this outcome covered while the app
    still told the user nothing about how far it had got.
    """
    app = logged_in_app
    driver = app.driver
    port = nest_instance["port"]

    # Three photos, so the pass has a total worth reporting and a position that is
    # not trivially "1 of 1".
    tags = _backup_on_with(app, tmp_path, 3)
    _, completed_before = passes(driver)

    # ⚠ ARM BEFORE THE LIBRARY CHANGES — the load-bearing ordering. The photos
    # already in the library were backed up by the enable's pass, so a pass over
    # them alone never reaches `changes.record` at all and there would be nothing
    # to hold; the fresh photo below is what gives this pass a file to send. But
    # adding it also wakes the app's own ingress immediately, so arming afterwards
    # races that pass and loses whenever it wins — the same "arm before the
    # process boundary" rule `helpers/rpc_hold` states for a cold-start pin.
    arm_rpc_hold(port, INGEST_KIND)
    try:
        fresh = a_tag()
        driver.add_photo_to_library(str(a_real_png(tmp_path, tag=fresh)))
        tags.append(fresh)

        # Arrival, not a sleep: the app has issued the ingest RPC and the nest is
        # holding it, so the pass is provably parked inside a file it has not
        # finished. Without this the reads below could land before the app had
        # even started the pass and would be measuring its startup speed
        # (convention 14).
        wait_for_held_rpc(
            port, INGEST_KIND,
            diagnose=lambda: (
                "no ingest RPC arrived, so no pass ever reached the fresh photo: "
                f"pass counters {passes(driver)}, "
                f"{describe_photo_backup_funnel(driver)}. A library change with "
                "backup on must start a pass by itself — if that is what failed, "
                "the finding belongs to outcome 3 "
                "(test_photo_backup_unattended.py), not to this readout"
            ),
        )

        # The element renders only while `isBackingUp`, so its mere presence is
        # already "a pass is running" — the position is what this test is for.
        shown = wait_until(
            lambda: (driver.get_text("photo-backup-sync-progress") or None)
            if driver.is_visible("photo-backup-sync-progress") else None,
            budgets.UI_SETTLE_S,
            diagnose=lambda: (
                "a pass is parked inside its ingest RPC, so the app is "
                "provably backing up, yet photo-backup-sync-progress is not "
                "readable: "
                f"visible={driver.is_visible('photo-backup-sync-progress')!r}, "
                f"{driver.diagnose('photo-backup-sync-progress')}, "
                f"{describe_photo_backup_funnel(driver)}"
            ),
        )

        live = funnel(driver)
        total = live["assets_seen"]
        assert total >= len(tags), (
            "the running pass did not see every photo in the library "
            f"(assets_seen={total}, library holds {len(tags)}), so the total it "
            "is reporting is not the library's"
        )
        # The countdown is live: a pass parked on its first file has NOT finished
        # processing every asset, so something must still be pending. This is what
        # a dead `pendingCount` (declared, never written — the bug this file's
        # docstring records) fails on.
        assert live["pending"] > 0, (
            "the pass is parked inside a file it has not finished, yet the app "
            f"reports nothing pending (pending={live['pending']}, "
            f"assets_seen={total}) — the progress countdown is not being written, "
            "so the user is shown a spinner and no position. "
            f"{describe_photo_backup_funnel(driver)}"
        )
        # And the READOUT carries that position. Matched as an ordered PAIR
        # (processed, then total) rather than two substring checks: PhotoKit sorts
        # newest-first, so the fresh photo is the pass's FIRST asset and
        # `processed` is legitimately 0 here — and `"0" in shown` is satisfiable
        # by almost any string, which would leave the whole assertion resting on
        # the total alone. The pair is what distinguishes "N of M" from a phrase
        # that merely happens to contain a digit.
        processed = total - live["pending"]
        assert re.search(rf"\b{processed}\b.*\b{total}\b", shown), (
            "photo-backup-sync-progress does not tell the user how far the pass "
            f"has got — it reads {shown!r}, which does not name a position "
            f"({processed} of {total}). `folders.md` § Photo backup promises a "
            "progress readout, not an indeterminate spinner: this is what the "
            "dead `pendingCount` looked like from outside the app"
        )
    finally:
        # ALWAYS release: an armed kind parks every later request of that kind
        # too, including the ones the tests after this one issue.
        release_rpc_hold(port, INGEST_KIND)

    # The pass must also be able to FINISH once released — a held window that
    # wedges the feature would be a worse bug than the one under test.
    await_pass_after(
        driver, completed_before,
        what="releasing the held ingest RPC",
    )


@pytest.mark.feature("photo-backup")
def test_sync_now_starts_a_pass_that_nothing_else_would_have_started(
    logged_in_app, tmp_path
):
    """Press Sync now on a quiet app and require a NEW completed pass.

    The attribution is the whole test. Nothing touches the photo library between
    the quiesce and the click, so the foreground observer has no change to react
    to, the OS scheduler is not driving anything, and the enable's own pass is
    provably finished (`await_quiet` — Sync now is `.disabled(isSyncing ||
    engine.isBackingUp)`, so enabled IS "no pass in flight"). The only thing left
    that can raise `passes_completed` is the button.
    """
    app = logged_in_app
    driver = app.driver

    tags = _backup_on_with(app, tmp_path, 2)
    before_started, before_completed = passes(driver)

    driver.click("photo-backup-sync-now-button")

    await_pass_after(
        driver, before_completed,
        what="pressing photo-backup-sync-now-button on a quiesced app",
    )
    started_after, _ = passes(driver)
    assert started_after > before_started, (
        "a pass completed after the click but no new pass STARTED, so the "
        f"completion was not the button's ({before_started} -> {started_after}). "
        "Either the counters are not being written at the in-flight edge or the "
        "app counted a pass it never began"
    )

    # And it was a real pass over the real library, not an early return: every
    # photo was seen, and correctly recognised as already backed up by the
    # enable's pass.
    after = funnel(driver)
    assert after["assets_seen"] >= len(tags), (
        "the pass the button started did not read the photo library "
        f"(assets_seen={after['assets_seen']}, library holds {len(tags)}) — it "
        f"returned early. {describe_photo_backup_funnel(driver)}"
    )
