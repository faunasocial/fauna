"""tier_3 — a photo arriving mid-pass is backed up exactly once, by exactly one
trailing pass.

Target state: `docs/goal/ui/folders.md` § Photo backup (apple + android) →
*What drives the ingress*, which names the triggers, and the section's promise that
"new photos go up by themselves — you never have to open the app or press
anything". This file guards the pass **lifecycle** that promise rests on.

**The bug this exists for, and the wrong fix it rules out.**
`PhotoBackupEngine.syncNewPhotos()` had no re-entry guard: six call sites across
five triggers reach it — the enable toggle's grant branch, the PhotoKit change
observer, the `BGProcessingTask` body, the Sync-now button, and a catch-up pass at
launch on each shell — so a photo arriving mid-pass wakes the observer while another
pass is still running, and both passes export, strip and ingest the same asset (the
`PhotoBackupRecord` marking it synced is written only *after* a successful ingest).
The obvious fix is `guard !isBackingUp else { return 0 }`, and it is worse: the
running pass has already taken its `PHAsset.fetchAssets` snapshot, so the mid-pass
photo is not in it, and refusing the second call means **nothing** picks that photo
up until the next OS slice — or never, if the app closes first. android ships
exactly that guard today (`PhotoBackupEngine.kt`'s `if (_isSyncing.value) return 0`,
and its set is not even atomic with its test). So the engine coalesces instead:
single-flight, plus one trailing re-run that picks up whatever the snapshot missed.

**Both failure modes are asserted, because a test for one passes in the other's
world.** A test that only checked "the mid-pass photo arrives" is green under
uncoalesced concurrency; a test that only checked "one pass at a time" is green
under the lossy guard.

  - **Nothing is dropped** — the photo added mid-pass reaches the nest. Under the
    bare guard it would not, and this is the assertion that fails there.
  - **Nothing is done twice** — the two photos produce exactly two `create` changes
    and no `modify`. Under uncoalesced concurrency the first photo is ingested by
    both passes, so its path takes a second change, and this is the assertion that
    fails there.

**The window is HELD, not raced** (convention 14, "Pre-fetch windows"). The nest's
`rpc-hold` hook parks the ingest's own `fauna.sync.changes.record` until released,
which is what makes "while a pass is running" a state this test stands in rather
than an interval it hopes to hit. `wait_for_held_rpc` proves the app really issued
the request before the second photo is added.

**What is deliberately NOT asserted.** The tempting mid-pass read — "`holding` is
still 1, so no second pass parked" — is a race, not a check: the observer fires
asynchronously, so a green result may only mean it had not fired yet. The two
assertions above are both *positive* and settle after the release, so neither can
pass by arriving early.

**Why this module is iOS-only** — macOS runs the same engine against the
machine-global host Photos library, out of reach of an ordinary launch under
convention 10; the macOS column runs this very function from
`tests/real_session/test_photo_backup_macos.py` (convention 12's macOS arm).
"""

from __future__ import annotations

import pytest

from common.auth import sync_changes_list, user_folders_list
from helpers import budgets
from helpers.photo_backup import (
    a_real_png,
    a_tag,
    await_photo_in_media,
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
#: `SyncEngine::upload_file` → `record_change`. Holding it parks a pass inside its
#: first file, which is where a second trigger has to find it.
INGEST_KIND = "fauna.sync.changes.record"


def _photo_set(port: int, secret_key: str) -> str:
    """The set the ingress resolved to, asked of the nest — the shared resolver
    names it `"Photo Library"` (`folders.md` § Photo backup → *Target set
    model*)."""
    folders = user_folders_list(port, secret_key=secret_key).get("folders", [])
    return next(
        (f["name"] for f in folders if f.get("name") == "Photo Library"),
        "",
    )


def _changes(port: int, secret_key: str, folder: str) -> list[dict]:
    return sync_changes_list(
        port, secret_key=secret_key, folder=folder,
    ).get("changes", [])


@pytest.mark.feature("photo-backup")
def test_a_photo_arriving_mid_pass_is_backed_up_once_by_one_trailing_pass(
    logged_in_app, tmp_path, nest_instance, test_user
):
    """Park a pass inside its first upload, add a second photo while it is parked,
    release, and require both photos stored exactly once each.
    """
    app = logged_in_app
    driver = app.driver
    port = nest_instance["port"]
    secret_key = test_user["signing_key"].encode().hex()

    prepare_photos_access(app)
    enable_photo_backup(app)
    granted(app)
    await_quiet(driver)

    folder = wait_until(
        lambda: _photo_set(port, secret_key) or None,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            "the nest holds no photo-library set after enabling backup, so the "
            "resolver never created or adopted one"
        ),
    )
    # Everything already recorded in the set. The two photos below are identified by
    # being above this high-water mark — `SyncChange.path` is None (the path travels
    # sealed), so no change can be matched by filename.
    baseline_seq = max((c.get("seq", 0) for c in _changes(port, secret_key, folder)),
                       default=0)

    # ⚠ ARM BEFORE THE FIRST PHOTO. Adding it wakes the app's ingress at once, so
    # arming afterwards races that pass and loses whenever it wins (the same
    # "arm before the boundary" rule `helpers/rpc_hold` states for a cold start).
    arm_rpc_hold(port, INGEST_KIND)
    try:
        first, second = a_tag(), a_tag()
        driver.add_photo_to_library(str(a_real_png(tmp_path, tag=first)))

        # The pass is now provably parked inside the first photo's ingest — not
        # "probably running by now".
        wait_for_held_rpc(
            port, INGEST_KIND,
            diagnose=lambda: (
                "no ingest RPC arrived, so no pass ever reached the first photo: "
                f"pass counters {passes(driver)}, "
                f"{describe_photo_backup_funnel(driver)}. A library change with "
                "backup on must start a pass by itself — if that is what failed, "
                "the finding belongs to outcome 3 (test_photo_backup_unattended.py)"
            ),
        )
        started_mid_pass, _ = passes(driver)

        # THE SECOND PHOTO, while the first pass is held open. Its own pass cannot
        # run — that is the point — so the engine must remember the request.
        driver.add_photo_to_library(str(a_real_png(tmp_path, tag=second)))
    finally:
        # ALWAYS release: an armed kind parks every later request of that kind,
        # including the ones the tests after this one issue.
        release_rpc_hold(port, INGEST_KIND)

    # ── Assertion 1: NOTHING WAS DROPPED. The bare `guard !isBackingUp` fails here:
    # the held pass's snapshot predates the second photo, so only a remembered
    # request can carry it.
    await_photo_in_media(
        app, second,
        what="a photo that appeared while a pass was parked inside its first upload "
             "(so only a coalesced trailing pass can have carried it)",
    )
    await_photo_in_media(app, first, what="the pass that was parked and released")

    app.backups.navigate_folders()
    await_quiet(driver)

    # Exactly one trailing pass — not none (dropped) and not one per mid-pass
    # request. Counted from the MID-PASS baseline, which already includes the parked
    # pass, so the only pass that may have begun since is the trailing one.
    started_after, _ = passes(driver)
    assert started_after == started_mid_pass + 1, (
        "the coalesce did not produce exactly one trailing pass: passes_started went "
        f"{started_mid_pass} -> {started_after} across the release. One more is the "
        "trailing re-run; none means the mid-pass request was dropped (the lossy "
        "bare guard); two or more means each mid-pass request queued its own pass "
        f"instead of collapsing. {describe_photo_backup_funnel(driver)}"
    )

    # ── Assertion 2: NOTHING WAS DONE TWICE. Uncoalesced concurrency fails here:
    # the second pass would re-export the first photo (whose record is written only
    # after a successful ingest), landing a second change on the same path.
    ours = [c for c in _changes(port, secret_key, folder)
            if c.get("seq", 0) > baseline_seq]
    creates = [c for c in ours if c.get("change_type") == "create"]
    others = [c for c in ours if c.get("change_type") != "create"]
    assert len(creates) == 2 and not others, (
        "the two photos did not produce exactly two creates and nothing else: "
        f"{[(c.get('change_type'), c.get('size_bytes')) for c in ours]!r}. A third "
        "change, or a `modify`, means one photo was ingested twice — two passes ran "
        "concurrently over the same asset, which is what the single-flight gate "
        "exists to prevent"
    )
    assert len({c.get("manifest_hash") for c in creates}) == 2, (
        "the two creates carry the same content hash, so the two fixtures were not "
        "distinct photos and this test proved nothing about double-ingest"
    )

    # And the funnel describes ONE pass, not a blend of two: the trailing pass saw
    # both photos and had exactly the second one left to upload.
    last = funnel(driver)
    assert last["uploaded"] + last["already_synced"] <= last["assets_seen"], (
        "the per-pass funnel is internally inconsistent "
        f"({last['uploaded']} uploaded + {last['already_synced']} already-synced > "
        f"{last['assets_seen']} seen), which is what two passes resetting and then "
        f"interleaving one counter set looks like. {describe_photo_backup_funnel(driver)}"
    )
