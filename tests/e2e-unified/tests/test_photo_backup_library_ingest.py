"""tier_3 — a photo in the phone's photo library reaches the nest and shows in Media.

Target state: `docs/goal/ui/folders.md` § Photo backup (apple + android) — the
photo library is a **backup-type set** whose platform ingress (PhotoKit on apple)
feeds it, and "the unified **Media** explorer shows the files inside that set
alongside every other set". This file is the witness for
`docs/features/photo-backup.md` outcome 1, the page's only outcome and — until
this went green, 2026-09-02 — a literal `(none)`, one of the five that block
every column at once (`feature-catalog.md` § Implementation status today).

**The feature was built and the witness was missing, which is the dangerous
shape.** Both halves shipped over a year of work — the shared resolver
(`fauna_folders_machine::photo_library`, adopt-or-create over the ordinary
wizard) and apple's ingress (`PhotoBackupEngine`) — and folders.md has recorded
"apple COMPLETE" since 2026-07-13. What nothing proved is the part the user
actually experiences: that a photo sitting in the library ends up readable on
the nest. That gap is exactly how the pre-B3 bug survived (§ Photo backup's own
warning: every ingest uploaded its chunks and then had `changes.record` rejected
into a set that never existed, and the per-item `catch` swallowed it into a
banner) — a silent failure this walk would have caught the day it landed.

**Why this module is iOS-only.** macOS instantiates the same engine against the
host Photos library (`folders.md` § Photo backup: "the always-running app watches
the library directly"), which is machine-global state an ordinary e2e launch must
not touch (convention 10); the iOS simulator's library is per-device and
throwaway. The macOS column runs this very function from
`tests/real_session/test_photo_backup_macos.py`, the opt-in venue against the dev
VM's own disposable library (convention 12's macOS arm).

**tier_3, and it earns the tier.** Real iOS app, real PhotoKit, real shared-Rust
sealed ingest, real nest, real `fauna.sync.changes.record`. Nothing is stubbed:
the photo is a real file put into a real (simulated) photo library, and the
assertion reads the Media explorer the user would read.

**Every mutation goes through the app UI** (convention 8). The photo injection is
fixture setup — the e2e stand-in for pointing a camera at something — and is the
only step outside the app; enabling backup and running the pass are the app's own
`photo-backup-*` controls, and the verdict is read off the Media page.
"""

from __future__ import annotations

import pytest

from actions.media import MediaActions
from helpers import budgets
from helpers.photo_backup import (
    a_real_png,
    a_tag,
    await_quiet,
    enable_photo_backup,
    granted,
    prepare_photos_access,
)
from helpers.waiting import describe_photo_backup_funnel, wait_until

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


# The fixture builder, the grant preamble, the enable step and the grant check
# moved to `helpers/photo_backup.py` when outcomes 3/5/6/7/8 got witnesses of
# their own (2026-09-20): five journeys needing the identical four-step setup is
# exactly the copy-per-test drift priorities #1/#4 refuse, and every comment that
# was here about WHY each step is shaped as it is moved with it.


# GREEN since 2026-09-02, and the citation under `docs/features/photo-backup.md`
# outcome 1 landed in the same commit — the outcome was that page's only one and a
# literal `(none)`, one of the five that block every column at once
# (`feature-catalog.md` § Implementation status today).
#
# **What was actually wrong was the fixture, not the product.** This file spent a
# session landed strict-xfail on the reading that the sealed ingest was dropping
# the file — the pre-B3 failure recurring (`folders.md` § Photo backup's own ⚠)
# on a feature the goal doc has called COMPLETE since 2026-07-13. It was not.
# `xcrun simctl privacy <udid> grant photos <bundle>` exits 0 and writes a
# correct-looking TCC row stamped `auth_version = 1`; the iOS 26.5 runtime's
# `tccd` reads that row, treats generation 1 as a stale consent and prompts
# anyway (`Got 1 auth from db ... flags: 0` followed immediately by
# `AUTHREQ_PROMPTING` in its own log), so `PHPhotoLibrary.authorizationStatus`
# stayed `notDetermined`, `PHAsset.fetchAssets` returned nothing, and the pass
# completed having never had a file to lose. `IosDriver._finish_photos_grant`
# bumps that one column and carries the paired control that proves it.
#
# Two lessons this file now encodes rather than repeats:
#   - a fixture step that can fail invisibly is indistinguishable, from outside
#     the app, from a product bug — so the grant, the `addmedia`, and the
#     authorization are each asserted where they happen, not inferred later;
#   - "the surface rendered" is not "the permission was granted". The status and
#     actions sections are gated on the Toggle's own `@State`, so Sync now
#     appearing proved nothing about PhotoKit — the old code read it as proof.
@pytest.mark.feature("photo-backup")
def test_a_photo_in_the_library_reaches_the_nest_and_shows_in_media(
    logged_in_app, tmp_path
):
    """The whole journey, as a phone user lives it: a photo is in the library,
    backup is on, and the photo turns up on the nest.

    The two halves are both load-bearing and neither is redundant:

      1. The app reports a completed pass (`photo-backup-last-sync-text`
         appears — `lastBackupDate` is set only after `syncNewPhotos()` returns).
      2. The photo is **in Media**, which reads the nest's own file list. Half 1
         alone is satisfied by a pass that uploaded nothing, and that is precisely
         the pre-B3 failure this feature was fixed to end.
    """
    app = logged_in_app
    driver = app.driver

    # Pre-grant Photos + cold-relaunch, so PhotoKit resolves `.authorized` with no
    # SpringBoard prompt for a driver to fail to dismiss.
    prepare_photos_access(app)

    # "A photo taken on the phone" — the fixture stand-in for the camera.
    tag = a_tag()
    photo = a_real_png(tmp_path, tag=tag)
    driver.add_photo_to_library(str(photo))

    # Turn backup on through the app's own control, exactly as a user would, and
    # wait for its enabled half. ⚠ That the surface rendered says NOTHING about
    # PhotoKit (the sections are gated on the Toggle's own `@State`), which is why
    # the grant check below is separate — see `helpers.photo_backup`.
    enable_photo_backup(app)

    # Sync now is `.disabled(isSyncing || engine.isBackingUp)`, and enabling
    # backup starts a pass by itself: `requestPhotoAccess()`'s grant branch calls
    # `startObserving()` and then `syncNewPhotos()` directly. So the button is
    # legitimately disabled the moment the grant lands, and clicking it here
    # without waiting is driving a control no user could have clicked — the
    # bridge refuses it outright (409 "element is disabled"). Waiting for enabled
    # is both the real user path and a latency-independent barrier meaning "no
    # pass is in flight" (convention 14), which is exactly the precondition a
    # second, explicitly-driven pass needs.
    await_quiet(driver)
    driver.click("photo-backup-sync-now-button")

    # ASSERT THE FIXTURE BEFORE BLAMING THE PRODUCT — a pass that ran without the
    # Photos grant is indistinguishable from a broken ingest from out here, and
    # that ambiguity once cost a whole session's diagnosis.
    granted(app)

    # Half 1 — a pass really ran to completion. `photo-backup-last-sync-text`
    # renders only once `lastBackupDate` is set, which `syncNewPhotos()` does on
    # its way out, so the element's presence IS the barrier (convention 14: a
    # latency-independent state, no settle sleep).
    assert wait_until(
        lambda: driver.is_visible("photo-backup-last-sync-text"),
        budgets.MAIL_AGENT_WRITEBACK_S,
        diagnose=lambda: (
            "the backup pass never reported a completion time; progress was "
            f"{driver.is_visible('photo-backup-sync-progress')!r}, "
            f"error={app.error_text()!r}, "
            f"{describe_photo_backup_funnel(driver)}"
        ),
    )

    # Whatever the pass reported, read the app's OWN error while the surface that
    # renders it is still on screen — `PhotoBackupEngine` writes per-item ingest
    # failures to `errorMessage`, and the banner is gone the moment we navigate
    # away. Captured here so the Media assertion below can quote it.
    ingest_error = app.error_text()

    # The resolver ran, and the nest really has the set. This splits the two ways
    # half 2 can fail, which otherwise look identical from Media: no set at all
    # (`resolvePhotoLibrarySet` never created one — the pre-B3 shape) versus a set
    # that exists but received no file (the ingest itself).
    # Re-enter the page first. The folder list is a nav-EDGE read (the same
    # shape `MediaActions.reenter` documents for Media), and we entered Folders
    # BEFORE the pass ran — so the rows on screen are the pre-resolution snapshot,
    # in which a set the pass has just created cannot appear however long we poll.
    def _folder_titles():
        MediaActions(driver).navigate()
        app.backups.navigate_folders()
        return [app.backups.folder_title(i)
                for i in range(app.backups.folder_count())]

    folder_names = wait_until(
        lambda: (_folder_titles() or None),
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            "the folder list stayed empty across a re-entry, so the backup pass "
            "created no set at all despite reporting completion; "
            f"app error={ingest_error!r}"
        ),
    )
    assert any("Photo" in n for n in folder_names), (
        "no photo-library set exists after a completed backup pass, so the "
        "resolver never created (or adopted) one — every ingest in that pass "
        "had nowhere to land. Folder rows: "
        f"{folder_names!r}; app error={ingest_error!r}"
    )

    # Half 2 — and it actually reached the nest. Media reads `fauna.media.list`,
    # so this is nest-side truth read through the surface the outcome names, not
    # a restatement of the app's own counter.
    media = MediaActions(driver)
    def _media_rows_carrying_the_tag():
        # Media pulls `fauna.media.list` on the nav EDGE only, so a poll that
        # stays on the page re-reads the first visit's snapshot forever
        # (`MediaActions.reenter`'s own docstring). Re-enter every attempt.
        media.reenter()
        return [n for n in media.item_names() if tag in n] or None

    names = wait_until(
        _media_rows_carrying_the_tag,
        budgets.MAIL_AGENT_WRITEBACK_S,
        diagnose=lambda: (
            f"the photo never appeared in Media. The pass reported completion and "
            f"the photo-library set exists, so the loss is in the ingest — and "
            f"the app's own pass funnel says where: "
            f"{describe_photo_backup_funnel(driver)}. "
            f"Media showed: {media.item_names()!r} ({media.item_count()} row(s)); "
            f"the app's own ingest error was {ingest_error!r}"
        ),
    )
    assert names, f"expected a Media row carrying {tag!r}"
