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

import socket

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from conftest import _bridge_admin_post, _provision_baseline_contributor
from helpers.waiting import wait_until

pytestmark = pytest.mark.tier_3


def _ws(nest, contributor):
    return WsRpcAdminClient(nest["url"], actor_id=contributor.actor_id,
                            signing_key=bytes(contributor.recipient["signing_key"]))


def _admin_ws(nest):
    sk = nest["admin"]["signing_key"]
    return WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk))


def _contributors(nest, seal_helper_binary, tag, n=3):
    """`n` fresh opted-in contributors, each with a sealed model, a holder copy
    and a keyless grant to the nest's own aggregation holder."""
    return [
        _provision_baseline_contributor(
            nest_instance=nest, seal_helper_binary=seal_helper_binary,
            local_part=f"withdraw-{tag}-{i}", token=f"qz{tag}{i}wx", spam_messages=5,
        )[0]
        for i in range(n)
    ]


def _publish(nest):
    with _admin_ws(nest) as ws:
        reply = ws.call("fauna.bridges.publish_spam_baseline", {})
    assert reply["published"] is True and not reply.get("deferred"), (
        f"precondition: a baseline over three new contributors must be served; got {reply!r}")
    return reply


def _served(nest):
    with _admin_ws(nest) as ws:
        return ws.call("fauna.bridges.get_spam_baseline_state", {})


def _accepts(port: int) -> bool:
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=1.0):
            return True
    except OSError:
        return False


def _open_mail_gate_and_wait_for_the_bridges(venue, budget_s: float = 120.0) -> None:
    """The admin opens the deployment's mail gate; return once both bridges serve.

    Opening the gate races each bridge's first config fetch (the race
    `test_mail_bridge_mta.py::_open_mail_gate_and_wait_for_the_mta` documents):
    a bridge whose fetch lands after the gate opened serves at once and never
    exits, while one that had already idled exits for rebind and needs the venue
    to play the supervisor. So wait, per role, for whichever state it reaches,
    and rebind only the ones that exited.
    """
    venue.admin_opens_mail_gate()
    roles = {"mta": (venue.mta, venue.mx_port), "mda": (venue.mda, venue.imaps_port)}

    def _states():
        states = {}
        for role, (bridge, port) in roles.items():
            if _accepts(port):
                states[role] = "serving"
            elif bridge.proc.poll() is not None:
                states[role] = "exited"
            else:
                return None
        return states

    states = wait_until(
        _states,
        budget_s,
        interval=0.5,
        diagnose=lambda: "a mail bridge neither served nor exited for rebind after the gate opened",
    )
    exited = {role for role, state in states.items() if state == "exited"}
    if exited:
        venue.rebind_after_enable(mta="mta" in exited, mda="mda" in exited)


@pytest.mark.feature("spam")
def test_departure_withdraws_the_served_baseline_at_once(dedicated_mail_nest, seal_helper_binary):
    venue = dedicated_mail_nest
    nest = venue.nest
    # The venue boots with mail off, and an MDA idling on a shut gate runs no
    # baseline drain — so open the deployment gate (as the admin does) and wait
    # for the bridges to serve again before any publish needs the holder.
    _open_mail_gate_and_wait_for_the_bridges(venue)

    # Arm 1 — stopping the contribution.
    first = _contributors(nest, seal_helper_binary, "optout")
    _publish(nest)
    assert _served(nest)["published"] is True
    with _ws(nest, first[0]) as ws:
        ws.call("fauna.bridges.set_baseline_contribution", {"contribute": False})
    state = _served(nest)
    assert state["published"] is False, (
        f"opting out must withdraw the served baseline at once, before any republish; "
        f"got {state!r}")

    # Arm 2 — resetting the filter.
    second = _contributors(nest, seal_helper_binary, "reset")
    _publish(nest)
    with _ws(nest, second[0]) as ws:
        ws.call("fauna.bridges.reset_spam_model", {})
    state = _served(nest)
    assert state["published"] is False, (
        f"resetting the filter must withdraw the served baseline at once; got {state!r}")

    # Arm 3 — deleting the account. The deletion is a scheduled pending action;
    # the account (and its training) goes when it executes, which the test hook
    # runs through the real executor.
    third = _contributors(nest, seal_helper_binary, "delete")
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
