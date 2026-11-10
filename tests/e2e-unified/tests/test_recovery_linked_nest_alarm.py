"""tier_3 e2e: a recovery opened at a linked nest raises the alarm on the
device bound to the other nest, and one veto there clears it at both.

Goal doc: ``docs/goal/behavior/succession-linked-nests.md`` § Every nest the
identity is linked to, clause (c) — "a seed-alone replacement is requested at
every linked nest, and each runs its own 30-day window … The reconcile also
reads each linked nest's pending replacement and feeds the standing alert with
it, because a seed holder can open a window at a nest no device is bound to;
the veto contests the window at every nest that holds one." Catalog:
``docs/features/take-your-account-back.md`` outcome 38.

**Why the window is opened at the linked nest ALONE.** A thief holding the
seed can ask at any nest the identity has an account on, and the one the
owner's devices are bound to is the nest the owner would notice. The window
that matters is the one opened somewhere else — which is exactly the case the
shared half was built for (``ceremonies.rs::a_window_at_the_linked_nest_alone_
raises_the_alert_on_a_device_bound_to_the_other``,
``conformance_linked_fanout_gesture.rs::a_window_held_only_at_the_linked_nest_
is_cleared_by_the_veto_at_the_bound_nest``). What no unit or conformance test
reaches is the composition in a real app: the runtime's secondary leg reading
B's window, the session-start sweep posting that reading into the banner, and
the Settings veto offering itself and reaching B.

**How the reading reaches the banner, and why each round pokes the pump.** The
leg runs in the account runtime and records what it read in the process's
linked readings, and the sweep loop's wait posts them the moment they change
(``fauna_client_core::recovery_pending``, module note; ``critical-alerts.md``
§ Mechanism → *How often the detector runs*, ruled 2026-11-07 — before it the
sweep posted them only at its next pass, up to 6 h later). The
leg rides the runtime's full or seed pass — assembly, a reconnect,
``reconcile_now`` and the 300 s backstop (``account-runtime.md``
§ Multi-instance concurrency → *The seed-leg role*) — and a sign-in on an app
already signed in as the same identity re-runs the sweep but NOT the leg, so each round asks the runtime for its pass through
``account_pump_now`` — ``reconcile_now``, the ticker's own work on demand,
which on a seed-holding non-holder (tui beside its sync agent) runs the seed
pass and its secondary leg — and reads the banner again. The poke is
fire-and-forget and counts no seed pass, so a round polls the banner, bounded,
and pokes again (``_poke_until``) — convention 14: a green run pays one or two
pokes, and the budget only bounds a leg that never reads B. No session
establishment and no sweep pass is waited on, which is what the journey
witnesses: a banner that moved here moved on the leg's reading alone. The
negative rounds (the alarm STANDS after a nest's all-clear) keep the sweep's
own pass counter as their anchor (``_sweep_round``): a pass that began after
the round's sign-in has posted the readings as they then stood.

**Fixture state vs. the journey (convention 8).** B's account, the kit, B's
copy of the registration chain and the pairing row are what the link action
would have left; the thief's request at B is the attacker, not the user. Both
are arranged off the app, exactly as
``test_identity_succession_refusal.py::test_the_successors_device_retires_the_
stolen_key_at_a_linked_nest`` arranges them. Everything the owner does — read
the alarm, open Settings, veto with the kit in hand — is a UI gesture.

tier_3: needs real ``fauna-nest`` binaries (two of them).
"""
from __future__ import annotations

import time

import pytest

from helpers.budgets import ACCOUNT_RUNTIME_ASSEMBLY_S, ALERT_SWEEP_PASS_S
from helpers.succession import (
    register_recovery_kit,
    registration_chain,
    replacement_status,
    request_seed_alone_replacement,
    veto_pending_replacement,
)
from helpers.waiting import (
    account_pump_role,
    alert_sweep_passes,
    await_sweep_pass_after,
    poke_account_pump,
    wait_until,
)
from i18n.strings import S

pytestmark = pytest.mark.tier_3

_CEREMONY_S = 30.0
_HEADLINE = S.critical_alerts.recovery_replacement_pending


@pytest.fixture
def linked_nest_b(request, nest_mode, tmp_path_factory):
    """A second nest the dedicated identity also holds an account on."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "recovery-linked-b",
    )
    try:
        yield nest
    finally:
        cleanup()


def _alert_text(app) -> str:
    """Every active alert row's text joined; empty when the banner is absent
    (the presence rule, `critical-alerts.md` § Rendering contract)."""
    if not app.driver.is_visible("critical-alerts"):
        return ""
    return app.driver.get_text("critical-alert") or ""


#: One leg pass against two local nests, then the wait's post: how long a
#: round reads the banner before it pokes again.
_POKE_SETTLE_S = 15.0


def _poke_until(app, done, what: str) -> str:
    """Run the leg until the banner reads ``done(banner_text)``; return the
    banner text.

    Each round pokes the account runtime's pass (the leg's edge — module
    note), reads the banner until it answers or the round's settle passes,
    and pokes again: the leg records what it read at B and the sweep loop's
    wait posts it at once (``critical-alerts.md`` § Mechanism → *How often
    the detector runs*), so no session establishment and no sweep pass is
    waited on — a banner that moved here moved on the leg's reading alone.
    Latency-independent (convention 14): a green run pays one or two pokes,
    and the budget only bounds a leg that never reads B."""
    deadline = time.monotonic() + ACCOUNT_RUNTIME_ASSEMBLY_S
    text = _alert_text(app)
    while not done(text):
        if time.monotonic() >= deadline:
            raise AssertionError(
                f"{what}: no leg pass within {ACCOUNT_RUNTIME_ASSEMBLY_S:.0f}s "
                f"brought it about. Banner reads {text!r}; sweep passes "
                f"{alert_sweep_passes(app.driver)}; pump role "
                f"{account_pump_role(app.driver)}; error surface "
                f"{app.error_text()!r}"
            )
        poke_account_pump(app.driver)
        settle = min(time.monotonic() + _POKE_SETTLE_S, deadline)
        text = _alert_text(app)
        while not done(text) and time.monotonic() < settle:
            time.sleep(0.5)
            text = _alert_text(app)
    return text


@pytest.mark.feature("take-your-account-back")
def test_a_recovery_opened_at_a_linked_nest_raises_the_alarm_and_one_veto_clears_it_at_both(
    request, succeedable_app, nest_instance, linked_nest_b
):
    """linked to B → a thief opens a seed-alone replacement at B alone → the
    device bound to A shows the alarm on the feed page → the owner vetoes in
    Settings on A with the kit in hand → neither nest holds a window, and the
    alarm is gone.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from common.auth import register_user

    app, user = succeedable_app
    nest_a, nest_b = nest_instance, linked_nest_b
    actor_hex = user["actor_id_hex"]
    seed_hex = bytes(user["signing_key"]).hex()

    # ── Arrange: the identity linked to B, both nests holding one chain ──
    register_user(nest_b["port"], actor_hex, admin_signing_key=nest_b["admin"]["signing_key"])
    kit_secret = register_recovery_kit(
        nest_a["url"], actor_id_hex=actor_hex, identity_seed_hex=seed_hex
    )
    with WsRpcAdminClient(
        nest_b["url"], actor_id=bytes.fromhex(actor_hex), signing_key=bytes.fromhex(seed_hex)
    ) as ws:
        for record in registration_chain(nest_a["url"], actor_hex):
            ws.call("fauna.recovery.registration.submit", {"registration": record})
    assert registration_chain(nest_b["url"], actor_hex) == registration_chain(
        nest_a["url"], actor_hex
    ), "control: B must hold A's chain, or B could not park a seed-alone window"
    with WsRpcAdminClient(
        nest_a["url"], actor_id=bytes.fromhex(actor_hex), signing_key=bytes.fromhex(seed_hex)
    ) as ws:
        ws.call(
            "fauna.pair.add",
            {
                "private_nest_id": bytes.fromhex(nest_b["nest_id"]),
                "capabilities": ["mls_pull", "namespace_sync", "post_forward"],
                "nest_url": nest_b["url"],
            },
        )

    # ── The thief asks at B, and only at B ──────────────────────────────
    request_seed_alone_replacement(
        nest_b["url"], actor_id_hex=actor_hex, identity_seed_hex=seed_hex
    )
    assert replacement_status(
        nest_b["url"], actor_id_hex=actor_hex, identity_seed_hex=seed_hex
    ) is not None, "control: B holds the window"
    assert replacement_status(
        nest_a["url"], actor_id_hex=actor_hex, identity_seed_hex=seed_hex
    ) is None, "control: the bound nest holds nothing, so only B's reading can raise the alarm"

    # ── The device bound to A raises the alarm ───────────────────────────
    text = _poke_until(
        app,
        lambda t: _HEADLINE in t,
        "the window at the linked nest B must reach the banner on the device "
        "bound to A (the leg's linked reading, posted by the sweep loop's wait)",
    )
    assert "The replacement takes effect in" in text, (
        f"the detail line rides with the headline; got {text!r}"
    )

    # ── The owner vetoes on A with the kit in hand ──────────────────────
    app.settings.navigate()
    app.settings.open_recovery_kit()
    wait_until(
        lambda: app.driver.is_visible_scrolled("recovery-pending-veto-button"),
        _CEREMONY_S,
        diagnose=lambda: (
            "the alarm names a window at a linked nest, but Settings offers no "
            "veto: the section's status reads the bound nest alone "
            f"({app.settings.recovery_kit_status()!r}), so the one gesture "
            "that contests B's window is unreachable. Error surface: "
            f"{app.error_text()!r}"
        ),
    )
    app.settings.veto_pending_replacement(kit_secret)
    wait_until(
        lambda: app.driver.is_absent("recovery-pending-veto-button"),
        _CEREMONY_S,
        diagnose=lambda: (
            "the veto's re-read status must drop the button; status reads "
            f"{app.settings.recovery_kit_status()!r}, error surface: "
            f"{app.error_text()!r}"
        ),
    )
    assert not app.has_error(), f"the veto left an error: {app.error_text()!r}"

    # ── Neither nest holds a window ─────────────────────────────────────
    assert replacement_status(
        nest_b["url"], actor_id_hex=actor_hex, identity_seed_hex=seed_hex
    ) is None, "the veto on A must contest the window at the linked nest B"
    assert replacement_status(
        nest_a["url"], actor_id_hex=actor_hex, identity_seed_hex=seed_hex
    ) is None, "the bound nest must hold nothing after the veto"

    # ── …and the alarm is gone on the device ────────────────────────────
    _poke_until(
        app,
        lambda t: _HEADLINE not in t,
        "with no window left at any nest the alarm must clear",
    )


# ── critical-alerts outcome 10: the one alert over several nests ──────────
#
# `docs/goal/behavior/critical-alerts.md` § Feeders → *Several nests, one
# alert*: one alert key fed from every nest the identity is linked to; it
# stands while ANY source holds a window, and a transport failure to a nest
# returns instead of clearing. The two arms below are the ones the
# take-your-account-back witness above does not reach (it vetoes through the
# app, which contests every nest at once).


def _link_nest_b(nest_a, nest_b, user) -> str:
    """Arrange what the link action would have left: the identity's account on
    B, the kit and its chain on both, the pairing row on A. Returns the kit
    secret. Off the app on purpose (convention 8): the arrangement is the
    link, not the journey under test."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from common.auth import register_user

    actor_hex = user["actor_id_hex"]
    seed_hex = bytes(user["signing_key"]).hex()
    register_user(nest_b["port"], actor_hex, admin_signing_key=nest_b["admin"]["signing_key"])
    kit_secret = register_recovery_kit(
        nest_a["url"], actor_id_hex=actor_hex, identity_seed_hex=seed_hex
    )
    with WsRpcAdminClient(
        nest_b["url"], actor_id=bytes.fromhex(actor_hex), signing_key=bytes.fromhex(seed_hex)
    ) as ws:
        for record in registration_chain(nest_a["url"], actor_hex):
            ws.call("fauna.recovery.registration.submit", {"registration": record})
    with WsRpcAdminClient(
        nest_a["url"], actor_id=bytes.fromhex(actor_hex), signing_key=bytes.fromhex(seed_hex)
    ) as ws:
        ws.call(
            "fauna.pair.add",
            {
                "private_nest_id": bytes.fromhex(nest_b["nest_id"]),
                "capabilities": ["mls_pull", "namespace_sync", "post_forward"],
                "nest_url": nest_b["url"],
            },
        )
    return kit_secret


def _sweep_round(app, request, nest, user) -> str:
    """One session establishment, anchored on the sweep's own pass counter;
    returns the banner text after the pass that began after the sign-in."""
    from conftest import _login_app_as

    passes = alert_sweep_passes(app.driver)
    assert passes is not None, (
        "the app publishes no `alert_sweep_passes` — the sweep's causal "
        "barrier this journey anchors every round on (convention 11)"
    )
    _login_app_as(app, request, nest, user)
    await_sweep_pass_after(
        app.driver, passes[0], budget_s=ALERT_SWEEP_PASS_S, what="a sweep round"
    )
    app.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
    return _alert_text(app)


@pytest.fixture
def stoppable_nest_b(request, nest_mode, tmp_path_factory):
    """A second nest the test may stop mid-journey: yields ``(nest, stop)``;
    ``stop`` is idempotent and the fixture's teardown calls it too."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "recovery-linked-b-stoppable",
    )
    stopped = []

    def stop():
        if not stopped:
            stopped.append(True)
            cleanup()

    try:
        yield nest, stop
    finally:
        stop()


@pytest.mark.feature("critical-alerts")
def test_the_alarm_stands_while_any_nest_holds_a_window_and_clears_once_none_does(
    request, succeedable_app, nest_instance, linked_nest_b
):
    """A window at the linked nest B raises the alarm → a window at the bound
    nest A joins it → A's window is cancelled alone → the alarm STILL stands
    (B holds one) → B's is cancelled too → the alarm clears.

    B's window is opened first, and the alarm read from it alone, so the
    leg's reading of B is known to be in before A's window is cancelled: with
    both opened at once, A's own reading raises the alarm by itself, and the
    rounds after A's cancel would race the leg's first read of B instead of
    testing the union. The opens and cancels are arrangement (the thief's
    request and the kit holder's veto at one nest each), not the journey: what
    is observed is the banner across them."""
    app, user = succeedable_app
    nest_a, nest_b = nest_instance, linked_nest_b
    actor_hex = user["actor_id_hex"]
    seed_hex = bytes(user["signing_key"]).hex()
    kit_secret = _link_nest_b(nest_a, nest_b, user)
    request_seed_alone_replacement(
        nest_b["url"], actor_id_hex=actor_hex, identity_seed_hex=seed_hex
    )
    _poke_until(
        app,
        lambda t: _HEADLINE in t,
        "the window at B must raise the alarm before A's joins it",
    )

    request_seed_alone_replacement(
        nest_a["url"], actor_id_hex=actor_hex, identity_seed_hex=seed_hex
    )
    text = _sweep_round(app, request, nest_a, user)
    assert _HEADLINE in text, (
        f"windows at both nests must hold the one alarm; banner reads {text!r}, "
        f"error surface {app.error_text()!r}"
    )

    assert veto_pending_replacement(
        nest_a["url"], actor_id_hex=actor_hex, recovery_secret_hex=kit_secret
    ), "control: the kit's veto at A must cancel A's window"
    assert replacement_status(
        nest_a["url"], actor_id_hex=actor_hex, identity_seed_hex=seed_hex
    ) is None, "control: A holds nothing now"
    assert replacement_status(
        nest_b["url"], actor_id_hex=actor_hex, identity_seed_hex=seed_hex
    ) is not None, "control: B still holds its window"
    # Two rounds: which of the leg's read of B and the sweep pass lands first
    # is unordered (module note), and the alarm must stand after BOTH.
    for n in (1, 2):
        text = _sweep_round(app, request, nest_a, user)
        assert _HEADLINE in text, (
            f"round {n}: B still holds a window, so the bound nest reading "
            f"nothing must not clear the alarm; banner reads {text!r}, error "
            f"surface {app.error_text()!r}"
        )

    assert veto_pending_replacement(
        nest_b["url"], actor_id_hex=actor_hex, recovery_secret_hex=kit_secret
    ), "control: the kit's veto at B must cancel B's window"
    _poke_until(
        app,
        lambda t: _HEADLINE not in t,
        "with every nest reading nothing pending the alarm must clear",
    )


@pytest.mark.feature("critical-alerts")
def test_an_unreachable_linked_nest_never_clears_the_alarm(
    request, succeedable_app, nest_instance, stoppable_nest_b
):
    """A window at B alone raises the alarm on the device bound to A → B stops
    answering → A still reads nothing pending, yet the alarm stands: a nest
    that cannot be asked is not an all-clear."""
    app, user = succeedable_app
    nest_a = nest_instance
    nest_b, stop_b = stoppable_nest_b
    actor_hex = user["actor_id_hex"]
    seed_hex = bytes(user["signing_key"]).hex()
    _link_nest_b(nest_a, nest_b, user)
    request_seed_alone_replacement(
        nest_b["url"], actor_id_hex=actor_hex, identity_seed_hex=seed_hex
    )

    _poke_until(
        app,
        lambda t: _HEADLINE in t,
        "the window at B alone must raise the alarm before B goes away",
    )

    stop_b()
    for n in (1, 2):
        text = _sweep_round(app, request, nest_a, user)
        assert _HEADLINE in text, (
            f"round {n}: B cannot be asked, and A reads nothing pending — an "
            f"unreachable nest must leave the standing alarm alone; banner "
            f"reads {text!r}, error surface {app.error_text()!r}"
        )
