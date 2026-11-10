"""Only the guardian changes a child's policy — full-stack (tier_3) API E2E.

Witnesses `docs/features/family-safety.md` outcome **59** — *"Only the guardian
can change a child's policy: an admin is refused, and so is a guardian who is
themselves supervised or the child's own account"*
(`docs/goal/behavior/family-safety.md` § The trust shape — guardian ≠ admin,
invariant 1; § The guardianship link, "chains are impossible by construction").

The outside-observed twins of the nest's pins in
`bins/fauna-nest/tests/conformance_family.rs`
(`policy_update_is_guardian_only_and_persists`,
`mint_rejects_unknown_suspended_or_supervised_guardian`) and of the
`fauna.account.upgrade` arm that keeps a supervised designation from being shed
(`account_handlers.rs`, "supervised invite codes admit new accounts only"):

* the admin's and the child's own `fauna.family.policy.update` are refused
  with `fauna.family.permission_denied`, and the policy both sides read is
  unchanged; the guardian's identical write then lands;
* a supervised account cannot be named as anybody's guardian — not on a
  direct admission (no account is created) and not on an invite code — while
  the same calls naming an unsupervised adult go through;
* (beside the outcome, unmarked) an existing account cannot redeem a
  guardian-carrying code as a plan upgrade, while a plain code of the same plan
  upgrades it.

Every account is admin-admitted (`helpers.admin_wire`), so the module spends
none of the anonymous invite-request budget.

Process safety: no ``pkill``/``killall``; the session nest is torn down by its
own fixture.
"""

from __future__ import annotations

import os

import pytest
from nacl.signing import SigningKey

from clients.ws_rpc_admin_client import RpcCallError
from helpers.admin_wire import admin_client, admit_adult, admit_ward_directly
from helpers.family_wire import actor_ws, family_status, ward_row

pytestmark = pytest.mark.tier_3

#: `family_handlers.rs` — a policy write by anyone but the linked guardian.
FAMILY_PERMISSION_DENIED = "fauna.family.permission_denied"
#: The guardian validation at mint and at direct admission (no chains).
ADMIN_INVALID_PARAMS = "fauna.admin.invalid_params"
#: `account_handlers.rs` — the upgrade door's refusal of a supervised code.
ACCOUNT_INVALID_REQUEST = "fauna.account.invalid_request"


def _handle(stem: str) -> str:
    return f"famgop-{stem}-{os.urandom(3).hex()}"


def _policy_update(client, ward: dict, policy: dict) -> dict:
    return client.call(
        "fauna.family.policy.update",
        {"supervised_actor_id": bytes.fromhex(ward["actor_id_hex"]), "policy": policy},
    )


@pytest.mark.feature("family-safety")
def test_only_the_guardian_changes_the_childs_policy(nest_instance):
    """The admin and the child are refused the write the guardian makes.

    The tightened document flips `contact_approval` on. Both refusals are
    checked by the error code AND by reading the policy back from both sides —
    a refusal that still wrote would be a hole. Then the guardian's identical
    call lands and both sides read the new value, which is what shows the
    refusals were about who asked, not about the document.
    """
    guardian = admit_adult(nest_instance, _handle("guardian"))
    ward = admit_ward_directly(nest_instance, guardian["actor_id_hex"], _handle("ward"))

    before = family_status(nest_instance, ward)["policy"]
    assert before["contact_approval"] is False, (
        f"a fresh band-less link starts with contact approval off: {before!r}"
    )
    tightened = {**before, "contact_approval": True}

    with admin_client(nest_instance) as admin:
        with pytest.raises(RpcCallError) as by_admin:
            _policy_update(admin, ward, tightened)
    assert by_admin.value.code == FAMILY_PERMISSION_DENIED, (
        f"the admin role holds no oversight power: the admin's policy write must be "
        f"refused with {FAMILY_PERMISSION_DENIED}: {by_admin.value!r}"
    )

    with actor_ws(nest_instance, ward) as ws:
        with pytest.raises(RpcCallError) as by_child:
            _policy_update(ws, ward, tightened)
    assert by_child.value.code == FAMILY_PERMISSION_DENIED, (
        f"the child cannot edit their own policy: refused with "
        f"{FAMILY_PERMISSION_DENIED}: {by_child.value!r}"
    )

    assert family_status(nest_instance, ward)["policy"]["contact_approval"] is False, (
        "neither refused write may have changed the policy the child reads"
    )
    guardian_view = ward_row(family_status(nest_instance, guardian), ward)
    assert guardian_view is not None, "the ward is missing from its guardian's status"
    assert guardian_view["policy"]["contact_approval"] is False, (
        "neither refused write may have changed the policy the guardian reads"
    )

    with actor_ws(nest_instance, guardian) as ws:
        _policy_update(ws, ward, tightened)
    assert family_status(nest_instance, ward)["policy"]["contact_approval"] is True, (
        "the guardian's identical write must land and the child must read it"
    )
    assert (
        ward_row(family_status(nest_instance, guardian), ward)["policy"]["contact_approval"]
        is True
    ), "and the guardian must read the same document back"


@pytest.mark.feature("family-safety")
def test_a_supervised_account_cannot_be_named_anyones_guardian(nest_instance):
    """No chains: a supervised account is refused as the guardian of a new
    account on both doors that carry the designation outside an invite request
    — the direct admission and the invite code — and the refused admission
    leaves no account behind. The same two calls naming an unsupervised adult
    go through, so the refusal is the guardian's supervision and not the call.
    """
    adult = admit_adult(nest_instance, _handle("adult"))
    ward = admit_ward_directly(nest_instance, adult["actor_id_hex"], _handle("chain"))
    ward_id = bytes.fromhex(ward["actor_id_hex"])
    adult_id = bytes.fromhex(adult["actor_id_hex"])

    refused_actor = bytes(SigningKey.generate().verify_key)
    with admin_client(nest_instance) as admin:
        with pytest.raises(RpcCallError) as direct:
            admin.call(
                "fauna.admin.users.create",
                {
                    "actor_id": refused_actor,
                    "tier": "free",
                    "label": _handle("grandchild"),
                    "handle": _handle("grandchild"),
                    "guardian_actor": ward_id,
                },
            )
        with pytest.raises(RpcCallError) as minted:
            admin.call(
                "fauna.admin.invite_codes.create",
                {"tier": "free", "uses": 1, "guardian_actor": ward_id},
            )
    for door, refusal in (("direct admission", direct), ("invite code", minted)):
        assert refusal.value.code == ADMIN_INVALID_PARAMS, (
            f"naming a supervised account as guardian on the {door} must be refused "
            f"with {ADMIN_INVALID_PARAMS}: {refusal.value!r}"
        )

    # The discriminator: the same two doors with an unsupervised guardian. The
    # direct admission reuses the refused actor id, so it also shows the refused
    # call left no account behind (a second admission of an existing actor
    # would not go through).
    with admin_client(nest_instance) as admin:
        admin.call(
            "fauna.admin.users.create",
            {
                "actor_id": refused_actor,
                "tier": "free",
                "label": _handle("sibling"),
                "handle": _handle("sibling"),
                "guardian_actor": adult_id,
            },
        )
    with admin_client(nest_instance) as admin:
        code = admin.call(
            "fauna.admin.invite_codes.create",
            {"tier": "free", "uses": 1, "guardian_actor": adult_id},
        )["code"]
    assert code, "an unsupervised adult may be named guardian on an invite code"


def test_a_supervised_code_cannot_be_redeemed_as_a_plan_upgrade(nest_instance):
    """No catalog outcome names this arm (`family-safety.md` § Implementation
    status today, slice 1: "`fauna.account.upgrade` refuses supervised codes"),
    so it carries no feature mark; it lives here because it keeps the same
    designation from being shed.

    A guardian-carrying invite code admits new accounts only: redeemed on
    `fauna.account.upgrade` by an existing account it is refused, and the
    account's plan is unchanged — consuming it would drop the supervision the
    code carries. A plain code of the same plan upgrades the same account.
    """
    guardian = admit_adult(nest_instance, _handle("codeguard"))
    member = admit_adult(nest_instance, _handle("member"))

    with actor_ws(nest_instance, member) as ws:
        tier_before = ws.call("fauna.account.get", {})["tier"]
    assert tier_before != "community", f"the member must start below the target plan: {tier_before!r}"

    with admin_client(nest_instance) as admin:
        supervised_code = admin.call(
            "fauna.admin.invite_codes.create",
            {
                "tier": "community",
                "uses": 1,
                "guardian_actor": bytes.fromhex(guardian["actor_id_hex"]),
            },
        )["code"]
        plain_code = admin.call(
            "fauna.admin.invite_codes.create", {"tier": "community", "uses": 1}
        )["code"]

    with actor_ws(nest_instance, member) as ws:
        with pytest.raises(RpcCallError) as refusal:
            ws.call(
                "fauna.account.upgrade",
                {"tier": "community", "invite_code": supervised_code},
            )
        assert refusal.value.code == ACCOUNT_INVALID_REQUEST, (
            f"a supervised code redeemed as an upgrade must be refused with "
            f"{ACCOUNT_INVALID_REQUEST}: {refusal.value!r}"
        )
        assert ws.call("fauna.account.get", {})["tier"] == tier_before, (
            "the refused upgrade must leave the plan unchanged"
        )

        ws.call("fauna.account.upgrade", {"tier": "community", "invite_code": plain_code})
        assert ws.call("fauna.account.get", {})["tier"] == "community", (
            "a plain code of the same plan must upgrade the same account"
        )
    assert family_status(nest_instance, member).get("supervised_by") is None, (
        "no route here may have made the member supervised"
    )
