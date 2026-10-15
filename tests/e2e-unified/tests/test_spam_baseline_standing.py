"""tier_3: what the deployment's spam baseline does by itself once the admin
turns standing publish on, and what its state shows the admin
(`docs/goal/behavior/mail-spam.md` § Cold start → *Standing publish*;
`docs/features/admin-mail-policy.md` outcomes 36 and 37).

The nest's own republish runs once a day (`BASELINE_REPUBLISH_INTERVAL`), so
the test drives ONE tick of the production cadence at a time it names, through
the `test-hooks` endpoint `POST /api/v1/test/spam/baseline/standing-tick` — the
same `run_standing_publish_if_due` the cadence task calls with the clock
(convention 14: the time is an argument, never a wait).

A DEDICATED mail venue per test (`dedicated_mail_nest`): the baseline is a
deployment singleton the shared nest's other spam tests read, and the
never-published state is only observable on a deployment that has never
published. Its MDA is the aggregation holder (`helpers/spam_baseline_venue.py`).
Every step is a synchronous RPC whose reply follows the write.
"""

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from conftest import _bridge_admin_post
from helpers.spam_baseline_venue import baseline_contributors, open_mail_gate_and_wait_for_the_bridges

pytestmark = pytest.mark.tier_3

_HOUR_MS = 60 * 60 * 1000
_DAY_MS = 24 * _HOUR_MS
# A tick's publish awaits the holder's merged half (a bounded wait on the nest
# side), so the HTTP budget is wider than the helper's default.
_TICK_HTTP_BUDGET_S = 60.0

# What the admin's read of the baseline carries — the whole of it
# (`mail-spam.md` § Cold start → *What the admin can read*).
_STATE_FIELDS = {"published", "contributors", "sample_count", "published_at",
                 "skipped_contributors", "deferred", "standing"}


def _admin_ws(nest):
    sk = nest["admin"]["signing_key"]
    return WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk))


def _state(nest):
    with _admin_ws(nest) as ws:
        return ws.call("fauna.bridges.get_spam_baseline_state", {})


def _standing(nest, on: bool):
    with _admin_ws(nest) as ws:
        assert ws.call("fauna.bridges.put_spam_policy", {"baseline_standing_publish": on}).get("ok") is True


def _tick(nest, now_ms: int) -> dict:
    return _bridge_admin_post(nest["url"], nest["admin"]["token"],
                              "/api/v1/test/spam/baseline/standing-tick", {"now_ms": now_ms},
                              timeout=_TICK_HTTP_BUDGET_S)


def _now_ms() -> int:
    import time

    return int(time.time() * 1000)


@pytest.mark.feature("admin-mail-policy")
def test_standing_publish_republishes_the_baseline_once_a_day_under_the_floors(
        dedicated_mail_nest, seal_helper_binary):
    """With standing publish on, the nest publishes the baseline by itself; an
    hour later nothing runs; a day later, with three new contributors, it
    republishes over all six; a day after that, with nothing changed, the delta
    floor defers it and the served baseline stays; and with standing publish
    off, the baseline is withdrawn and a due tick runs nothing."""
    venue = dedicated_mail_nest
    nest = venue.nest
    open_mail_gate_and_wait_for_the_bridges(venue)
    baseline_contributors(nest, seal_helper_binary, "first")

    t0 = _now_ms()
    assert _tick(nest, t0) == {"ran": False}, "standing publish off: the cadence runs nothing"
    assert _state(nest)["published"] is False

    _standing(nest, True)
    first = _tick(nest, t0)
    assert first["ran"] is True and first["published"] is True, f"the first standing run publishes: {first!r}"
    state = _state(nest)
    assert state["published"] is True and state["contributors"] == 3 and state["standing"] is True, state
    first_published_at = state["published_at"]

    assert _tick(nest, t0 + _HOUR_MS) == {"ran": False}, "an hour later the republish is not due"
    assert _state(nest)["published_at"] == first_published_at

    baseline_contributors(nest, seal_helper_binary, "second")
    second = _tick(nest, t0 + _DAY_MS + _HOUR_MS)
    assert second["ran"] is True and second["published"] is True, f"a day later it republishes: {second!r}"
    state = _state(nest)
    assert state["contributors"] == 6 and state["published_at"] > first_published_at, (
        f"the republish is over all six contributors and newer: {state!r}")
    second_published_at = state["published_at"]

    third = _tick(nest, t0 + 2 * _DAY_MS + 2 * _HOUR_MS)
    assert third["ran"] is True and third["deferred"] is True, (
        f"a daily run with no contribution changed is deferred by the delta floor: {third!r}")
    state = _state(nest)
    assert state["published"] is True and state["published_at"] == second_published_at, (
        f"a deferred run keeps serving what the box had: {state!r}")
    assert state["deferred"] is True, state

    _standing(nest, False)
    assert _state(nest)["published"] is False, "turning standing publish off withdraws the baseline"
    assert _tick(nest, t0 + 4 * _DAY_MS) == {"ran": False}, "standing publish off: nothing runs"
    assert _state(nest)["published"] is False


@pytest.mark.feature("admin-mail-policy")
def test_baseline_state_shows_only_what_is_served_never_who_or_when_withdrawn(
        dedicated_mail_nest, seal_helper_binary):
    """The admin's read of the baseline carries only what is served now: no
    field naming a contributor, and once a contributor's departure withdraws
    it, the read is the same as on a deployment that never published — no
    withdrawal time, no withdrawal reason."""
    venue = dedicated_mail_nest
    nest = venue.nest
    open_mail_gate_and_wait_for_the_bridges(venue)

    never = _state(nest)
    assert set(never) == _STATE_FIELDS, f"the read carries exactly the documented fields: {never!r}"

    contributors = baseline_contributors(nest, seal_helper_binary, "privacy")
    with _admin_ws(nest) as ws:
        reply = ws.call("fauna.bridges.publish_spam_baseline", {})
    assert reply["published"] is True, f"precondition: three contributors publish: {reply!r}"
    served = _state(nest)
    assert set(served) == _STATE_FIELDS, f"a served baseline adds no field: {served!r}"
    assert served["published"] is True and served["contributors"] == 3, served
    actor_ids = {bytes(c.actor_id) for c in contributors}
    assert not any(isinstance(v, (bytes, bytearray)) and bytes(v) in actor_ids for v in served.values()), (
        f"no contributor is named: {served!r}")

    with WsRpcAdminClient(nest["url"], actor_id=contributors[0].actor_id,
                          signing_key=bytes(contributors[0].recipient["signing_key"])) as ws:
        ws.call("fauna.bridges.set_baseline_contribution", {"contribute": False})
    withdrawn = _state(nest)
    assert set(withdrawn) == _STATE_FIELDS, f"a withdrawal adds no field: {withdrawn!r}"
    served_now = {k: withdrawn[k] for k in ("published", "contributors", "sample_count", "published_at")}
    never_served = {k: never[k] for k in ("published", "contributors", "sample_count", "published_at")}
    assert served_now == never_served, (
        f"withdrawn must read as never published — no time, no reason: {withdrawn!r} vs {never!r}")
