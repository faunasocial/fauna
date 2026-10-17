"""A discarded driver retires its machine: throw-away launches on a shared
account leave no live grant behind.

Owner: ``docs/goal/architecture/e2e-launch-isolation.md`` § The convention —
convention 10, *A discarded driver retires its machine* (ruled 2026-10-13); the
gate it protects is ``fleet-scope-reclamation.md`` § Fleet-scope reclamation,
clause (1) — a walk mark counts at the retention gate while its key holds a live
grant on the account.

**What went wrong, measured.** A test that builds its own driver, signs it into
a SHARED actor (``helpers/shared_identity.py`` — ``test_user`` or the session
nest's admin) and tears it down used to walk away signed in. Its grant stayed
live, so its last walk mark kept counting and pinned the gate's watermark for
the rest of the run: every retire above it was withheld. In a 2026-10-13
reproduction over the first 122 modules of ``--app linux``, nine launches of
the account-switcher and add-account tests pinned the admin's watermark at
seq 285 inside two minutes, and the admin's fleet scope grew to 1,034 live rows
from 90 writers by a quarter of the way through — each later admin test's
sign-in leaving its ~25 rows unretired. The 2026-10-12 whole-suite sweep's
session account met the nest's 4096-entry cap the same way.

**What is counted.** Rows of the account that still carry a principal — the
exact set whose walk marks the gate counts (``retirable_through_seq`` admits a
mark only while a ``sync_devices`` row holds its key). A count, never a
duration (convention 14): read once each teardown has returned, behind the
sign-out's own barrier (the app acknowledges its reset only after the
enrollment's retirement round trip).

⚠ **A DEDICATED actor registered as shared for the test's own duration** —
never ``test_user`` itself, whose roster is the whole session's. ``test_user``
is still requested: its fixture lifts the device cap on the tier every test
user shares, so each discarded machine can hold its own row.
"""

from __future__ import annotations

import pytest

from helpers import enrollment
from helpers.app_surface import app_name, skip_unbuilt
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_3

#: Discarded launches per run. One would pin the defect; three make "one live
#: grant per discarded machine" read as a rate rather than a coincidence.
DISCARDS = 3


def _device_id(n: int) -> str:
    """A distinct forced device id per launch, so each discarded machine is its
    own row — one row per machine, as `helpers/fleet.py::SiblingSeats` does it.
    On one shared id every launch would re-grant the SAME row and a leak could
    never count past one."""
    return "d15ca4d0" * 7 + f"d15ca4{n:02x}"


def _launch_signed_in(app, request, nest_instance, user, n: int):
    """A fresh driver of the app under test, signed into ``user`` with its
    enrollment latched on a row that holds a grant. Returns ``(driver, row)``;
    the caller tears the driver down."""
    from actions import ActionLayer
    from conftest import _build_app_config
    from drivers import create_driver

    name = app_name(app.driver)
    driver = create_driver(name)
    driver.launch(_build_app_config(name, nest_instance, request))
    if not driver._discards_machine_at_teardown():
        driver.teardown()
        skip_unbuilt(
            driver,
            surface="the discarded-machine retirement (a driver-owned principal store)",
            detail=(
                "this driver keeps no per-launch principal slot, so its teardown "
                "cannot tell which shared actors its machine still holds"
            ),
            tracked=(
                "docs/goal/architecture/e2e-launch-isolation.md § The convention — "
                "convention 10, A discarded driver retires its machine (its Owed sentence)"
            ),
        )
    seat = ActionLayer(driver)
    enrollment.sign_in(seat, request, nest_instance, user, device_id=_device_id(n))
    _writer, row, _roster = enrollment.await_enrollment(seat, nest_instance["url"], user)
    granted = enrollment.principals(nest_instance["url"], user)
    assert granted.get(row), (
        f"launch {n}: the enrollment latched on row {row} but the nest lists it with no "
        f"grant ({granted}) — the arrangement this test counts never happened"
    )
    return driver, row


def test_discarded_machines_leave_no_live_grant_on_a_shared_account(
    app, request, nest_instance, test_user
):
    """N throw-away launches signed into a shared account, each torn down: after
    every teardown no row of the account carries a principal — none of them is
    left counting at the fleet scope's retention gate."""
    from conftest import _make_user
    from helpers import shared_identity

    nest_url = nest_instance["url"]
    user = _make_user(nest_instance)
    shared_identity.remember_shared_actor(
        user["actor_id_hex"], nest_url=nest_url, signing_key=user["signing_key"]
    )
    try:
        for n in range(1, DISCARDS + 1):
            driver, row = _launch_signed_in(app, request, nest_instance, user, n)
            driver.teardown()
            seen: dict = {}

            def no_live_grant():
                listed = enrollment.principals(nest_url, user)
                seen.clear()
                seen.update(listed)
                return not any(listed.values())

            wait_until(
                no_live_grant,
                enrollment.RETIREMENT_VISIBLE_S,
                interval=1.0,
                diagnose=lambda: (
                    f"discard {n} of {DISCARDS}: after the teardown the account still lists "
                    f"rows holding a grant: { {d: p for d, p in seen.items() if p} } (this "
                    f"launch's row was {row}). Each one is a walk mark the fleet scope's "
                    "retention gate keeps counting, so every later retire above it is "
                    "withheld. The driver's teardown did not retire the machine — "
                    "drivers/http_bridge.py::_retire_discarded_machine; grep the run's "
                    "output for \"[e2e] discarded machine\"."
                ),
            )
    finally:
        shared_identity.forget_shared_actor(user["actor_id_hex"])


def test_a_discarded_machine_on_a_dedicated_account_keeps_its_grant(
    app, request, nest_instance, test_user
):
    """The scope's other edge: a dedicated actor's machine is left exactly as its
    test stopped it. Journeys that stop a device to model a LOST one, and then
    remove it through the UI (`test_device_member_removal.py`), rest on this."""
    from conftest import _make_user

    nest_url = nest_instance["url"]
    user = _make_user(nest_instance)
    driver, row = _launch_signed_in(app, request, nest_instance, user, 1)
    driver.teardown()
    listed = enrollment.principals(nest_url, user)
    assert listed.get(row), (
        f"a dedicated account's discarded machine lost its grant (row {row}: {listed}). "
        "Retirement is for SHARED actors only — a dedicated actor's test decides what "
        "its stopped device means, and a lost-device journey removes it itself."
    )
