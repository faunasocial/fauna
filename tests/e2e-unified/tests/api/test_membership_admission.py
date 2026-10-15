"""Paid nest membership — the two admission seams, at the wire.

`docs/goal/behavior/monetization.md` § Pillar 4 → *Rail C*, seam 1
(Admission): a membership tier is an ordinary subscription tier the nest's
admin owns plus an Admin-class designation linking it to a quota tier
(`fauna.admin.membership_tiers.set`), and a payment for it admits a member:

- **a paid claim code doubles as an invite code** — pasted into the invite-code
  slot of `fauna.account.register`, it admits the actor at the linked quota
  tier, honoured under `open` and `invite_required` and refused under `closed`
  (`closed` means no self-service admission at all), leaving a refused code
  redeemable;
- **a pending join request backed by a verified payment auto-approves** — the
  prospect's `fauna.account.invite_request.submit` waits for an admin, and a
  signed provider webhook naming the prospect admits them with no admin
  decision at all.

Everything is driven over the wire a production caller uses: the admin's
choices through Admin-class WS-RPC (`testing.md` point 8's fixture-setup
carve-out — the admin authoring journeys are the app suites'), the prospect
through the pre-identity kinds the onboarding wizard rides, the payment through
the public webhook a provider's delivery system POSTs. No app is launched.
The engine half is pinned in-process by `bins/fauna-nest/tests/
conformance_membership.rs`; this file pins it through the binary's doors.

Each test admits its own fresh actors and mints its own codes on one
module-shared nest; registration mode is set by each test that depends on it,
so order is never load-bearing.
"""

import time

import pytest
from nacl.signing import SigningKey

from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_anon_client import WsRpcAnonClient
from common.auth import register_handled_actor
from helpers.membership_nest import (
    NEST_DOMAIN,
    QUOTA_TIER,
    admin_client as _admin,
    claim_row as _claim_row,
    handle as _handle,
    mint_membership_code as _mint_membership_code,
    pay_membership as _pay_membership,
    set_mode as _set_mode,
    start_member_nest,
    user_row as _user_row,
)

pytestmark = pytest.mark.tier_3


@pytest.fixture(scope="module")
def member_nest(request, nest_mode, tmp_path_factory):
    """A nest whose admin sells membership (`helpers.membership_nest`)."""
    nest, cleanup = start_member_nest(request, nest_mode, tmp_path_factory, "membership-nest")
    yield nest
    cleanup()


@pytest.mark.feature("join-a-nest", "admin-tiers")
@pytest.mark.parametrize("mode", ["open", "invite_required"])
def test_a_paid_membership_code_admits_like_an_invite_code(member_nest, mode):
    """Where the nest takes invite codes, a paid membership code is one: it
    admits the joiner at the membership's linked quota tier and is stamped
    redeemed by them — a single-use admission, like an invite."""
    _set_mode(member_nest, mode)
    code = _mint_membership_code(member_nest)

    joiner = register_handled_actor(
        member_nest["port"], _handle("member"), NEST_DOMAIN,
        invite_code=code, base_url=member_nest["url"])
    joiner_id = bytes(joiner["actor_id_bytes"])

    row = _user_row(member_nest, joiner_id)
    assert row is not None, f"the membership code must admit the joiner under {mode!r}"
    assert row["tier"] == QUOTA_TIER, (
        f"a member is admitted at the membership's linked quota tier, not "
        f"{row['tier']!r}"
    )
    assert _claim_row(member_nest, code).get("redeemed_by") == joiner_id

    # Single-use, like an invite: a second joiner cannot ride the same code.
    with pytest.raises(RpcCallError):
        register_handled_actor(
            member_nest["port"], _handle("second"), NEST_DOMAIN,
            invite_code=code, base_url=member_nest["url"])


@pytest.mark.feature("join-a-nest")
def test_a_paid_membership_code_is_refused_when_the_nest_takes_no_codes(member_nest):
    """`closed` means no self-service admission at all, paid or not: the code
    admits nobody and stays unredeemed, so it still works once the admin
    reopens joining."""
    _set_mode(member_nest, "closed")
    code = _mint_membership_code(member_nest)
    try:
        with pytest.raises(RpcCallError) as exc:
            register_handled_actor(
                member_nest["port"], _handle("closed"), NEST_DOMAIN,
                invite_code=code, base_url=member_nest["url"])
        assert exc.value.code == "fauna.account.registration_closed", exc.value.code
        assert _claim_row(member_nest, code).get("redeemed_by") is None, (
            "a refused admission must leave the code redeemable"
        )

        _set_mode(member_nest, "invite_required")
        joiner = register_handled_actor(
            member_nest["port"], _handle("later"), NEST_DOMAIN,
            invite_code=code, base_url=member_nest["url"])
        row = _user_row(member_nest, bytes(joiner["actor_id_bytes"]))
        assert row is not None and row["tier"] == QUOTA_TIER, f"{row!r}"
    finally:
        _set_mode(member_nest, "invite_required")


def _submit_join_request(member_nest, sk: SigningKey, handle: str) -> dict:
    """`fauna.account.invite_request.submit`, signed as the onboarding wizard
    signs it (`common.sig_domain`)."""
    from common.sig_domain import invite_submit_signed_message

    actor_id = bytes(sk.verify_key)
    ts = int(time.time() * 1000)
    message = "I paid for a membership"
    sig = sk.sign(invite_submit_signed_message(actor_id, handle, message, ts)).signature
    with WsRpcAnonClient(member_nest["url"]) as anon:
        return anon.call("fauna.account.invite_request.submit", {
            "actor_id": actor_id.hex(),
            "handle": handle,
            "message": message,
            "timestamp": ts,
            "signature": sig.hex(),
        })


@pytest.mark.feature("join-a-nest")
def test_a_join_request_a_verified_payment_backs_is_approved_without_an_admin(member_nest):
    """A prospect asks to join and waits for an admin; their payment for the
    membership arrives from the provider, and the request is approved on the
    spot — the account exists, under the handle they asked for, at the
    membership's quota tier. The admin approves nothing."""
    _set_mode(member_nest, "invite_required")
    sk = SigningKey.generate()
    prospect_id = bytes(sk.verify_key)
    handle = _handle("paid")

    submitted = _submit_join_request(member_nest, sk, handle)
    assert submitted.get("status") == "pending", f"{submitted!r}"
    assert _user_row(member_nest, prospect_id) is None, "no account before the payment"

    status, reply = _pay_membership(
        member_nest, prospect_id.hex(), int(time.time()) + 30 * 24 * 3600)
    assert status == 200 and reply.get("status") == "granted", f"{status} {reply!r}"
    assert reply.get("queued") is False, (
        f"a membership admission is in force at once, never queued for a client: {reply!r}"
    )

    row = _user_row(member_nest, prospect_id)
    assert row is not None, "the paid-for join request must be approved with no admin action"
    assert row["tier"] == QUOTA_TIER, f"admitted at the linked quota tier: {row!r}"
    assert row.get("handle") == handle, f"admitted under the handle requested: {row!r}"
    with _admin(member_nest) as admin:
        pending = admin.call("fauna.admin.invite_requests.list", {})
    assert handle not in {r.get("handle") for r in pending["invite_requests"]}, (
        f"the request must no longer wait for an admin: {pending!r}"
    )
