"""Tier_3 journey: while the home nest is down, an owner device gets its
account's records from a friend's nest that holds custody — with nothing to do —
goes back to the home nest once it answers, and never asks a friend whose
custody was revoked.

Goal docs: ``docs/goal/architecture/account-data-plane.md`` § Implementation
status today, ruling 6 of the custodian-NEST serve door (an ordinary pass runs
the nest arm only when its own-nest walk was ``NestPath::Unavailable`` —
``fauna_sync_engine::custodian_nest_restore::NestArmTrigger::runs``, called
from the pass in ``account_runtime.rs`` through ``peer_leg::custodian_nest_arm``;
on web ``fauna_account_plane::web_host::WebLegs::run`` through
``custodian_restore::pass_arm_with_derived_view``, gated on the same shared
reading, ``custodian_restore::own_nest_answered``; a revoked grant in the
dialer's own view is withheld before any dial,
``fauna_account_plane::custodian_restore::nest_custodian_arm``). The catalog
outcome is ``docs/features/nests-and-trust.md`` outcome 28.

The mechanism is pinned in Rust over two real nests
(``bins/fauna-nest/tests/conformance_custody_nest_serve_client.rs``,
``an_owner_device_restores_through_a_custodian_nest_only_when_its_own_nest_is_down``),
which constructs the custody and calls the arm directly. Only this journey
proves the chain a user lives: a custody minted through both apps' UIs, the
owner app's own passes, the home nest really gone and really back, and a revoke
from the custodian-nest row on the owner's Nests page (where a nest-anchored
custody renders — ``nests.md`` § Trust facet — custody rows).

**What tells whether the friend's nest was asked.** The custodian nest's own
count of the custodied-fleet handshake (``GET
/api/v1/test/custody_hosting/handshakes``, test-hooks only): every handshake
request that reached it (``asked``) and every one it admitted with a session
(``admitted``). Read on the nest, it answers whichever process on the owner's
machine dialed — the app or its co-located sync agent, whichever holds the
pump (``helpers.fleet.poked_pass`` follows the role) — so no app counter is
needed. An admitted handshake is the custodied-fleet door opened to that
device: the arm then walks the account-state planes over it.

Topology: ``two_nest_custody_rig`` — alice home on TLS nest A, bob on TLS nest
B, B's periodic pump held so every pump pass is this test's own ``run-now``
poke (convention 14). A is stopped and started in place on its own process
handle (``common.nest.stop_nest`` / ``start_nest_in_place``: the same port and
data dir, so alice's recorded nest URL survives). Every wait is a deadline poll
on state with a poked pass per try; the flat assertions are made across a fixed
number of poked passes, never a wall-clock window.

**Two roles, one device on most apps.** ``fleet_app`` mints the custody (the
ceremony); ``alice_app`` is the device under test. They are one app everywhere
but web, whose ceremony is a declared absence (``ui/devices.md`` § Where logic
lives): there the rig mints from a tui seat, signs the web app in on A as a
second fleet device (``sign_in_alice_app``), and — once the custody row has
reached the web app — retires the tui seat (``retire_fleet_seat``), because an
enrolled tui device would itself dial the friend's nest while A is down and
answer for the web app.
"""

from __future__ import annotations

import pytest
import requests

from common.nest import start_nest_in_place, stop_nest
from helpers.connection import wait_until_online
from helpers.custody_rig import (
    CUSTODY_S,
    custody_log_tail,
    mint_nest_custody,
    pump_until,
    receive_once,
)
from helpers.fleet import poked_pass
from helpers.waiting import account_pump_role, wait_until

pytestmark = [
    pytest.mark.tier_3, pytest.mark.tui, pytest.mark.linux, pytest.mark.android,
    pytest.mark.web, pytest.mark.macos, pytest.mark.ios,
]

#: One poked pass of alice's pump, by whichever process holds it.
_PASS_S = 120.0

#: Passes over which "the friend's nest was not asked" must hold.
_QUIET_PASSES = 3


def _handshakes(custodian: dict) -> dict:
    """The custodian nest's custodied-fleet handshake tally."""
    resp = requests.get(
        f"{custodian['url']}/api/v1/test/custody_hosting/handshakes",
        timeout=30,
        verify=False,
    )
    assert resp.status_code == 200, (
        f"custody_hosting handshakes returned {resp.status_code}: {resp.text}"
    )
    body = resp.json()
    return {"asked": int(body["asked"]), "admitted": int(body["admitted"])}


def _poll_nests(app, predicate, *, what: str, settle) -> None:
    """Deadline-poll a Nests-page predicate on ``app``, ``settle`` run (the
    pokes that move the state along) and the page re-entered each try (the
    custody facet folds on the nav edge)."""

    def ready() -> bool:
        settle()
        app.linked_nests.navigate()
        return predicate()

    wait_until(
        ready, CUSTODY_S, interval=0.5,
        diagnose=lambda: (
            f"never observed: {what}; custodian-nest rows "
            f"{app.nest_trust.custody_count()}, error surface {app.error_text()!r}"
            f"\n--- alice's custody log ---\n{custody_log_tail(app)}"
        ),
    )


def _pass(app, what: str) -> None:
    poked_pass(app.driver, what=what, budget_s=_PASS_S)


def _diagnosis(rig: dict, what: str, seen: dict) -> str:
    app = rig["alice_app"]
    return (
        f"never observed: {what}; custodian handshakes {seen}; alice's pump role "
        f"(runtime, holder) = {account_pump_role(app.driver)}; error surface "
        f"{app.error_text()!r}\n--- alice's custody log ---\n{custody_log_tail(app)}"
    )


def _assert_not_asked(rig: dict, *, why: str) -> None:
    """Across ``_QUIET_PASSES`` poked passes, nobody asked the custodian nest."""
    custodian, app = rig["custodian"], rig["alice_app"]
    before = _handshakes(custodian)
    for i in range(_QUIET_PASSES):
        _pass(app, f"alice's quiet pass {i + 1} ({why})")
    after = _handshakes(custodian)
    assert after["asked"] == before["asked"], (
        f"{why}: the friend's nest was asked across {_QUIET_PASSES} passes "
        f"(handshakes {before} → {after})\n"
        f"--- alice's custody log ---\n{custody_log_tail(app)}"
    )


@pytest.mark.real_conversations
@pytest.mark.feature("nests-and-trust")
def test_devices_fall_back_to_a_custodian_nest_only_while_the_home_nest_is_down(
    two_nest_custody_rig,
):
    rig = two_nest_custody_rig
    alice_app, home, custodian = rig["alice_app"], rig["home"], rig["custodian"]
    fleet_app = rig["fleet_app"]
    # The ceremony is the minter's; the device under test needs none.
    fleet_app.custody.require_supported()
    rig["sign_in_alice_app"]()

    # ── The custody, through both UIs, held on bob's NEST; B pulls once so it
    # holds alice's planes before her nest goes away.
    mint_nest_custody(rig)
    pump_until(rig, lambda r: r.get("pulled", 0) >= 1,
               what="B's first pull of alice's account from her home nest")

    def ceremony_then_pass() -> None:
        receive_once(fleet_app)
        _pass(alice_app, "alice's pass carrying the custody row to this device")

    _poll_nests(
        alice_app,
        lambda: alice_app.nest_trust.custody_count() >= 1,
        what="bob's nest as a custodian-nest row on alice's Nests page",
        settle=ceremony_then_pass,
    )
    # From here every handshake the friend's nest counts is the device under
    # test's own (on web the tui minter goes; elsewhere it IS the device).
    rig["retire_fleet_seat"]()

    # ── Home nest up: an ordinary pass never asks the friend's nest.
    _assert_not_asked(rig, why="the home nest answers")

    # ── Home nest down: the next passes ask the friend's nest and are admitted
    # — alice does nothing but keep the app open.
    stop_nest(home)
    start = _handshakes(custodian)
    seen: dict = {}

    def admitted_while_down() -> bool:
        _pass(alice_app, "alice's pass with her home nest down")
        seen.update(_handshakes(custodian))
        return seen["admitted"] > start["admitted"]

    wait_until(
        admitted_while_down, CUSTODY_S, interval=0.5,
        diagnose=lambda: _diagnosis(
            rig, "the friend's nest admitting alice's device while her nest is down",
            seen),
    )

    # ── Home nest back: once a pass reaches it, the friend's nest is not asked
    # again. The first passes after the restart may still find the connection
    # down (the app reconnects on its own schedule), so the wait is for one
    # pass that leaves the count alone, then the flat check over several.
    start_nest_in_place(home)
    wait_until_online(alice_app.driver)
    settled: dict = {}

    def one_quiet_pass() -> bool:
        before = _handshakes(custodian)
        _pass(alice_app, "alice's pass after her home nest came back")
        settled.update(_handshakes(custodian))
        return settled["asked"] == before["asked"]

    wait_until(
        one_quiet_pass, CUSTODY_S, interval=0.5,
        diagnose=lambda: _diagnosis(
            rig, "a pass after the home nest came back that left the friend's nest alone",
            settled),
    )
    _assert_not_asked(rig, why="the home nest answers again")

    # ── Revoked: alice takes bob's custody back from its row on her Nests page
    # (nest-first — her nest is up), then her nest goes down again; the
    # friend's nest is never asked.
    alice_app.linked_nests.navigate()
    alice_app.nest_trust.revoke_custody(0)
    _poll_nests(
        alice_app,
        lambda: alice_app.nest_trust.custody_count() == 0,
        what="bob's custodian-nest row leaving alice's Nests page after revoke",
        settle=lambda: _pass(alice_app, "alice's pass after the revoke"),
    )
    stop_nest(home)
    _assert_not_asked(rig, why="the custody was revoked and the home nest is down")
