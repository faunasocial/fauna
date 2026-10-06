"""A shared folder's write plane is **owner-pays, member-capped** — on a real
``fauna-nest`` binary.

Owner doc: ``docs/goal/behavior/file-sync.md`` § Multi-writer shared sets —
*"Owner-pays metering + per-member caps. The per-path size delta charges the
**set owner's** ``storage_bytes_used`` regardless of who records … When the
recorder is a member, the same transaction bumps ``bytes_used`` on their
``folder_member_access`` row (floored at 0) and refuses with a typed
``member_cap_exceeded`` when the owner-set ``byte_cap`` would be exceeded."*

What this adds over ``bins/fauna-nest/tests/conformance_shared_folders.rs``
(whose ``writer_member_records_reader_refused_owner_pays_cap_and_floor_real_router``
drives the same router in-process): the shared-folder **write plane had no
API-tier test at all**. Here two real accounts hold two real WS-RPC connections
to a real binary, the member is rostered through the production Welcome path,
and both halves of the sentence are read off surfaces a client actually has —
the typed error code, and the owner's own ``fauna.account.get`` quota block.

**Both halves, and the refusal's cost.** The cap alone would be satisfied by a
nest that simply refused every member write; the owner-pays clause alone by one
that charged the owner and never capped. So the test asserts, in order: the
member's write lands AND moves the *owner's* meter; the write that would pass
the cap is refused with the typed code; **the refusal charges nothing** (a
refusal that had already billed the owner would leave the meter permanently
ahead of the bytes actually stored); and raising the cap lets the very same
record through — which is what makes the refusal a *cap* verdict rather than
an unrelated denial.
"""

import pytest

import fauna_ffi

from clients._ws_rpc_core import RpcCallError
from common.auth import create_actor_and_register

from tests.api import conv_api
from tests.api.test_web_paywall_folder import _actor_client

pytestmark = pytest.mark.tier_3

FOLDER = "rw-cap-docs"
RAW_GROUP_ID = bytes([0x6B] * 24).hex()
MEMBER_DEVICE = bytes([0x7C] * 32)
CAP = 1000
WRITE_SIZE = 600

# A member's record on a sealed shared set carries a content-key generation
# stamp (`file-sync.md` § Multi-writer shared sets, the version floor). No key
# is published on this fixture, so the floor is absent and the stamp is not what
# is under test — it rides only so the record has the shape production sends.
GENERATION = 1


def _member_record(url: str, member: dict, path: str, size: int) -> dict:
    """The member's own `fauna.sync.changes.record` against the OWNER's folder.

    `path_sealed` is synthetic on purpose: this test asserts a *refusal code*
    and a *meter*, never a rendered label, and the seal a real member produces
    rides the set's M2 content-key generation, which this fixture does not
    publish. (`common.auth.sync_changes_record`'s owner-rooted default seal
    would be the wrong root here for exactly that reason.)

    Signed, as the member (writer-signed change records): the nest refuses an
    unsigned record `signature_required` before it ever reaches the meter.
    """
    return fauna_ffi.harness_record_change(
        url,
        bytes(member["signing_key"]),
        {
            "folder": FOLDER,
            "device_id": MEMBER_DEVICE.hex(),
            "path": path,
            "path_sealed": b"e2e-synthetic-member-seal",
            "size_bytes": size,
            "change_type": "create",
            "manifest_hash": bytes([0xC1] * 32).hex(),
            "content_key_version": GENERATION,
        },
    )


def _owner_storage_used(ws) -> int:
    """The owner's own transparency read — `fauna.account.get`'s quota block."""
    return ws.call("fauna.account.get", {})["quota"]["storage"]["used_bytes"]


@pytest.mark.feature("share-a-folder")
def test_a_members_bytes_charge_the_owner_and_their_cap_refuses_them(nest_instance):
    """Owner-pays metering, and the owner-set per-member cap that bounds it."""
    url = nest_instance["url"]
    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]

    owner = create_actor_and_register(port, admin_signing_key=admin_sk)
    member = create_actor_and_register(port, admin_signing_key=admin_sk)

    # ── 1. The owner creates the set and binds it to an MLS group. ──
    fauna_ffi.harness_create_set(
        url, bytes(owner["signing_key"]),
        {"name": FOLDER},
    )
    share = conv_api.folder_share(port, owner, FOLDER, RAW_GROUP_ID)
    channel_id = share["channel_id"]

    # ── 2. The member joins the roster through the production Welcome path —
    # NOT by a DAO poke. `resolve_writable_folder` demands roster membership
    # AND an explicit `writer` role row, so a test that granted only the role
    # would be refused at the gate before ever reaching the meter. ──
    conv_api.set_inbox_mode(port, member, "open")
    conv_api.welcome_deliver(
        port,
        owner,
        member["actor_id_hex"],
        channel_id,
        b"\x01\x02\x03",
        kind={"type": "folder", "group_id": RAW_GROUP_ID},
    )
    conv_api.folder_set_access(
        port, owner, FOLDER, member["actor_id_hex"], "writer", byte_cap=CAP
    )

    with _actor_client(url, member) as member_ws:
        member_ws.call(
            "fauna.sync.register",
            {
                "device_id": MEMBER_DEVICE.hex(),
                "label": "Member laptop",
                "capabilities": "read,write",
            },
        )

        with _actor_client(url, owner) as owner_ws:
            before = _owner_storage_used(owner_ws)

            # ── 3. The member writes, under the cap. ──
            assert _member_record(url, member, "shared/first.bin", WRITE_SIZE)["seq"] > 0

            # ── 4. …and it is the OWNER's meter that moved. The member is
            # charged nothing; the owner pays for what their guest stores. ──
            after_first = _owner_storage_used(owner_ws)
            assert after_first == before + WRITE_SIZE, (
                "a member's bytes must charge the set owner: "
                f"{before} → {after_first}, expected +{WRITE_SIZE}"
            )
            assert _owner_storage_used(member_ws) == 0, (
                "…and must NOT charge the member who wrote them"
            )

            # ── 5. The second write would put the member at 1200 against a cap
            # of 1000: refused, typed, client-renderable. ──
            with pytest.raises(RpcCallError) as capped:
                _member_record(url, member, "shared/second.bin", WRITE_SIZE)
            assert capped.value.code == "fauna.sync.member_cap_exceeded", (
                capped.value.code
            )

            # ── 6. A refusal charges nothing. Without this the meter could run
            # ahead of the bytes on disk by one write per refusal, and the
            # owner would be billed for storage that was never accepted. ──
            assert _owner_storage_used(owner_ws) == after_first, (
                "a cap refusal must leave the owner's meter where it was"
            )

            # ── 7. The owner raises the cap; the same record lands. This is
            # what identifies step 5 as a *cap* verdict — an unrelated denial
            # would still refuse here. ──
            conv_api.folder_set_access(
                port, owner, FOLDER, member["actor_id_hex"], "writer",
                byte_cap=CAP * 2,
            )
            assert _member_record(url, member, "shared/second.bin", WRITE_SIZE)["seq"] > 0
            assert _owner_storage_used(owner_ws) == after_first + WRITE_SIZE
