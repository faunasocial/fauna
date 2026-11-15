"""tier_3 e2e: the successor's owed nests in Settings, and the dismiss that
ends one on the user's word.

Goal docs: ``docs/goal/behavior/succession-linked-nests.md`` § The road →
*What the dial does with an entry it cannot land*, its second rule (*the user
ends what the runtime cannot*, ruled 2026-11-07); ``docs/goal/ui/settings.md``
§ Recovery kit → *The successor's owed nests*.

The journey: the user links a second nest by its address (the pairing row
that names it, with that address), takes a kit, and that second nest goes away.
Then the user takes the account back. The succession burns the pairing and
keeps its destination as an owed nest; Settings lists it with the address
and what this device's delivery found (the nest is gone, so: unreachable),
and the dismiss settles it — read back by re-entering the section, which
re-reads the list from the nest, so a green here means the nest no longer
serves the entry, not merely that the app hid it.

tier_3: two real ``fauna-nest`` binaries. tui first (the lead app); the other
six join with the trickle-down batch.
"""
from __future__ import annotations

import pytest

from common.auth import register_user
from helpers.app_surface import require_surface
from helpers.succession_ceremony import (
    require_stolen_gate,
    wait_for_successor_actor,
)
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_3

_SECRET_HEX_LEN = 64

# Everything below is one session read or one pump pass on a live nest; the
# ceilings are generous and a green run pays nothing for them (convention 14).
_STATUS_RENDER_S = 30.0
_LINK_S = 20.0
# The owed list renders on the Account hydrate, and the outcome needs one pass
# of the successor's runtime to have dialed the gone nest: the backstop pass
# is every five minutes, but every reconnect and the first sign-in pass also
# deliver, so the first pass after the relaunch is the one this waits on.
_OWED_OUTCOME_S = 420.0
_DISMISS_S = 30.0


@pytest.fixture(scope="function")
def owed_nest_b(request, nest_mode, tmp_path_factory):
    """The second nest the user links and then loses — started by the mode
    provider like every harness nest, and stopped by the test itself (its
    own cleanup, called early) so the owed entry's address is unreachable."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "owed-nest-b",
    )
    state = {"stopped": False}

    def stop() -> None:
        if not state["stopped"]:
            state["stopped"] = True
            cleanup()

    try:
        yield nest, stop
    finally:
        stop()


@pytest.mark.feature("take-your-account-back")
def test_a_successor_sees_the_nest_its_recovery_has_not_reached_and_dismisses_it(
    ungranted_app, nest_instance, owed_nest_b
):
    app = ungranted_app
    require_surface(app.driver, "recovery-kit-owed-nest-item")
    nest_b, stop_nest_b = owed_nest_b

    app.settings._navigate_subpage("status")
    app.wait_for("account-actor-id", timeout=_STATUS_RENDER_S)
    old_actor = app.settings.actor_id()
    assert len(old_actor) == _SECRET_HEX_LEN, (
        f"the Status page must render the actor id; got {old_actor!r}, "
        f"error surface: {app.error_text()!r}"
    )

    # ── Precondition, through the UI: link nest B by its ADDRESS, so the
    #    pairing row the succession burns carries one.
    register_user(
        nest_b["port"], old_actor, admin_signing_key=nest_b["admin"]["signing_key"],
    )
    app.linked_nests.navigate()
    app.linked_nests.link(nest_b["url"])
    assert app.linked_nests.wait_for_pairing_count(1, timeout=_LINK_S), (
        f"the link must add one pairing; error: {app.linked_nests.page_error_text()!r}"
    )

    # The kit, while B is up (its ceremony carries the chain to the linked nest).
    app.settings.navigate()
    app.settings.open_recovery_kit()
    app.settings.create_recovery_kit()
    app.wait_for("recovery-kit-secret-display", timeout=30.0)
    held = app.settings.recovery_kit_secret()
    assert len(held) == _SECRET_HEX_LEN, f"error surface: {app.error_text()!r}"

    # B goes away: the owed entry's address now answers nothing.
    stop_nest_b()

    app.settings.navigate()
    app.settings.open_recovery_kit()
    require_stolen_gate(app)
    app.settings.succeed_identity_with_held_kit(held)
    new_actor = wait_for_successor_actor(app, old_actor)
    assert new_actor != old_actor

    # ── The list: one entry, B's address, and what the delivery found.
    def read() -> tuple[int, list[str], list[str]]:
        app.settings.navigate()
        app.settings.open_recovery_kit()
        return (
            app.settings.owed_nest_count(),
            app.settings.owed_nest_addresses(),
            app.settings.owed_nest_outcomes(),
        )

    unreachable = S.settings.recovery_kit.owed_nest_unreachable
    seen: dict = {}

    def shows_unreachable() -> bool:
        seen["read"] = read()
        count, _addresses, outcomes = seen["read"]
        return count == 1 and outcomes == [unreachable]

    wait_until(
        shows_unreachable,
        _OWED_OUTCOME_S,
        diagnose=lambda: (
            f"owed list (count, addresses, outcomes) = {seen.get('read')!r}, "
            f"line={app.settings.owed_nests_status()!r}, error={app.error_text()!r}"
        ),
    )
    count, addresses, _ = seen["read"]
    assert addresses == [nest_b["url"]], (
        f"the entry names the address the pairing recorded; got {addresses!r}"
    )
    assert app.settings.owed_nests_status(), "the section's one line renders"

    # ── The dismiss: settled on the user's word, read back off the nest.
    app.settings.dismiss_owed_nest(0)
    wait_until(
        lambda: read()[0] == 0,
        _DISMISS_S,
        diagnose=lambda: f"owed list after dismiss = {read()!r}, error={app.error_text()!r}",
    )
    assert not app.settings.owed_nests_status(), (
        "nothing owed → no line (the honest empty state)"
    )
