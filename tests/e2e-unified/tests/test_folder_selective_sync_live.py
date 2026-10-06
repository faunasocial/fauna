"""tier_3 — a folder's exclusions stop syncing a path and clearing them lets it
through again, on a RUNNING seat with nothing restarted
(``local-folder-sync`` outcome 10; ``file-sync.md`` § Status, the selective-sync
RESOLVED bullet).

**Why this test exists.** The selective-sync lists round-tripped through every
app's UI with *zero effect* on what synced until 2026-08-27, and the only test
that covered them — ``test_folders.py::test_edit_folder_paths`` — asserted the
round trip of the text alone. That is the half that stayed green for the
setting's whole broken life. This is the other half: the exclusion is armed
through the app UI and a file matching it never reaches the other seat, then the
exclusion is cleared through the app UI and the SAME file — never re-authored —
crosses.

**The flow it asserts end to end** (``file-sync.md`` § Status; the lists ride the
same authoritative ``fauna.folders.list`` read as the mode and install into the
RUNNING engine's matcher in ``SyncEngine::refresh_sync_mode``, at loop entry and
on every rescan tick): owner types the exclusion into ``folder-exclude-paths``
and saves → ``fauna.folders.update`` persists it on the nest folder row → the
owner's engine re-reads the row on its next rescan tick →
``install_selective_sync`` rebuilds the ``IgnoreMatcher`` (each pattern gets a
``{pat}/**`` twin, ``ignore.rs``) → the upload doors consult ``is_ignored`` and
skip the file (``provider_face``, ``watcher``, ``full_scan_filtered``) → the
bytes never reach the nest → the member's bound folder never receives them.

**Latency-independence (convention 14) — how the negative assertion is made
sound without a wall clock.** A "did not arrive" assertion is only meaningful
once the rail has demonstrably carried something *else*, and only once the
exclusion is demonstrably installed. Both are arranged by CONSTRUCTION rather
than by waiting:

* the exclusion is armed **before the share, the promotion, the accept and both
  binds** — minutes of real choreography, and many 30 s rescan ticks (the
  harness default on the compile-gated ``FAUNA_E2E_RESCAN_MS`` seam,
  ``always_resident::rescan_interval``), elapse between the arm and the first
  excluded write, so the matcher is certainly armed on the owner's running
  engine by the time anything is written;
* the excluded file is written **together with a visible sibling**, and the
  absence is asserted at the instant the sibling ARRIVES — a state-defined
  moment at which the rail has provably carried a change authored in the same
  breath, not a moment on a clock;
* the release half is fully deterministic on its own: the same file, never
  touched again, crosses only after the field is cleared.

Every mutation is driven through the app UI (convention 8); nothing is
restarted at any point, which is the outcome's own "without restarting
anything".
"""

import secrets

import pytest

from conftest import get_available_apps
from helpers.waiting import wait_until
from helpers.folder_content import (
    agent_diagnosis as _agent_diagnosis,
)
from helpers.folder_content import (
    atomic_write as _atomic_write,
)
from helpers.folder_content import (
    bind_location_under_set as _bind_location_under_set,
)

pytestmark = [
    pytest.mark.tier_3,
    # tui is the lead app (`testing.md` § Default app and nest mode). Each
    # further leg arrives with its own marker AND an `_SUPPORTED_APPS` entry —
    # a marker alone silently keeps driving tui underneath, the trap
    # `test_share_pump_two_actor.py`'s own marker block records.
    pytest.mark.tui,
]

#: Apps whose leg has landed. Grow this — and the markers above — together.
_SUPPORTED_APPS = ("tui",)

#: The excluded directory, and the file under it that must be held back. The
#: matcher adds a `{pat}/**` twin per configured pattern (`ignore.rs`), so the
#: bare directory name covers everything beneath it.
_EXCLUDED_DIR = "private"

# ── Named budgets (convention 14: generous ceilings, deadline polls) ─────────
# Nest-mediated hydration crosses two agents plus a nest round trip on the
# rescan cadence — the content-sync twin's number.
_HYDRATION_S = 360.0
# Un-arming is picked up by the next rescan tick's `full_scan_filtered`, which
# re-scans the watch dir with the new (empty) matcher and finds the file as new.
# One tick is 30 s here; the ceiling is not.
_RELEASE_S = 360.0


def _seat_pairs():
    """One (owner, member) pair per supported app that is drivable HERE.

    Both seats run the SAME app: the journey proves one app's glue against
    itself, and a full cross product would multiply a long two-GUI journey by
    the square of the app count for combinations no leg owes.
    """
    available = get_available_apps()
    return [(app, app) for app in _SUPPORTED_APPS if app in available]


def _read_or_none(path):
    try:
        return path.read_text()
    except (FileNotFoundError, OSError):
        return None


@pytest.mark.parametrize(
    "folder_share_owner_app,folder_share_recipient_app",
    _seat_pairs(),
    indirect=True,
)
@pytest.mark.real_conversations
# Documented-long (convention 9: bounded always): two GUI apps + two real
# agents + a real MLS share + two hydrations across a cleared exclusion.
@pytest.mark.timeout(1800)
@pytest.mark.feature("local-folder-sync")
def test_an_excluded_path_stops_syncing_until_the_exclusion_is_cleared(
    request, folder_share_owner_app, folder_share_recipient_app, tmp_path
):
    """An excluded path never reaches the other seat; clearing the exclusion
    lets the same file through, with nothing restarted."""
    from tests.api import conv_api

    owner_app, nest, _owner = folder_share_owner_app
    member_app, _nest2, member = folder_share_recipient_app

    # Both seats' agent logs on EVERY outcome — a pytest-timeout kill unwinds
    # through no `pytest.fail`, so without this the one failure mode that
    # strands the run leaves no evidence.
    request.addfinalizer(lambda: print(_agent_diagnosis(member_app, "member")))
    request.addfinalizer(lambda: print(_agent_diagnosis(owner_app, "owner")))

    # The member's login-time KeyPackage publish is best-effort async, and the
    # owner's share must fetch one to admit them to the set's MLS group.
    wait_until(
        lambda: conv_api.keypackage_count(nest["port"], member, member["actor_id_hex"]) > 0,
        30,
        interval=1.0,
        diagnose=lambda: (
            "the member never published a fetchable KeyPackage, so the owner's "
            "share cannot admit them to the set's MLS group"
        ),
    )

    set_name = f"selsync-{secrets.token_hex(4)}"
    ob = owner_app.backups
    ob.navigate_folders()
    ob.create_folder_via_wizard(set_name)

    # ── 1. ARM the exclusion, first thing ────────────────────────────────────
    # Deliberately before the share/promote/accept/bind choreography below: that
    # takes minutes and many 30 s rescan ticks, so the matcher is certainly
    # armed on the owner's running engine by the time anything is written. This
    # is what makes the later absence a fact about the filter rather than a race
    # with the install.
    ob.find_and_expand_folder_until(set_name, "folder-exclude-paths")
    ob.set_exclude_paths(_EXCLUDED_DIR)
    ob.save_paths()

    # Read it back from the nest-authoritative summary — the list refreshes on
    # save and the rows rebuild collapsed, so this re-expands rather than
    # trusting the row to have stayed open.
    ob.find_and_expand_folder_until(set_name, "folder-exclude-paths")
    assert ob.get_exclude_paths() == _EXCLUDED_DIR, (
        "the exclusion must persist on the nest folder row before anything is "
        f"written — the row is the lists' only home ({ob.get_exclude_paths()!r}); "
        f"error={owner_app.error_text()!r}"
    )

    # ── 2. Share → writer → accept (the content-sync choreography) ───────────
    # ⚠ `find_and_expand_folder` TOGGLES the expander, and the read-back above
    # left this row OPEN — calling it here would CLOSE the row and take
    # `folder-share-button` off screen (a 404 on the click, which is exactly
    # how this test first failed). The `_until` variant re-expands on every
    # poll until the widget it is asked for is visible, so it converges from
    # either state; it is the only safe way to re-enter a row this test has
    # already been inside.
    owner_row = ob.find_and_expand_folder_until(set_name, "folder-share-button")
    ob.open_share_dialog()
    ob.share_recipient(handle=member["handle"], actor_id_hex=member["actor_id_hex"])

    wait_until(
        lambda: ob.shared_member_count() == 1,
        20,
        interval=0.5,
        diagnose=lambda: (
            f"the share should land exactly one member; error={owner_app.error_text()!r}"
        ),
    )

    # A share defaults the member to Reader, and an engine REFUSES to run for a
    # reader (`decide_engine_content_binding`), so the promotion is a
    # precondition of hydration, not a variation of this test.
    ob.set_member_access("writer", 0, row=owner_row)
    wait_until(
        lambda: ob.member_access(0, row=owner_row) == "writer",
        15,
        interval=0.5,
        diagnose=lambda: (
            "the member must persist as writer — a reader never binds an engine, "
            f"so nothing downstream could run; error={owner_app.error_text()!r}"
        ),
    )

    mb = member_app.backups
    mb.navigate_folders()
    mb.wait_for_pending_shares(1)
    mb.accept_pending_share(0)
    assert mb.wait_for_pending_shares(0) == 0, (
        f"[member] accepting should consume the knock; error={member_app.error_text()!r}"
    )

    # The folder list re-fetches on page-VISIBLE, so the predicate toggles away
    # and back after each miss — polling without the toggle reads an empty list
    # forever (the proven pattern in the content-sync twin).
    def _accepted_set_listed():
        titles = [mb.folder_title(i) for i in range(mb.folder_count())]
        if any(set_name in title for title in titles):
            return True
        mb.navigate_devices()
        mb.navigate_folders()
        return False

    wait_until(
        _accepted_set_listed,
        90,
        interval=1.0,
        diagnose=lambda: (
            f"[member] the accepted set {set_name!r} never appeared in the folder "
            f"list within 90s; error={member_app.error_text()!r}\n"
            + _agent_diagnosis(member_app, "member")
        ),
    )

    # ── 3. Both seats bind a location ────────────────────────────────────────
    owner_folder = tmp_path / "owner-bound"
    owner_folder.mkdir()
    _bind_location_under_set(owner_app, set_name, owner_folder, seat="owner")

    member_folder = tmp_path / "member-bound"
    member_folder.mkdir()
    _bind_location_under_set(member_app, set_name, member_folder, seat="member")

    # ── 4. Write the excluded file and its visible sibling TOGETHER ──────────
    # The sibling is the positive control: its arrival is the state-defined
    # instant at which the rail has provably carried a change authored in the
    # same breath as the excluded one, which is what makes the absence below an
    # assertion rather than a wait.
    visible = f"visible-{secrets.token_hex(3)}.txt"
    visible_payload = f"this one is not excluded — {secrets.token_hex(8)}\n"
    secret_rel = f"{_EXCLUDED_DIR}/secret-{secrets.token_hex(3)}.txt"
    secret_payload = f"this one is excluded — {secrets.token_hex(8)}\n"

    (owner_folder / _EXCLUDED_DIR).mkdir()
    _atomic_write(owner_folder / secret_rel, secret_payload)
    _atomic_write(owner_folder / visible, visible_payload)

    wait_until(
        lambda: _read_or_none(member_folder / visible) == visible_payload,
        _HYDRATION_S,
        interval=1.0,
        diagnose=lambda: (
            f"[member] the UNEXCLUDED {visible!r} never hydrated within "
            f"{_HYDRATION_S:.0f}s — the ordinary rail is broken, so the exclusion "
            f"half below cannot be read as a filter result. "
            f"error={member_app.error_text()!r}\n"
            + _agent_diagnosis(member_app, "member")
        ),
    )

    # ── 5. THE HOLD: the excluded file is not there, at that same instant ────
    held = _read_or_none(member_folder / secret_rel)
    assert held is None, (
        f"[member] {secret_rel!r} CROSSED while {_EXCLUDED_DIR!r} was an armed "
        f"exclusion on the set's nest row — the selective-sync filter did not "
        f"reach the owner's running engine, or its upload doors did not consult "
        f"it. The unexcluded sibling {visible!r} arrived, so the rail itself is "
        f"live and this is the filter, not the transport. got={held!r}\n"
        + _agent_diagnosis(owner_app, "owner")
    )

    # ── 6. CLEAR the exclusion through the app UI — nothing restarted ────────
    ob.navigate_folders()
    ob.find_and_expand_folder_until(set_name, "folder-exclude-paths")
    ob.set_exclude_paths("")
    ob.save_paths()

    ob.find_and_expand_folder_until(set_name, "folder-exclude-paths")
    assert ob.get_exclude_paths() == "", (
        "clearing the field must persist as an empty list on the nest row — a "
        "row carrying no seal at all installs the empty list, which is what lets "
        "a user's *clear the exclusions* gesture un-filter a running seat "
        f"(`file-sync.md` § Status); got={ob.get_exclude_paths()!r}"
    )

    # ── 7. THE RELEASE: the SAME file, never re-authored, now crosses ────────
    # The next rescan tick re-scans the watch dir through the un-armed matcher
    # (`full_scan_filtered`) and finds the file as new. Nothing was restarted and
    # nothing on disk was touched since step 4 — so its arrival can only be the
    # cleared exclusion taking effect on the running seat.
    wait_until(
        lambda: _read_or_none(member_folder / secret_rel) == secret_payload,
        _RELEASE_S,
        interval=1.0,
        diagnose=lambda: (
            f"[member] {secret_rel!r} still had not arrived {_RELEASE_S:.0f}s after "
            f"the exclusion was cleared through the app UI. The file has not been "
            f"touched on disk since it was written, so the release path is the "
            f"rescan tick's `full_scan_filtered` re-scan through the un-armed "
            f"matcher — un-arming did not reach the running engine, or the "
            f"re-scan did not pick the file up. "
            f"got={_read_or_none(member_folder / secret_rel)!r} "
            f"error={member_app.error_text()!r}\n"
            + _agent_diagnosis(owner_app, "owner")
            + "\n"
            + _agent_diagnosis(member_app, "member")
        ),
    )

    assert not member_app.has_error(), (
        f"[member] the run surfaced an error: {member_app.error_text()!r}"
    )
