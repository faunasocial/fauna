"""Backups snapshot wire-contract — full-stack (tier_3) API E2E.

Guards the client→nest request/response shapes for the Backups page's
**list**, **prune**, and **integrity-check** over the
``fauna.filesync.snapshot.*`` WS-RPC kinds (Track B15) — the twins the
web app adopted for the backups WS-RPC migration (the shared
``fauna-client-snapshots`` adapter, surfaced to the SPA through
``libs/fauna-wasm`` and to the Backups page via ``apps/fauna-web/src/lib/rpc.ts``).

This **replaces** the earlier HTTP-twin contract test (which drove
``/api/v1/snapshots/{prune,check}`` to catch two `api.ts` wire-format
bugs — ``keep_count`` vs ``keep_last`` and ``{snapshot_id}`` vs
``{folder}``). Those bugs lived in the now-deleted HTTP client; the
typed WS-RPC path (``SnapshotPruneRequest.policy`` /
``SnapshotCheckRequest.folder``) makes them structurally impossible, so
the assertions below are the positive contract for the kinds the page now
calls — the prune ``policy.keep_last`` field reaching the retention
engine, and the folder-scoped ``check`` returning ``status: "ok"``.

Process safety: no ``pkill``/``killall``; the nest is the
session-scoped ``nest_instance`` and is torn down by its own fixture.
"""

from __future__ import annotations

import secrets
import time

import blake3
import pytest

import fauna_ffi

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import create_actor_and_register, seed_snapshot_with_file_bytes

pytestmark = pytest.mark.tier_3


def _ws(nest_instance, actor) -> WsRpcAdminClient:
    """A WS-RPC client signing as `actor` (the folder owner / admin).

    `actor` is either a `create_actor_and_register` dict (carrying
    `actor_id_bytes` + `signing_key`) or `nest_instance["admin"]`.
    """
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


def _list_count(nest_instance, actor, folder) -> int:
    """Folder-scoped snapshot count via `fauna.filesync.snapshot.list`."""
    with _ws(nest_instance, actor) as ws:
        reply = ws.call("fauna.filesync.snapshot.list", {"folder": folder})
    return len(reply["rows"])


def _wait_for_next_wall_clock_second():
    """Block until the wall clock crosses into a new second.

    `snapshots` carries `UNIQUE(folder_id, created_at)` at second granularity
    (`bins/fauna-nest/src/db/snapshots.rs:424`); a same-second repeat dedups
    to the existing row rather than erroring (backup-restore.md § 1 — "no
    client-causable error state"). So seeding N *distinct* snapshots through
    the real `create_folder` wire call genuinely requires N distinct
    wall-clock seconds — there is no field or flag that differentiates them
    instead. # sleep-ok: waiting on the storage layer's own second-granularity
    dedup boundary, not on an async event to settle (e2e-conventions.md
    convention 14).
    """
    start = int(time.time())
    while int(time.time()) == start:
        time.sleep(0.05)


def _make_owned_folder(nest_instance, owner) -> str:
    """A fresh, uniquely-named folder owned by `owner`, bound over the canonical
    Admin WS-RPC surface (``fauna.admin.folders.create`` — the only way to set
    an owner; ``folders.name`` is unique on the session-scoped nest)."""
    name = f"backups-{secrets.token_hex(4)}"
    admin_sk = nest_instance["admin"]["signing_key"]
    admin_client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin_sk.verify_key),
        signing_key=bytes(admin_sk),
    )
    with admin_client:
        admin_client.call(
            "fauna.admin.folders.create",
            {"name": name, "actor_id": owner["actor_id_bytes"]},
        )
    return name


def _seed_folder(nest_instance, owner, n_snapshots):
    """Create a fresh folder owned by `owner` plus `n_snapshots` DISTINCT
    snapshots.

    The folder is bound to the actor via the canonical Admin WS-RPC surface
    (``fauna.admin.folders.create`` — the only way to set an owner). Each
    snapshot is then created **by the owner** over
    ``fauna.filesync.snapshot.create_folder`` — that kind is owner-scoped
    (review N1: a user only snapshots their own sets), mirroring the production
    flow where the owner's device captures the snapshot, not the admin.
    ``nest_instance`` is session-scoped and ``folders.name`` is unique, so each
    set gets a random name. Requires ``n_snapshots >= 1``. Returns the folder
    name.

    For ``n_snapshots > 1``, each create after the first waits for a fresh
    wall-clock second first (`_wait_for_next_wall_clock_second`) — back-to-back
    calls within the same second dedup to one row (see that helper).
    """
    name = _make_owned_folder(nest_instance, owner)
    with _ws(nest_instance, owner) as owner_ws:
        for i in range(n_snapshots):
            if i > 0:
                _wait_for_next_wall_clock_second()
            owner_ws.call(
                "fauna.filesync.snapshot.create_folder",
                {"folder": name},
            )
    return name


@pytest.mark.feature("snapshots")
def test_list_folder_scoped(nest_instance):
    """``fauna.filesync.snapshot.list`` with a ``folder`` returns that set's
    snapshots (the folder-scoped mode, Track B15 fold-in — folder rows
    carry ``message_kind == None``)."""
    actor = create_actor_and_register(nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"])
    fs = _seed_folder(nest_instance, actor, n_snapshots=1)

    with _ws(nest_instance, actor) as ws:
        reply = ws.call("fauna.filesync.snapshot.list", {"folder": fs})
    assert len(reply["rows"]) == 1, reply
    assert reply["rows"][0]["message_kind"] is None, reply["rows"][0]


def test_prune_keep_last_reaches_the_policy(nest_instance):
    """``fauna.filesync.snapshot.prune`` honours ``policy.keep_last``: with 6
    active snapshots, ``keep_last: 1`` marks 5 prunable and the hard floor
    (`SNAPSHOT_HARD_FLOOR = 3`, `bins/fauna-nest/src/backup/retention.rs:235`)
    hands the newest 2 of those back, so exactly 3 are pruned (the field
    reaches ``RetentionPolicy.keep_last`` through the typed wire, AND the
    floor holds — the same scenario `conformance_filesync_snapshot.rs`'s
    ``prune_dry_run_then_actual`` pins in-process; this is its wire twin).
    Six, not one: with only one active snapshot the floor alone would clamp
    every policy to ``pruned: 0``, which is indistinguishable from the field
    never reaching the engine at all."""
    actor = create_actor_and_register(nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"])
    fs = _seed_folder(nest_instance, actor, n_snapshots=6)
    assert _list_count(nest_instance, actor, fs) == 6

    # `pruned` is the authoritative signal that the policy was applied. The list
    # count is not asserted afterward: prune queues a *soft* delete, so the row
    # still lists until GC — `pruned` is the contract, not the surviving row.
    with _ws(nest_instance, actor) as ws:
        reply = ws.call(
            "fauna.filesync.snapshot.prune",
            {"folder": fs, "dry_run": False, "policy": {"keep_last": 1}},
        )
    assert reply["pruned"] == 3, reply
    assert reply["remaining"] == 3, reply


@pytest.mark.feature("snapshots")
def test_prune_never_crosses_the_hard_floor(nest_instance):
    """The hard floor pins the *shipped* behaviour the two tests above used to
    assert the opposite of: an aggressive policy (``keep_last: 0``, "keep
    nothing recent") over a set already AT the floor prunes nothing — the
    floor is checked even when the union engine alone would prune everything
    (`filesync_handlers.rs:1668-1674`, apps row 302)."""
    actor = create_actor_and_register(nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"])
    fs = _seed_folder(nest_instance, actor, n_snapshots=1)

    with _ws(nest_instance, actor) as ws:
        reply = ws.call(
            "fauna.filesync.snapshot.prune",
            {"folder": fs, "dry_run": False, "policy": {"keep_last": 0}},
        )
    assert reply["pruned"] == 0, reply
    assert reply["remaining"] == 1, reply
    assert _list_count(nest_instance, actor, fs) == 1


def test_prune_dry_run_reports_without_deleting(nest_instance):
    """``dry_run: True`` reports the prune candidate count without mutating —
    the rows survive and the reply echoes ``dry_run``."""
    actor = create_actor_and_register(nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"])
    fs = _seed_folder(nest_instance, actor, n_snapshots=6)

    with _ws(nest_instance, actor) as ws:
        reply = ws.call(
            "fauna.filesync.snapshot.prune",
            {"folder": fs, "dry_run": True, "policy": {"keep_last": 1}},
        )
    assert reply["dry_run"] is True, reply
    assert reply["pruned"] == 3, reply
    assert reply["remaining"] == 3, reply
    assert _list_count(nest_instance, actor, fs) == 6


@pytest.mark.feature("snapshots")
def test_check_integrity_folder_scoped_ok(nest_instance):
    """``fauna.filesync.snapshot.check`` with ``{folder, verify_content}``
    returns ``status: "ok"`` + empty ``errors`` on an intact set."""
    actor = create_actor_and_register(nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"])
    fs = _seed_folder(nest_instance, actor, n_snapshots=1)

    with _ws(nest_instance, actor) as ws:
        reply = ws.call(
            "fauna.filesync.snapshot.check",
            {"folder": fs, "verify_content": False},
        )
    assert reply["status"] == "ok", reply
    assert reply["structured_errors"] == [], reply


@pytest.mark.feature("snapshots")
def test_cross_user_snapshot_get_denied(nest_instance):
    """A second user cannot read another user's snapshot over the real WS-RPC
    wire via ``fauna.filesync.snapshot.get`` — the owner gate closes the F1 IDOR
    on the parallel control plane (review 2026-06-27 § N1). Snapshot ids are
    enumerable ``AUTOINCREMENT``, so before the fix any authed user walked
    ``1..N`` across all users' backup file lists + manifest hashes.
    """
    owner = create_actor_and_register(nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"])
    fs = _seed_folder(nest_instance, owner, n_snapshots=1)
    with _ws(nest_instance, owner) as owner_ws:
        rows = owner_ws.call("fauna.filesync.snapshot.list", {"folder": fs})["rows"]
    assert len(rows) == 1, rows
    snapshot_id = rows[0]["id"]

    # The owner reads their own snapshot fine.
    with _ws(nest_instance, owner) as owner_ws:
        own = owner_ws.call("fauna.filesync.snapshot.get", {"snapshot_id": snapshot_id})
    assert own["id"] == snapshot_id, own

    # A different registered user is rejected — not their snapshot.
    attacker = create_actor_and_register(nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"])
    with _ws(nest_instance, attacker) as attacker_ws:
        with pytest.raises(RpcCallError) as excinfo:
            attacker_ws.call("fauna.filesync.snapshot.get", {"snapshot_id": snapshot_id})
    assert excinfo.value.code == "fauna.filesync.snapshot.permission_denied", excinfo.value


@pytest.mark.feature("snapshots")
def test_snapshot_get_lists_the_seeded_file(nest_instance):
    """``fauna.filesync.snapshot.get`` returns the snapshot's FILE LISTING —
    the row source every app's snapshot-detail pane renders
    (`snapshot-file-download-button`, one row per entry).

    The API-side twin of the UI journeys in ``tests/test_backups_download.py``
    and ``tests/test_backups.py::test_snapshot_file_download_button_downloads_
    sealed_bytes``: those assert rows APPEAR, this asserts the wire actually
    carries them, so a red UI test is immediately attributable to one side.

    Added 2026-07-29 closing a real hole — the pre-existing
    ``test_cross_user_snapshot_get_denied`` calls the same kind but only
    asserts the reply's ``id``, and seeds an EMPTY set, so nothing pinned
    ``files`` being populated at all.
    """
    owner = create_actor_and_register(
        nest_instance["port"],
        admin_signing_key=nest_instance["admin"]["signing_key"],
    )
    # The seed below records a signed change into the set, so the set is born
    # custody-first as its owner (`fauna_ffi.harness_create_set`) rather than
    # over the admin create, which leaves no set nonce to sign under.
    fs = f"backups-{secrets.token_hex(4)}"
    fauna_ffi.harness_create_set(
        nest_instance["url"], bytes(owner["signing_key"]), {"name": fs}
    )
    path = f"docs/listed-{secrets.token_hex(4)}.bin"
    data = secrets.token_bytes(1024)
    seeded = seed_snapshot_with_file_bytes(
        nest_instance["port"],
        secret_key=bytes(owner["signing_key"]).hex(),
        folder=fs,
        path=path,
        data=data,
    )

    with _ws(nest_instance, owner) as ws:
        reply = ws.call("fauna.filesync.snapshot.get",
                        {"snapshot_id": seeded["snapshot_id"]})

    assert reply["file_count"] == 1, (
        f"snapshot metadata should count the seeded file: {reply!r}"
    )
    files = reply["files"]
    assert len(files) == 1, (
        f"snapshot.get should list the one seeded file — an empty listing is "
        f"what leaves every app's detail pane with zero download-button "
        f"rows: {reply!r}"
    )
    # Since the S9 flip, ``SnapshotFileEntry.path`` rests the ratified
    # scrub sentinel ("") unconditionally — the label is ``path_hash`` (a
    # pure fn of the path, no sealing key needed) — same trap and same fix
    # as ``test_media_seed.py``'s ``_media_hashes`` (`file-sync.md` § Sealed
    # names & paths).
    assert files[0]["path"] == "", (
        f"plaintext path should be scrubbed post-S9-flip: {files[0]!r}"
    )
    assert bytes(files[0]["path_hash"]) == blake3.blake3(path.encode()).digest(), (
        f"listed path_hash should match the seeded path's hash: {files[0]!r}"
    )
    assert files[0]["size_bytes"] == len(data), (
        f"listed size should be the seeded byte length: {files[0]!r}"
    )
