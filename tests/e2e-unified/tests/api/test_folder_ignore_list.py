"""tier_3 API e2e: a folder's own ignore list keeps files off the nest.

Outcome ``local-folder-sync`` 12 (`docs/goal/behavior/sync-engine-deployments.md`
§ Control Plane Principle): files matched by a folder's ``.faunaignore`` are
never uploaded — not when they appear, not when they change, not on a rescan.

The writer is the apps' own shared engine with no app in the loop
(``fauna_ffi.harness_sync_pass``: one converge over the watched directory — the
rescan every engine runs at start and on reconnect); everything asserted is
read off the nest: the folder's change log and the account's stored bytes. The
event-driven arm (a file written while an app's watcher runs) has no headless
host, so it is the app tier's (``tests/test_filesync_seats.py``).

The control is the same file after its line leaves the ignore list: the next
pass uploads it, so the passes before held it back by the list, not by a
broken upload.
"""

from __future__ import annotations

import secrets

import pytest

import fauna_ffi
from conftest import _make_user
from tests.api.test_web_paywall_folder import _actor_client

pytestmark = pytest.mark.tier_3

DEVICE = secrets.token_bytes(32)
#: Large and incompressible, so its upload could not hide in the stored-bytes count.
BIG = 2 * 1024 * 1024


@pytest.mark.feature("local-folder-sync")
def test_ignored_files_are_never_uploaded_on_a_change_or_a_rescan(nest_instance, tmp_path):
    nest = nest_instance
    owner = _make_user(nest)
    secret = bytes(owner["signing_key"])
    name = f"ignore-{secrets.token_hex(3)}"
    folder_id = fauna_ffi.harness_create_set(nest["url"], secret, {"name": name})["id"]
    watch, state = tmp_path / "watch", tmp_path / "state"
    watch.mkdir()

    def sync() -> list[str]:
        recorded, pending = fauna_ffi.harness_sync_pass(
            nest["url"], secret, f"local:{folder_id}", watch, state, DEVICE
        )
        assert pending == 0, f"{pending} files left pending: {recorded!r}"
        return recorded

    def on_nest() -> tuple[int, int]:
        with _actor_client(nest["url"], owner) as ws:
            changes = ws.call(
                "fauna.sync.changes.list", {"folder_id": folder_id, "since": 0}
            )["changes"]
            used = ws.call("fauna.account.get", {})["quota"]["storage"]["used_bytes"]
        return len(changes), used

    (watch / ".faunaignore").write_text("*.secret\n")
    (watch / "keep.txt").write_bytes(b"this one syncs")
    (watch / "big.secret").write_bytes(secrets.token_bytes(BIG))

    first = sync()
    assert "keep.txt" in first, f"control: a file the list does not match goes up: {first!r}"
    assert "big.secret" not in first, f"an ignored file was recorded: {first!r}"
    changes, used = on_nest()
    assert used < BIG, f"the nest stores {used} bytes — the ignored file went up"

    # A change to the ignored file, then a plain rescan: neither uploads it.
    (watch / "big.secret").write_bytes(secrets.token_bytes(BIG))
    assert sync() == [], "a change to an ignored file was recorded"
    assert sync() == [], "a rescan recorded something"
    assert on_nest() == (changes, used), "the nest gained a record or bytes"

    # Control: off the list, the same file goes up on the next pass.
    (watch / ".faunaignore").write_text("")
    third = sync()
    assert "big.secret" in third, f"the file did not go up once unlisted: {third!r}"
    after_changes, after_used = on_nest()
    assert after_changes > changes and after_used >= used + BIG, (
        f"the unlisted file left no trace on the nest: {(after_changes, after_used)}"
    )
