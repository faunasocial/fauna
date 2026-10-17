"""A tier's limits bind every member on it, and a limit the admin changes
applies from then on — on a real ``fauna-nest`` binary, through the doors a
member's app rides.

Owner docs: ``docs/goal/architecture/nest/public-mode.md`` § Tiers (a tier is
the quota: inbox bytes, storage bytes, devices, the largest single upload, and
feeds) and ``docs/goal/behavior/admin.md`` § 2 Users (*the tier is the quota*,
enforced on a multi-tenant nest). Each cap is driven to its refusal by the
production kind it gates, then the admin moves the cap with
``fauna.admin.tiers.update`` — the Settings page's door — and the very next
request follows the new number, with no restart and no re-admission:

* **upload size** (``max_blob_size``) and **inbox** (``max_inbox_bytes``) —
  ``fauna.inbox.send`` delivering to a member of the tier
  (``db/admin.rs::check_quota_in_tx``);
* **storage** (``max_storage_bytes``) — ``fauna.sync.changes.record`` metered
  against the set owner's tier (``sync_handlers.rs``);
* **feeds** (``max_feeds``) — ``fauna.feed.create`` (``feed_routes.rs``).

The device cap is witnessed by ``test_device_cap_api.py``. Before this file
the refusals above were pinned only in-process
(``conformance_feed_tier_cap.rs``, ``db/mod.rs``'s quota unit test), which the
feature catalog cannot cite, and nothing at any tier showed a changed cap
taking effect.

⚠ Each test admits a dedicated actor onto a tier of its own, named per test, on
the shared session nest: the shared ``free`` tier is lifted by the session
fixture (``common.auth.set_tier_caps``) and other tests' actors sit on it.
"""

import secrets

import pytest

from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common import build_email_inbox_payload
from common.auth import (
    create_actor_and_register,
    ensure_tier,
    set_tier_caps,
    set_user_tier,
    sync_changes_list,
    sync_changes_record,
    sync_register,
    user_create_folder,
)

pytestmark = pytest.mark.tier_3


def _ws(nest, actor) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"], actor_id=actor["actor_id_bytes"], signing_key=bytes(actor["signing_key"]))


def _member_on_own_tier(nest, **caps) -> tuple[dict, str]:
    """A fresh account on a fresh tier carrying ``caps`` (the rest copied from
    `free`). Returns ``(actor, tier_name)``."""
    sk = nest["admin"]["signing_key"]
    tier = f"e2e-limits-{secrets.token_hex(4)}"
    ensure_tier(nest["port"], admin_signing_key=sk, name=tier, base_url=nest["url"], **caps)
    actor = create_actor_and_register(nest["port"], admin_signing_key=sk, base_url=nest["url"])
    set_user_tier(nest["port"], actor["actor_id_hex"], tier, admin_signing_key=sk,
                  base_url=nest["url"])
    return actor, tier


def _recap(nest, tier: str, **caps) -> None:
    set_tier_caps(nest["port"], admin_signing_key=nest["admin"]["signing_key"],
                  tier=tier, base_url=nest["url"], **caps)


def _send_mail(nest, sender, recipient) -> dict:
    payload, _ = build_email_inbox_payload(
        sender["signing_key"], recipient["actor_id_hex"],
        "Over the limit?", "x" * 512, node_url=nest["url"])
    with _ws(nest, sender) as ws:
        return ws.call("fauna.inbox.send", {
            "recipient_actor_id": recipient["actor_id_hex"],
            "recipient_nest_url": None,
            "payload_bytes": payload,
        })


def _inbox(nest, actor) -> list:
    with _ws(nest, actor) as ws:
        return ws.call("fauna.inbox.fetch", {"limit": 32})["items"]


def _open_inbox(nest, actor) -> None:
    # Open, so a mail from a non-contact delivers rather than waiting as a knock.
    with _ws(nest, actor) as ws:
        ws.call("fauna.inbox.mode.set", {"mode": "open"})


@pytest.mark.feature("admin-tiers")
@pytest.mark.parametrize("cap, reason", [
    ("max_blob_size", "payload exceeds max blob size"),
    ("max_inbox_bytes", "inbox quota exceeded"),
])
def test_the_upload_size_and_inbox_caps_bind_and_a_raised_cap_applies_at_once(
    nest_instance, cap, reason
):
    """A mail to a member whose tier allows one byte (of upload, or of inbox)
    is refused and lands nothing; the admin raises that one cap and the same
    mail delivers on the next try."""
    nest = nest_instance
    recipient, tier = _member_on_own_tier(nest, **{cap: 1})
    sender = create_actor_and_register(
        nest["port"], admin_signing_key=nest["admin"]["signing_key"], base_url=nest["url"])
    _open_inbox(nest, recipient)

    with pytest.raises(RpcCallError) as refused:
        _send_mail(nest, sender, recipient)
    assert refused.value.code == "fauna.protocol.malformed", refused.value.code
    assert reason in str(refused.value.details), refused.value.details
    assert _inbox(nest, recipient) == [], "a refused delivery must land nothing"

    _recap(nest, tier, **{cap: 10 * 1024 * 1024})
    assert _send_mail(nest, sender, recipient).get("inbox_id") is not None
    assert len(_inbox(nest, recipient)) == 1, "the raised cap must apply to the very next mail"


@pytest.mark.feature("admin-tiers")
def test_the_storage_cap_binds_and_a_changed_cap_applies_at_once(nest_instance):
    """A file past the tier's storage allowance is refused; raised, the same
    file records; lowered under what is already stored, the files stay and the
    next byte is refused."""
    nest = nest_instance
    actor, tier = _member_on_own_tier(nest, max_storage_bytes=1000)
    secret = bytes(actor["signing_key"]).hex()
    device = secrets.token_hex(32)
    sync_register(nest["port"], secret_key=secret, device_id=device, base_url=nest["url"])
    folder = f"limits-{secrets.token_hex(4)}"
    user_create_folder(nest["port"], folder, secret_key=secret, base_url=nest["url"])

    def record(path: str, size: int) -> dict:
        return sync_changes_record(
            nest["port"], secret_key=secret, folder=folder, device_id=device, path=path,
            manifest_hash=secrets.token_hex(32), size_bytes=size, base_url=nest["url"])

    record("a.bin", 600)
    with pytest.raises(RpcCallError) as refused:
        record("b.bin", 600)
    assert refused.value.code.endswith("storage_quota_exceeded"), refused.value.code

    _recap(nest, tier, max_storage_bytes=2000)
    record("b.bin", 600)

    _recap(nest, tier, max_storage_bytes=100)
    listed = sync_changes_list(nest["port"], secret_key=secret, folder=folder,
                               base_url=nest["url"])
    assert len([c for c in listed["changes"] if c.get("change_type") == "create"]) == 2, (
        f"lowering the cap must never take away what is stored: {listed!r}"
    )
    with pytest.raises(RpcCallError) as still:
        record("c.bin", 1)
    assert still.value.code.endswith("storage_quota_exceeded"), still.value.code


@pytest.mark.feature("admin-tiers")
def test_the_feed_cap_binds_and_a_changed_cap_applies_at_once(nest_instance):
    """One feed past the tier's allowance is refused; raised, it is made;
    lowered to nothing, every feed already made stays and the next is
    refused."""
    nest = nest_instance
    actor, tier = _member_on_own_tier(nest, max_feeds=1)

    def create(ws, name: str) -> str:
        return ws.call("fauna.feed.create",
                       {"name": name, "rules": [], "combination": "any"})["feed_id"]

    def assert_refused(ws, name: str) -> None:
        with pytest.raises(RpcCallError) as refused:
            create(ws, name)
        assert refused.value.code == "fauna.feed.permission_denied", refused.value.code
        assert "feed limit reached" in str(refused.value.details), refused.value.details

    with _ws(nest, actor) as ws:
        first = create(ws, "first")
        assert_refused(ws, "second")

    _recap(nest, tier, max_feeds=2)
    with _ws(nest, actor) as ws:
        second = create(ws, "second")

    _recap(nest, tier, max_feeds=0)
    with _ws(nest, actor) as ws:
        kept = {f["feed_id"] for f in ws.call("fauna.feed.list", {})["feeds"]}
        assert {first, second} <= kept, f"a lowered cap must not take feeds away: {kept!r}"
        assert_refused(ws, "third")
