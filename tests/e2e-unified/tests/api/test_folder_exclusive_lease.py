"""A folder's exclusive edit lease is held by ONE device at a time — the nest
half, on a real ``fauna-nest`` binary.

Owner doc: ``docs/goal/behavior/file-sync.md`` § Folders — *"Exclusive leases
allow one device at a time to hold write access to a folder. … Leases expire
automatically after a configured TTL."*

What this adds over the in-process DAO unit test
(``bins/fauna-nest/src/db/mod.rs``, which drives ``try_acquire_upload_lease``
directly): the refusal is driven over a real WS-RPC connection to a real binary
by a **second device of the same account**, which is the shape the promise is
made in — and it asserts the *typed refusal a client renders*
(``fauna.folders.conflict``), not merely the DAO's boolean.

**The exclusion is asserted in both directions, deliberately.** A first-mover
privilege ("whoever asked first keeps winning") and a genuine mutual exclusion
look identical from one direction only. So the test releases A's lease, lets B
take it, and then checks that *A* is now the refused one — the same assertion
with the roles swapped, which no first-mover implementation can pass.

⚠ **The client half of § Folders' lease sentence does not exist yet.** The same
goal sentence promises that *"while a lease is held, other devices treat the
folder as read-only"* — and no client in the tree reads a held lease: the only
caller of ``FoldersClient::lease_acquire`` is its own unit test, and neither
``libs/fauna-sync-engine`` nor ``bins/fauna-sync`` consults one. This file
witnesses the NEST half, which is all the ``[nest]`` outcome claims; the client
half is recorded in ``file-sync.md`` § Status and queued as its own build
track.
"""

import pytest

import fauna_ffi

from clients._ws_rpc_core import RpcCallError
from common.auth import create_actor_and_register

from tests.api.test_web_paywall_folder import _actor_client

pytestmark = pytest.mark.tier_3

FOLDER = "lease-plane"
DEVICE_A = bytes([0x1A] * 32)
DEVICE_B = bytes([0x2B] * 32)


def _register(ws, device_id: bytes, label: str) -> None:
    ws.call(
        "fauna.sync.register",
        {"device_id": device_id.hex(), "label": label, "capabilities": "read,write"},
    )


def _acquire(ws, device_id: bytes) -> dict:
    return ws.call(
        "fauna.folders.lease.acquire",
        {"name": FOLDER, "device_id": device_id.hex()},
    )


def _release(ws, device_id: bytes) -> dict:
    return ws.call(
        "fauna.folders.lease.release",
        {"name": FOLDER, "device_id": device_id.hex()},
    )


@pytest.mark.feature("local-folder-sync")
def test_only_one_device_at_a_time_holds_a_folders_exclusive_lease(nest_instance):
    """One holder; a second device asking while it is held is refused; and the
    exclusion swaps over cleanly when the holder lets go."""
    url = nest_instance["url"]
    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]

    owner = create_actor_and_register(port, admin_signing_key=admin_sk)

    with _actor_client(url, owner) as ws:
        fauna_ffi.harness_create_set(
            url, bytes(owner["signing_key"]),
            {"name": FOLDER},
        )
        # Both seats are real registered devices of the one account — the shape
        # a lease exists to arbitrate between.
        _register(ws, DEVICE_A, "Device A")
        _register(ws, DEVICE_B, "Device B")

        # ── 1. A takes the lease. ──
        assert _acquire(ws, DEVICE_A)["acquired"] is True

        # ── 2. …and may renew it. This is what makes the refusal in step 3 an
        # assertion about the *holder identity* rather than about "a lease row
        # exists": a nest that refused every second acquire would pass step 3
        # for the wrong reason. ──
        assert _acquire(ws, DEVICE_A)["acquired"] is True, (
            "the holder must be able to renew its own lease"
        )

        # ── 3. B asks while A holds it: refused, typed. ──
        with pytest.raises(RpcCallError) as held:
            _acquire(ws, DEVICE_B)
        assert held.value.code == "fauna.folders.conflict", held.value.code
        assert "lease" in (held.value.details or "").lower(), (
            f"the refusal must say what is in the way: {held.value.details!r}"
        )

        # ── 4. A lets go; B takes it. ──
        assert _release(ws, DEVICE_A)["released"] is True
        assert _acquire(ws, DEVICE_B)["acquired"] is True

        # ── 5. The exclusion with the roles swapped — now A is the refused one.
        # A first-mover implementation (one that simply kept letting the first
        # asker win) passes every step above and fails exactly here. ──
        with pytest.raises(RpcCallError) as swapped:
            _acquire(ws, DEVICE_A)
        assert swapped.value.code == "fauna.folders.conflict", swapped.value.code

        # ── 6. And a release frees it for whoever asks next. ──
        assert _release(ws, DEVICE_B)["released"] is True
        assert _acquire(ws, DEVICE_A)["acquired"] is True
