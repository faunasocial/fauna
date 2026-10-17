"""Cutting a member off — eviction, suspension and restore, witnessed at the wire
(`docs/goal/behavior/admin.md` § Cutting a user off — eviction and suspension).

The nest outcomes of `docs/features/admin-users.md` that the one cut-off state
decides, driven over the production kinds with no app in the loop (the admin's
buttons are the app suites'):

* **29** — an evicted member keeps full access for fourteen days, is cut off for
  fourteen more, and only then is the account deleted.
* **30** — suspending a member who is being evicted cuts them off at once and
  calls the deletion off.
* **37** — a suspended member is cut off at once: the connection they held is
  closed `4401`, and nothing new authenticates.
* **38** — a restored member comes back as they were: same handle, same tier,
  the same data, nothing to set up again.

The ladder's two fourteen-day windows are hard-coded and the worker ticks from
the timestamps stored on the account's row, so the ladder is driven through the
`test-hooks` endpoint `POST /api/v1/test/eviction/advance`: it moves ONE
account's eviction clock forward and runs the production eviction tick once —
never a wait (convention 14). Scoped to the one account, it is safe on the
shared session nest.
"""

from __future__ import annotations

import json
import secrets
import struct
import urllib.request

import pytest
import websocket

from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import mint_token_via_handshake
from conftest import _make_user
from helpers.admin_wire import admin_client

pytestmark = pytest.mark.tier_3

DAY = 24 * 3600
#: `transport.md` § Revocation teardown — the close a suspended actor's socket gets.
CLOSE_AUTH_REVOKED = 4401


def _ws(nest, member) -> WsRpcAdminClient:
    return WsRpcAdminClient(nest["url"], actor_id=bytes.fromhex(member["actor_id_hex"]),
                            signing_key=bytes(member["signing_key"]))


def _advance(nest, member, secs: int) -> dict:
    """Move ``member``'s eviction clock forward ``secs`` and run one tick."""
    body = json.dumps({"actor_id": member["actor_id_hex"], "secs": secs}).encode()
    req = urllib.request.Request(f"{nest['url']}/api/v1/test/eviction/advance", data=body,
                                 headers={"Content-Type": "application/json"}, method="POST")
    with urllib.request.urlopen(req, timeout=30.0) as resp:
        assert resp.status == 200, f"eviction hook returned {resp.status}"
        return json.loads(resp.read())


def _eviction(nest, member) -> dict | None:
    """``member``'s row in the admin's evictions list, or None."""
    with admin_client(nest) as admin:
        rows = admin.call("fauna.admin.evictions.list", {})["evictions"]
    want = bytes.fromhex(member["actor_id_hex"])
    row = next((r for r in rows if bytes(r["actor_id"]) == want), None)
    return (row or {}).get("eviction") if row else None


def _exists(nest, member) -> bool:
    with admin_client(nest) as admin:
        users = admin.users_list_all()
    want = bytes.fromhex(member["actor_id_hex"])
    return any(bytes(u["actor_id"]) == want for u in users)


def _can_read(nest, member) -> bool:
    """Whether ``member`` can sign in afresh and read their own account."""
    try:
        mint_token_via_handshake(nest["url"], member["signing_key"])
        with _ws(nest, member) as ws:
            ws.call("fauna.account.get", {})
    except RpcCallError:
        return False
    return True


def _admin_call(nest, kind: str, member, **extra) -> dict:
    with admin_client(nest) as admin:
        return admin.call(kind, {"actor_id": bytes.fromhex(member["actor_id_hex"]), **extra})


def _close_code(client: WsRpcAdminClient) -> int:
    """Read the held socket until the nest closes it; its close code."""
    raw = client._ws
    assert raw is not None, "the member's connection was never open"
    while True:
        opcode, data = raw.recv_data(control_frame=True)
        if opcode == websocket.ABNF.OPCODE_CLOSE:
            return struct.unpack("!H", data[:2])[0] if len(data) >= 2 else 1005


# ── 29: the eviction ladder ──────────────────────────────────────────────────


@pytest.mark.feature("admin-users")
def test_an_evicted_member_keeps_access_fourteen_days_is_cut_off_fourteen_more_then_deleted(
        nest_instance):
    nest = nest_instance
    member = _make_user(nest)
    _admin_call(nest, "fauna.admin.users.evict", member, reason="moving on", category="capacity")

    assert _advance(nest, member, 0)["shifted"] is True
    assert _can_read(nest, member), "an evicted member keeps full access at first"

    _advance(nest, member, 13 * DAY)
    assert _eviction(nest, member)["status"] == "warning"
    assert _can_read(nest, member), "on day 13 the member still has full access to take their data out"

    tick = _advance(nest, member, DAY + 60)
    assert tick["suspended"] >= 1, tick
    assert _eviction(nest, member)["status"] == "suspended"
    assert not _can_read(nest, member), "past day 14 the member is cut off"

    _advance(nest, member, 13 * DAY)
    assert _exists(nest, member), "on day 27 the account must still exist, cut off"
    assert not _can_read(nest, member)

    tick = _advance(nest, member, DAY + 60)
    assert tick["deleted"] >= 1, tick
    assert not _exists(nest, member), "past day 28 the account must be deleted"
    assert _eviction(nest, member) is None


# ── 30: suspending mid-eviction calls the deletion off ───────────────────────


@pytest.mark.feature("admin-users")
def test_suspending_a_member_being_evicted_cuts_them_off_and_calls_the_deletion_off(
        nest_instance):
    nest = nest_instance
    member = _make_user(nest)
    _admin_call(nest, "fauna.admin.users.evict", member, reason="moving on", category="capacity")
    assert _can_read(nest, member), "still in the warning window before the suspension"

    _admin_call(nest, "fauna.admin.users.suspend", member, reason="abuse now", category="abuse")
    eviction = _eviction(nest, member)
    assert eviction["status"] == "suspended" and eviction["delete_at"] is None, (
        f"the suspension must cut off now and clear the deletion: {eviction!r}"
    )
    assert not _can_read(nest, member), "the suspension cuts the member off at once"

    tick = _advance(nest, member, 60 * DAY)
    assert tick["deleted"] == 0, tick
    assert _exists(nest, member), "long past both windows, the suspended account is not deleted"
    assert _eviction(nest, member)["status"] == "suspended"


# ── 37: a suspension cuts off the connections already open ───────────────────


@pytest.mark.feature("admin-users")
def test_a_suspended_member_is_cut_off_at_once_their_open_connection_closed(nest_instance):
    nest = nest_instance
    member = _make_user(nest)
    held = _ws(nest, member)
    with held:
        assert held.call("fauna.account.get", {})["actor_id"] == member["actor_id_hex"]
        _admin_call(nest, "fauna.admin.users.suspend", member, reason="abuse now", category="abuse")
        assert _close_code(held) == CLOSE_AUTH_REVOKED, (
            "the suspension must close the connection the member already held"
        )
    with pytest.raises(RpcCallError) as ei:
        mint_token_via_handshake(nest["url"], member["signing_key"])
    assert ei.value.code, "a suspended member's fresh sign-in must be refused"
    assert not _can_read(nest, member), "nothing the suspended member asks is answered"


# ── 38: a restored member comes back as they were ────────────────────────────


@pytest.mark.feature("admin-users")
def test_a_restored_member_comes_back_as_they_were(nest_instance):
    nest = nest_instance
    member = _make_user(nest)
    feed_name = f"kept-{secrets.token_hex(3)}"
    with _ws(nest, member) as ws:
        ws.call("fauna.feed.create", {"name": feed_name, "rules": [], "combination": "any"})
        before = ws.call("fauna.account.get", {})

    _admin_call(nest, "fauna.admin.users.suspend", member, reason="sorting it out", category="other")
    assert not _can_read(nest, member)
    _admin_call(nest, "fauna.admin.users.cancel_eviction", member)

    mint_token_via_handshake(nest["url"], member["signing_key"])
    with _ws(nest, member) as ws:
        after = ws.call("fauna.account.get", {})
        feeds = ws.call("fauna.feed.list", {})["feeds"]
    assert (after["handle"], after["tier"]) == (before["handle"], before["tier"]), (
        f"same handle and tier after the restore: before {before!r}, after {after!r}"
    )
    assert after.get("eviction") is None, after
    assert any(f.get("name") == feed_name for f in feeds), (
        f"what the member stored before the suspension must still be there: {feeds!r}"
    )
