"""E2E API test (tier_3): the MLS state-replica plane `fauna.mls.{get,put}`.

Drives the `__mls` reserved-folder persistence plane directly against a real
`fauna-nest` binary (no browser). This is the transport half of the
cross-device MLS group-state sync design
(`docs/goal/behavior/devices.md` § Cross-device MLS group-state sync;
`docs/goal/behavior/file-sync.md` § MLS state replica): a replica blob sealed
under the owner's `BackupKey` on one device must reach another of their
devices byte-equal, and a concurrent write must CONFLICT — never silently
clobber (the replica carries user-irrecoverable own-message history).

The shared-Rust capture → seal → unseal → restore correctness is proven by the
`fauna-mls` / `fauna-conversations` tier_1 tests (`state_replica::tests`,
`store::history::tests`); this test proves the nest plane.

Wire contract (verified against `bins/fauna-nest/src/mls_replica_handlers.rs` +
`libs/fauna-protocol/src/mls_replica.rs`):

* ``fauna.mls.put`` — ``{"path": str, "blob": bytes, "base"?: "Absent" | {"Hash": [32]}}``
  → ``{"ok": true}`` or the ``fauna.mls.conflict`` error on a CAS mismatch
* ``fauna.mls.get`` — ``{"path": str}`` → ``{"blob": bytes | None}``

Both kinds are ``User``-class; the connection actor is the owner (no actor_id
on the wire), so a caller reads/writes only their own replica.
"""

import cbor2
import pytest

from common import create_actor_and_register
from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient

pytestmark = pytest.mark.tier_3


def _client(node_url: str, actor: dict) -> WsRpcAdminClient:
    """A fresh User-class WS-RPC client for ``actor`` — two calls for the same
    actor model two distinct *devices* (distinct connections, one identity)."""
    return WsRpcAdminClient(
        node_url,
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


# A sealed-shaped blob: the leading 0x01 is the ChaCha20 version byte a real
# `BackupKey`-sealed replica carries (raw-opaque storage guarantee).
def _sealed(marker: int) -> bytes:
    return bytes([0x01]) + bytes([marker]) * 256


def _blake3_or_skip(blob: bytes) -> bytes:
    """The nest content-addresses on blake3 of the sealed bytes; the
    `ReplicaBase::Hash` digest rides as a 32-byte byte string."""
    try:
        from blake3 import blake3  # type: ignore
    except ImportError:
        pytest.skip("blake3 python package not installed")
    return blake3(blob).digest()


@pytest.mark.feature("conversations")
def test_replica_round_trip_across_devices(nest_instance):
    """Device A persists the provider replica; device B (same actor) reads it
    byte-equal; a CAS-based overwrite on A is seen by a fresh read on B."""
    port = nest_instance["port"]
    node_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    actor = create_actor_and_register(port, admin_signing_key=admin_sk)
    blob_v1 = _sealed(0xAB)

    with _client(node_url, actor) as device_a:
        reply = device_a.call(
            "fauna.mls.put", {"path": "provider", "blob": blob_v1, "base": "Absent"}
        )
        assert reply.get("ok") is True, f"put must ack ok, got {reply!r}"

    with _client(node_url, actor) as device_b:
        got = device_b.call("fauna.mls.get", {"path": "provider"})
        assert got.get("blob") == blob_v1, "device B must read device A's replica byte-equal"

    # CAS overwrite against the loaded base; B's next read sees the latest.
    blob_v2 = _sealed(0xCD)
    base = {"Hash": _blake3_or_skip(blob_v1)}
    with _client(node_url, actor) as device_a:
        device_a.call("fauna.mls.put", {"path": "provider", "blob": blob_v2, "base": base})
    with _client(node_url, actor) as device_b:
        got = device_b.call("fauna.mls.get", {"path": "provider"})
        assert got.get("blob") == blob_v2, "device B must see the CAS-overwritten latest"


def test_replica_concurrent_write_conflicts_not_clobbers(nest_instance):
    """The day-one CAS property: a second writer holding a stale base gets
    `fauna.mls.conflict` and the stored blob is untouched — the no-data-loss
    floor for the irrecoverable own-message history the replica carries."""
    port = nest_instance["port"]
    node_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    actor = create_actor_and_register(port, admin_signing_key=admin_sk)
    blob_a = _sealed(0x11)

    with _client(node_url, actor) as device_a:
        device_a.call("fauna.mls.put", {"path": "provider", "blob": blob_a, "base": "Absent"})

    # Device B raced device A: it also observed "no replica" and asserts Absent.
    with _client(node_url, actor) as device_b:
        with pytest.raises(RpcCallError) as exc:
            device_b.call(
                "fauna.mls.put", {"path": "provider", "blob": _sealed(0x22), "base": "Absent"}
            )
        assert exc.value.code == "fauna.mls.conflict", (
            f"stale Absent must conflict, got {exc.value.code}"
        )
        # A's write stands untouched.
        got = device_b.call("fauna.mls.get", {"path": "provider"})
        assert got.get("blob") == blob_a, "the landed write must be untouched by the conflict"


def test_replica_paths_are_independent(nest_instance):
    """`provider` and per-channel `history/*` are distinct blobs in the one
    `__mls` set; an Absent-base write to one never conflicts the other."""
    port = nest_instance["port"]
    node_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    actor = create_actor_and_register(port, admin_signing_key=admin_sk)
    provider, hist = _sealed(0x33), _sealed(0x44)
    with _client(node_url, actor) as device:
        device.call("fauna.mls.put", {"path": "provider", "blob": provider, "base": "Absent"})
        device.call("fauna.mls.put", {"path": "history/aa11", "blob": hist, "base": "Absent"})
        assert device.call("fauna.mls.get", {"path": "provider"}).get("blob") == provider
        assert device.call("fauna.mls.get", {"path": "history/aa11"}).get("blob") == hist
        assert device.call("fauna.mls.get", {"path": "history/bb22"}).get("blob") is None


# Mirror of `MAX_MLS_REPLICA_BYTES` (libs/fauna-protocol/src/mls_replica.rs):
# the 2 MiB WS frame minus 64 KiB of request-envelope headroom.
MAX_MLS_REPLICA_BYTES = 2 * 1024 * 1024 - 64 * 1024


def test_replica_put_rejects_oversize_blob(nest_instance):
    """A sealed blob over `MAX_MLS_REPLICA_BYTES` is rejected with the
    clean `fauna.mls.too_large` domain error (defense-in-depth vs. a raw WS-frame
    drop); a blob exactly at the cap still fits under the 2 MiB frame — the 64 KiB
    envelope headroom — and is accepted."""
    port = nest_instance["port"]
    node_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    actor = create_actor_and_register(port, admin_signing_key=admin_sk)
    over = bytes([0x01]) + bytes([0xEE]) * MAX_MLS_REPLICA_BYTES  # one byte over
    at_cap = bytes([0x01]) * MAX_MLS_REPLICA_BYTES

    with _client(node_url, actor) as device:
        with pytest.raises(RpcCallError) as exc:
            device.call("fauna.mls.put", {"path": "provider", "blob": over, "base": "Absent"})
        assert exc.value.code == "fauna.mls.too_large", (
            f"an over-cap blob must reject with too_large, got {exc.value.code}"
        )
        # The rejected put stored nothing.
        assert device.call("fauna.mls.get", {"path": "provider"}).get("blob") is None
        # Exactly at the cap fits under the frame and is accepted.
        reply = device.call(
            "fauna.mls.put", {"path": "provider", "blob": at_cap, "base": "Absent"}
        )
        assert reply.get("ok") is True, f"an at-cap blob must be accepted, got {reply!r}"


# The channel ingest's AEAD-shape floor: an inner payload shorter than a
# ChaCha20-Poly1305 nonce (12) + tag (16) cannot be sealed output, so the nest
# rejects it `channel_aead_shape` before it ever reaches the commit gate
# (`bins/fauna-nest/src/storage/sealed.rs::classify_envelope_shape`; the strict
# channel ingest, `encryption-at-rest.md` § Don't do these → *Trust-the-uploader
# interim*). It is the same floor the blob pipeline applies.
_AEAD_SHAPE_FLOOR = 28


def _channel_envelope(variant: str, payload: bytes) -> bytes:
    """Canonical dag-cbor for a `ChannelEnvelope::{Application,Commit}` — an
    externally-tagged serde enum newtype variant is a one-key map
    ``{"Commit": <bstr>}`` (the opaque MLS bytes ride as a CBOR byte string via
    `serde_bytes`). The nest strict-decodes this to read the variant *without*
    opening the ciphertext (`libs/fauna-mls/src/types.rs`).

    ⚠ the commit-gate test
    below failed `ingest rejected: channel_aead_shape`, and it was the TEST's
    fixtures that were wrong — its payloads were 11 and 16 bytes, under the
    floor. The floor is deliberate and correct (a payload that short is provably
    not AEAD output, so accepting it would let plaintext into the sealed plane),
    so the fix is plausible payloads, not a weaker nest.

    The floor is asserted here rather than silently padded: a caller that means
    to test a *short* payload should say so at its own call site, and a caller
    that does not should fail while writing the test rather than read a
    server-side reject as a gate failure — which is exactly the misdiagnosis
    this cost.
    """
    assert len(payload) >= _AEAD_SHAPE_FLOOR, (
        f"a channel payload must be AEAD-shaped (>= {_AEAD_SHAPE_FLOOR} bytes: "
        f"nonce + tag); got {len(payload)}. The nest rejects shorter ones "
        "`channel_aead_shape` before the commit gate runs."
    )
    return cbor2.dumps({variant: payload}, canonical=True)


def test_channel_send_commit_gate_serializes_commits(nest_instance):
    """The device-owned-epoch commit gate (`fauna.conversations.channel.send`'s
    `expect_no_commit_since`, devices.md § Cross-device MLS group-state sync):
    once a `Commit` lands at seq N, a send whose precondition predates it is
    rejected `fauna.conversations.channel.stale`; the caught-up retry lands.
    `Application` records never trip the gate (only `Commit`s raise the mark)."""
    port = nest_instance["port"]
    node_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    actor = create_actor_and_register(port, admin_signing_key=admin_sk)
    channel = "e5" * 32  # 32-byte hex channel id

    def send(device, *, expect=None, envelope):
        payload = {"channel_id": channel, "envelope": envelope}
        if expect is not None:
            payload["expect_no_commit_since"] = expect
        return device.call("fauna.conversations.channel.send", payload)

    with _client(node_url, actor) as device_a, _client(node_url, actor) as device_b:
        # Device A posts an MLS Commit (ungated) — it lands and raises the mark.
        r1 = send(device_a, envelope=_channel_envelope("Commit", bytes([0xC1]) * _AEAD_SHAPE_FLOOR))
        commit_seq = r1["seq"]
        assert commit_seq >= 1

        # Device B raced: it processed only up to `commit_seq - 1`, so its commit
        # is from a stale epoch → the gate rejects it.
        with pytest.raises(RpcCallError) as exc:
            send(
                device_b,
                expect=commit_seq - 1,
                envelope=_channel_envelope("Commit", bytes([0xC2]) * _AEAD_SHAPE_FLOOR),
            )
        assert exc.value.code == "fauna.conversations.channel.stale", (
            f"a commit from a stale epoch must be rejected, got {exc.value.code}"
        )

        # An APPLICATION message that landed after the commit does NOT raise the
        # mark, so a catch-up commit is still accepted (busy chat can't livelock).
        send(
            device_a,
            envelope=_channel_envelope(
                "Application", b"hello world".ljust(_AEAD_SHAPE_FLOOR, b"\x00")
            ),
        )

        # Device B rebases: it processed A's commit (now at `commit_seq`) and
        # retries with the correct precondition → accepted.
        r2 = send(
            device_b,
            expect=commit_seq,
            envelope=_channel_envelope("Commit", bytes([0xC3]) * _AEAD_SHAPE_FLOOR),
        )
        assert r2["seq"] > commit_seq, "the caught-up retry must land after the commit"


def test_replica_reserved_set_not_user_listed(nest_instance):
    """`__mls` is a reserved folder: persisting a replica must not surface it
    in the user-facing `fauna.folders.list` (file-sync.md § MLS state replica)."""
    port = nest_instance["port"]
    node_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    actor = create_actor_and_register(port, admin_signing_key=admin_sk)
    with _client(node_url, actor) as device:
        device.call("fauna.mls.put", {"path": "provider", "blob": _sealed(0x55), "base": "Absent"})
        listed = device.call("fauna.folders.list", {})
        names = [fs.get("name") for fs in listed.get("folders", [])]
        assert "__mls" not in names, f"__mls must not be user-listed, got {names!r}"
