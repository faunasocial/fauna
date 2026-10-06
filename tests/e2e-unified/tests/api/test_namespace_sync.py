"""Paired-nest namespace sync over the federation channel (pull/push).

The HTTP twins ``POST /api/v1/nest-sync/{pull,push}`` were deleted in the
WS-RPC-everywhere rip-out; paired namespace sync now rides the nest↔nest
federation channel as ``fauna.federation.sync.{pull,push}``
(``bins/fauna-nest/src/federation_handlers.rs``). On the channel the peer is
verified once at handshake (``fauna.federation.hello``), so the request structs
drop the body ``nest_id`` + per-request ``envelope`` the HTTP twins carried —
the ``is_paired`` gate keys on the connection's **verified** initiator nest_id.

Bytes ride as native CBOR byte strings (no hex/base64): ``entry_id`` /
``ciphertext`` / ``actor_sig`` are passed and compared as ``bytes``.
"""
import os
import sqlite3

import pytest
from nacl.secret import SecretBox
from nacl.utils import random as nacl_random

from clients.ws_rpc_federation_client import FederationChannelClient, RpcCallError
from common import (
    create_actor_and_register,
    start_nest,
)
from tests.api import ws_api

pytestmark = pytest.mark.tier_3


@pytest.fixture(scope="module")
def binary(nest_binary):
    """Ride the shared fixture rather than calling `build_node()` here.

    Building directly put no binary-shaped name in these tests' fixture closure,
    so `--nest docker` SELECTED them and ran `cargo build -p fauna-nest` inside
    the run while reporting the image — a false ✅ against a nest the image never
    served. Measured 2026-08-28 as a ~13-minute stall in a docker-mode
    `tests/api/` run. Requesting `nest_binary` is both the uniform shape and what
    makes `nest_surface`'s closure rule see the dependency.
    """
    return nest_binary


@pytest.fixture()
def paired(binary, tmp_path):
    pub = start_nest(binary, tmp_path / "pub", 13060)
    priv = start_nest(
        binary, tmp_path / "priv", 13061,
        config_name="test-private-paired.toml",
    )
    actor = create_actor_and_register(pub["port"], admin_signing_key=pub["admin"]["signing_key"])
    # The user authorizes the pairing via the bearer `fauna.pair.add` kind
    # (the peer handshake `POST /api/v1/pair` was retired) — stored under the
    # actor on the public nest, so the public nest's `is_paired(actor, priv)`
    # check passes when the private nest dials the federation channel.
    ws_api.add_pairing(pub["port"], actor, bytes.fromhex(priv["nest_id"]))
    yield {"public": pub, "private": priv, "actor": actor}
    priv["proc"].kill()
    pub["proc"].kill()
    priv["proc"].wait()
    pub["proc"].wait()


def test_sync_pull_empty(paired):
    """Sync pull on a paired nest returns empty entries initially."""
    pub = paired["public"]
    priv = paired["private"]
    actor = paired["actor"]

    # The actor's self-namespace (= its public key) — the only one a pairing
    # reaches (`private-mode.md` § Namespace Sync).
    namespace = actor["actor_id_hex"]
    # The private nest is the channel initiator (signs the hello with its
    # identity key); the public nest is the listener whose namespace store we read.
    with FederationChannelClient(initiator=priv, target=pub) as fed:
        reply = fed.call("fauna.federation.sync.pull", {
            "actor_id": actor["actor_id_hex"],
            "namespace": namespace,
            "since": 0,
        })
    assert reply["entries"] == []
    assert reply["up_to"] == 0


@pytest.mark.feature("nests-and-trust")
def test_sync_pull_rejected_when_not_paired(request, nest_mode, tmp_path_factory):
    """Sync pull from an unpaired nest is rejected (forbidden).

    The channel handshake itself is open (any well-formed peer signature is
    accepted — it buys attribution, not authorization), so the rejection happens
    inside the ``sync.pull`` handler: ``is_paired(actor, initiator_nest_id)`` is
    false for a nest that was never paired → ``fauna.federation.forbidden``.
    """
    from conftest import _start_dedicated_nest

    pub, cleanup_pub = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "unpaired-public")
    initiator, cleanup_initiator = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "unpaired-initiator")
    try:
        with FederationChannelClient(initiator=initiator, target=pub) as fed:
            with pytest.raises(RpcCallError) as exc_info:
                fed.call("fauna.federation.sync.pull", {
                    "actor_id": "cc" * 32,
                    "namespace": "dd" * 32,
                    "since": 0,
                })
        assert exc_info.value.code == "fauna.federation.forbidden"
    finally:
        cleanup_initiator()
        cleanup_pub()


@pytest.mark.feature("nests-and-trust")
def test_sync_push_and_pull_roundtrip(paired):
    """Push entries to a paired nest, then pull them back."""
    pub = paired["public"]
    priv = paired["private"]
    actor = paired["actor"]

    namespace = actor["actor_id_hex"]
    entry_id = bytes.fromhex("11" * 32)

    with FederationChannelClient(initiator=priv, target=pub) as fed:
        # Push — channel-authed, so no body nest_id / envelope. `FedSyncPushEntry`
        # carries no `updated_at` (the nest stamps it on store). The byte fields
        # ride the dag-cbor wire as byte strings — pass raw `bytes`.
        push_result = fed.call("fauna.federation.sync.push", {
            "actor_id": actor["actor_id_hex"],
            "namespace": namespace,
            "entries": [{
                "entry_id": entry_id,
                "ciphertext": b"encrypted calendar data",
                "actor_sig": b"\x00" * 64,
            }],
        })
        assert push_result["up_to"] > 0

        # Pull — the entry comes back keyed by the same 32-byte entry_id (a byte
        # string).
        pull_result = fed.call("fauna.federation.sync.pull", {
            "actor_id": actor["actor_id_hex"],
            "namespace": namespace,
            "since": 0,
        })
    assert len(pull_result["entries"]) == 1
    assert pull_result["entries"][0]["entry_id"] == entry_id


@pytest.mark.feature("nests-and-trust")
def test_sync_refuses_a_namespace_that_is_not_the_paired_actors(paired):
    """A pairing reaches the paired actor's own namespace and no other.

    The pairing check alone says the calling nest may sync *for this actor*;
    naming another account's namespace beside it must be refused on both legs,
    or a nest paired for one account reads and overwrites every account's
    entries.
    """
    pub = paired["public"]
    priv = paired["private"]
    actor = paired["actor"]
    foreign = "aa" * 32

    with FederationChannelClient(initiator=priv, target=pub) as fed:
        with pytest.raises(RpcCallError) as pull_exc:
            fed.call("fauna.federation.sync.pull", {
                "actor_id": actor["actor_id_hex"],
                "namespace": foreign,
                "since": 0,
            })
        assert pull_exc.value.code == "fauna.federation.forbidden"

        with pytest.raises(RpcCallError) as push_exc:
            fed.call("fauna.federation.sync.push", {
                "actor_id": actor["actor_id_hex"],
                "namespace": foreign,
                "entries": [{
                    "entry_id": bytes.fromhex("22" * 32),
                    "ciphertext": b"not yours to write",
                    "actor_sig": b"\x00" * 64,
                }],
            })
        assert push_exc.value.code == "fauna.federation.forbidden"


@pytest.mark.feature("nests-and-trust")
def test_the_relay_holds_only_sealed_bytes_it_cannot_read(paired):
    """A nest relaying your synced data holds only sealed bytes it cannot read.

    Owner doc: ``docs/goal/architecture/nest/private-mode.md`` § Namespace Sync —
    *"The public nest never decrypts namespace data — it stores and forwards
    opaque ciphertext."*

    A negative property, so the witness is the relay's own disk: the client
    seals a plaintext carrying a unique marker under a key only it holds, pushes
    it, and then (1) the relay's stored ``namespace_entries`` row carries exactly
    the sealed bytes — not re-encoded, not opened; (2) the marker appears in NO
    file under the relay's data dir (database, WAL, logs); and (3) the bytes the
    relay forwards on pull still open to the marker under the client's key — so
    what sits on the relay really is the user's data, sealed, and not an empty
    stand-in that would pass (1) and (2) vacuously.
    """
    pub = paired["public"]
    priv = paired["private"]
    actor = paired["actor"]
    namespace = actor["actor_id_hex"]
    entry_id = bytes.fromhex("33" * 32)

    marker = b"fauna-relay-plaintext-marker-" + os.urandom(8).hex().encode()
    box = SecretBox(nacl_random(SecretBox.KEY_SIZE))  # the client's key; never sent
    sealed = bytes(box.encrypt(b"calendar entry: " + marker))
    assert marker not in sealed

    with FederationChannelClient(initiator=priv, target=pub) as fed:
        fed.call("fauna.federation.sync.push", {
            "actor_id": actor["actor_id_hex"],
            "namespace": namespace,
            "entries": [{
                "entry_id": entry_id,
                "ciphertext": sealed,
                "actor_sig": b"\x00" * 64,
            }],
        })
        pulled = fed.call("fauna.federation.sync.pull", {
            "actor_id": actor["actor_id_hex"],
            "namespace": namespace,
            "since": 0,
        })

    # (1) The stored row is the sealed bytes, verbatim.
    conn = sqlite3.connect(f"file:{pub['db_path']}?mode=ro", uri=True, timeout=10.0)
    try:
        rows = conn.execute(
            "SELECT ciphertext FROM namespace_entries WHERE entry_id = ?",
            (entry_id,),
        ).fetchall()
    finally:
        conn.close()
    assert [bytes(r[0]) for r in rows] == [sealed], (
        f"the relay must store the client's sealed bytes verbatim, got {rows!r}"
    )

    # (2) The plaintext marker is nowhere on the relay's disk.
    data_dir = os.path.dirname(pub["db_path"])
    leaked = []
    for root, _dirs, files in os.walk(data_dir):
        for name in files:
            path = os.path.join(root, name)
            try:
                with open(path, "rb") as f:
                    if marker in f.read():
                        leaked.append(path)
            except OSError:
                continue
    assert not leaked, f"plaintext reached the relay's disk: {leaked!r}"

    # (3) What the relay forwards is the user's data, still sealed to them.
    forwarded = [e for e in pulled["entries"] if bytes(e["entry_id"]) == entry_id]
    assert len(forwarded) == 1, pulled
    assert marker in box.decrypt(bytes(forwarded[0]["ciphertext"]))
