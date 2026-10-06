"""tier_3 — backing up never takes anything away from the phone.

Target state: `docs/goal/ui/folders.md` § Photo backup (apple + android),
`folders.md:324` — photos are **backup-type (one-way)** rather than Sync
*precisely because* "the OS owns the photo library — two-way sync would imply
deleting from or writing into the camera roll on nest-side changes". This file is
the witness for `docs/features/photo-backup.md` outcome 7: *"Backing up never
takes anything away from the phone: nothing that happens to a photo on the nest
reaches back into the camera roll."*

**Why witness something that is true by construction.** It is: the ingress exports
each asset to a temp file, sends that, and deletes only that (`PhotoBackupEngine`'s
`defer { try? FileManager.default.removeItem(at: tempUrl) }`, and the engine
contains no `PHAssetChangeRequest` or any other write to the library). But "true by
construction" is a statement about today's construction, and the whole reason this
folder is backup-type is that the *other* mode would delete from the camera roll —
so the day someone reaches for the ordinary sync engine here, or adds a two-way
binding for the photo set, the failure is a user's photos disappearing from their
phone and nothing in the suite says a word. It is also the single most destructive
thing this feature could do, and `principles.md`' no-user-data-loss invariant makes
it irrecoverable by definition: a deleted camera-roll photo is exactly data the
user cannot recreate.

**The delete goes through the app UI** (convention 8): Media → open the photo →
`media-delete-button` → confirm, the same `MediaMachine::delete` a user drives.
An API call standing in for the user would not exercise the client-side path that
could, in a broken world, propagate the tombstone back into PhotoKit.

**The camera roll is read through the app's own eyes**, because that is the only
honest read available: `PHAsset.fetchAssets` is what "the phone still has the
photo" means, and the pass funnel publishes its count (`assets_seen`). A pass
driven after the nest-side delete that still sees every photo is the assertion.

**Why this module is iOS-only** — macOS runs the same engine against the
machine-global host Photos library, which convention 10 keeps every ordinary launch
away from, and it would be a particularly poor idea to point *this* test at a real
person's photo library. The macOS column runs this very function from
`tests/real_session/test_photo_backup_macos.py`, the opt-in venue against the dev
VM's own disposable library (convention 12's macOS arm), whose guard refuses to run
anywhere but a VM.
"""

from __future__ import annotations

import pytest

from actions.media import MediaActions
from helpers import budgets
from helpers.photo_backup import (
    a_real_png,
    a_tag,
    await_pass_after,
    await_photo_in_media,
    await_quiet,
    enable_photo_backup,
    funnel,
    granted,
    media_rows_carrying,
    passes,
    prepare_photos_access,
)
from helpers.waiting import describe_photo_backup_funnel, wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.ios,
]


@pytest.mark.feature("photo-backup")
def test_deleting_the_backed_up_copy_leaves_the_camera_roll_alone(
    logged_in_app, tmp_path
):
    """Back a photo up, delete it on the nest through the app, and require the
    photo to still be in the phone's library.

    Three states have to be distinguished for the assertion to mean anything, and
    each gets its own barrier:

      1. the photo reached the nest (else the delete deletes nothing and the test
         passes vacuously),
      2. the nest-side delete really took (else nothing happened to the photo on
         the nest, and "the camera roll was not touched" is trivially true),
      3. the library still holds the photo afterwards — the outcome itself.
    """
    app = logged_in_app
    driver = app.driver

    prepare_photos_access(app)
    tag = a_tag()
    driver.add_photo_to_library(str(a_real_png(tmp_path, tag=tag)))
    enable_photo_backup(app)
    granted(app)

    # (1) It reached the nest. `await_photo_in_media` reads `fauna.media.list`
    # through the Media page, so this is nest-side truth.
    await_photo_in_media(app, tag, what="enabling backup")

    # …and it leaves the app ON Media, where no `photo-backup-*` control exists.
    # `await_quiet` reads the Sync-now button, so it has to be told where to look
    # first — otherwise it spends its whole budget waiting for a control that is
    # simply on another page (measured: a 90 s `not visible ... count=0`, which
    # reads as a missing control rather than a misplaced test).
    app.backups.navigate_folders()
    await_quiet(driver)

    # How many assets the library holds, as the app sees it. Captured BEFORE the
    # delete, because it is the number the post-delete pass is compared against.
    assets_before = funnel(driver)["assets_seen"]
    assert assets_before > 0, (
        "the app's own pass reports an empty photo library, so there is no photo "
        f"for this test to protect. {describe_photo_backup_funnel(driver)}"
    )

    # (2) Delete it on the nest, through the app, exactly as a user would.
    media = MediaActions(driver)
    media.reenter()
    names = media.item_names()
    index = next(i for i, n in enumerate(names) if tag in n)
    media.open_item_detail(index)
    assert tag in media.detail_name(), (
        f"opened the wrong row: detail shows {media.detail_name()!r}, wanted the "
        f"one carrying {tag!r} (rows were {names!r})"
    )
    media.delete_open_item()

    gone = wait_until(
        lambda: media_rows_carrying(driver, tag) is None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            "the nest-side delete never took — Media still lists the photo, so "
            "nothing has happened on the nest and this test would assert nothing "
            f"about the camera roll. Rows: {MediaActions(driver).item_names()!r}, "
            f"error={app.error_text()!r}"
        ),
    )
    assert gone

    # (3) THE OUTCOME. Drive a fresh pass and require it to still see every photo
    # it saw before: `PHAsset.fetchAssets` is what "the phone still has it" means,
    # and a tombstone that had reached back into the camera roll would show up
    # here as a library one asset shorter.
    #
    # The pass is driven by the Sync-now button because this test is not about any
    # one trigger — any real pass answers the question — and the button is the one
    # trigger every platform has: macOS has no scheduler, so the iOS-only
    # `photo_backup_scheduled_pass_now` poke this used to drive would refuse there
    # (`tests/real_session/test_photo_backup_macos.py` runs this journey on macOS).
    app.backups.navigate_folders()
    await_quiet(driver)
    _, completed_before = passes(driver)
    driver.click("photo-backup-sync-now-button")
    await_pass_after(driver, completed_before, what="the nest-side delete")

    after = funnel(driver)
    assert after["assets_seen"] == assets_before, (
        "DELETING THE BACKED-UP COPY REACHED BACK INTO THE CAMERA ROLL. The "
        f"library held {assets_before} asset(s) before the nest-side delete and "
        f"{after['assets_seen']} after it. `folders.md:324` makes the photo "
        "library backup-type (one-way) precisely so that a nest-side change can "
        "never delete from or write into the camera roll, and "
        "`principles.md`'s no-user-data-loss invariant makes a lost camera-roll "
        f"photo irrecoverable. {describe_photo_backup_funnel(driver)}"
    )
