"""E2E API test (tier_3): a repeated delete of an already-tombstoned sync path
is a nest-side no-op, not a fresh row.

``record_sync_change_metered``
(``bins/fauna-nest/src/db/sync_storage.rs``) already dedupes an EXACT replay —
same acting actor, manifest, change type and ``content_key_version`` — before
this row's fix landed. That check is deliberately keyed on
``content_key_version`` too, because a create/modify with a bumped version is
a genuine new authored edition, not a replay. A delete carries no content to
version, so a second delete of the same already-tombstoned path — arriving
with a *different* ``content_key_version`` than the first (a stale client
retrying after its own set's content key rotated, or simply omitting the
field a newer client sends) — used to slip past that check and append a
redundant tombstone row, re-firing the remote-change nudge at every other
device for a change that had already landed.

This is the wire-level proof of the fix in
``record_sync_change_metered`` (the unit-level proof, including the harder
cross-actor case, is
``db::sync_storage::tests::record_no_ops_a_cross_actor_delete_of_an_already_tombstoned_path``
in the same file): the RPC layer now returns the ORIGINAL tombstone's ``seq``
for the second delete, and ``fauna.sync.changes.list`` shows no extra row.
"""

import hashlib

import pytest

from common import create_actor_and_register
from common.auth import (
    sync_changes_list,
    sync_changes_record,
    sync_register,
    user_create_folder,
)

pytestmark = pytest.mark.tier_3

_PATH = "notes/todo.txt"


def _manifest() -> str:
    return hashlib.blake2b(b"manifest:" + _PATH.encode(), digest_size=32).hexdigest()


def test_repeated_delete_of_an_already_tombstoned_path_is_a_no_op(nest_instance):
    port = nest_instance["port"]
    url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    actor = create_actor_and_register(port, admin_signing_key=admin_sk)
    secret_key = bytes(actor["signing_key"]).hex()
    device_id = hashlib.blake2b(b"sync-delete-idem:" + secret_key.encode(),
                                 digest_size=32).hexdigest()

    sync_register(port, secret_key=secret_key, device_id=device_id,
                  capabilities="read,write", base_url=url)
    user_create_folder(port, "delete-idem-set", secret_key=secret_key, base_url=url)

    sync_changes_record(port, secret_key=secret_key, folder="delete-idem-set",
                        device_id=device_id, path=_PATH, manifest_hash=_manifest(),
                        size_bytes=42, change_type="create",
                        content_key_version=1, base_url=url)

    first_delete = sync_changes_record(
        port, secret_key=secret_key, folder="delete-idem-set",
        device_id=device_id, path=_PATH, manifest_hash=None, size_bytes=0,
        change_type="delete", content_key_version=1, base_url=url,
    )

    # A second delete of the SAME already-tombstoned path, carrying a
    # DIFFERENT content_key_version — the one axis the exact-replay dedupe
    # above keys on, so this is not caught by that check alone. It must still
    # no-op: same seq back, no new row.
    second_delete = sync_changes_record(
        port, secret_key=secret_key, folder="delete-idem-set",
        device_id=device_id, path=_PATH, manifest_hash=None, size_bytes=0,
        change_type="delete", content_key_version=2, base_url=url,
    )

    assert second_delete["seq"] == first_delete["seq"], (
        "a repeated delete of an already-tombstoned path must return the "
        "existing tombstone's seq, not mint a fresh one"
    )

    changes = sync_changes_list(port, secret_key=secret_key,
                                folder="delete-idem-set", base_url=url)["changes"]
    assert len(changes) == 2, (
        "the repeated delete must append no row: exactly the create and the "
        f"first delete, got {len(changes)}"
    )
    delete_rows = [c for c in changes if c["change_type"] == "delete"]
    assert len(delete_rows) == 1, "only one tombstone row should ever exist for the path"
