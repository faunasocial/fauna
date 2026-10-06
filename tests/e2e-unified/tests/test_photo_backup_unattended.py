"""tier_3 — once backup is on, new photos go up BY THEMSELVES.

Target state: `docs/goal/ui/folders.md` § Photo backup (apple + android) — the
photo library is a backup-type set on **continuous ingress**, and its § *What
drives the ingress* names the per-platform triggers. This file is the witness for
`docs/features/photo-backup.md` outcome 3: *"Once backup is on, new photos go up
by themselves — you never have to open the app or press anything."*

**The outcome that most needs a witness, because its failure is silent and has
happened before — twice, on the sibling app.** `folders.md` records both: android's
`photo-backup-enable-toggle` persisted an `autoPhotoBackup` flag *nothing
automatic ever read*, so backup was manual-only for months behind a green toggle;
and a second, independent silent no-op (a never-minted `deviceId`) made every
pass upload nothing on top of it. Neither showed a user anything except photos
quietly not arriving. The existing journey cannot catch that class at all — it
presses `photo-backup-sync-now-button`, so it witnesses the manual path and would
stay green through exactly the bug this outcome exists to name.

**And apple had its own instance of it, found while writing this file
(2026-09-20).** `PhotoBackupEngine.startObserving()` registered a
`PhotoLibraryObserver(engine: self)` built inline at the call site, and
`PHPhotoLibrary.register(_:)` keeps only a WEAK reference to its observer — so the
observer was deallocated as `startObserving()` returned and
`photoLibraryDidChange` never fired again. A green toggle over an inert path, the
android shape exactly. `test_a_new_photo_goes_up_with_nothing_pressed` below is
what catches it; the fix is the engine's `libraryObserver` property.

**Two production triggers, two tests.** iOS reaches this outcome by two different
paths and a witness for one says nothing about the other:

  - **In the foreground** — PhotoKit's change notification, i.e. the retained
    observer above.
  - **In the background** — the `social.fauna.sync.upload` `BGProcessingTask`
    (`folders.md:334`). iOS grants no period control, so no test can wait for it
    and `BGProcessingTask` has no public initializer either; it is driven through
    `fauna_e2e_agent::PHOTO_BACKUP_SCHEDULED_PASS_NOW`, convention 14's `run_now`
    poke, which calls the *production* handler body
    (`BackgroundScheduler.runScheduledUploadPass`) and not a test-only shortcut
    around it.

**Why this module is iOS-only.** macOS instantiates the same engine against the
HOST Photos library, which is machine-global state an ordinary e2e launch must not
touch (convention 10 — and `FaunaMacApp.swift`'s own funnel comment says so). The
macOS column runs the first test below from
`tests/real_session/test_photo_backup_macos.py`, the opt-in venue against the dev
VM's own disposable library (convention 12's macOS arm). The second test never
speaks for macOS: it witnesses iOS's `BGProcessingTask`, and macOS has no
scheduler (`folders.md` § Photo backup), which is why the macos mark lives on that
module's one imported test and never on this module.

**Nothing is pressed after the enable.** That is the whole assertion, so the tests
touch no `photo-backup-*` control past it: the barriers are the funnel's pass
counters (a `get_state` field read, convention 11) and Media's own row list.
"""

from __future__ import annotations

import pytest

from helpers import budgets
from helpers.photo_backup import (
    a_real_png,
    a_tag,
    funnel,
    await_pass_after,
    await_photo_in_media,
    await_quiet,
    enable_photo_backup,
    granted,
    passes,
    prepare_photos_access,
)
from helpers.waiting import describe_photo_backup_funnel

pytestmark = [
    pytest.mark.tier_3,
    # iOS only in THIS module, and by design rather than by lag — see the module
    # docstring's "Why this module is iOS-only" (macOS runs it from
    # `tests/real_session/test_photo_backup_macos.py`). android's identical ingress (`PhotoBackupEngine.kt`,
    # MediaStore) is equally built and equally unwitnessed, but android e2e runs
    # on no development machine today (the emulator lives on the VM host —
    # `feature-catalog.md` § Columns today), so its column stays honestly `(none)`
    # until that is wired; it is a separate, already-tracked gap, not this file's.
    pytest.mark.ios,
]


def _backup_on_with_one_photo(app, tmp_path) -> None:
    """Get to the state both tests start from: Photos granted, one photo already
    backed up, and no pass in flight.

    Enabling is itself a trigger — `requestPhotoAccess()`'s grant branch calls
    `startObserving()` and then `syncNewPhotos()` directly — so this leaves a
    completed first pass behind, and `await_quiet` is the latency-independent
    statement that it is over (convention 14). Both tests need that: a pass
    already running would make the pass they are about to attribute
    unattributable.
    """
    prepare_photos_access(app)
    first = a_tag()
    app.driver.add_photo_to_library(str(a_real_png(tmp_path, tag=first)))
    enable_photo_backup(app)
    granted(app)
    await_quiet(app.driver)


@pytest.mark.feature("photo-backup")
def test_a_new_photo_goes_up_with_nothing_pressed(logged_in_app, tmp_path):
    """A photo appears in the library while backup is already on, and reaches the
    nest with no control touched.

    This is the outcome's own sentence, driven literally. The photo is added
    AFTER backup is on and after the enable's own pass has finished, so no pass
    that could have carried it was in flight when it appeared: the only thing
    that can pick it up is the app's continuous ingress.
    """
    app = logged_in_app
    driver = app.driver
    _backup_on_with_one_photo(app, tmp_path)

    # The baseline the attribution rests on. Read BEFORE the library changes.
    _, completed_before = passes(driver)

    # "A photo taken on the phone" — the fixture stand-in for the camera, and the
    # only step outside the app (the existing journey's docstring explains why
    # that is setup rather than a convention-8 breach).
    tag = a_tag()
    driver.add_photo_to_library(str(a_real_png(tmp_path, tag=tag)))

    # ⚠ NOTHING IS PRESSED FROM HERE ON. Every call below is a read.
    await_pass_after(
        driver, completed_before,
        what="a photo appeared in the library with no control touched",
        timeout=budgets.MAIL_AGENT_WRITEBACK_S,
    )
    await_photo_in_media(
        app, tag,
        what="an unattended pass (nothing was pressed after the enable)",
    )


@pytest.mark.feature("photo-backup")
def test_the_scheduled_background_pass_runs_the_real_ingest(logged_in_app, tmp_path):
    """The `social.fauna.sync.upload` slice really drives a backup pass over the
    real library — the background half of "you never have to open the app".

    **What this asserts, and what it deliberately does not.** It drives the
    production handler body and asserts the pass that comes out is a real pass
    over the real library: it saw the assets PhotoKit holds and correctly
    recognised them as already backed up. It does NOT assert that this particular
    pass uploaded a new photo, and that is a considered choice rather than a gap:
    with the foreground observer live (the other test's subject) a freshly-added
    photo is raced by the observer's own pass, so an upload could not be
    attributed to the poke. The upload path is witnessed by the test above; what
    only this test can witness is that the scheduled entry point is WIRED — the
    precise thing android's `autoPhotoBackup` flag was not, and the failure that
    a registered-but-inert `BGProcessingTask` would otherwise hide for ever.
    """
    app = logged_in_app
    driver = app.driver
    _backup_on_with_one_photo(app, tmp_path)

    before = passes(driver)
    funnel_before = describe_photo_backup_funnel(driver)
    assets_seen_before = funnel(driver)["assets_seen"]
    assert assets_seen_before > 0, (
        "the setup pass saw no assets at all, so this test would assert nothing "
        f"about the scheduled pass: {funnel_before}"
    )

    # Convention 14's run_now poke. Fire-and-forget by contract — the barrier is
    # the pass counter below, never this ack.
    driver.call_command("photo_backup_scheduled_pass_now")

    await_pass_after(
        driver, before[1],
        what="the photo_backup_scheduled_pass_now poke (the production "
             "BGProcessingTask body)",
    )

    after = funnel(driver)
    assert after["assets_seen"] == assets_seen_before, (
        "the scheduled pass did not read the same photo library the foreground "
        f"pass did (saw {after['assets_seen']}, expected {assets_seen_before}) — "
        "so the handler is wired to something other than the real ingress. "
        f"{describe_photo_backup_funnel(driver)}"
    )
    assert after["already_synced"] == assets_seen_before, (
        "the scheduled pass re-uploaded photos it should have recognised as "
        "already backed up, so it is not sharing the foreground path's record "
        f"store. {describe_photo_backup_funnel(driver)}"
    )
