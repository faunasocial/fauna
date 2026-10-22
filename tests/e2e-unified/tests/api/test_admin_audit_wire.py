"""The admin audit log at the wire — the API half of the admin Logs page's audit
section (`docs/goal/behavior/admin.md` § The admin audit log on `admin-logs`;
e2e convention 5).

`fauna.admin.audit.list` serves an audited admin action as the newest entry,
named for the admin who acted, with the format its hash was computed under;
consecutive entries link; and `fauna.admin.audit.integrity`'s head is that
newest entry. A failure here is nest-side, whatever the app journey shows.
"""
from __future__ import annotations

import secrets

import pytest

from helpers.admin_wire import admin_client

pytestmark = pytest.mark.tier_3


@pytest.mark.feature("admin-logs")
def test_an_audited_action_is_the_newest_linked_entry_and_the_head(nest_instance):
    nest = nest_instance
    admin_actor = bytes(nest["admin"]["signing_key"].verify_key)
    name = f"audit-wire-{secrets.token_hex(3)}"
    with admin_client(nest) as client:
        client.call(
            "fauna.admin.tiers.create",
            {
                "name": name,
                "max_inbox_bytes": 1000,
                "max_storage_bytes": 1000,
                "max_devices": 4,
                "max_blob_size": 1000,
                "max_feeds": 10,
            },
        )
        integrity = client.call("fauna.admin.audit.integrity", {})
        entries = client.call("fauna.admin.audit.list", {"limit": 5})["entries"]
        older = client.call(
            "fauna.admin.audit.list", {"limit": 5, "before_id": entries[-1]["id"]}
        )["entries"]

    newest = entries[0]
    assert (newest["action"], newest["target"]) == ("tier.create", name), entries[:3]
    assert bytes(newest["actor_id"]) == admin_actor
    assert newest["entry_hash_version"] == 2
    assert [e["id"] for e in entries] == sorted((e["id"] for e in entries), reverse=True)
    for newer, prev in zip(entries, entries[1:]):
        assert newer["prev_hash"] == prev["entry_hash"], (newer["id"], prev["id"])
    assert (integrity["head_id"], integrity["head_hash"]) == (
        newest["id"],
        newest["entry_hash"],
    )
    assert integrity["chain_length"] >= len(entries)
    assert all(e["id"] < entries[-1]["id"] for e in older), "before_id pages back"
