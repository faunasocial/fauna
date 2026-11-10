"""File versions: what they cost, and who may see them — on a real
``fauna-nest`` binary, over the WS-RPC wire.

Owner doc: ``docs/goal/behavior/file-versions.md``.

**Metering** (§ Retention, ruling 4, built 2026-08-18): *every recorded version
counts against the owner's storage* — a modify, a shrink included, charges its
full ``size_bytes``; the only things that give bytes back are pruning a version
and deleting the folder. The test reads the owner's own transparency figure
(``fauna.account.get`` → ``quota.storage.used_bytes``) after each step of one
file's life: write 1000 B, overwrite with 400 B (the figure must NOT fall — the
old version is still retained and metered), overwrite with 100 B, bound the
folder to one version per path and run the real prune pipeline (the same
``schedule_version_auto_prune`` sweep the GC cycle runs, then the executor, via
the two ``/api/v1/test`` hooks that exist because that cycle is 6-hourly) — the
oldest version is soft-pruned and its 1000 B are credited — then delete the
folder, which returns the figure to where it started.

**Audience** (§ Wire surface): ``fauna.files.versions.list`` reads only the
caller's *label-audience* sets (owner + roster member). The admin-discovery
grant does not reach version history, and a stranger holds no grant at all: both
get the same absent-not-denied empty list a path with no history gets — no
existence oracle on a guessed ``path_hash``. The member's own read of the very
same request is non-empty, which is what makes the two empty answers an
audience verdict and not a wrong request.

Process safety: no ``pkill``/``killall``; the nest is the session-scoped
``nest_instance`` and tears itself down.
"""

from __future__ import annotations

import secrets

import blake3
import pytest

from conftest import _bridge_admin_post, live_admin_token

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import (
    create_actor_and_register,
    sync_changes_record,
    sync_register,
    user_create_folder,
)
from helpers.set_names import addressed

from tests.api import conv_api

pytestmark = pytest.mark.tier_3


def _ws(nest, actor) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        nest["url"],
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


def _path_hash(path: str) -> bytes:
    """``fauna_core::sync::path_hash`` — plain BLAKE3 over the relative path."""
    return blake3.blake3(path.encode()).digest()


def _used(ws) -> int:
    return ws.call("fauna.account.get", {})["quota"]["storage"]["used_bytes"]


@pytest.mark.feature("media")
def test_every_version_counts_shrinking_frees_nothing_pruning_and_deleting_free(nest_instance):
    port = nest_instance["port"]
    owner = create_actor_and_register(
        port, admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    secret = bytes(owner["signing_key"]).hex()
    device_id = secrets.token_hex(32)
    sync_register(port, secret_key=secret, device_id=device_id)

    folder = f"vermeter-{secrets.token_hex(4)}"
    path = f"docs/report-{secrets.token_hex(4)}.bin"

    def record(size: int, change_type: str):
        sync_changes_record(
            port,
            secret_key=secret,
            folder=folder,
            device_id=device_id,
            path=path,
            manifest_hash=secrets.token_bytes(32).hex(),
            size_bytes=size,
            change_type=change_type,
        )

    with _ws(nest_instance, owner) as ws:
        baseline = _used(ws)
        user_create_folder(port, folder, secret_key=secret)
        assert _used(ws) == baseline, "creating an empty folder must cost nothing"

        record(1000, "create")
        after_v1 = _used(ws)
        assert after_v1 == baseline + 1000, (baseline, after_v1)

        # Shrink: the old 1000 B version is still retained, so nothing is freed
        # — the new 400 B version is metered on top.
        record(400, "update")
        after_shrink = _used(ws)
        assert after_shrink >= after_v1, (
            f"shrinking a file must free nothing: {after_v1} → {after_shrink}"
        )
        assert after_shrink == after_v1 + 400, (after_v1, after_shrink)

        record(100, "update")
        after_v3 = _used(ws)
        assert after_v3 == after_shrink + 100, (after_shrink, after_v3)

        # Bound the history to one version per path, then run the real prune
        # pipeline: evaluate → schedule → (made due) executor soft-prunes. The
        # head is never a candidate and the floor keeps the newest prior
        # version, so exactly the oldest (1000 B) goes.
        ws.call(
            "fauna.folders.update",
            addressed(folder, version_retention={"max_versions_per_path": 1, "max_age_days": 0}),
        )
        token = live_admin_token(nest_instance)
        assert token, "the session nest is claimed; an admin bearer must mint"
        scheduled = _bridge_admin_post(
            nest_instance["url"], token, "/api/v1/test/version_prune/evaluate", {}
        )
        assert scheduled.get("scheduled", 0) >= 1, scheduled
        ran = _bridge_admin_post(
            nest_instance["url"], token, "/api/v1/test/pending_actions/run_due", {}
        )
        assert ran.get("executed", 0) >= 1, ran

        after_prune = _used(ws)
        assert after_prune == after_v3 - 1000, (
            f"pruning the oldest 1000 B version must credit it: {after_v3} → {after_prune}"
        )

        # Deleting the folder frees everything it still charges.
        ws.call("fauna.folders.delete", addressed(folder))
        assert _used(ws) == baseline, (
            "deleting the folder must return the storage figure to its baseline"
        )


_SHARED_GROUP_ID = bytes([0x5E] * 24).hex()


@pytest.mark.feature("media")
def test_only_the_folders_audience_sees_a_files_versions(nest_instance):
    url = nest_instance["url"]
    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]

    owner = create_actor_and_register(port, admin_signing_key=admin_sk)
    member = create_actor_and_register(port, admin_signing_key=admin_sk)
    stranger = create_actor_and_register(port, admin_signing_key=admin_sk)
    admin_actor = {"actor_id_bytes": bytes(admin_sk.verify_key), "signing_key": admin_sk}
    secret = bytes(owner["signing_key"]).hex()
    device_id = secrets.token_hex(32)
    sync_register(port, secret_key=secret, device_id=device_id)

    folder = f"veraud-{secrets.token_hex(4)}"
    path = f"notes/{secrets.token_hex(4)}.txt"
    user_create_folder(port, folder, secret_key=secret)
    for size, change_type in ((300, "create"), (200, "update")):
        sync_changes_record(
            port,
            secret_key=secret,
            folder=folder,
            device_id=device_id,
            path=path,
            manifest_hash=secrets.token_bytes(32).hex(),
            size_bytes=size,
            change_type=change_type,
        )

    # Roster the member through the production Welcome path as a reader.
    share = conv_api.folder_share(port, owner, folder, _SHARED_GROUP_ID)
    conv_api.set_inbox_mode(port, member, "open")
    conv_api.welcome_deliver(
        port,
        owner,
        member["actor_id_hex"],
        share["channel_id"],
        b"\x01\x02\x03",
        kind={"type": "folder", "group_id": _SHARED_GROUP_ID},
    )
    conv_api.folder_set_access(port, owner, folder, member["actor_id_hex"], "reader")

    scoped = addressed(folder, path_hash=_path_hash(path))
    unscoped = {"path_hash": _path_hash(path)}

    def listed(actor: dict, payload: dict) -> list:
        with _ws(nest_instance, actor) as ws:
            return ws.call("fauna.files.versions.list", payload)["versions"]

    for payload in (scoped, unscoped):
        assert sorted(v["size_bytes"] for v in listed(owner, payload)) == [200, 300]
        assert sorted(v["size_bytes"] for v in listed(member, payload)) == [200, 300], (
            "a roster member must see the folder's versions"
        )

    for who, actor in (("an admin outside the audience", admin_actor), ("a non-member", stranger)):
        for payload in (scoped, unscoped):
            assert listed(actor, payload) == [], (
                f"{who} must get nothing from versions.list"
            )
