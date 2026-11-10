"""The guardian-marked device, observed from outside — full-stack (tier_3) API E2E.

Witnesses `docs/features/family-safety.md` outcome **64** — *"Graduation revokes
every guardian-marked device; until the guardian unmarks it the child can
neither remove the marked device nor revoke its grant, and a marked device the
child removes comes back as its successor with the mark intact"*
(`docs/goal/behavior/family-safety.md` § Full visibility for young children →
*The device marker*, rulings 2026-07-16 and 2026-10-01).

The outside-observed twins of the nest's pins in
`bins/fauna-nest/tests/conformance_family.rs`
(`graduation_revokes_the_marked_guardian_device`,
`marked_device_refuses_ward_deletion_until_the_guardian_unmarks`,
`marked_device_refuses_the_wards_grant_revoke_until_the_guardian_unmarks`,
`a_key_displaced_from_the_marked_row_can_be_revoked_and_a_successor_registers`):

* the child's own `fauna.sync.devices.delete` of the marked row and its
  `fauna.sync.device_grant.revoke` of the marked row's key are refused with
  `fauna.sync.guardian_marked`, the row and its key stay (the key still signs
  in), and once the guardian unmarks the same two calls go through;
* graduation removes every marked row and ends its key's sign-in, and leaves
  the child's own device alone;
* the successor clause, at the nest: the child's account can take the marked
  row's key off it, but a successor principal the guardian's device registers
  lands on the SAME row, the mark intact, and is the row's only key that signs
  in — while the removed key is tombstoned and never registers again. The
  fleet-plane half (the ward's client writing the sealed `Removed` row, the
  guardian's client minting its successor unasked) runs inside the apps and is
  pinned by
  `conformance_account_plane_bind::the_guardian_marked_device_removed_on_the_fleet_plane_returns_as_a_successor`;
  this module observes what the nest keeps.

The nest cannot tell the guardian's enrolled device from the child's own: both
authenticate as the child's account, so every device call here is made with the
child's key, as the conformance pins make it.

Every account is admin-admitted (`helpers.admin_wire`), so the module spends
none of the anonymous invite-request budget.

Process safety: no ``pkill``/``killall``; the session nest is torn down by its
own fixture.
"""

from __future__ import annotations

import os

import pytest
from nacl.signing import SigningKey

from clients.ws_rpc_admin_client import RpcCallError
from helpers.admin_wire import admit_adult, admit_ward_directly
from helpers.enrollment import (
    device_sign_in,
    grant_device_principal,
    principals,
    register_device,
    register_granted_device,
)
from helpers.family_wire import actor_ws, family_status

pytestmark = pytest.mark.tier_3

#: `sync_handlers.rs` — the child's removal or grant revoke of a marked row.
SYNC_GUARDIAN_MARKED = "fauna.sync.guardian_marked"
#: `sync_handlers.rs` — a tombstoned key registering again.
SYNC_GRANT_REVOKED = "fauna.sync.device_grant_revoked"


def _handle(stem: str) -> str:
    return f"fammark-{stem}-{os.urandom(3).hex()}"


def _family(nest) -> tuple[dict, dict]:
    guardian = admit_adult(nest, _handle("guardian"))
    ward = admit_ward_directly(nest, guardian["actor_id_hex"], _handle("ward"))
    return guardian, ward


def _mark(nest, guardian: dict, ward: dict, device_id: str, marked: bool) -> None:
    with actor_ws(nest, guardian) as ws:
        ws.call(
            "fauna.family.device.mark",
            {
                "supervised_actor_id": bytes.fromhex(ward["actor_id_hex"]),
                "device_id": device_id,
                "marked": marked,
            },
        )


def _devices(nest, ward: dict) -> dict[str, dict]:
    """The child's own device list, ``{device_id: row}``."""
    with actor_ws(nest, ward) as ws:
        rows = ws.call("fauna.sync.devices.list", {}).get("devices") or []
    return {r["device_id"]: r for r in rows}


def _delete_device(nest, ward: dict, device_id: str) -> None:
    with actor_ws(nest, ward) as ws:
        ws.call("fauna.sync.devices.delete", {"device_id": device_id})


def _revoke_grant(nest, ward: dict, principal_hex: str) -> None:
    """The app/user arm of the grant revoke, on the child's session."""
    with actor_ws(nest, ward) as ws:
        ws.call("fauna.sync.device_grant.revoke", {"device_key": principal_hex})


def _signs_in(nest, ward: dict, device_key: SigningKey) -> bool:
    try:
        reply = device_sign_in(nest["url"], bytes.fromhex(ward["actor_id_hex"]), device_key)
    except RpcCallError:
        return False
    return bool(reply.get("token"))


@pytest.mark.feature("family-safety")
def test_the_child_can_neither_remove_a_marked_device_nor_revoke_its_grant_until_unmarked(
    nest_instance,
):
    """The two refusals and their lift.

    Two marked rows, so each door has a row of its own to go through after the
    unmark (a deletion also clears the row's grant). Before the mark the child
    removes an unmarked device freely — the refusal is the mark, not the door.
    The mark is in the child's own device list throughout (transparency).
    """
    guardian, ward = _family(nest_instance)
    own = register_device(nest_instance["url"], ward, "kid's phone")
    to_remove = register_device(nest_instance["url"], ward, "parent's tablet")
    key_seed = os.urandom(32)
    revoke_key = SigningKey(key_seed)
    to_revoke, principal = register_granted_device(
        nest_instance["url"], ward, "parent's laptop", principal_seed=key_seed
    )

    _delete_device(nest_instance, ward, own)
    assert own not in _devices(nest_instance, ward), (
        "an unmarked device is the child's to remove"
    )

    _mark(nest_instance, guardian, ward, to_remove, True)
    _mark(nest_instance, guardian, ward, to_revoke, True)
    listed = _devices(nest_instance, ward)
    for device_id in (to_remove, to_revoke):
        assert listed[device_id]["guardian_marked"] is True, (
            f"the child's own device list must show the mark on {device_id}: {listed[device_id]!r}"
        )

    with pytest.raises(RpcCallError) as removal:
        _delete_device(nest_instance, ward, to_remove)
    assert removal.value.code == SYNC_GUARDIAN_MARKED, (
        f"the child's removal of a marked device must be refused with "
        f"{SYNC_GUARDIAN_MARKED}: {removal.value!r}"
    )
    assert to_remove in _devices(nest_instance, ward), "the refused removal must leave the row"

    with pytest.raises(RpcCallError) as revoke:
        _revoke_grant(nest_instance, ward, principal)
    assert revoke.value.code == SYNC_GUARDIAN_MARKED, (
        f"the child's revoke of a marked device's grant must be refused with "
        f"{SYNC_GUARDIAN_MARKED}: {revoke.value!r}"
    )
    assert principals(nest_instance["url"], ward).get(to_revoke) == principal, (
        "the refused revoke must leave the grant on the marked row"
    )
    assert _signs_in(nest_instance, ward, revoke_key), (
        "and the marked device's key must still sign in"
    )

    _mark(nest_instance, guardian, ward, to_remove, False)
    _mark(nest_instance, guardian, ward, to_revoke, False)
    _delete_device(nest_instance, ward, to_remove)
    assert to_remove not in _devices(nest_instance, ward), (
        "once unmarked, the same removal must go through"
    )
    _revoke_grant(nest_instance, ward, principal)
    assert not _signs_in(nest_instance, ward, revoke_key), (
        "once unmarked, the same revoke must go through and end the key's sign-in"
    )


@pytest.mark.feature("family-safety")
def test_graduation_revokes_every_guardian_marked_device(nest_instance):
    """Two marked rows (one carrying a granted key) and the child's own device.
    After the guardian graduates the child, both marked rows are gone from the
    child's device list, the marked key no longer signs in, and the child's own
    device is still there — oversight ends, and the device that carried it with
    it, nothing else."""
    guardian, ward = _family(nest_instance)
    own = register_device(nest_instance["url"], ward, "kid's phone")
    marked_plain = register_device(nest_instance["url"], ward, "parent's tablet")
    key_seed = os.urandom(32)
    marked_key = SigningKey(key_seed)
    marked_granted, _principal = register_granted_device(
        nest_instance["url"], ward, "parent's laptop", principal_seed=key_seed
    )
    _mark(nest_instance, guardian, ward, marked_plain, True)
    _mark(nest_instance, guardian, ward, marked_granted, True)
    assert _signs_in(nest_instance, ward, marked_key), (
        "the marked device's key must sign in before graduation — else the "
        "revocation below proves nothing"
    )

    with actor_ws(nest_instance, guardian) as ws:
        ws.call(
            "fauna.family.graduate",
            {"supervised_actor_id": bytes.fromhex(ward["actor_id_hex"])},
        )

    assert family_status(nest_instance, ward).get("supervised_by") is None, (
        "the graduated account is no longer supervised"
    )
    listed = _devices(nest_instance, ward)
    for device_id in (marked_plain, marked_granted):
        assert device_id not in listed, (
            f"graduation must revoke the marked device {device_id}; the child's "
            f"device list still holds it: {sorted(listed)}"
        )
    assert own in listed, "graduation must leave the child's own device alone"
    assert not _signs_in(nest_instance, ward, marked_key), (
        "the revoked marked device's key must no longer sign in"
    )


@pytest.mark.feature("family-safety")
def test_a_removed_marked_device_returns_as_its_successor_with_the_mark(nest_instance):
    """The nest's half of the successor clause.

    The child's account takes the marked row's key off it the one way the nest
    leaves open — a key of its own registered onto the row displaces the
    guardian's, and a displaced key rides no marked row, so the child's revoke
    of it lands and tombstones it. The guardian's device then registers its
    successor principal onto the same row: the row is still listed, still
    marked, now carries the successor, which signs in, while the removed key
    neither signs in nor registers again.
    """
    guardian, ward = _family(nest_instance)
    first_seed = os.urandom(32)
    first_key = SigningKey(first_seed)
    row, first_principal = register_granted_device(
        nest_instance["url"], ward, "parent's tablet", principal_seed=first_seed
    )
    _mark(nest_instance, guardian, ward, row, True)

    # The child's removal: displace, then revoke the displaced key.
    grant_device_principal(nest_instance["url"], ward, row)
    _revoke_grant(nest_instance, ward, first_principal)
    assert not _signs_in(nest_instance, ward, first_key), (
        "the removed key must no longer sign in"
    )

    # The guardian's device comes back under a successor principal.
    successor_seed = os.urandom(32)
    successor_key = SigningKey(successor_seed)
    successor_principal = grant_device_principal(
        nest_instance["url"], ward, row, principal_seed=successor_seed
    )
    listed = _devices(nest_instance, ward)
    assert row in listed, "the successor returns on the same row"
    assert listed[row]["guardian_marked"] is True, (
        f"the row must keep its mark across the removal and the successor: {listed[row]!r}"
    )
    assert principals(nest_instance["url"], ward)[row] == successor_principal, (
        "the row carries the successor's grant"
    )
    assert _signs_in(nest_instance, ward, successor_key), "and the successor signs in"
    assert not _signs_in(nest_instance, ward, first_key), (
        "while the removed key stays out"
    )

    with pytest.raises(RpcCallError) as again:
        grant_device_principal(nest_instance["url"], ward, row, principal_seed=first_seed)
    assert again.value.code == SYNC_GRANT_REVOKED, (
        f"the removed, tombstoned key must never register again: {again.value!r}"
    )
    # The successor still rides a marked row, so the refusal now covers it.
    with pytest.raises(RpcCallError) as covered:
        _revoke_grant(nest_instance, ward, successor_principal)
    assert covered.value.code == SYNC_GUARDIAN_MARKED, (
        f"the successor's grant is under the mark like its predecessor's was: {covered.value!r}"
    )
