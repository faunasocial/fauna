"""An account at its tier's device cap is TOLD so, on the Devices page, with
the remedy — and the remedy taken there clears it.

Owner: ``docs/goal/behavior/devices.md`` § Step 4 — Register for sync (the
refusal: "a typed error the app renders so the user can ask their admin for a
bigger tier") and ``docs/goal/ui/devices.md`` § Errors & edge cases (where it
renders). The quota itself is ``admin.md`` § 2 Users — the tier *is* the quota.

**What went wrong, measured (2026-09-15).** The nest has refused a new device
past ``max_devices`` with the typed ``fauna.sync.device_limit_exceeded`` since
2026-09-02, and nothing on any app said so: ``RpcError::localized`` had no arm
for it (every app showed the generic "something went wrong"), and the shared
register sites — the account runtime's enrollment pass above all — only logged
it. A third device on a free tier (two devices) signed in fine, enrolled
nothing, and synced nothing, with no sentence anywhere telling its owner why or
what to do. This journey drives exactly that: a dedicated actor on a one-device
tier whose one slot is already taken, a sign-in on this app, and the Devices
page naming the cap and both remedies.

**The remedy is taken through the app (convention 8):** the user removes the
device holding the slot with ``device-remove-button``, the account pump's next
pass registers this machine, and the notice comes down. That half is asserted
where this app's runtime holds the pump (the pass is poked, then awaited on
its own completion counter); a non-holder's slot is freed the same way, but the
co-located agent's pass that would clear the notice runs on its own cadence,
which a test must not wait out (convention 14) — so there the journey ends at
the roster having lost the removed device.

⚠ **A DEDICATED actor on a DEDICATED tier, never the shared ``test_user`` or
the ``free`` tier.** The session fixture lifts ``free``'s device cap so the
shared actor never meets this refusal by accident; observing the cap means
admitting an actor of this test's own onto a tier of its own
(``common.auth.ensure_tier`` + ``set_user_tier``), leaving the shared tier alone.

⚠ **Every read is a deadline poll on state, never a settle-sleep** (convention
14). tui hydrates the Devices page on the nav edge only, so the poll re-enters
the page between reads — the enrollment pass that meets the refusal runs
concurrently with the first nav.
"""

from __future__ import annotations

import time

import pytest

from common.auth import ensure_tier, set_user_tier
from helpers import enrollment
from helpers.waiting import (
    account_pump_cycles,
    account_pump_role,
    await_pump_cycle_after,
    poke_account_pump,
    wait_until,
)
from i18n.strings import S

pytestmark = pytest.mark.tier_3

#: The tier this test admits its actor onto: one device, so a single fixture
#: register fills it and this app's own enrollment is the one past the cap.
CAPPED_TIER = "e2e-one-device"

#: The account pump's enrollment pass meeting the refusal after sign-in, then
#: the page hydrating it — nest round trips plus a nav edge, in the same class
#: as ``helpers.enrollment.ENROLLMENT_LATCH_S``. A green run pays only the real
#: latency.
REFUSAL_VISIBLE_S = 240.0

#: How often the poll re-enters the Devices page so tui's nav-edge hydrate
#: re-reads the slot — not a settle, a re-hydrate cadence.
REHYDRATE_EVERY_S = 2.0

#: After the remedy: one poked pass on a holder, then the nav that reads the
#: slot it cleared. Generous by design.
CLEARED_VISIBLE_S = 120.0


def _devices_error_text(app) -> str:
    driver = app.driver
    if not driver.is_visible("error-message"):
        return ""
    return driver.get_text("error-message") or ""


def _wait_on_devices_page(app, predicate, budget_s: float, *, what: str):
    """Deadline-poll ``predicate(error_text)`` on the Devices page, re-entering
    the page every ``REHYDRATE_EVERY_S`` so a nav-edge hydrate re-reads the
    account runtime's slot."""
    app.backups.navigate_devices()
    last_nav = time.monotonic()
    last: dict = {}

    def probe():
        nonlocal last_nav
        if time.monotonic() - last_nav >= REHYDRATE_EVERY_S:
            app.backups.navigate_devices()
            last_nav = time.monotonic()
        text = _devices_error_text(app)
        last["error"] = text
        last["cards"] = app.backups.device_count()
        return predicate(text)

    return wait_until(
        probe,
        budget_s,
        interval=0.5,
        diagnose=lambda: (
            f"{what} (last error-message: {last.get('error')!r}, device-cards painted: "
            f"{last.get('cards')!r}, pump role: {account_pump_role(app.driver)!r}). "
            "grep the app log (and the co-located sync agent's) for 'enrollment:'."
        ),
    )


@pytest.mark.feature("devices")
def test_a_capped_account_is_told_on_the_devices_page_and_the_remedy_clears_it(
    app, request, nest_instance
):
    """One-device tier, slot taken by a fixture device, sign in here: Settings →
    Devices names the cap and both remedies, and this machine holds no row.
    Remove the fixture device from that page: on a pump-holding app the next
    pass enrolls this machine and the notice comes down."""
    from conftest import _make_user

    nest_url = nest_instance["url"]
    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]
    notice = S.error.sync.device_limit_exceeded

    user = _make_user(nest_instance)
    ensure_tier(port, admin_signing_key=admin_sk, name=CAPPED_TIER, base_url=nest_url, max_devices=1)
    set_user_tier(port, user["actor_id_hex"], CAPPED_TIER, admin_signing_key=admin_sk, base_url=nest_url)
    slot_holder = enrollment.register_device(nest_url, user, "the-only-slot")
    assert set(enrollment.roster(nest_url, user)) == {slot_holder}

    # ── The refusal, rendered where the remedy lives ──
    enrollment.sign_in(app, request, nest_instance, user)
    _wait_on_devices_page(
        app,
        lambda text: notice in text,
        REFUSAL_VISIBLE_S,
        what="the Devices page never named the device-cap refusal",
    )
    # Nothing was written past the cap: the roster is still the one fixture
    # device, and this machine's row is exactly what the page says is missing.
    assert set(enrollment.roster(nest_url, user)) == {slot_holder}, (
        "the refused register must not have inserted a row"
    )

    # ── The remedy, through the app: remove the device holding the slot ──
    app.backups.navigate_devices()
    app.driver.wait_for("device-card", timeout=15)
    assert app.backups.device_count() == 1, app.driver.diagnose("device-card")
    app.backups.remove_device(0)
    wait_until(
        lambda: slot_holder not in enrollment.roster(nest_url, user),
        30.0,
        diagnose=lambda: f"the roster still lists the removed device: {enrollment.roster(nest_url, user)!r}",
    )

    role = account_pump_role(app.driver)
    assert role is not None, "guarded by enrollment.sign_in's role check"
    _runtime_up, is_holder = role
    # Which branch ran is part of the run's record (convention 6): a green
    # run must say whether it witnessed the clear or only the freed slot.
    print(
        f"[device-cap] slot freed; pump role={role!r} → "
        f"{'asserting the clear' if is_holder else 'non-holder: clear unwitnessed here'}",
        flush=True,
    )
    if not is_holder:
        # The slot is free; the pass that enrolls this machine and clears the
        # notice belongs to the co-located agent's pump, on its own cadence
        # (the 300 s backstop, or its next reconnect). Waiting that out is the
        # defunct wall-clock wait convention 14 forbids, and the holder branch
        # below proves the same clear on the same shared code.
        return

    baseline = account_pump_cycles(app.driver)[0]
    poke_account_pump(app.driver)
    await_pump_cycle_after(app.driver, baseline, budget_s=60.0, what="the freed slot")
    _wait_on_devices_page(
        app,
        lambda text: notice not in text,
        CLEARED_VISIBLE_S,
        what="the device-cap notice stayed up after the slot was freed and a pass ran",
    )
    # And the machine is enrolled now — its row is the roster.
    _writer, latched_row, roster = enrollment.await_enrollment(app, nest_url, user)
    assert latched_row in roster and slot_holder not in roster, (latched_row, roster)
