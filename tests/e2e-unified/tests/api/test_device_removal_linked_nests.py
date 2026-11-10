"""tier_3 nest witness that a device leaving the account reaches every nest the
account is linked to: removed or signed out while bound to one linked nest, it
is dropped on the other too, and that nest stops accepting it
(``account-sync-plane.md`` § The bind leg, rulings 7 and 8 — removal evidence
leaves a nest only once every linked replica holds it, and a removed member's
grant is revoked at every linked replica).

The promise is kept by the shared client code every app hosts — the secondary
leg each native app's runtime runs through
``fauna_client_account_runtime::native_linked_nest_connector`` — so it is
witnessed app-less through the e2e harness library (``feature-catalog.md``
§ The two surfaces): ``fauna_ffi.harness_device_removal_linked`` runs three
devices of one account over the real wire against two nests, each linked at the
other, and both nests are read from outside, through their own SQLite files.

Every test starts its own two nests: a removal changes both for good.
"""

from __future__ import annotations

import sqlite3

import pytest

import fauna_ffi
from common.auth import create_actor_and_register, register_user
from tests.api import ws_api

pytestmark = pytest.mark.tier_3

#: The account's fleet scope (``fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE``
#: under the reserved-folder prefix) — where the device set rests.
FLEET_SCOPE_FOLDER = "__state-fleet"

#: The pairing's capabilities: the canonical self-sync set
#: (``fauna_protocol::pair::default_self_sync``), ``account_replica`` included —
#: the one that makes the linked nest a replica of the account plane.
SELF_SYNC = [
    "mls_pull", "namespace_sync", "post_forward", "mail_pull", "nostr_push", "account_replica",
]


@pytest.fixture
def linked_nests(request, nest_mode, tmp_path_factory):
    from conftest import _start_dedicated_nest

    nest_a, cleanup_a = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "removal-a")
    try:
        nest_b, cleanup_b = _start_dedicated_nest(
            request, nest_mode, tmp_path_factory, "removal-b"
        )
        try:
            yield nest_a, nest_b
        finally:
            cleanup_b()
    finally:
        cleanup_a()


def _account_on_both_linked(nest_a, nest_b) -> dict:
    """One identity with an account on both nests, each linked at the other as
    the Nests page links them."""
    actor = create_actor_and_register(
        nest_a["port"], base_url=nest_a["url"], admin_signing_key=nest_a["admin"]["signing_key"]
    )
    register_user(
        nest_b["port"], actor["actor_id_hex"], base_url=nest_b["url"],
        admin_signing_key=nest_b["admin"]["signing_key"],
    )
    ws_api.add_pairing(
        nest_a["port"], actor, bytes.fromhex(nest_b["nest_id"]), capabilities=SELF_SYNC,
        nest_url=nest_b["peer_url"],
    )
    ws_api.add_pairing(
        nest_b["port"], actor, bytes.fromhex(nest_a["nest_id"]), capabilities=SELF_SYNC,
        nest_url=nest_a["peer_url"],
    )
    return actor


def _read(db_path: str, sql: str, args: tuple) -> list[tuple]:
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        return conn.execute(sql, args).fetchall()
    finally:
        conn.close()


def _fleet_rows_under(nest, actor_id: bytes, writer_hex: str) -> int:
    """The live rows of the account's fleet scope ``nest`` keeps under ``writer``."""
    rows = _read(
        nest["db_path"],
        "SELECT c.origin_writer FROM sync_changes c JOIN folders f ON f.id = c.folder_id"
        " WHERE f.actor_id = ? AND f.name = ?"
        "   AND c.superseded_at IS NULL AND c.origin_writer IS NOT NULL",
        (actor_id, FLEET_SCOPE_FOLDER),
    )
    return sum(1 for (writer,) in rows if bytes(writer).hex() == writer_hex)


def _grant_state(nest, actor_id: bytes, key_hex: str) -> tuple[bool, bool]:
    """Whether ``nest`` still holds a device grant for ``key`` on any of the
    account's device rows, and whether it keeps the key revoked — the record
    that refuses any later mint for it."""
    key = bytes.fromhex(key_hex)
    held = _read(
        nest["db_path"],
        "SELECT 1 FROM sync_devices WHERE actor_id = ? AND auth_device_key = ?",
        (actor_id, key),
    )
    revoked = _read(
        nest["db_path"],
        "SELECT 1 FROM revoked_device_grants WHERE actor_id = ? AND auth_device_key = ?",
        (actor_id, key),
    )
    return bool(held), bool(revoked)


@pytest.mark.feature("devices")
@pytest.mark.parametrize(
    ("case", "leaving_at"),
    [("sign_out", "a"), ("remove", "a"), ("remove", "b")],
    ids=["signed-out-at-a", "removed-while-at-a", "removed-while-at-b"],
)
def test_a_device_leaving_at_one_linked_nest_is_dropped_on_the_other(
    linked_nests, case, leaving_at
):
    """`devices` outcome 21: a device you remove or sign out of while it is
    bound to one of your linked nests is dropped on your other linked nest too,
    which stops accepting it.

    Three devices of one account on two linked nests: ``near`` bound to A,
    ``far`` bound to B, and the leaving one bound to A (it signs out, or is lost
    and removed by ``near``) or to B (lost, and removed by ``near`` at A). The
    device bound to B reads it gone; neither nest keeps a fleet row under it; no
    nest holds a grant for its key any more, and the nest it was bound to keeps
    that key revoked."""
    nest_a, nest_b = linked_nests
    actor = _account_on_both_linked(nest_a, nest_b)
    report = fauna_ffi.harness_device_removal_linked(
        nest_a["url"], bytes(actor["signing_key"]), other_url=nest_b["url"], case=case,
        leaving_at=leaving_at,
    )
    leaving = report["leaving"]
    assert len({report["near"], leaving, report["far"]}) == 3, (
        f"three devices, three fleet ids: {report}"
    )
    assert report["near_counts_leaving"] is False, (
        f"the sibling at A still counts the departed device a member: {report}"
    )
    assert report["far_reads"] and report["far_reads"][-1] is False, (
        f"the device bound to B still counts the departed device a member: {report}"
    )
    assert report["far_counts_leaving"] is False, report
    assert (report["rows_at_a"], report["rows_at_b"]) == (0, 0), (
        f"a nest (A, B) still lists fleet rows under the departed device: {report}"
    )

    actor_id = actor["actor_id_bytes"]
    on_disk = {
        name: _fleet_rows_under(nest, actor_id, leaving)
        for name, nest in (("A", nest_a), ("B", nest_b))
    }
    assert on_disk == {"A": 0, "B": 0}, (
        f"a nest keeps live fleet rows under the departed device {leaving}: {on_disk}"
    )
    grants = {
        name: _grant_state(nest, actor_id, leaving)
        for name, nest in (("A", nest_a), ("B", nest_b))
    }
    assert not any(held for held, _ in grants.values()), (
        f"a nest still holds a grant for the departed device's key (held, revoked): {grants}"
    )
    bound = "A" if leaving_at == "a" else "B"
    assert grants[bound][1], (
        f"the nest the departed device was bound to ({bound}) keeps no revocation of its "
        f"key, so nothing refuses a later mint for it (held, revoked): {grants}"
    )
