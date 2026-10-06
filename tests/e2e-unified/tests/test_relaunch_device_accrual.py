"""A relaunch must not make the app a new device: one actor, N relaunches, one roster.

Owner: ``docs/goal/architecture/apps/sync-agent-credentials.md`` § Implementation
status today (the RULED 2026-09-13 entry), and
``docs/goal/architecture/e2e-conventions.md`` convention 10 — its principal-slot
carve-out.

**What went wrong, measured (history).** The store writer key lives in the
per-actor slot of the ``fauna-account-store`` credential namespace. The linux and
tui drivers give every launch a fresh credential dir, so every relaunch minted a
new writer key; windows keeps its credential dir across a relaunch, but the
``reset()`` that follows every relaunch erased the slot all the same. Under the
two-row shape the account runtime enrolled each such key on a fresh
writer-pub-hex placeholder row that nothing retired, and the module-boundary
relaunch — once per test module — walked a whole-suite linux run's session actor
into the nest's 64-device tier cap about a third of the way in.

A real install keeps that slot across a restart. The drivers model that: the
actor's principal slot is restored at its first sign-in after a relaunch
(``drivers/http_bridge.py``, the principal-slot carry) — **together with its
account-store replica** (2026-09-15): a writer lives exactly as long as its
journal (``account-replica-posture.md`` § The store device principal,
refinement 11), so a slot restored over a fresh dir is a key the app abandons and
re-mints. Since the one-credential shape (``sync-agent-credentials.md``
§ Credential model, RULED 2026-09-28) a lost slot no longer adds a row — the
fresh key re-enrolls on the machine's named row — so of the two assertions below
the WRITER-KEY one is what catches a lost carry, and the roster one pins that a
relaunch still never adds a row. This test relaunches through the
module-boundary contract itself — the path a whole-suite run takes. Its sibling
below does the same for an un-forced sign-in, whose NAMED row's id the app
derives from the install device secret — convention 10's second survivor (the
RULED 2026-09-20 entry of ``sync-agent-credentials.md`` § Credential model).

⚠ **A DEDICATED actor, never the shared ``test_user``** — the count must be exact,
and the shared actor's roster is the whole session's. ``test_user`` is still
requested: its fixture lifts the device cap on the tier every test user shares,
so a regression grows this roster instead of meeting a two-device refusal first.

⚠ **The roster is read behind a causal barrier, never a settle-sleep** (convention
14; the reads are shared with the sign-out cycle test in ``helpers/enrollment.py``): only once THIS launch's enrollment has latched — the slot's
``grant-registered`` record names a row the nest lists; read any earlier and a
regression passes on a roster that has not grown YET. The writer-key comparison
closes the other side: an app that ignored the restored slot and minted anyway
would satisfy the barrier on the restored record before its own pass ran.
"""

from __future__ import annotations

import pytest

from helpers import enrollment, module_relaunch
from helpers.app_surface import declared_absence

pytestmark = pytest.mark.tier_3

#: Relaunches per run. One would pin the defect; three make "one new row per
#: relaunch" read as a rate rather than a coincidence, for three more app boots.
RELAUNCHES = 3


def test_relaunching_against_one_actor_adds_no_device_row(app, request, nest_instance, test_user):
    """N cold relaunches, one signed-in actor: the nest's roster for that actor
    after the last relaunch is the roster after the first sign-in, and the store
    writer key is the one the first launch minted.

    Before the principal-slot carry each relaunch minted a writer key because the
    fresh credential dir held no slot — under the two-row shape, one new
    ``fauna``-labelled placeholder row per relaunch, the rate that exhausted the
    session actor's device cap; under one row, a fresh principal on the same
    row, which the writer-key assertion catches."""
    from conftest import _make_user

    driver = app.driver
    if not driver.supports_cold_relaunch():
        declared_absence(
            driver,
            capability="a cold relaunch of the app process",
            doc=(
                "docs/goal/architecture/e2e-conventions.md § The conventions, "
                "convention 10 — an app that cannot relaunch is a declared absence "
                "(tests/test_module_relaunch.py pins which)"
            ),
        )

    nest_url = nest_instance["url"]
    user = _make_user(nest_instance)
    enrollment.sign_in(app, request, nest_instance, user)
    machine_key, _row, baseline = enrollment.await_enrollment(app, nest_url, user)

    # The relaunches ride the fixture's own module-boundary decision, so the
    # names below are boundaries it has never seen. Restore the real one after,
    # or the next test in this module pays one more relaunch for nothing.
    real_module = getattr(driver, "_e2e_last_module", None)
    try:
        for n in range(1, RELAUNCHES + 1):
            outcome = module_relaunch.at_module_boundary(driver, f"{__name__}#relaunch-{n}")
            assert outcome is not None and "FAILED" not in outcome, (
                f"relaunch {n} did not cold-relaunch the app: {outcome!r}"
            )
            driver.reset()
            enrollment.sign_in(app, request, nest_instance, user)
            writer, _row, roster = enrollment.await_enrollment(app, nest_url, user)

            added = {row: roster[row] for row in sorted(set(roster) - set(baseline))}
            assert not added, (
                f"relaunch {n} of {RELAUNCHES} made the app a NEW device for the same "
                f"actor: {added} joined the roster {sorted(baseline)}. The one-credential "
                "shape (sync-agent-credentials.md § Credential model, RULED 2026-09-28) "
                "enrolls every launch on the machine's named row, so any new row is a "
                "second row that shape rules out."
            )
            assert writer == machine_key, (
                f"relaunch {n} of {RELAUNCHES} kept the roster but NOT the store writer "
                "key: the app minted a fresh one instead of loading the restored slot — "
                "the relaunch lost the actor's principal slot, and the drivers' "
                "principal-slot carry (drivers/http_bridge.py) did not restore it."
            )
    finally:
        driver._e2e_last_module = real_module


def test_relaunching_an_un_forced_sign_in_keeps_its_named_row(
    app, request, nest_instance, test_user
):
    """N cold relaunches, one actor signing in with an id the app REFUSES to adopt
    (``enrollment.UNFORCED_DEVICE_ID`` — the production get-or-create, which some
    forty-five test files reach by accident): the app's own device id after the
    last relaunch is the one the first launch registered, and the roster did not
    grow.

    The case above forces the device id through the session patch, so the named
    row is the same one every launch *by construction*. An un-forced sign-in
    derives it from the install device secret and the account (the RULED
    2026-09-20 entry of ``sync-agent-credentials.md`` § Credential model), and the
    harness's fresh dirs per launch minted a fresh secret each time — one new
    named row per relaunch per un-forced actor, the per-launch accrual the
    principal carry had closed for the placeholder. The drivers now carry the
    secret at launch (``drivers/http_bridge.py``, the install-device-secret carry).

    **The barrier** is the app's own persisted id appearing in the nest's roster
    (``enrollment.await_named_row``), behind the enrollment latch — never a roster
    read on its own, which a regression passes while its new row is in flight."""
    from conftest import _make_user

    driver = app.driver
    if not driver.supports_cold_relaunch():
        declared_absence(
            driver,
            capability="a cold relaunch of the app process",
            doc=(
                "docs/goal/architecture/e2e-conventions.md § The conventions, "
                "convention 10 — an app that cannot relaunch is a declared absence "
                "(tests/test_module_relaunch.py pins which)"
            ),
        )

    nest_url = nest_instance["url"]
    user = _make_user(nest_instance)

    def sign_in_unforced() -> tuple[str, dict[str, str]]:
        enrollment.sign_in(
            app, request, nest_instance, user, device_id=enrollment.UNFORCED_DEVICE_ID
        )
        enrollment.await_enrollment(app, nest_url, user)
        return enrollment.await_named_row(app, nest_url, user)

    named_row, baseline = sign_in_unforced()

    real_module = getattr(driver, "_e2e_last_module", None)
    try:
        for n in range(1, RELAUNCHES + 1):
            outcome = module_relaunch.at_module_boundary(driver, f"{__name__}#un-forced-{n}")
            assert outcome is not None and "FAILED" not in outcome, (
                f"relaunch {n} did not cold-relaunch the app: {outcome!r}"
            )
            driver.reset()
            own, roster = sign_in_unforced()

            assert own == named_row, (
                f"relaunch {n} of {RELAUNCHES}: the un-forced sign-in registered under a NEW "
                f"device id {own}; the machine's named row is {named_row} (roster "
                f"{sorted(roster)}). The relaunch lost the install device secret the id is "
                "derived from — the drivers' install-device-secret carry "
                "(drivers/http_bridge.py) did not lay it down before the app started."
            )
            added = {row: roster[row] for row in sorted(set(roster) - set(baseline))}
            assert not added, (
                f"relaunch {n} of {RELAUNCHES} kept the named row but the roster still grew: "
                f"{added} joined {sorted(baseline)}."
            )
    finally:
        driver._e2e_last_module = real_module
