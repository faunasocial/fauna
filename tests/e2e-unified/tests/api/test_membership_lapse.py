"""Paid nest membership after admission — the move, the lapse, the renewal,
and the terms an already-bought window is held to, at the wire.

`docs/goal/behavior/monetization.md` § Pillar 4 → *Rail C*, steps 2–3: a
member who buys membership is moved to the linked quota tier at once; when the
paid window runs out the member drops to the designation's lapse tier — a
quota downgrade and nothing else, never a suspension, an eviction or a
deletion; a renewal restores the paid tier; a tier the admin set by hand is
never clobbered; and the (quota, lapse) pair is frozen at admission, so
re-pointing or clearing the designation changes nothing for members already
admitted. Membership admission also never consumes the free-account cap.

Ports the in-process scenarios of `bins/fauna-nest/tests/
conformance_membership.rs` (`lapse_reconcile_*`, `a_renewal_restores_a_lapsed_member`,
`a_re_pointed_designation_still_lapses_the_member_admitted_under_the_old_pair`,
`a_cleared_designation_still_lapses_its_already_admitted_members`) onto the
binary's doors: the payment through the public webhook a provider POSTs, the
admin's choices through Admin-class WS-RPC (`testing.md` point 8's
fixture-setup carve-out), and the member's own reads through the doors their
app rides. No app is launched.

**Latency-independent** (e2e convention 14). A lapse is expressed as a paid
window that has ALREADY ended when the payment lands — a state, not a wait —
and the hourly sweep that notices it is driven by
`POST /api/v1/test/membership_lapse/run-now` (`membership_lapse_test_hook.rs`,
compiled only under `test-hooks`), which runs the sweep's exact reconcile once
and replies only when it is done. The periodic sweep's own tick is hourly, so
it never lands inside a test.

This file starts its own nest (`helpers.membership_nest`) so the designation it
re-points and the free cap it sets never reach another suite. Every test admits
its own fresh actors and restores any nest-wide choice it changes, so order is
never load-bearing.
"""

import io
import time
import zipfile

import pytest
import requests

from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import (
    create_actor_and_register,
    ensure_tier,
    register_handled_actor,
    set_user_tier,
)
from helpers.membership_nest import (
    NEST_DOMAIN,
    QUOTA_TIER,
    admin_client,
    designate,
    handle,
    mint_membership_code,
    pay_membership,
    set_mode,
    start_member_nest,
    user_row,
)

pytestmark = pytest.mark.tier_3

#: The tier a member drops to when their window runs out. One feed fewer than
#: the paid tier, so a lapsed member holding two feeds is over its allowance —
#: which is exactly the state "keeps everything already stored" is about.
LAPSE_TIER = "lapsed"
#: The admin's by-hand tier (outcome 7) and the quota tier a re-pointed
#: designation names (outcome 8).
BY_HAND_TIER = "by-hand"
REPOINTED_TIER = "member-plus"

DAY = 24 * 3600


@pytest.fixture(scope="module")
def member_nest(request, nest_mode, tmp_path_factory):
    """A nest selling membership at `member` (2 feeds), lapsing to `lapsed`
    (1 feed)."""
    nest, cleanup = start_member_nest(
        request, nest_mode, tmp_path_factory, "membership-lapse-nest",
        lapse_tier=LAPSE_TIER,
        quota_caps={"max_feeds": 2},
        lapse_caps={"max_feeds": 1},
    )
    for tier in (BY_HAND_TIER, REPOINTED_TIER):
        ensure_tier(nest["port"], base_url=nest["url"], admin_signing_key=nest["admin_sk"],
                    name=tier)
    yield nest
    cleanup()


def _member(member_nest) -> dict:
    """A fresh account the admin admitted at `free` — someone already on the
    nest, about to buy membership."""
    return create_actor_and_register(
        member_nest["port"], base_url=member_nest["url"],
        admin_signing_key=member_nest["admin_sk"])


def _ws(member_nest, actor) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        member_nest["url"], actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]))


def _tier(member_nest, actor) -> str:
    row = user_row(member_nest, bytes(actor["actor_id_bytes"]))
    assert row is not None, "the account must still exist"
    return row["tier"]


def _pay(member_nest, actor, valid_until: int) -> None:
    status, reply = pay_membership(member_nest, actor["actor_id_hex"], valid_until)
    assert status == 200 and reply.get("status") == "granted", f"{status} {reply!r}"
    assert reply.get("queued") is False, (
        f"a membership is in force at once, never queued for a client: {reply!r}"
    )


def _lapse_sweep(member_nest) -> int:
    """One synchronous membership-lapse reconcile — the causal barrier."""
    resp = requests.post(
        f"{member_nest['url']}/api/v1/test/membership_lapse/run-now", json={}, timeout=60)
    assert resp.status_code == 200, (
        f"test-hooks membership_lapse run-now returned {resp.status_code}: {resp.text}"
    )
    body = resp.json()
    assert body.get("ok") is True, body
    return body["downgraded"]


def _expired() -> int:
    return int(time.time()) - 60


def _in_force() -> int:
    return int(time.time()) + 30 * DAY


@pytest.mark.feature("admin-tiers")
def test_a_member_who_buys_membership_is_moved_to_its_tier_at_once(member_nest):
    """Outcome 4, the member half: someone already on the nest buys membership
    and is on the linked quota tier the moment the payment lands — no admin
    action, no sweep — and the new tier's allowance is theirs at once."""
    buyer = _member(member_nest)
    assert _tier(member_nest, buyer) == "free"

    _pay(member_nest, buyer, _in_force())

    assert _tier(member_nest, buyer) == QUOTA_TIER
    with _ws(member_nest, buyer) as ws:
        for i in range(2):  # the paid tier's whole allowance
            ws.call("fauna.feed.create", {"name": f"paid-{i}", "rules": [], "combination": "any"})
    # A sweep finds nothing to do for a member whose window is still running.
    _lapse_sweep(member_nest)
    assert _tier(member_nest, buyer) == QUOTA_TIER


@pytest.mark.feature("admin-tiers")
def test_a_lapsed_member_drops_to_the_lapse_tier_and_keeps_everything(member_nest):
    """Outcome 5: a member whose window has run out drops to the lapse tier
    and to nothing worse — still an account, never suspended, still signing
    in, still reading every feed they made under the paid tier (more than the
    lapse tier would let them make now) and still exporting it all. The
    downgrade gates only what is NEW: a feed past the lapse allowance is
    refused."""
    member = _member(member_nest)
    _pay(member_nest, member, _expired())
    assert _tier(member_nest, member) == QUOTA_TIER

    made = set()
    with _ws(member_nest, member) as ws:
        for i in range(2):
            made.add(ws.call("fauna.feed.create",
                             {"name": f"kept-{i}", "rules": [], "combination": "any"})["feed_id"])

    assert _lapse_sweep(member_nest) >= 1
    row = user_row(member_nest, bytes(member["actor_id_bytes"]))
    assert row is not None, "a lapse must never delete the account"
    assert row["tier"] == LAPSE_TIER, f"a lapsed member drops to the lapse tier: {row!r}"
    assert row["suspended"] is False, f"a lapse must never suspend: {row!r}"
    assert row.get("eviction") is None, f"a lapse must never start an eviction: {row!r}"

    with _ws(member_nest, member) as ws:  # a fresh sign-in still works
        listed = {f["feed_id"] for f in ws.call("fauna.feed.list", {})["feeds"]}
        assert made <= listed, f"every feed made while paid must still be there: {listed!r}"
        for feed_id in made:
            ws.call("fauna.feed.posts", {"feed_id": feed_id})  # still readable
        with pytest.raises(RpcCallError) as refused:
            ws.call("fauna.feed.create", {"name": "new", "rules": [], "combination": "any"})
        assert refused.value.code == "fauna.feed.permission_denied", refused.value.code
        assert "feed limit reached" in str(refused.value.details), refused.value.details

    resp = requests.get(
        f"{member_nest['url']}/api/v1/export",
        headers={"Authorization": f"Bearer {member['token']}"}, timeout=120)
    assert resp.status_code == 200, (
        f"a lapsed member must still be able to export: {resp.status_code} {resp.text[:300]}"
    )
    with zipfile.ZipFile(io.BytesIO(resp.content)) as z:
        assert "export/manifest.json" in z.namelist()


@pytest.mark.feature("admin-tiers")
def test_a_lapsed_member_who_renews_is_put_back_on_the_paid_tier(member_nest):
    """Outcome 6: a renewal puts a lapsed member back on the paid tier at once,
    and the restored member is not lapsed again while the new window runs."""
    member = _member(member_nest)
    _pay(member_nest, member, _expired())
    _lapse_sweep(member_nest)
    assert _tier(member_nest, member) == LAPSE_TIER

    _pay(member_nest, member, _in_force())
    assert _tier(member_nest, member) == QUOTA_TIER, "a renewal restores the paid tier"

    _lapse_sweep(member_nest)
    assert _tier(member_nest, member) == QUOTA_TIER, (
        "a member whose renewed window is running must not lapse"
    )


@pytest.mark.feature("admin-tiers")
def test_a_tier_given_by_hand_is_never_undone_by_a_lapse(member_nest):
    """Outcome 7: once the admin has moved a member by hand, the membership
    running out leaves that choice alone — the sweep downgrades only a member
    still sitting on the tier the membership put them on."""
    member = _member(member_nest)
    _pay(member_nest, member, _expired())
    assert _tier(member_nest, member) == QUOTA_TIER

    set_user_tier(member_nest["port"], member["actor_id_hex"], BY_HAND_TIER,
                  admin_signing_key=member_nest["admin_sk"], base_url=member_nest["url"])
    _lapse_sweep(member_nest)
    assert _tier(member_nest, member) == BY_HAND_TIER, (
        "a lapse must never clobber the admin's by-hand tier"
    )


@pytest.mark.feature("admin-tiers")
def test_re_pointing_or_clearing_the_membership_never_changes_admitted_members_terms(member_nest):
    """Outcome 8: the (quota, lapse) pair is frozen when a member is admitted.
    Re-pointing the membership at another quota tier and another lapse tier
    neither moves a member already admitted nor changes where they lapse to;
    clearing the membership altogether does not let them keep the paid tier
    for ever — they still lapse, to the tier they were admitted under."""
    re_pointed = _member(member_nest)
    _pay(member_nest, re_pointed, _expired())
    admin = member_nest["admin_actor"]
    try:
        designate(admin, REPOINTED_TIER, "free")
        assert _tier(member_nest, re_pointed) == QUOTA_TIER, (
            "re-pointing must not move a member already admitted"
        )
        _lapse_sweep(member_nest)
        assert _tier(member_nest, re_pointed) == LAPSE_TIER, (
            "a member lapses to the lapse tier they were admitted under, not the re-pointed one"
        )

        # Cleared: a fresh admission under the original pair, then no
        # designation at all when its window is noticed.
        designate(admin, QUOTA_TIER, LAPSE_TIER)
        late = _member(member_nest)
        _pay(member_nest, late, _expired())
        assert _tier(member_nest, late) == QUOTA_TIER
        admin._ws_call("fauna.admin.membership_tiers.clear", {"tier_name": "supporter"})
        assert _tier(member_nest, late) == QUOTA_TIER, "clearing must not move an admitted member"
        _lapse_sweep(member_nest)
        assert _tier(member_nest, late) == LAPSE_TIER, (
            "a cleared membership's admitted member still lapses, to the frozen lapse tier"
        )
    finally:
        designate(admin, QUOTA_TIER, LAPSE_TIER)


@pytest.mark.feature("admin-tiers")
def test_people_who_join_by_paying_do_not_count_against_the_free_account_cap(member_nest):
    """Outcome 9: with the free-account cap full, a free sign-up is refused,
    while a joiner holding a paid membership code is admitted — at the member
    tier, so they do not take a free seat either."""
    with admin_client(member_nest) as admin:
        listed = admin.call("fauna.admin.users.list", {"limit": 10_000})
    users = listed.get("users", [])
    assert len(users) == listed.get("total"), f"one page must hold every account: {listed!r}"
    free_now = sum(1 for u in users if u.get("tier") == "free")
    try:
        set_mode(member_nest, "open", max_free_users=free_now)

        with pytest.raises(RpcCallError) as refused:
            register_handled_actor(member_nest["port"], handle("free"), NEST_DOMAIN,
                                   base_url=member_nest["url"])
        assert refused.value.code == "fauna.account.free_limit_reached", refused.value.code

        code = mint_membership_code(member_nest)
        joiner = register_handled_actor(member_nest["port"], handle("paid"), NEST_DOMAIN,
                                        invite_code=code, base_url=member_nest["url"])
        assert _tier(member_nest, joiner) == QUOTA_TIER

        # The cap is unchanged by the paid joiner: free sign-up is still
        # refused at exactly the same count.
        with pytest.raises(RpcCallError) as still:
            register_handled_actor(member_nest["port"], handle("free"), NEST_DOMAIN,
                                   base_url=member_nest["url"])
        assert still.value.code == "fauna.account.free_limit_reached", still.value.code
    finally:
        set_mode(member_nest, "invite_required")
