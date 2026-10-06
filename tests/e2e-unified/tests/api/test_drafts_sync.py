"""E2E API test (tier_3): draft persistence over `fauna.drafts.{get,put}`.

Drives the `__drafts` reserved-folder persistence plane directly against a
real `fauna-nest` binary (no browser). This is the cross-device half of the
draft-persistence v2 success criterion (`docs/goal/behavior/file-sync.md`
§ Drafts Sync): a user's drafts, sealed under their `BackupKey` on one device
and persisted here, must propagate byte-identically to another of their devices.

The shared-Rust serialize → seal → unseal → restore byte-equality is proven by
the `fauna-conversations` tier_1 test (`store::drafts::tests`); this test proves
the nest plane carries the opaque sealed blob across two connections of the same
actor and the catch-up overwrite semantics — the bytes the nest stores and
returns are the (sealed) draft, byte-equal.

Wire contract (verified against `bins/fauna-nest/src/drafts_handlers.rs` +
`libs/fauna-protocol/src/drafts.rs`):

* ``fauna.drafts.put`` — ``{"path": str, "blob": <sealed bytes>}`` → ``{"ok": true}``
* ``fauna.drafts.get`` — ``{"path": str}`` → ``{"blob": <bytes> | None}``

Both kinds are ``User``-class; the connection actor is the owner (no actor_id on
the wire), so a caller reads/writes only their own drafts.
"""

import pytest

from common import create_actor_and_register
from clients.ws_rpc_admin_client import WsRpcAdminClient

pytestmark = pytest.mark.tier_3


def _client(node_url: str, actor: dict) -> WsRpcAdminClient:
    """A fresh User-class WS-RPC client for ``actor`` against ``node_url``.

    Constructed directly (not via a shared cache) so two calls for the same
    actor model two distinct *devices* — distinct WebSocket connections sharing
    one identity, which is exactly the cross-device draft-sync scenario.
    """
    return WsRpcAdminClient(
        node_url,
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


# A sealed-shaped blob: the leading 0x01 is the ChaCha20 version byte a real
# `BackupKey`-sealed draft carries, so this also exercises the raw-opaque-storage
# guarantee (the nest must not mis-decode a 0x01-prefixed blob as zstd).
def _sealed(marker: int) -> bytes:
    return bytes([0x01]) + bytes([marker]) * 256


@pytest.mark.feature("drafts-survive")
def test_drafts_round_trip_across_devices(nest_instance):
    """Device A persists a draft blob; device B (same actor) reads it byte-equal,
    and an overwrite on A is seen by a fresh read on B (catch-up)."""
    port = nest_instance["port"]
    node_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    actor = create_actor_and_register(port, admin_signing_key=admin_sk)
    blob_v1 = _sealed(0xAB)

    # Device A persists the conversations-rail draft set.
    with _client(node_url, actor) as device_a:
        reply = device_a.call("fauna.drafts.put", {"path": "conversations", "blob": blob_v1})
        assert reply.get("ok") is True, f"put must ack ok, got {reply!r}"

    # Device B — a *separate* connection for the same identity — catches up.
    with _client(node_url, actor) as device_b:
        got = device_b.call("fauna.drafts.get", {"path": "conversations"})
        assert got.get("blob") == blob_v1, "device B must read device A's draft byte-equal"

    # Device A overwrites; device B's next read sees the latest (catch-up).
    blob_v2 = _sealed(0xCD) + b"\x00\x99"
    with _client(node_url, actor) as device_a:
        device_a.call("fauna.drafts.put", {"path": "conversations", "blob": blob_v2})
    with _client(node_url, actor) as device_b:
        got = device_b.call("fauna.drafts.get", {"path": "conversations"})
        assert got.get("blob") == blob_v2, "device B must see the overwritten latest draft"


def test_drafts_get_empty_returns_none(nest_instance):
    """A path the actor has never written returns no blob (client starts empty)."""
    port = nest_instance["port"]
    node_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    actor = create_actor_and_register(port, admin_signing_key=admin_sk)
    with _client(node_url, actor) as device:
        got = device.call("fauna.drafts.get", {"path": "conversations"})
        assert got.get("blob") is None, f"unwritten path must return None, got {got!r}"


def test_drafts_paths_are_independent(nest_instance):
    """Distinct rail paths are distinct blobs within the one `__drafts` set."""
    port = nest_instance["port"]
    node_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    actor = create_actor_and_register(port, admin_signing_key=admin_sk)
    conv, posts = _sealed(0x11), _sealed(0x22)
    with _client(node_url, actor) as device:
        device.call("fauna.drafts.put", {"path": "conversations", "blob": conv})
        device.call("fauna.drafts.put", {"path": "posts", "blob": posts})
        assert device.call("fauna.drafts.get", {"path": "conversations"}).get("blob") == conv
        assert device.call("fauna.drafts.get", {"path": "posts"}).get("blob") == posts
        assert device.call("fauna.drafts.get", {"path": "events"}).get("blob") is None


def test_drafts_rail_enumeration_is_closed(nest_instance):
    """The rail `path` is a closed three-constant set, enforced on BOTH kinds.

    This is half of how the rail is bounded (`reserved-folders.md` § Drafts
    Sync): a client-chosen key made an actor's `__drafts` footprint unbounded in
    the *count* of live rows, each of which GC pins forever. Proven end to end
    against the real binary, since the refusal is the security property.
    """
    port = nest_instance["port"]
    node_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    actor = create_actor_and_register(port, admin_signing_key=admin_sk)
    with _client(node_url, actor) as device:
        for rail in ("conversations", "posts", "events"):
            blob = _sealed(0x40 + len(rail))
            device.call("fauna.drafts.put", {"path": rail, "blob": blob})
            assert device.call("fauna.drafts.get", {"path": rail}).get("blob") == blob, (
                f"ratified rail {rail!r} must round-trip"
            )

        for bogus in ("conversations/42", "Conversations", "notarail"):
            with pytest.raises(Exception) as put_err:
                device.call("fauna.drafts.put", {"path": bogus, "blob": _sealed(0x55)})
            assert "malformed" in str(put_err.value), (
                f"put of unratified rail {bogus!r} must be refused, got {put_err.value!r}"
            )
            with pytest.raises(Exception) as get_err:
                device.call("fauna.drafts.get", {"path": bogus})
            assert "malformed" in str(get_err.value), (
                f"get of unratified rail {bogus!r} must be refused, got {get_err.value!r}"
            )


def test_drafts_blob_cap_is_enforced(nest_instance):
    """A blob over `MAX_DRAFTS_BLOB_BYTES` (1 MiB) is refused before any store
    write; one at the cap is legitimate and still round-trips.

    The cap is the other half of the bound — three rails times this cap is the
    whole per-actor `__drafts` footprint, which is why the rail is not metered
    against the storage quota (`reserved-folders.md` § Drafts Sync).
    """
    port = nest_instance["port"]
    node_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]
    cap = 1024 * 1024

    actor = create_actor_and_register(port, admin_signing_key=admin_sk)
    with _client(node_url, actor) as device:
        with pytest.raises(Exception) as err:
            device.call(
                "fauna.drafts.put",
                {"path": "conversations", "blob": bytes([0x01]) + b"\xab" * cap},
            )
        assert "malformed" in str(err.value), (
            f"an over-cap drafts blob must be refused, got {err.value!r}"
        )
        assert device.call("fauna.drafts.get", {"path": "conversations"}).get("blob") is None, (
            "a refused over-cap put must leave nothing stored"
        )

        at_cap = bytes([0x01]) + b"\xcd" * (cap - 1)
        device.call("fauna.drafts.put", {"path": "conversations", "blob": at_cap})
        assert device.call("fauna.drafts.get", {"path": "conversations"}).get("blob") == at_cap, (
            "a blob exactly at the cap must round-trip"
        )


@pytest.mark.feature("drafts-survive")
def test_drafts_reserved_set_not_user_listed(nest_instance):
    """`__drafts` is a reserved folder: persisting a draft must not surface it
    in the user-facing `fauna.folders.list` (file-sync.md § Drafts Sync)."""
    port = nest_instance["port"]
    node_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    actor = create_actor_and_register(port, admin_signing_key=admin_sk)
    with _client(node_url, actor) as device:
        device.call("fauna.drafts.put", {"path": "conversations", "blob": _sealed(0x33)})
        listed = device.call("fauna.folders.list", {})
        names = [fs.get("name") for fs in listed.get("folders", [])]
        assert "__drafts" not in names, f"__drafts must not be user-listed, got {names!r}"
        # No reserved (`__`-prefixed) set should appear in the user-facing list.
        assert not any(n and n.startswith("__") for n in names), (
            f"no reserved folder may be user-listed, got {names!r}"
        )
