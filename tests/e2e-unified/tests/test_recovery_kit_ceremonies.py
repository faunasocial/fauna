"""tier_3 e2e: the Recovery kit section's four states and the ceremonies behind
them — lost, veto, and the no-escrow repair.

Goal docs: ``docs/goal/ui/settings.md`` § Recovery kit (the four states of
``recovery-kit-status``, read from the registration chain so a kit made on
another device shows; the repair affordance that renders only in the no-escrow
state) and § User actions (lost opens the 30-day window and shows the new kit
at once; the veto cancels a pending replacement with the kit you hold);
``docs/goal/behavior/identity-succession.md`` § The RecoveryKey and § Seed
escrow own the ceremonies themselves.

``test_recovery_kit_settings.py`` covers create and the kit-in-hand replace.
This module covers the rest of the section, and each journey is one a unit test
cannot stand in for:

- **The status line tells the four states apart, from the nest.** The shared
  projection is unit-tested; what is not is that each state *reaches* the
  line — including a kit registered by another device, which this device
  never saw minted.
- **The repair restores phrase recovery without retiring the kit.** The chain
  head is read back from the nest before and after, and a factory-reset
  restore with the SAME phrase proves the sealed copy is really there.
- **Lost shows the new kit now, lands nothing until the window runs out, and
  then the NEW phrase restores.** The chain head is unchanged and the nest
  holds a pending window; past it the new kit is the head, the section reads
  "registered, no escrow" (landing retires the sealed copy with the old key),
  the held-kit repair re-seals under the new phrase, and that phrase brings the
  account back on a device that lost everything.
- **A replacement someone else asked for is cancelled with the held kit.** The
  "someone else" parks the request over the API — the identity-secret thief
  this ceremony exists for never touches this device.

The window *running out* is reached through the ``test-hooks`` door
``helpers.succession.land_due_replacements``: it runs the nest's production
landing sweep with a later ``now``, so no journey waits out 30 days.

Fixture setup through ``helpers/succession.py`` is convention 8's carve-out:
it arranges what ANOTHER device or a thief did; every gesture of the user under
test goes through the app. Latency-independent throughout (convention 14): each
ceremony re-reads the status on its own outcome, and every wait is a generous
deadline on that read.

tier_3: needs a real ``fauna-nest`` binary. Runs on any app whose section
renders the ceremony — today tui, the lead app; the others join as their legs
land.
"""
from __future__ import annotations

import time

import pytest

from helpers.recovery_restore import (
    assert_restored_the_same_account,
    lose_every_device_and_open_restore,
    qualified_account,
    wait_for_restore_landing,
)
from helpers.succession import (
    land_due_replacements,
    register_recovery_kit,
    registration_chain,
    replacement_status,
    request_seed_alone_replacement,
)
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_3

# The kit is 32 bytes as lowercase hex.
_SECRET_HEX_LEN = 64

# How long a ceremony's own status re-read may take to show. Generous, paid
# only on red (convention 14).
_CEREMONY_S = 30.0

# The waiting period, in seconds (`identity-succession.md` § The RecoveryKey →
# *Replacement*; the nest's `RECOVERY_REPLACE_GRACE_SECS`).
_WINDOW_S = 30 * 24 * 3600

_T = S.settings.recovery_kit
# The pending line carries a day count; its fixed words before the count are
# what every app renders from the shared string.
_PENDING_HEAD = _T.status_replacement_pending(days="\0").split("\0")[0]


def _reopen(app) -> None:
    """Leave and re-enter the section, so the next read is a fresh chain read."""
    app.settings.navigate()
    app.settings.open_recovery_kit()


def _await_status(app, matches, what: str) -> str:
    """Wait for ``recovery-kit-status`` to satisfy ``matches``; return it."""
    wait_until(
        lambda: matches(app.settings.recovery_kit_status()),
        _CEREMONY_S,
        diagnose=lambda: (
            f"the status line never read {what}; it reads "
            f"{app.settings.recovery_kit_status()!r}, error surface: "
            f"{app.error_text()!r}"
        ),
    )
    return app.settings.recovery_kit_status()


def _seed_hex(user) -> str:
    return bytes(user["signing_key"]).hex()


@pytest.mark.feature("recovery-kit")
def test_the_status_names_each_of_the_four_states_including_a_kit_made_elsewhere(
    succeedable_app, nest_instance
):
    """never created → a kit another device registered (no sealed copy here)
    → repaired → a replacement pending: each reads as its own state.

    Every transition except the repair happens OFF this device, so each reading
    after a fresh navigation can only have come from the chain on the nest.
    """
    app, user = succeedable_app
    url = nest_instance["url"]

    app.settings.navigate()
    app.settings.open_recovery_kit_or_skip()
    _await_status(app, lambda s: s == _T.status_never_created, "never created")

    # Another device registers a kit — through the API, which registers no
    # sealed copy, so this is also the no-escrow state.
    held = register_recovery_kit(
        url, actor_id_hex=user["actor_id_hex"], identity_seed_hex=_seed_hex(user)
    )
    _reopen(app)
    _await_status(
        app,
        lambda s: s == _T.status_registered_no_escrow,
        "registered with no sealed copy (a kit made on another device)",
    )

    app.settings.reseal_escrow_with_held(held)
    _await_status(app, lambda s: s == _T.status_registered, "registered")

    # Another device — or a thief — asks to replace it with the identity alone.
    request_seed_alone_replacement(
        url, actor_id_hex=user["actor_id_hex"], identity_seed_hex=_seed_hex(user)
    )
    _reopen(app)
    _await_status(app, lambda s: s.startswith(_PENDING_HEAD), "replacement pending")
    assert app.driver.is_visible_scrolled("recovery-pending-veto-button"), (
        "the pending state carries its own way out — the veto — beside the line"
    )


@pytest.mark.feature("recovery-kit")
def test_a_kit_whose_phrase_cannot_restore_is_repaired_without_retiring_it(
    succeedable_app, nest_instance
):
    """a registered kit with no sealed copy → the page says the phrase cannot
    restore → repair with the held kit → the SAME kit now restores the account.
    """
    app, user = succeedable_app
    url = nest_instance["url"]
    account = qualified_account(app, nest_instance)

    held = register_recovery_kit(
        url, actor_id_hex=user["actor_id_hex"], identity_seed_hex=_seed_hex(user)
    )
    chain_before = registration_chain(url, user["actor_id_hex"])

    app.settings.navigate()
    app.settings.open_recovery_kit_or_skip()
    _await_status(
        app, lambda s: s == _T.status_registered_no_escrow, "registered, no escrow"
    )
    assert app.driver.is_visible_scrolled("recovery-kit-escrow-reseal-button"), (
        "the no-escrow state renders its own repair; it is missing, error "
        f"surface: {app.error_text()!r}"
    )

    app.settings.reseal_escrow_with_held(held)
    _await_status(app, lambda s: s == _T.status_registered, "registered")
    assert app.driver.is_absent("recovery-kit-escrow-reseal-button"), (
        "the repair renders ONLY in the no-escrow state"
    )
    assert not app.settings.recovery_kit_secret(), (
        "the repair mints nothing, so no new kit may be shown"
    )
    assert registration_chain(url, user["actor_id_hex"]) == chain_before, (
        "the repair must not retire the held kit — the registration chain moved"
    )

    # The proof that matters to the user: the phrase they held all along now
    # brings the account back on a device that has lost everything.
    lose_every_device_and_open_restore(app, nest_instance)
    app.onboarding.restore_from_recovery_kit(held, account=account)
    wait_for_restore_landing(app)
    assert_restored_the_same_account(app)


@pytest.mark.feature("recovery-kit")
def test_a_lost_kit_is_replaced_with_the_identity_alone_and_waits_out_its_window(
    succeedable_app, nest_instance
):
    """create → "I lost my kit" → the new kit is shown at once, the section
    reads replacement pending, and nothing has landed on the chain yet →
    the window runs out → the new kit is the head, the section reads
    registered-no-escrow → repair with the NEW kit → the NEW phrase restores
    the account on a device that has lost everything.

    "Registered, no escrow" after the landing is the specified state, not a
    defect: the nest deletes the escrow row whenever a link changes the
    registered key, the landed seed-alone window included, and the re-put is
    always kit-in-hand (`identity-succession.md` § Seed escrow → *Lifecycle on
    the nest*).
    """
    app, user = succeedable_app
    url = nest_instance["url"]
    account = qualified_account(app, nest_instance)

    app.settings.navigate()
    app.settings.open_recovery_kit_or_skip()
    app.settings.create_recovery_kit()
    app.wait_for("recovery-kit-secret-display", timeout=_CEREMONY_S)
    held = app.settings.recovery_kit_secret()
    chain_before = registration_chain(url, user["actor_id_hex"])

    _reopen(app)
    assert app.is_enabled("recovery-kit-lost-button"), (
        "a registered kit enables the seed-alone replacement; status reads "
        f"{app.settings.recovery_kit_status()!r}"
    )
    requested_at = int(time.time())
    app.settings.replace_lost_kit()

    _await_status(app, lambda s: s.startswith(_PENDING_HEAD), "replacement pending")
    new_kit = app.settings.recovery_kit_secret()
    assert len(new_kit) == _SECRET_HEX_LEN, (
        "the new kit is shown at once — there is no second chance to show it; "
        f"got {len(new_kit)} chars, error surface: {app.error_text()!r}"
    )
    assert new_kit != held, "the replacement is a NEW kit"
    assert app.driver.is_visible_scrolled("recovery-pending-veto-button")

    # Nothing has landed: the chain head is the kit the user no longer has, and
    # the nest is holding a window that closes a waiting period from now.
    assert registration_chain(url, user["actor_id_hex"]) == chain_before, (
        "a seed-alone replacement must wait out its window, but the chain moved"
    )
    pending = replacement_status(
        url, actor_id_hex=user["actor_id_hex"], identity_seed_hex=_seed_hex(user)
    )
    assert pending is not None, "the nest holds no pending replacement"
    lands_at = int(pending["lands_at"])
    assert requested_at + _WINDOW_S - 3600 <= lands_at <= int(time.time()) + _WINDOW_S, (
        f"the window must close a waiting period after the request; lands_at "
        f"is {lands_at - requested_at}s after it"
    )

    # The window runs out uncontested: the nest's own landing sweep, run with
    # a later clock (convention 14 — never wait out the 30 days).
    swept = land_due_replacements(url)
    assert swept["landed"] >= 1, f"the due replacement never landed: {swept}"
    chain_after = registration_chain(url, user["actor_id_hex"])
    assert len(chain_after) == len(chain_before) + 1, (
        "after the window the new kit takes over as the chain head; the chain "
        f"went {len(chain_before)} → {len(chain_after)} links"
    )

    # The landing changed the registered key, so the sealed copy under the old
    # one is gone: the section says the phrase cannot restore yet and offers
    # the kit-in-hand repair.
    _reopen(app)
    _await_status(
        app, lambda s: s == _T.status_registered_no_escrow, "registered, no escrow"
    )
    assert app.driver.is_absent("recovery-pending-veto-button"), (
        "with the window closed nothing pends, so the veto no longer renders"
    )
    assert app.driver.is_visible_scrolled("recovery-kit-escrow-reseal-button"), (
        "the landed kit has no sealed copy, so the section must offer the "
        f"repair; error surface: {app.error_text()!r}"
    )

    app.settings.reseal_escrow_with_held(new_kit)
    _await_status(app, lambda s: s == _T.status_registered, "registered")
    assert registration_chain(url, user["actor_id_hex"]) == chain_after, (
        "the repair must not retire the kit that just took over"
    )

    # The proof that matters to the user: the NEW phrase brings the account
    # back on a device that has lost everything.
    lose_every_device_and_open_restore(app, nest_instance)
    app.onboarding.restore_from_recovery_kit(new_kit, account=account)
    wait_for_restore_landing(app)
    assert_restored_the_same_account(app)


@pytest.mark.feature("recovery-kit")
def test_a_replacement_you_did_not_ask_for_is_cancelled_with_the_kit_you_hold(
    succeedable_app, nest_instance
):
    """create → someone holding the identity secret asks to replace the kit →
    the section reads pending → veto with the held kit → registered again, and
    the nest holds nothing pending.
    """
    app, user = succeedable_app
    url = nest_instance["url"]

    app.settings.navigate()
    app.settings.open_recovery_kit_or_skip()
    app.settings.create_recovery_kit()
    app.wait_for("recovery-kit-secret-display", timeout=_CEREMONY_S)
    held = app.settings.recovery_kit_secret()
    chain_before = registration_chain(url, user["actor_id_hex"])

    # The request this user did not make.
    request_seed_alone_replacement(
        url, actor_id_hex=user["actor_id_hex"], identity_seed_hex=_seed_hex(user)
    )
    _reopen(app)
    _await_status(app, lambda s: s.startswith(_PENDING_HEAD), "replacement pending")

    app.settings.veto_pending_replacement(held)
    _await_status(app, lambda s: s == _T.status_registered, "registered")
    assert app.driver.is_absent("recovery-pending-veto-button"), (
        "with nothing pending, the veto no longer renders"
    )
    assert not app.has_error(), f"the veto left an error: {app.error_text()!r}"
    assert replacement_status(
        url, actor_id_hex=user["actor_id_hex"], identity_seed_hex=_seed_hex(user)
    ) is None, "the nest still holds the replacement the veto cancelled"
    assert registration_chain(url, user["actor_id_hex"]) == chain_before, (
        "the cancelled replacement must never reach the chain"
    )
