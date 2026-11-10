"""tier_3 e2e: a keep or remove answered on one device is settled on the others.

Outcome 37 of ``docs/features/take-your-account-back.md``;
``docs/goal/behavior/succession-aftermath.md`` § Re-key scope → *Adjudicating
what the aftermath carries across*. After a recovery the aftermath carries
every backup destination, trust and member across and then ASKS the owner
about each one until they keep or remove it, because a seed thief could have
added any of them in the window before the ceremony. The ratified encoding
(the 2026-08-06 blockquote, "The verdict rests ON the mark; a *Keep* never
deletes it") exists for exactly the property this journey witnesses: **two of
the owner's devices working one backlog must not erase each other or re-ask
what the other already answered.** The aftermath doc names the gap this closes
in its own status section: "no tier_3 journey asserts the cross-device half …
because a second seat is what proves a merge."

**The shape.** Seat A is the app under test: it enrolls a destination, runs
the ceremony and becomes the successor. Seat B is a second device of that
successor, brought up the only way a device can join an identity it was never
told about — the recovery kit the ceremony's closing act showed (the restore
door ``test_recovery_kit_restore.py`` drives). Both seats then read the same
inherited row raised; seat A keeps it; seat B, re-reading, shows it settled.

**Why the destination plane.** It is the plane the gap note names, its verdict
rides the account-state plane's CAS + merge path, and the two-device property
is pinned in-crate (``backup_enroll.rs``:
``a_keep_and_a_concurrent_removal_both_survive``). What no in-crate pin can
reach is two real apps against one real nest.

⚠ **Seat B must see the row RAISED before seat A answers.** Without that read
"settled on B" would pass on a seat that never painted the mark at all — a B
that reads nothing proves nothing about a merge.

⚠ **Absence, not emptiness, is the un-raised state** (the aftermath file's
rule): every "not raised" read polls the mark's visibility, never its text.

Convention 8: the destination, the ceremony, the restore and the Keep are all
the user's gestures in the apps. The destination nest's registration of the
owner is arrangement, as in the single-seat journey. Convention 14: every wait
is a deadline poll on state the app shows.

tier_3: a real ``fauna-nest`` binary and two seats of the app under test.
"""
from __future__ import annotations

from contextlib import contextmanager

import pytest

from common.auth import register_user
from helpers.diagnostics import aftermath_log_lines as _aftermath_log
from helpers.diagnostics import backup_regrant_line as _backup_regrant_line
from helpers.recovery_restore import (
    assert_restored_the_same_account,
    lose_every_device_and_open_restore,
    qualified_account,
    sign_in_after_restore,
    wait_for_restore_landing,
)
from helpers.succession_ceremony import (
    SECRET_HEX_LEN,
    SUCCESSION_AND_RELAUNCH_S,
    kit_on_screen,
    require_stolen_gate,
    settled_actor_id,
    wait_for_successor_actor,
)
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_3

# The marks are raised by the post-store-ready pass and a Keep is one load +
# save + re-read against the nest — the aftermath journeys' budget. A second
# device reads the plane on its own nav edge, so the same ceiling covers it.
_ADJUDICATION_S = 120.0
# How long a re-entered Backups page has to finish reading its list before a
# read of it means anything (the aftermath file's `_LIST_HYDRATION_S`).
_LIST_HYDRATION_S = 15.0


@contextmanager
def _a_fresh_seat(request, app, nest_instance):
    """One more seat of the app under test with nothing on it — a new device.

    Built directly (as ``test_account_instance_lock_tui.py`` builds its
    seats), not through ``_alice_extra_seat``: that launcher signs the seat in
    with a seed, and the device this journey needs holds no seed at all until
    the user types the kit into it. A native app's new device is a second
    launch with its own fresh state root (convention 10), isolated per app by
    ``_second_seat_config`` (macOS's own home, iOS's second simulator); web's
    is a twin page
    in its own ``BrowserContext`` — fresh ``localStorage`` and IndexedDB, the
    browser's analogue of a second machine (``WebBridgeDriver.open_twin_page``,
    the idiom ``succession_member_app`` seats its web member with).
    """
    from conftest import ActionLayer, _second_seat_config, create_driver
    from helpers.app_surface import app_name

    name = app_name(app.driver)
    if name == "web":
        driver = app.driver.open_twin_page()
    else:
        driver = create_driver(name)
        driver.launch(_second_seat_config(name, nest_instance, request))
    try:
        yield ActionLayer(driver)
    finally:
        try:
            driver.screenshot("teardown-succession-second-device")
        except Exception:  # noqa: BLE001 — teardown must not mask the result
            pass
        driver.teardown()


def _reopen_backups(app) -> None:
    """Leave Backups and come back, then wait for the list to read.

    Entering Backups re-reads the ``fauna.state.backup`` destinations, so this
    makes a read after it a statement about what is at rest; via another page
    because re-navigating to the page you are on need not re-fire its enter
    hook. The hydration wait is soft — a genuinely empty list costs its budget
    and the caller's assertion reports (the aftermath file's
    ``_reopen_backups`` states why the wait is needed at all).
    """
    import time

    app.settings.navigate()
    app.backups.navigate()
    deadline = time.monotonic() + _LIST_HYDRATION_S
    while time.monotonic() < deadline:
        if app.backups.destination_count() >= 1:
            return
        time.sleep(0.3)


def _raised(app) -> bool:
    """A fresh read of seat ``app``'s Backups page: is row 0 raised?"""
    try:
        _reopen_backups(app)
        return app.backups.destination_unattested_mark_visible(0)
    except Exception:  # noqa: BLE001 — the callers' diagnoses read the page
        return False


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("take-your-account-back")
def test_a_keep_on_one_device_is_settled_on_the_other(
    request, ungranted_app, nest_instance, second_nest
):
    """enroll a destination → succeed on A → B joins from the kit → both read
    it raised → Keep on A → B reads it settled.

    Seat B is a fresh seat of the app under test (``_a_fresh_seat``): a
    second launch on tui, linux and macos, a launch on a second simulator on
    ios, a twin page on web. android joins when its
    column can hold two seats of the app at once (one emulator holds one
    install of the package).
    """
    app = ungranted_app
    app.backups.require_destination_management_supported()

    # ── Arrangement: the destination nest knows this owner ──────────────────
    owner = settled_actor_id(app)
    assert len(owner) == SECRET_HEX_LEN, (
        "the owner's actor id must be readable before registering it on the "
        f"destination nest; got {owner!r}, error={app.error_text()!r}"
    )
    try:
        register_user(
            second_nest["port"], owner,
            admin_signing_key=second_nest["admin"]["signing_key"],
        )
    except Exception:  # noqa: BLE001 — already registered
        pass
    # The account as a user would type it on a device that has none — read
    # while a session exists; the handle travels to the successor.
    account = qualified_account(app, nest_instance)

    # ── Precondition: a destination the owner enrolled, not raised ──────────
    app.backups.navigate()
    app.backups.add_destination(second_nest["url"], name="Cross-device")
    wait_until(
        lambda: app.backups.destination_count() >= 1,
        _ADJUDICATION_S,
        diagnose=lambda: (
            "the destination must be enrolled BEFORE the ceremony or this test "
            f"proves nothing: count={app.backups.destination_count()} "
            f"error={app.error_text()!r}"
        ),
    )
    assert not app.backups.destination_unattested_mark_visible(0), (
        "a destination the owner just added must not be raised before any "
        "succession; mark reads "
        f"{app.backups.destination_unattested_mark_text(0)!r}"
    )

    # ── The ceremony on A, keeping the kit its closing act shows ────────────
    app.settings.navigate()
    app.settings.open_recovery_kit()
    app.settings.create_recovery_kit()
    app.wait_for("recovery-kit-secret-display", timeout=30.0)
    held = app.settings.recovery_kit_secret()
    assert len(held) == SECRET_HEX_LEN, (
        f"the kit is 64-hex, got {len(held)}; error={app.error_text()!r}"
    )
    app.settings.navigate()
    app.settings.open_recovery_kit()
    require_stolen_gate(app)
    app.settings.succeed_identity_with_held_kit(held)
    # Read FIRST and with no navigation of our own: entering Account clears a
    # kit on screen (shown-once custody), and this is the one kit the user
    # holds for the successor (`helpers/inherited_corpus.py`
    # `seed_under_a_then_succeed` reads it the same way).
    wait_until(
        lambda: kit_on_screen(app) not in ("", held),
        SUCCESSION_AND_RELAUNCH_S,
        diagnose=lambda: (
            f"no fresh kit on screen for the successor; error={app.error_text()!r}"
        ),
    )
    successor_kit = kit_on_screen(app)
    assert len(successor_kit) == SECRET_HEX_LEN
    new_actor = wait_for_successor_actor(app, owner)

    # ── A: the aftermath settles and raises the inherited row ───────────────
    # A causal barrier on the account's own progress line (the single-seat
    # journey's), then the mark.
    wait_until(
        lambda: _backup_regrant_line(app) == S.settings.recovery_kit.backup_regrant_done,
        _ADJUDICATION_S,
        diagnose=lambda: (
            f"leg 2's line reads {_backup_regrant_line(app)!r}; "
            f"error={app.error_text()!r}"
        ),
    )
    wait_until(
        lambda: _raised(app),
        _ADJUDICATION_S,
        diagnose=lambda: (
            "seat A must raise the inherited destination for review before a "
            "second device can be asked anything about it: "
            f"count={app.backups.destination_count()} "
            f"mark={app.backups.destination_unattested_mark_text(0)!r} "
            f"error={app.error_text()!r}; aftermath log {_aftermath_log(app)}"
        ),
    )

    with _a_fresh_seat(request, app, nest_instance) as seat_b:
        # ── B joins the successor from the kit ──────────────────────────────
        lose_every_device_and_open_restore(seat_b, nest_instance)
        seat_b.onboarding.restore_from_recovery_kit(successor_kit, account=account)
        wait_for_restore_landing(seat_b)
        assert_restored_the_same_account(seat_b)
        sign_in_after_restore(seat_b)
        # Polled: the Status page paints after its nav edge returns.
        b_actor = wait_until(
            lambda: settled_actor_id(seat_b) or None,
            SUCCESSION_AND_RELAUNCH_S,
            diagnose=lambda: (
                "the second device never rendered an actor id after signing "
                f"in; error={seat_b.error_text()!r}"
            ),
        )
        assert b_actor == new_actor, (
            "the second device must come up as the SAME successor seat A "
            f"became; B reads {b_actor!r}, A is {new_actor!r}"
        )

        # ── B reads the same row raised ─────────────────────────────────────
        wait_until(
            lambda: _raised(seat_b),
            _ADJUDICATION_S,
            diagnose=lambda: (
                "the second device must read the inherited destination raised "
                "too — the mark rests in the account's plane, not on seat A — "
                f"but it reads count={seat_b.backups.destination_count()} "
                f"mark={seat_b.backups.destination_unattested_mark_text(0)!r} "
                f"error={seat_b.error_text()!r}. Without this read 'settled on "
                "B' below would prove nothing."
            ),
        )

        # ── A keeps it ──────────────────────────────────────────────────────
        assert app.backups.destination_keep_visible(0), (
            "a raised row offers Keep on seat A; error="
            f"{app.error_text()!r}"
        )
        app.backups.keep_destination(0)
        wait_until(
            lambda: not _raised(app),
            _ADJUDICATION_S,
            diagnose=lambda: (
                "Keep on seat A must settle the row there first: mark reads "
                f"{app.backups.destination_unattested_mark_text(0)!r}; "
                f"error={app.error_text()!r}"
            ),
        )

        # ── B reads it settled — the verdict merged, not lost ───────────────
        wait_until(
            lambda: not _raised(seat_b),
            _ADJUDICATION_S,
            diagnose=lambda: (
                "the Keep answered on seat A must be settled on the second "
                "device too (succession-aftermath.md § Re-key scope → "
                "Adjudicating what the aftermath carries across: the verdict "
                "rests on the mark), but seat B still reads "
                f"{seat_b.backups.destination_unattested_mark_text(0)!r}; "
                f"count={seat_b.backups.destination_count()} "
                f"error={seat_b.error_text()!r}. The owner would be asked "
                "again on every other device about a row already answered."
            ),
        )
        assert seat_b.backups.destination_count() >= 1, (
            "Keep is not Remove: the destination stays enrolled on the second "
            "device as well"
        )
        assert not seat_b.backups.destination_keep_visible(0), (
            "Keep must retire with its mark on the second device too"
        )
