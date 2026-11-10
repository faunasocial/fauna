"""family-safety outcome 63, the lapse arm — a proposed hand-over of guardianship
lapses after seven days.

`docs/goal/behavior/family-safety.md` § Graduation & transfer → *Expiry*: "a
pending transfer not answered within **7 days** lapses (lazily: an expired row
is invisible to reads and un-acceptable …)".

The guardian proposes through the UI (convention 8); the nest's ``test-hooks``
endpoint ``POST /api/v1/test/family/transfer_age``
(`bins/fauna-nest/src/guardian_policy_test_hook.rs`) then moves the proposal's
``created_at`` back, because the lazy expiry reads the wall clock. Two steps, so
the negative has a positive beside it (convention 14): six days old the
proposal still shows on the guardian's editor and in the proposed guardian's
status; seven days and a minute old it is gone from both, the proposed
guardian — whose only family tie it was — has no Family page, and an accept is
refused. Own pair: the module's shared pair's reset would cancel the proposal
this test needs to age.
"""

import json
import urllib.request

import pytest

from clients.ws_rpc_admin_client import RpcCallError
from helpers.budgets import RPC_ROUNDTRIP_S, UI_SETTLE_S
from helpers.family_wire import actor_ws, family_status, ward_row
from helpers.waiting import wait_until
from tests.test_family import (
    _admit_adult,
    _admit_own_pair,
    _family_status_read_landed,
    _guardian_proposes,
    _unique_suffix,
)

pytestmark = pytest.mark.tier_3

DAY_S = 86_400


def _age_pending_transfer(nest_instance, ward_hex: str, seconds: int) -> int:
    """The nest's ``test-hooks`` endpoint: move the ward's pending proposal
    ``seconds`` into the past; returns the rows written."""
    body = json.dumps({"actor_id": ward_hex, "seconds": seconds}).encode()
    req = urllib.request.Request(
        f"{nest_instance['url']}/api/v1/test/family/transfer_age",
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=30.0) as resp:
        assert resp.status == 200, f"transfer-age hook returned {resp.status}"
        return json.loads(resp.read())["written"]


def _incoming(nest_instance, target_identity) -> list:
    return family_status(nest_instance, target_identity).get("incoming_transfers") or []


@pytest.mark.feature("family-safety")
def test_family_a_proposed_hand_over_lapses_after_seven_days(admin_app, request, nest_instance):
    from conftest import _login_app_as

    app = admin_app
    guardian_identity, ward_identity, _guardian_handle, ward_handle = _admit_own_pair(
        nest_instance, "lapse"
    )
    ward_hex = ward_identity["actor_id_hex"]
    target_handle = f"family-target-lapse-{_unique_suffix()}"
    target_identity = _admit_adult(nest_instance, target_handle)

    _guardian_proposes(app, request, nest_instance, guardian_identity,
                       ward_handle, target_identity["actor_id_hex"], target_handle)

    # Six days old: still live, on both sides.
    assert _age_pending_transfer(nest_instance, ward_hex, 6 * DAY_S) == 1
    app.family.reload()
    app.family.select_ward_by_handle(ward_handle)
    assert app.family.transfer_pending_visible(), (
        f"a six-day-old proposal must still show. error={app.error_text()!r}"
    )
    assert len(_incoming(nest_instance, target_identity)) == 1, (
        "a six-day-old proposal must still reach the proposed guardian"
    )

    # Seven days and a minute: lapsed.
    assert _age_pending_transfer(nest_instance, ward_hex, DAY_S + 60) == 1

    def _pending_gone() -> bool:
        app.family.reload()
        app.family.select_ward_by_handle(ward_handle)
        return not app.family.transfer_pending_visible()

    wait_until(
        _pending_gone,
        RPC_ROUNDTRIP_S + UI_SETTLE_S,
        diagnose=lambda: (
            "a lapsed proposal still renders on the guardian's editor: "
            f"{app.driver.diagnose('family-transfer-pending')} error={app.error_text()!r}"
        ),
    )
    assert ward_handle in app.family.ward_handles(), "the lapse leaves the link standing"
    assert ward_row(family_status(nest_instance, guardian_identity),
                    ward_identity)["pending_transfer"] is None
    assert _incoming(nest_instance, target_identity) == [], (
        "a lapsed proposal must be gone from the proposed guardian's status"
    )

    # The proposed guardian: no prompt, so no Family page — read after the
    # app's own status read has answered.
    app.driver.reset()
    _family_status_read_landed(
        app,
        nest_instance["port"],
        lambda: _login_app_as(app, request, nest_instance, target_identity),
    )
    assert not app.family.tab_visible(), (
        "family-tab must hide for an adult whose only family tie was a lapsed "
        f"hand-over. {app.driver.diagnose('family-tab')} "
        f"reference={app.driver.diagnose('settings-tab')}"
    )

    # …and the lapsed proposal cannot be accepted.
    with actor_ws(nest_instance, target_identity) as target:
        with pytest.raises(RpcCallError) as refused:
            target.call(
                "fauna.family.transfer.accept",
                {"supervised_actor_id": bytes.fromhex(ward_hex)},
            )
    assert "not_found" in str(refused.value.code), (
        f"a lapsed proposal is refused as no pending transfer: {refused.value.code!r}"
    )
