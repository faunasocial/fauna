"""The two-nest custody journeys' shared steps, over ``two_nest_custody_rig``.

Every journey that needs a NEST-anchored custody on that rig walks the same
opening — a cross-nest 1:1 carrier, alice's offer, bob's accept on his nest —
and drives the custodian's pump the same way: one ``run-now`` poke per pass,
both seats' receive cycles poked first so the ceremony payloads keep flowing
(convention 14: state, never a cadence). Lifted from the kit-only restore
journey (``test_custody_recovery_copy_journey.py``) when the cold-device
restore journey (``test_custody_cold_device_restore.py``) needed the same
steps.
"""

from __future__ import annotations

import time

import requests

from helpers.budgets import MLS_HANDSHAKE_S, RECEIVE_CYCLE_S
from helpers.waiting import (
    await_receive_cycle_after,
    conv_receive_cycles,
    poke_receive_cycle,
    wait_until,
)
from tests.api import conv_api

#: The custody ceremony plus the custodian pump's first pulls, poked every try.
CUSTODY_S = 240.0


def receive_once(app) -> None:
    """One poked receive cycle, completion-anchored."""
    cycles = conv_receive_cycles(app.driver)
    started = cycles[0] if cycles else None
    poke_receive_cycle(app.driver)
    await_receive_cycle_after(
        app.driver, started, budget_s=RECEIVE_CYCLE_S, what="the ceremony payload"
    )


def pump_once(custodian: dict) -> dict:
    """One custodian-nest custody-hosting pass; returns its report."""
    resp = requests.post(
        f"{custodian['url']}/api/v1/test/custody_hosting/run-now",
        timeout=120,
        verify=False,
    )
    assert resp.status_code == 200, (
        f"custody_hosting run-now returned {resp.status_code}: {resp.text}"
    )
    return resp.json()


def custody_log_tail(app) -> str:
    """The custody/recovery lines from one seat's app stderr (convention 6)."""
    try:
        text = app.driver.app_stderr_text() or ""
    except Exception as e:  # noqa: BLE001 — diagnostics must not mask the assert
        return f"(app stderr unreadable: {e})"
    wanted = ("custody", "recovery", "escrow", "generation", "custodian")
    lines = [ln for ln in text.splitlines() if any(w in ln for w in wanted)]
    return "\n".join(lines[-60:]) or "(no custody lines in the app log)"


def open_carrier(alice_app, bob_app, bob: dict, custodian: dict) -> None:
    """The ceremony's carrier: a real 1:1 MLS conversation alice opens with
    bob ACROSS the nests (the mint's host list derives from 2-member conv
    channels), bound on bob's side before any ceremony payload rides it. Bob's
    key-package pool must settle first — a >=1 read races his login top-up."""
    quiet_s = 3.0
    deadline = time.time() + MLS_HANDSHAKE_S
    last, stable_since, settled = None, None, False
    while time.time() < deadline:
        count = conv_api.keypackage_count(custodian["port"], bob, bob["actor_id_hex"])
        now = time.time()
        if count != last:
            last, stable_since = count, now
        elif count >= 1 and now - stable_since >= quiet_s:
            settled = True
            break
        time.sleep(0.5)
    assert settled, f"bob's key-package pool never settled (last {last!r})"

    alice_app.conversations.real_resolve_send_new(
        f"{bob['handle']}@{custodian['authority']}", "custody carrier"
    )
    receive_once(bob_app)


def poll_devices(app, predicate, *, what: str, budget_s: float = CUSTODY_S) -> None:
    """Deadline-poll a Devices-page predicate, a receive cycle poked and the
    page re-entered each try (the facet folds on the nav edge)."""
    def ready() -> bool:
        receive_once(app)
        app.backups.navigate_devices()
        return predicate()

    wait_until(ready, budget_s, interval=0.5, diagnose=lambda: f"never observed: {what}")


def pump_until(rig: dict, predicate, *, what: str) -> dict:
    """Poke the custodian's pump (both seats' receive cycles first, so the
    ceremony payloads keep flowing) until ``predicate(report)`` holds."""
    last: dict = {}

    def converged() -> bool:
        receive_once(rig["fleet_app"])
        receive_once(rig["bob_app"])
        last.clear()
        last.update(pump_once(rig["custodian"]))
        return predicate(last)

    wait_until(
        converged,
        CUSTODY_S,
        interval=0.5,
        diagnose=lambda: (
            f"never observed: {what}; last pass report: {last}"
            f"\n--- bob's custody log ---\n{custody_log_tail(rig['bob_app'])}"
            f"\n--- alice's custody log ---\n{custody_log_tail(rig['fleet_app'])}"
        ),
    )
    return dict(last)


def mint_nest_custody(rig: dict) -> None:
    """The custody, through both UIs: alice offers bob to hold her copies; bob
    takes it on HIS NEST ("My nest"), so the hosting row lands on the
    custodian nest and the accept carries its address."""
    alice_app, bob_app = rig["fleet_app"], rig["bob_app"]  # alice's fleet seat
    bob, custodian = rig["bob"], rig["custodian"]
    open_carrier(alice_app, bob_app, bob, custodian)
    alice_app.backups.navigate_devices()
    # A cross-nest thread is labelled by the handle alice addressed it to,
    # where a same-nest one shows the peer's hex (the select refuses an
    # unoffered value loudly, listing what it painted).
    alice_app.custody.mint_offer(f"{bob['handle']}@{custodian['authority']}")
    poll_devices(
        bob_app,
        lambda: bob_app.custody.offer_count() >= 1
        and bob_app.custody.offer_has_nest_choice(0),
        what="the consent card WITH the nest choice on bob's Devices page",
    )
    bob_app.custody.accept_offer_on_nest(0)
