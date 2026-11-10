"""tier_1 proofs for the staging box's placement fallback in
``helpers/staging_box.py``: the ordered locations the drive buys at, and a buy
the provider refused before creating anything not counting against the
certificate budget (``org-nests.md`` § fauna.build → *Disposable*, rule 3).
"""
from __future__ import annotations

import pytest

from helpers import staging_box as sb

pytestmark = pytest.mark.tier_1

_REFUSED = {"overall": "Failed", "final_error": None, "result": None, "steps": [
    {"kind": "Server", "status": "Failed",
     "last_error": "412 resource_unavailable (error during placement)"}]}


def test_a_placement_refusal_at_the_server_step_is_recognised():
    assert sb.placement_refused(_REFUSED)
    assert not sb.placement_refused({**_REFUSED, "overall": "Succeeded"})
    assert not sb.placement_refused({**_REFUSED, "result": {"server_id": "1"}})
    other = {"overall": "Failed", "result": None, "steps": [
        {"kind": "Dns", "status": "Failed", "last_error": "412 something"}]}
    assert not sb.placement_refused(other)


def test_the_order_is_fsn1_then_nbg1_for_a_day_after_a_refusal():
    t0 = "2026-11-08T17:00:00Z"
    assert sb.placement_location({}, at=t0) == "fsn1"
    st = sb.note_placement_refused({}, location="fsn1", bought_at=t0, at=t0)
    assert sb.placement_location(st, at="2026-11-09T16:59:59Z") == "nbg1"
    assert sb.placement_location(st, at="2026-11-09T17:00:00Z") == "fsn1"
    both = sb.note_placement_refused(st, location="nbg1", bought_at=t0, at=t0)
    assert sb.placement_location(both, at="2026-11-08T18:00:00Z") == "fsn1"


def test_a_refused_buy_leaves_the_certificate_ledger():
    a, b = "2026-11-05T00:00:00Z", "2026-11-08T17:00:00Z"
    st = sb.note_box_bought(sb.note_box_bought({}, at=a), at=b)
    out = sb.note_placement_refused(st, location="fsn1", bought_at=b, at=b)
    assert out["box_buys"] == [a]
