"""A **signed-in** app that is force-quit must relaunch reset-ready (7-app contract).

`testing.md` § point 10 — the `app` fixture cold-relaunches the session-scoped
app at every test-MODULE boundary via `driver.recover()`, then `reset()`s it for
the incoming module. That pair is the contract; this file is its only end-to-end
proof for the case that actually breaks.

**Why a dedicated test, when relaunch is already exercised everywhere.** It
isn't — not this shape. The two existing native relaunch proofs both dodge the
failing precondition:

  * `test_account_switcher_windows.py::…_ghost…` relaunches **mid-onboarding**,
    before any account is registered. With no active account there is nothing
    for launch-collision detection to probe, so the branch under test here never
    runs.
  * `tests/test_module_relaunch.py` is tier_1 over a fake driver — it proves the
    boundary *decision* (who relaunches, how often), never that a real relaunched
    process comes back.

So the state every real sweep is in after its first `logged_in_app` module — a
registered, signed-in account whose lock file and scoped store exist — reached
`recover()` with **no test covering it at all**. That gap is what let row 39
 run for four sessions: a relaunch that strands
the new process in the launch-collision chooser never starts its TestAgent, so
every later `reset()` dies as "App did not acknowledge reset within 10.0s" and
the fixture reports the rest of the session as environmental skips — coverage
loss that reads like success. windows is where it bites because windows'
`recover()` is the one that deliberately **reuses** the data dir
(`drivers/windows.py`) — it relaunches into its own predecessor's app data
rather than a fresh one, so a survivor still holding that state is reachable in
a way it is not elsewhere; the contract is 7-app, so the test is too. (Its
credential store is emptied per relaunch like every other driver's, so the
relaunched app itself comes back signed out — which is why the precondition
below is asserted BEFORE the relaunch, not after.)

The assertions deliberately mirror the fixture's own call sequence rather than
probing something adjacent: `recover()` then `reset()`, in that order, because
those are the exact two calls whose failure the fixture converts into a skip.

**What this test is and is not.** It is a *contract pin*: run as its own module
it launches, signs in, relaunches and resets for real (green on windows). It is
NOT the detector for the surviving-instance bug — when that bug is live, the
`app` fixture's own module-boundary relaunch fails *before* this test's body ever
runs, so this reports `s`, not red. The detector is the fixture's skip plus
`WindowsDriver.recover()`'s failure report (app liveness, the epoch-fence delta,
the `[launch-*]` lines, and the relaunched process's own log). The Success
criterion is therefore that this test passes *inside* a multi-module sequence,
not merely alone.
"""

from __future__ import annotations

import pytest

from helpers.app_surface import declared_absence

pytestmark = pytest.mark.tier_3


@pytest.mark.feature("connect-and-sign-in")
def test_a_signed_in_app_relaunches_reset_ready(logged_in_app):
    app = logged_in_app
    driver = app.driver

    if not driver.supports_cold_relaunch():
        declared_absence(
            driver,
            capability="a relaunchable app session (module-boundary cold relaunch)",
            doc="architecture/e2e-conventions.md § point 10 — android's bridge keeps "
            "no relaunchable session; the absence is pinned by "
            "tests/test_module_relaunch.py",
        )

    # Precondition, asserted rather than assumed: the whole point of this test is
    # that the app is SIGNED IN across the relaunch. A fixture that quietly landed
    # unauthenticated would exercise the mid-onboarding path the existing tests
    # already cover, and pass for the wrong reason.
    session = driver.get_state("session") or {}
    assert session.get("authenticated"), (
        "logged_in_app must be authenticated before the relaunch — otherwise this "
        "test silently degrades into the mid-onboarding case that is already covered"
    )

    # ── The module-boundary relaunch, exactly as helpers/module_relaunch.py makes
    # it. A False here is the bridge refusing to launch on top of a survivor
    # (flaui-bridge/SessionManager.cs::Quit), which is itself a real failure —
    # never a reason to skip. ──
    assert driver.recover(), (
        "the module-boundary relaunch of a SIGNED-IN app failed. On windows this "
        "is the bridge refusing to relaunch over a surviving instance that still "
        "holds the account's state; every later test in the session would be "
        "reported as an environmental skip"
    )

    # ── Exactly ONE app process may own this session's state afterwards. ──
    #
    # This is the assertion whose absence let row 39 run for four sessions. A
    # relaunch that leaves its predecessor alive still returns True here — the new
    # process starts fine — and the damage lands on the *next* launch, which finds
    # the account lock held and shows the chooser instead. So "recover() succeeded"
    # is not the property; "there is one instance" is, and only the OS can say so
    # (a leaked instance is invisible to every in-app observable — that is what made
    # the bug present as an unrelated per-test timeout three relaunches later).
    #
    # Latency-independent by construction (convention 14): a count, taken after a
    # call that has already returned, with no wall-clock reasoning anywhere.
    instances = driver.live_app_instance_count()
    if instances is not None:
        assert instances == 1, (
            f"{instances} app processes own this session's isolated state after a "
            "relaunch — the harness leaked an instance. The survivor holds the "
            "account's instance lock, so the NEXT relaunch will land in the "
            "launch-collision chooser, never start its TestAgent, and turn every "
            "later module into an environmental skip"
        )

    # ── And it must come back ready. This is the call that times out when the
    # relaunched process never starts its TestAgent (e.g. it is sitting in the
    # launch-collision chooser): reset() polls /app/state for its own ack, which a
    # process that never configured its agent can never publish. ──
    driver.reset()

    # reset() returns only on an acked, unauthenticated app; re-read it so the
    # failure names the state rather than the absence of an exception.
    after_session = driver.get_state("session") or {}
    assert not after_session.get("authenticated", False), (
        "the relaunched app acked reset but is still authenticated — the incoming "
        "module would start against its predecessor's session"
    )
