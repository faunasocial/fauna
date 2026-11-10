"""tier_3 e2e: the kit in hand is not the only one — two holders, and a kit
that is someone else's.

Goal doc: ``docs/goal/behavior/identity-succession.md`` § Enforcement on the
home nest — the typed outcome's landed-for-another arm ("never presented as
'try again'") and the pre-mint refusal of a kit naming a different account.
Catalog: ``docs/features/take-your-account-back.md`` outcome 50.

A sibling of ``test_identity_succession_ceremony.py``, kept apart so its
witnesses do not collide with that module's. Same opening (``_kit_in_hand``:
create a kit through the UI, come back to the stolen gate), same budgets.

**The other holder is fixture setup** (convention 8's carve-out, the shape
``test_identity_succession_refusal.py`` uses): the device that got there first
is not the device under test, so its succession is signed by
``libs/fauna-client-recovery/examples/recovery_fixture.rs`` and submitted over
the anonymous wire. Everything this device does goes through its UI.

**The race is staged, never raced.** The app's own submit is PARKED at the
nest (``helpers/rpc_hold.py``) after it has read the chain and signed; the
other holder's statement — minted before the hold so only one wire call sits
inside the window — is let through by re-arming the hold with a one-request
pass budget (the parked request stays parked: re-arming leaves the gate in
``Hold``); then the hold is released and the app's submit meets a succession
that already landed. That is the one order in which landed-for-another is the
true answer, reached by arrival order alone (convention 14).

tier_3: needs a real ``fauna-nest`` binary built with ``test-hooks``.
"""
from __future__ import annotations

import secrets

import pytest

from helpers.succession_ceremony import (
    SUCCESSION_AND_RELAUNCH_S,
    require_stolen_gate,
    settled_actor_id,
)
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_3

_SECRET_HEX_LEN = 64
_STATUS_RENDER_S = 30.0
_SUBMIT_KIND = "fauna.recovery.succession.submit"


def _kit_in_hand(app) -> tuple[str, str]:
    """Read the signed-in actor, create a kit through the UI, and come back to
    the stolen gate. Returns ``(old_actor, held_kit)``."""
    app.settings._navigate_subpage("status")
    app.wait_for("account-actor-id", timeout=_STATUS_RENDER_S)
    old_actor = app.settings.actor_id()
    assert len(old_actor) == _SECRET_HEX_LEN, (
        f"the Status page must render the signed-in actor id; got {old_actor!r}, "
        f"error surface: {app.error_text()!r}"
    )
    app.settings.open_recovery_kit()
    app.settings.create_recovery_kit()
    app.wait_for("recovery-kit-secret-display", timeout=30.0)
    held = app.settings.recovery_kit_secret()
    assert len(held) == _SECRET_HEX_LEN, (
        f"the kit is 64-hex, got {len(held)}; error surface: {app.error_text()!r}"
    )
    app.settings.navigate()
    app.settings.open_recovery_kit()
    require_stolen_gate(app)
    return old_actor, held


def _another_account_with_a_kit(nest_instance) -> tuple[str, str]:
    """A second, unrelated account holding its own registered kit — fixture
    state (it is nobody this device signs in as). Returns ``(actor, kit)``."""
    from conftest import _make_user
    from helpers.succession import register_recovery_kit

    other = _make_user(nest_instance)
    kit = register_recovery_kit(
        nest_instance["url"],
        actor_id_hex=other["actor_id_hex"],
        identity_seed_hex=bytes(other["signing_key"]).hex(),
    )
    assert len(kit) == _SECRET_HEX_LEN, f"the kit is 64-hex, got {len(kit)} chars"
    return other["actor_id_hex"], kit


def _assert_nothing_moved(app, nest_instance, *actors: str) -> None:
    """No succession recorded for any of ``actors``, and this device is still
    signed in as the first of them — the refusal came before anything could
    commit."""
    from helpers.succession import succession_statements

    for actor in actors:
        assert succession_statements(nest_instance["url"], actor) == [], (
            f"a refused kit must move nothing, but {actor[:16]}… now has a "
            "recorded succession"
        )
    app.settings._navigate_subpage("status")
    app.wait_for("account-actor-id", timeout=_STATUS_RENDER_S)
    assert settled_actor_id(app) == actors[0], (
        "the device must still be signed in as its own account after a refused "
        f"kit; reads {settled_actor_id(app)!r}, error={app.error_text()!r}"
    )


@pytest.mark.standalone_only
@pytest.mark.feature("take-your-account-back")
def test_when_another_holder_of_the_kit_got_there_first_the_ceremony_says_so_as_final(
    succeedable_app, nest_instance
):
    """another device holding the same kit succeeds first → this device's
    ceremony reads landed-for-another, naming that successor, final — never the
    failure headline a "try again" would follow — and this device is not
    switched to anything."""
    from helpers.rpc_hold import (
        arm_rpc_hold,
        release_rpc_hold,
        rpc_hold_status,
        wait_for_held_rpc,
    )
    from helpers.succession import (
        mint_statement,
        registration_chain,
        submit_statement,
        succession_statements,
    )

    app, user = succeedable_app
    port = nest_instance["port"]
    nest_url = nest_instance["url"]
    old_actor, held = _kit_in_hand(app)

    # The other holder's statement, signed now so the window below holds one
    # wire call only. Same kit, its own freshly minted successor.
    other_successor, other_statement = mint_statement(
        old_actor_id_hex=old_actor,
        recovery_secret_hex=held,
        successor_seed_hex=secrets.token_bytes(32).hex(),
        registrations=registration_chain(nest_url, old_actor),
    )

    holding_before = rpc_hold_status(port, _SUBMIT_KIND)["holding"]
    arm_rpc_hold(port, _SUBMIT_KIND)
    try:
        app.settings.succeed_identity_with_held_kit(held)
        # The app has read the chain, signed, and sent — its submit is parked.
        wait_for_held_rpc(port, _SUBMIT_KIND, count=holding_before + 1)
        # Let exactly one request through: the other holder's. The app's stays
        # parked (the gate is still `Hold`).
        arm_rpc_hold(port, _SUBMIT_KIND, pass_first=1)
        echoed = submit_statement(nest_url, other_statement)
        assert echoed == other_successor, (
            f"the other holder's succession must land first; nest echoed {echoed}"
        )
        assert rpc_hold_status(port, _SUBMIT_KIND)["holding"] == holding_before + 1, (
            "the app's submit must still be parked while the other holder lands, "
            "or the order this test stages is not the order that ran"
        )
    finally:
        release_rpc_hold(port, _SUBMIT_KIND)

    expected = S.settings.recovery_kit.stolen_landed_for_another(actor=other_successor)
    wait_until(
        lambda: app.error_text() == expected,
        SUCCESSION_AND_RELAUNCH_S,
        diagnose=lambda: (
            "a ceremony whose kit another holder already used must paint "
            "settings.recovery_kit.stolen_landed_for_another verbatim, naming "
            f"that successor ({other_successor}); error surface: "
            f"{app.error_text()!r}"
        ),
    )
    failed_headline = S.settings.recovery_kit.stolen_ceremony_failed(message="\x00").split(
        "\x00"
    )[0]
    assert failed_headline not in app.error_text(), (
        "landed-for-another is final: it must never read as a failure the user "
        "could retry"
    )

    # The nest records the other holder's succession and only it: first
    # succession wins, and this device's statement moved nothing.
    statements = succession_statements(nest_url, old_actor)
    assert len(statements) == 1, (
        f"exactly one succession must be recorded for the account, got {len(statements)}"
    )
    # And this device adopted nothing: it holds no key for the other holder's
    # successor, so it is never signed in as it. Its own identity WAS
    # succeeded, so the old session is gone too (measured 2026-11-07: the
    # Status page no longer renders and the launch routes to the import flow
    # the sentence names) — hence a one-sided read, never a wait for Status.
    # Read after the sentence above, since navigating clears it.
    shown = settled_actor_id(app)
    assert shown != other_successor, (
        "the device must not switch to the other holder's successor — it holds "
        f"no key for it; Status reads {shown!r}"
    )


@pytest.mark.feature("take-your-account-back")
def test_a_kit_of_another_account_is_refused_before_anything_moves(
    succeedable_app, nest_instance
):
    """a kit registered for a DIFFERENT account, pasted into this account's
    ceremony → refused before the submit (the kit-mismatch sentence), no
    succession recorded for either account, and this device still signed in as
    itself."""
    app, _user = succeedable_app
    old_actor, _held = _kit_in_hand(app)
    other_actor, other_kit = _another_account_with_a_kit(nest_instance)

    app.settings.succeed_identity_with_held_kit(other_kit)

    expected = S.settings.recovery_kit.stolen_ceremony_failed(
        message=S.errors.recovery_prior_kit_mismatch
    )
    wait_until(
        lambda: app.error_text() == expected,
        SUCCESSION_AND_RELAUNCH_S,
        diagnose=lambda: (
            "a kit belonging to another account must be refused before the "
            "submit with the kit-mismatch sentence; error surface: "
            f"{app.error_text()!r}"
        ),
    )
    _assert_nothing_moved(app, nest_instance, old_actor, other_actor)


@pytest.mark.feature("take-your-account-back")
def test_a_kit_naming_another_account_says_so_rather_than_calling_it_stale(
    succeedable_app, nest_instance
):
    """a kit whose payload NAMES a different account → refused before a
    successor is minted, with a sentence that says it is another account's kit,
    distinguishable from the stale-kit one (identity-succession.md § Enforcement
    on the home nest)."""
    app, _user = succeedable_app
    old_actor, _held = _kit_in_hand(app)
    other_actor, other_kit = _another_account_with_a_kit(nest_instance)

    app.settings.succeed_identity_with_held_kit(
        f"fauna://recovery?secret={other_kit}&actor={other_actor}"
    )

    stale = S.settings.recovery_kit.stolen_ceremony_failed(
        message=S.errors.recovery_prior_kit_mismatch
    )
    failed_headline = S.settings.recovery_kit.stolen_ceremony_failed(message="\x00").split(
        "\x00"
    )[0]
    wait_until(
        lambda: app.error_text().startswith(failed_headline),
        SUCCESSION_AND_RELAUNCH_S,
        diagnose=lambda: (
            "a kit naming another account must be refused; error surface: "
            f"{app.error_text()!r}"
        ),
    )
    assert app.error_text() != stale, (
        "a kit naming another account must say so, not call the kit stale "
        f"(the PriorKitMismatch sentence); reads {app.error_text()!r}"
    )
    _assert_nothing_moved(app, nest_instance, old_actor, other_actor)
