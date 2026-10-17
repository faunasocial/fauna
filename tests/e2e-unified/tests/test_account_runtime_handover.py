"""The engine-role handover between an app and its co-located sync agent,
witnessed end to end: the agent takes the role the app yields, and the app
takes it back when the agent stops.

`docs/goal/architecture/account-runtime.md` § Multi-instance concurrency →
*The agent holds the role when present — the presence lock and the yield*,
parts (3)–(4), and that bullet's "Proofs the build owes".

**What this catches that nothing else can.** The Rust tier pins each half on
an in-process rig (`conformance_account_runtime.rs`): two runtimes, one store
dir, a presence lock the test takes and drops by hand. None of it can see the
two real processes — the app's shared host layer publishing the nudge seat the
agent's pipe answers, the agent's mount actually taking `agent.lock`, the kernel
actually releasing both locks when the agent process exits. A host that wires
the yield and never the nudge leaves the role unheld until the agent's own
30 s-class tick; an agent whose `Shutdown` path leaked the presence lock would
keep every app out for good. Both read green on every rig.

Unix only by its observation path: the agent is a direct child under e2e
(`agent_spawner::ChildSpawner`) whose stderr lands in this launch's `app.err`,
and its control socket is per-launch (`sync_agent_socket`). An app with no
co-located agent declares the absence; an app with one this witness cannot
reach yet routes through `skip_unbuilt` (convention 7).

⚠ Never assert a wall-clock delay here (convention 14): the measured yield →
take is about 200 ms, but every wait below is a named budget + deadline poll on
a state or a log line only the asserted path prints.
"""

from __future__ import annotations

import pytest

from helpers import budgets
from helpers.app_surface import app_name, declared_absence, skip_unbuilt
from helpers.sync_agent_ipc_unix import shutdown_agent
from helpers.waiting import (
    account_pump_role,
    account_runtime_role_or_skip,
    poke_account_pump,
    wait_until,
)

pytestmark = pytest.mark.tier_3

# The agent's INFO line on the NUDGE path only (`pipe_server.rs`,
# `handle_reconcile_account_runtime`): its own tick takes the role silently, so
# this line is the nudge's signature, not the take's.
AGENT_TOOK_ON_NUDGE = "account pass nudge: took the engine role an app's runtime yielded"
# The app's stand-down (`account_driver::stand_down!`), every wake that yields.
APP_HANDED_OVER = "engine role handed over"
# The app's re-election win, on the tick ("previous holder exited") or on
# `reconcile_now` ("reconcile-now") alike.
APP_ACQUIRED = "engine singleton acquired"

# From sign-in to a settled role: the agent's mount (about 6 s on an idle box,
# measured 2026-10-08) plus the app's presence poll, which doubles from 500 ms
# toward the backstop interval. Generous by design — the assertion is "the
# agent ends up holding", never "quickly".
SETTLE_S = 180
# The agent's take after the yield, once the role has settled. The nudge line
# is written before the reply, so it is in the log by the time the role reads
# settled; the budget only covers the log write reaching the file.
NUDGE_LINE_S = 30
# From the agent's exit to the app's re-election win, the app poked each round.
TAKE_BACK_S = 120


def _agent_socket_or_skip(driver) -> str:
    """This launch's agent control socket, or the convention-7 skip that says
    why this app cannot run the witness."""
    socket_path = getattr(driver, "sync_agent_socket", None)
    if socket_path:
        return socket_path
    if app_name(driver) in ("web", "android", "ios"):
        declared_absence(
            driver,
            capability="co-located sync agent (the handover has no second process)",
            doc=(
                "account-runtime.md § Multi-instance concurrency → The agent holds "
                "the role when present (web has no agent; a phone's probe finds the "
                "presence lock free and the first-come election stands)"
            ),
        )
    skip_unbuilt(
        driver,
        surface="the handover witness's agent seam (a per-launch agent control "
        "socket the harness can send Shutdown to, and the agent's log in "
        "app_stderr_text)",
        detail=(
            "the windows agent logs to its own file and serves a pipe "
            "(windows_sync_agent.shutdown_agent is that half); tui on macOS "
            "exposes no per-launch socket yet"
        ),
        tracked=(
            "account-runtime.md § Implementation status today, the 2026-10-08 "
            "entry"
        ),
    )


def _role_diagnosis(driver) -> str:
    """The role, and every handover line this launch logged — convention 6."""
    log = driver.app_stderr_text()
    lines = [
        line for line in log.splitlines()
        if "engine role" in line or "engine singleton" in line
        or "account pass nudge" in line or "the sync agent hosts this store" in line
    ]
    tail = "\n    ".join(lines[-20:]) or "(none)"
    return f"role now {account_pump_role(driver)}; handover lines in app.err:\n    {tail}"


def _first_assembled_role(driver):
    """The first ``(True, is_holder)`` read after sign-in, polled tightly: the
    app's hold lasts only until its agent mounts, so a coarse poll would read
    the settled role and lose the yield this witness is about."""
    def read():
        role = account_pump_role(driver)
        return role if role and role[0] else None
    return wait_until(
        read,
        budgets.ACCOUNT_RUNTIME_ASSEMBLY_S,
        interval=0.1,
        diagnose=lambda: (
            "the app never assembled an account-store runtime — "
            + _role_diagnosis(driver)
        ),
    )


def _await_agent_holds(driver, *, what: str):
    """Wait until the app reads assembled and NOT the holder — on a box whose
    agent is up, the settled role (the agent holds it, by rule)."""
    wait_until(
        lambda: account_pump_role(driver) == (True, False),
        SETTLE_S,
        interval=0.25,
        diagnose=lambda: (
            f"{what}: the app never settled as a non-holder beside its agent. "
            "Still (True, True) with no 'engine role handed over' line → the "
            "app's presence re-read never saw the agent (did the agent mount? "
            "is agent.lock taken?); handed over but still no take → read the "
            "agent's lines. " + _role_diagnosis(driver)
        ),
    )


def test_the_agent_takes_the_engine_role_the_app_yields(logged_in_app):
    """Part (4): the agent takes the role the app yields, on the app's nudge.

    The app's runtime usually wins the election at sign-in — the agent's mount
    comes seconds later — so the app reads ``(True, True)`` first, sees the
    agent's presence lock on its next re-read, stands down and nudges the agent
    for a pass. The agent's take on that nudge logs a line its own tick never
    prints, so the line proves the NUDGE did the take, whatever the latencies.

    When the agent won outright (it mounted before the app's runtime assembled)
    there is no yield to witness: only the settled role is asserted.

    What turns it red: the host layer no longer nudging on the holder →
    non-holder edge (the agent then takes the role only at its own tick, and
    silently), or the agent's nudge arm no longer taking a free role.
    """
    driver = logged_in_app.driver
    _agent_socket_or_skip(driver)
    account_runtime_role_or_skip(driver)

    # The log BEFORE the role: a (True, True) read then proves the yield (and
    # so the take) came after this baseline.
    takes_before = driver.app_stderr_text().count(AGENT_TOOK_ON_NUDGE)
    first = _first_assembled_role(driver)

    _await_agent_holds(driver, what="after sign-in")

    # Which branch ran, in the captured output (`-rP`): a green run that never
    # saw the app hold proved the settle alone.
    print(f"handover witness: first assembled role {first}")
    if first != (True, True):
        return  # the agent won at start: no yield happened, the settle is the claim

    wait_until(
        lambda: driver.app_stderr_text().count(AGENT_TOOK_ON_NUDGE) > takes_before,
        NUDGE_LINE_S,
        diagnose=lambda: (
            "the app held the role at sign-in and has since settled as a "
            f"non-holder, but the agent never logged {AGENT_TOOK_ON_NUDGE!r}: "
            "the nudge took nothing (the app's (True, False) is its own view — "
            "the role may sit unheld until the agent's own tick takes it, "
            "silently). Look for the app's 'asking the agent for its pass' "
            "(DEBUG) line: absent → the host layer did not nudge; present → "
            "the agent's ReconcileAccountRuntime arm did not take the free "
            "role. " + _role_diagnosis(driver)
        ),
    )


def test_the_app_takes_the_engine_role_back_when_the_agent_stops(logged_in_app):
    """Part (2)'s other edge: with its agent gone, the app contends again and
    holds the role.

    The agent is stopped through its own ``Shutdown`` verb and waited out to
    process exit, which is what releases its ``agent.lock`` and ``engine.lock``
    (both flocks, closed with the process). The app is then poked each round —
    ``reconcile_now``, one of the two sites (with the backstop tick) where a
    non-holder re-tries the election — until its re-election win is in the log
    or the role reads ``(True, True)``.

    **The respawn.** The app's provisioning loop finds the socket absent on its
    next convergence tick and spawns a fresh agent, which mounts seconds later
    and takes the role back by the same rule. So the app's ``(True, True)`` is a
    window, not a resting state, and the take-back is asserted on its log line
    too: a win the respawned agent has already reversed still counts. The one
    ordering that would read red falsely — the respawned agent mounting before
    the first poke after the exit — needs a full convergence tick plus a mount
    inside one poll interval.

    What turns it red: the app's presence probe still reading the dead agent as
    present (a leaked ``agent.lock``), or a non-holder that no longer re-tries.
    """
    driver = logged_in_app.driver
    socket_path = _agent_socket_or_skip(driver)
    account_runtime_role_or_skip(driver)
    _first_assembled_role(driver)
    _await_agent_holds(driver, what="before the agent is stopped")

    acquired_before = driver.app_stderr_text().count(APP_ACQUIRED)
    stopped_pid = shutdown_agent(socket_path)

    def took_back():
        poke_account_pump(driver)
        return (
            account_pump_role(driver) == (True, True)
            or driver.app_stderr_text().count(APP_ACQUIRED) > acquired_before
        )

    wait_until(
        took_back,
        TAKE_BACK_S,
        interval=0.5,
        diagnose=lambda: (
            f"the agent (pid {stopped_pid}) exited, yet the app, poked each round, "
            "never took the engine role back. Every re-try logged the agent as "
            "present → the presence probe reads a lock the dead agent cannot "
            "hold (leaked, or a respawned agent mounted first: check for a new "
            "'account pass nudge' line); no line at all → the non-holder no "
            "longer re-tries on reconcile_now. " + _role_diagnosis(driver)
        ),
    )
