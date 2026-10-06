"""tier_3 — the macOS column of `docs/features/photo-backup.md`, against the REAL
System Photo Library (the real-session photo-library venue).

Target state: `docs/goal/ui/folders.md` § Photo backup (apple + android) — its
macOS line, *"the always-running app watches the library directly; no
scheduler"*, puts macOS inside the feature, so the column owes every outcome the
page lists. The journeys are the iOS witnesses', **imported and called, never
copied** (priorities #1/#4): each test below runs the very function the ios column
runs, so the two columns cannot disagree on a byte of what they assert.

**Why these live here and not as a `macos` mark on the originals.** The ios legs
drive PhotoKit on the simulator's own throwaway library. A Mac has no such
library: `photolibraryd` serves the user's System Photo Library daemon-side, no
launch isolation redirects it, and a plain mark would have every default macOS
sweep write photos into the host's real library — the machine-global state
convention 10 forbids. So the macOS legs sit behind convention 12's three gates:
this directory is collected only under `--real-session`; the ambient guard's macOS
arm (`conftest.py`) requires the logged-in GUI session, `photolibraryd`, and **a
virtual machine** (the user approved the macOS VM's own disposable library,
2026-09-26 — never a person's); and the machine-wide lock is keyed
`real-photos-library`.

**What the venue is** (`drivers/macos.py` § the photo-library venue): the bare
debug binary wrapped in a minimal `.app` under the fixed test id
`PHOTO_LIBRARY_BUNDLE_ID`, signed with the login keychain's Apple Development
identity, launched through Launch Services. The stable signature is what lets a
Photos grant survive rebuilds; the Launch Services launch is what makes the app —
not the terminal the run started in — the process TCC charges the grant to.

**The one human step.** macOS offers no way to pre-grant Photos (the user TCC
store is SIP-protected; `tccutil` only resets), so the grant is one "Allow" click,
given once to that bundle id — plus, on the key's first use, the keychain's
"Always Allow" for `codesign` (typed with the login password). The person at the
screen runs `just mac-photos-e2e-grant` (`helpers/photo_library_grant.py`: no
nest, no queue, both dialogs within seconds of its "input needed now" line);
`test_the_one_time_photos_grant_is_in_place` is the gate every
other test here stands on, and it names the recipe when the grant is missing.

**What this venue does not prove** (stated here, where a reader meets it):

- The photo enters the library through PhotoKit **inside the app process**
  (`fauna_e2e_agent::PHOTO_BACKUP_SEED_LIBRARY`, a real `PHAssetCreationRequest`)
  rather than from a camera or another app, so the change observer is notified of
  a change its own process made. The engine's read path — `PHAsset.fetchAssets`,
  the export, the strip, the sealed ingest — is untouched and real.
- **Debris accumulates.** Every seeded photo stays in the VM's library (a PhotoKit
  delete raises a confirmation no harness can answer; the user accepted the
  debris), and each test signs in a fresh actor, so its first pass backs up every
  earlier run's photos too. The assertions are all relative (`>=`, above a
  baseline), so this costs time, not correctness.

**Not here: the scheduled background pass.** `test_photo_backup_unattended.py::
test_the_scheduled_background_pass_runs_the_real_ingest` witnesses iOS's
`BGProcessingTask`; macOS has no scheduler (`folders.md` § Photo backup), so
outcome 3 on this column rests on the foreground observer and the coalescing
journey alone — which is why only the one unattended test is imported.
"""

from __future__ import annotations

import os

import pytest

from tests import (
    test_photo_backup_byte_fidelity as byte_fidelity,
    test_photo_backup_library_ingest as library_ingest,
    test_photo_backup_one_way as one_way,
    test_photo_backup_pass_coalescing as pass_coalescing,
    test_photo_backup_pass_progress as pass_progress,
    test_photo_backup_unattended as unattended,
)

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.macos,
    # Selects the photo-library launch for every macOS app of the run
    # (`conftest._macos_photo_library_venue`) — the only launch whose driver will
    # seed or read the real library.
    pytest.mark.real_photos_library,
]

#: How long the grant gate waits for an answer. Short, so an unattended run with no
#: grant fails in seconds naming the recipe. The grant itself is never given from a
#: pytest run: the nest prebuild and the e2e slot queue make the moment a dialog
#: appears unpredictable, so the person-at-the-screen step is
#: `helpers/photo_library_grant.py`, which asks only once everything else is ready.
GRANT_WAIT_S = float(os.environ.get("FAUNA_PHOTOS_GRANT_WAIT_S", "15"))


def test_the_one_time_photos_grant_is_in_place(app):
    """The venue's precondition: this bundle id holds Photos read-write access.

    Asks the OS through the app. Already granted — the ordinary case — it answers
    at once with no prompt; undecided, it raises the real system alert, which
    `just mac-photos-e2e-grant` is where a person answers it. Every other test
    here would otherwise fail at its first `add_photo_to_library` with the same
    finding, less plainly.
    """
    answer = app.driver.request_photos_access(timeout=GRANT_WAIT_S)
    assert answer in ("authorized", "limited"), (
        f"the photo-library venue's test bundle has Photos access {answer!r}. The "
        "grant is a one-time human step on this Mac's screen: run "
        "`just mac-photos-e2e-grant` and click Allow on the Photos prompt (and "
        "'Always Allow' if the keychain asks whether codesign may use the key). "
        "`denied` means an earlier prompt was refused — re-enable 'Fauna E2E "
        "Photos' under System Settings > Privacy & Security > Photos."
    )


@pytest.mark.feature("photo-backup")
def test_a_photo_in_the_library_reaches_the_nest_and_shows_in_media(
    logged_in_app, tmp_path
):
    """Outcomes 1, 2 and 4 — the ios journey, on the real library."""
    library_ingest.test_a_photo_in_the_library_reaches_the_nest_and_shows_in_media(
        logged_in_app, tmp_path)


@pytest.mark.feature("photo-backup")
def test_a_new_photo_goes_up_with_nothing_pressed(logged_in_app, tmp_path):
    """Outcome 3, the foreground observer — the always-running app's own trigger."""
    unattended.test_a_new_photo_goes_up_with_nothing_pressed(logged_in_app, tmp_path)


@pytest.mark.feature("photo-backup")
def test_a_photo_arriving_mid_pass_is_backed_up_once_by_one_trailing_pass(
    logged_in_app, tmp_path, nest_instance, test_user
):
    """Outcome 3, the coalesced trailing pass."""
    pass_coalescing.test_a_photo_arriving_mid_pass_is_backed_up_once_by_one_trailing_pass(
        logged_in_app, tmp_path, nest_instance, test_user)


@pytest.mark.feature("photo-backup")
def test_a_running_pass_shows_how_far_it_has_got(logged_in_app, tmp_path, nest_instance):
    """Outcome 5 — the held in-flight window and its position readout."""
    pass_progress.test_a_running_pass_shows_how_far_it_has_got(
        logged_in_app, tmp_path, nest_instance)


@pytest.mark.feature("photo-backup")
def test_sync_now_starts_a_pass_that_nothing_else_would_have_started(
    logged_in_app, tmp_path
):
    """Outcome 6 — Sync now, attributed by the pass counters."""
    pass_progress.test_sync_now_starts_a_pass_that_nothing_else_would_have_started(
        logged_in_app, tmp_path)


@pytest.mark.feature("photo-backup")
def test_deleting_the_backed_up_copy_leaves_the_camera_roll_alone(
    logged_in_app, tmp_path
):
    """Outcome 7 — a nest-side delete never reaches back into the library. Here the
    library is a real Mac's, which is exactly the case the outcome protects."""
    one_way.test_deleting_the_backed_up_copy_leaves_the_camera_roll_alone(
        logged_in_app, tmp_path)


@pytest.mark.feature("photo-backup")
def test_the_stored_copy_is_the_phones_pixels_with_only_the_metadata_gone(
    logged_in_app, tmp_path, nest_instance, test_user
):
    """Outcome 8 — the nest keeps the library's pixels, metadata stripped losslessly."""
    byte_fidelity.test_the_stored_copy_is_the_phones_pixels_with_only_the_metadata_gone(
        logged_in_app, tmp_path, nest_instance, test_user)
