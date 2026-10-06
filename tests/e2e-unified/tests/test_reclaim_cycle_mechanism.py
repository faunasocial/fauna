"""tier_3 self-check for the shared ``--reclaim-cycle`` mechanism (TRACK 2).

The ``--reclaim-cycle`` opt-in (wired in conftest's ``nest_instance``) puts the
session's shared nest through ONE real ``factory_reset → re-claim (same identity)``
cycle BEFORE any test or downstream fixture (``test_user`` / ``mail_bridge_*`` /
``logged_in_app`` / ``admin_app``) materialises — so every suite that rides the
shared nest exercises **post-reclaim** provisioning, the broad sibling of the
bespoke ``test_factory_reset_calendar_reclaim.py``.

This test pins that contract directly, in BOTH directions:

* **flag on** — the shared nest must actually have been reset-cycled and come back
  claimed. Without this assertion a refactor that silently turns the flag into a
  no-op would make every ``reclaim_cycle``-marked suite "pass" against a *fresh*
  nest, hiding the exact regressions the mode exists to catch (a green
  ``--reclaim-cycle`` run that never re-claimed is worse than no run).
* **flag off** — the mechanism must be inert: the default path other sessions
  share must see a plain first-claim nest, never a silently reset-cycled one.

Client-independent: it asserts nest-level facts only (no UI driver), so it runs
once regardless of ``--client`` and is *kept* under a ``--client linux`` run (it
is not in conftest's client-independent file list, so it is not deselected).
"""

import pytest

from clients.ws_rpc_anon_client import WsRpcAnonClient

pytestmark = [pytest.mark.tier_3, pytest.mark.reclaim_cycle]


def test_reclaim_cycle_flag_cycles_the_shared_nest(request, nest_instance):
    reclaim_on = request.config.getoption("--reclaim-cycle")
    cycled = bool(nest_instance.get("reclaim_cycled"))

    if not reclaim_on:
        # Default path (no flag): the opt-in must NOT have leaked — the shared
        # nest is a plain first-claim nest, exactly as every non-reclaim session
        # (the overwhelming majority) expects.
        assert not cycled, (
            "nest_instance was reclaim-cycled without --reclaim-cycle — the "
            "opt-in leaked into the default path that other sessions share"
        )
        return

    # Flag on: the shared nest MUST have been through the reset cycle...
    assert cycled, (
        "--reclaim-cycle was set but nest_instance was NOT reclaim-cycled — the "
        "flag is a no-op. Every reclaim_cycle suite would run on a FRESH nest and "
        "the post-reclaim regressions this mode exists to catch would be invisible."
    )
    # ...re-claimed with the same admin identity (the cycle restores admin)...
    assert nest_instance.get("admin") is not None, (
        "reclaim-cycled nest has no admin — the re-claim (same identity) step "
        "did not run"
    )
    # ...and report claimed=True over the wire (proves the re-claim took, not just
    # that the dict was stamped).
    with WsRpcAnonClient(nest_instance["url"]) as anon:
        status = anon.call("fauna.setup.status", {})
    assert status.get("claimed") is True, (
        f"post-reclaim nest should report claimed=True; setup.status={status!r}"
    )
