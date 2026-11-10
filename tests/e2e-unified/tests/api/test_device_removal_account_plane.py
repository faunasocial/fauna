"""tier_3 nest witnesses of what removing a device does to the account plane
the nest stores sealed: the removed device stops being able to read what the
account writes afterwards (``account-data-taxonomy.md`` § The generation
machinery → *The mint protocol, trigger (b)*), and what only it had recorded is
kept for the other devices (``delegable-scope-reclamation.md`` § Delegable-scope
reclamation, parts (3) and (4)).

Both promises are kept by the shared client code every app hosts, so they are
witnessed app-less through the e2e harness library (``feature-catalog.md``
§ The two surfaces): ``fauna_ffi.harness_device_removal`` runs the production
account runtime as three devices of one account over the real wire — one that
stays, one that is lost and removed by the stayer through the Devices page's
member door, and a fresh one that reads the account back — and the nest is read
from outside, through its own SQLite file.

Each test makes its own account: a removal changes the account for good.
"""

from __future__ import annotations

import secrets
import sqlite3

import pytest

import fauna_ffi
from common.auth import create_actor_and_register

pytestmark = pytest.mark.tier_3

#: The delegable scope's folder (``fauna_protocol::account_state::ACCOUNT_STATE_SCOPE``
#: under the reserved-folder prefix) — where read markers and the delegable
#: preference records rest, one row per (item, writer).
DELEGABLE_SCOPE_FOLDER = "__state"


def _channels(n: int) -> list[str]:
    return [secrets.token_hex(32) for _ in range(n)]


def _delegable_census(db_path: str, actor_id: bytes) -> dict[str, int]:
    """The live rows of the account's delegable scope on the nest, counted by
    the device writer each rests under (fleet id, hex)."""
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        rows = conn.execute(
            "SELECT c.origin_writer, COUNT(*)"
            "  FROM sync_changes c JOIN folders f ON f.id = c.folder_id"
            " WHERE f.actor_id = ? AND f.name = ?"
            "   AND c.superseded_at IS NULL AND c.origin_writer IS NOT NULL"
            " GROUP BY c.origin_writer",
            (actor_id, DELEGABLE_SCOPE_FOLDER),
        ).fetchall()
    finally:
        conn.close()
    return {bytes(writer).hex(): count for writer, count in rows}


def _remove(nest_instance, *, sole: list[str], shared: list[str], words: list[str]):
    owner = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    report = fauna_ffi.harness_device_removal(
        nest_instance["url"], bytes(owner["signing_key"]), sole=sole, shared=shared, words=words
    )
    return owner, report


@pytest.mark.feature("devices")
def test_a_removed_device_cannot_read_what_the_account_writes_after_the_removal(nest_instance):
    """`devices` outcome 23: a device you removed cannot read what your account
    writes after the removal.

    A device that stays and one that is lost settle on one account; the lost
    one closes still holding every key it was given, and the stayer removes it.
    The stayer no longer counts it a member, and its first sealed write after
    the removal is sealed under a generation minted after the removal — one the
    removed device was never given — rather than the tip it already holds."""
    _, report = _remove(nest_instance, sole=[], shared=_channels(1), words=["after"])
    assert report["stays"] != report["lost"], f"two devices, two fleet ids: {report}"
    assert report["stays_counts_lost"] is False, (
        f"the stayer still counts the removed device a member after its passes: {report}"
    )
    fresh_mints = set(report["minted_after"]) - set(report["minted_before"])
    assert fresh_mints, (
        "the first write after the removal was sealed under a generation the removed "
        f"device already held — no generation was minted past it: {report}"
    )


@pytest.mark.feature("devices")
def test_removing_a_device_keeps_for_the_others_what_only_it_had_recorded(nest_instance):
    """`devices` outcome 24: removing a device keeps for your other devices what
    only it had recorded, such as the conversations it alone marked read.

    The lost device marks five conversations read that no other device did,
    writes a preference record, and marks five more read past the stayer. After
    the stayer removes it, the nest holds no delegable row under the removed
    device — every row rests under a device still on the account — and a device
    that signs in afterwards reads every one of those marks and the record."""
    sole, shared = _channels(5), _channels(5)
    words = ["kept", secrets.token_hex(4)]
    owner, report = _remove(nest_instance, sole=sole, shared=shared, words=words)

    census = _delegable_census(nest_instance["db_path"], owner["actor_id_bytes"])
    assert report["lost"] not in census, (
        f"rows still rest on the nest under the removed device {report['lost']}: {census}"
    )
    assert set(census) <= {report["stays"], report["fresh"]}, (
        f"rows under a writer that is neither the stayer {report['stays']} nor the "
        f"fresh device {report['fresh']}: {census}"
    )
    markers = report["fresh_markers"]
    unread = {ch: markers.get(ch) for ch in sole if markers.get(ch) != 1}
    unread |= {ch: markers.get(ch) for ch in shared if markers.get(ch) != 2}
    assert not unread, (
        "a device signing in after the removal lost marks only the removed device "
        f"recorded (channel → what it read, want 1 for its own, 2 for the shared): {unread}"
    )
    assert report["fresh_words"] == " ".join(words), (
        f"the removed device's preference record did not survive for the others: {report}"
    )
