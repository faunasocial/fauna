"""tier_3: a contributor who stops contributing, resets their filter or deletes
their account takes their training out of the deployment's shared starting point
AT ONCE — the published spam baseline is withdrawn by the departure itself, not
by the admin's next republish (`docs/goal/behavior/mail-spam.md` § Cold start,
*A contributor's departure withdraws the baseline*).

`test_spam_baseline_drain.py` sees a withdrawal only through a republish (its
revoke → publish → withheld arm); this file witnesses the three departures with
no publish between the departure and the read. Each arm stands up a served
baseline over three fresh opted-in contributors (the k-anonymity floor), then
one of them departs and the admin's `get_spam_baseline_state` — the baseline's
current state — must already read `published=false`. Every later arm adds three
new contributors, so its publish clears the delta floor (§ *The floor applies to
every published DELTA*) and lands.

A DEDICATED mail nest (`dedicated_mail_nest`): the account-deletion arm needs the
`pending_actions/run_due` test hook, which runs every due action nest-wide, and
the baseline is a deployment singleton the shared nest's other spam tests read.
Its MDA is the aggregation holder: every model rests sealed, so a baseline is
only ever merged off-box by the granted holder from the contributors' sealed
copies (`_provision_baseline_contributor` — what each contributor's app toggle
does). The MDA drains only once the admin has opened the mail gate, so the test
opens it first. Every step is a synchronous RPC whose reply follows the write — convention
14.
"""

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from conftest import _bridge_admin_post
from helpers.spam_baseline_venue import (
    baseline_contributors,
    open_mail_gate_and_wait_for_the_bridges,
)

pytestmark = pytest.mark.tier_3


def _ws(nest, contributor):
    return WsRpcAdminClient(nest["url"], actor_id=contributor.actor_id,
                            signing_key=bytes(contributor.recipient["signing_key"]))


def _admin_ws(nest):
    sk = nest["admin"]["signing_key"]
    return WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk))


def _publish(nest):
    with _admin_ws(nest) as ws:
        reply = ws.call("fauna.bridges.publish_spam_baseline", {})
    assert reply["published"] is True and not reply.get("deferred"), (
        f"precondition: a baseline over three new contributors must be served; got {reply!r}")
    return reply


def _served(nest):
    with _admin_ws(nest) as ws:
        return ws.call("fauna.bridges.get_spam_baseline_state", {})


@pytest.mark.feature("spam")
def test_departure_withdraws_the_served_baseline_at_once(dedicated_mail_nest, seal_helper_binary):
    venue = dedicated_mail_nest
    nest = venue.nest
    # The venue boots with mail off, and an MDA idling on a shut gate runs no
    # baseline drain — so open the deployment gate (as the admin does) and wait
    # for the bridges to serve again before any publish needs the holder.
    open_mail_gate_and_wait_for_the_bridges(venue)

    # Arm 1 — stopping the contribution.
    first = baseline_contributors(nest, seal_helper_binary, "optout")
    _publish(nest)
    assert _served(nest)["published"] is True
    with _ws(nest, first[0]) as ws:
        ws.call("fauna.bridges.set_baseline_contribution", {"contribute": False})
    state = _served(nest)
    assert state["published"] is False, (
        f"opting out must withdraw the served baseline at once, before any republish; "
        f"got {state!r}")

    # Arm 2 — resetting the filter.
    second = baseline_contributors(nest, seal_helper_binary, "reset")
    _publish(nest)
    with _ws(nest, second[0]) as ws:
        ws.call("fauna.bridges.reset_spam_model", {})
    state = _served(nest)
    assert state["published"] is False, (
        f"resetting the filter must withdraw the served baseline at once; got {state!r}")

    # Arm 3 — deleting the account. The deletion is a scheduled pending action;
    # the account (and its training) goes when it executes, which the test hook
    # runs through the real executor.
    third = baseline_contributors(nest, seal_helper_binary, "delete")
    _publish(nest)
    with _ws(nest, third[0]) as ws:
        ws.call("fauna.account.delete", {})
    assert _served(nest)["published"] is True, (
        "a scheduled deletion inside its cool-off leaves the account, so the "
        "baseline still stands")
    ran = _bridge_admin_post(nest["url"], nest["admin"]["token"],
                             "/api/v1/test/pending_actions/run_due", {})
    assert ran.get("executed", 0) >= 1, f"the account deletion never applied: {ran}"
    state = _served(nest)
    assert state["published"] is False, (
        f"a deleted account's training must leave the served baseline at once; got {state!r}")
