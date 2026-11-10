"""tier_3: the harness's backup legs reach real nests, app-less — a smoke of
the exports, not a witness of any outcome.

The legs a nest-to-nest backup's `[nest]` outcomes are kept by are CLIENT Rust
(`client-device-custodian.md` § Implementation status today: the recovery leg
`fauna_sync_engine::recovery::recover_from_destination`, the re-seed delivery
leg), and `feature-catalog.md` § The two surfaces (ruling 2026-10-13) says such
an outcome is witnessed app-less through the harness library, never through a
second driver. This module proves each export of that library
(`libs/fauna-ffi/src/cabi/harness/backup.rs`) and the nest's restore tear seam
(`bins/fauna-nest/src/materialize_tear_test_hook.rs`) reach real nests, so an
outcome witness has something to drive:

    box A claimed, one mail       -> harness_backup_enroll (D)  -> D registered
    A's sweep                     -> D holds A's mail
    harness_backup_audit_pass     -> one row, D's
    harness_backup_recover        -> the shared driver: nothing regressed
    A lost; A' rebuilt, same seed -> the tear armed after the journal adopt
    harness_backup_pull_back      -> torn: not whole, the seam fired once
    harness_backup_pull_back      -> the retry completes it: whole, mail back

Every wait is a synchronous barrier (the run-now sweep, each leg's own return),
never a sleep (convention 14).
"""

import os

import pytest
import requests

import fauna_ffi
from common.auth import claim_admin, register_user
from helpers.backup_seed import import_one_mail, inbox_count, push_backup

pytestmark = pytest.mark.tier_3


def _tear(nest: dict, at) -> dict:
    resp = requests.post(
        f"{nest['url']}/api/v1/test/backup/materialize_tear", json={"at": at}, timeout=30)
    assert resp.status_code == 200, f"arm the tear: {resp.status_code} {resp.text}"
    return resp.json()


def _tear_report(nest: dict) -> dict:
    resp = requests.get(f"{nest['url']}/api/v1/test/backup/materialize_tear", timeout=30)
    assert resp.status_code == 200, f"read the tear: {resp.status_code} {resp.text}"
    return resp.json()


def _held_at(dest: dict, owner: dict) -> set:
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    with WsRpcAdminClient(
        dest["url"],
        actor_id=owner["actor_id_bytes"],
        signing_key=bytes(owner["signing_key"]),
    ) as client:
        items = client.call("fauna.backup.custody.list", {}).get("items", [])
    return {i.get("folder_name") for i in items}


def test_the_backup_legs_and_the_tear_seam_reach_real_nests(
    request, nest_mode, tmp_path_factory, tmp_path
):
    from conftest import _start_dedicated_nest

    box_a, cleanup_a = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "legs-box-a")
    request.addfinalizer(cleanup_a)
    box_d, cleanup_d = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "legs-dest")
    request.addfinalizer(cleanup_d)
    owner = box_a["admin"]
    secret = bytes(owner["signing_key"])
    seed_hex = owner["deployment_seed"]
    assert seed_hex, "the claim reply must carry the deployment seed the rebuild needs"
    register_user(
        box_d["port"], owner["actor_id_hex"], admin_signing_key=box_d["admin"]["signing_key"]
    )
    state_path = str(tmp_path / "audit-state.cbor")

    fauna_ffi.harness_enable_mail(box_a["url"], secret)
    import_one_mail(box_a, owner)
    assert inbox_count(box_a, owner) == 1

    # ── enroll ───────────────────────────────────────────────────────────────
    destination_id, rows = fauna_ffi.harness_backup_enroll(
        box_a["url"], secret, box_d["url"], "Off-site")
    assert destination_id and rows
    push_backup(box_a)
    assert "__mail" in _held_at(box_d, owner), (
        "NEST-SIDE: after the enroll and A's sweep, D holds A's mail")

    # ── audit pass ───────────────────────────────────────────────────────────
    audited = fauna_ffi.harness_backup_audit_pass(box_a["url"], secret, state_path)
    assert [r["destination_id"] for r in audited] == [destination_id], audited
    assert os.path.exists(state_path), "the pass persisted its store at the test's path"
    assert not audited[0]["recover_offered"], (
        f"nothing regressed, so no recovery is offered: {audited!r}")

    # ── recovery: nothing regressed, the shared driver answers so ────────────
    recovered = fauna_ffi.harness_backup_recover(
        box_a["url"], secret, destination_id, state_path)
    assert recovered["whole"] is False, (
        f"a run with nothing to recover is not whole: {recovered!r}")

    # ── total loss, and the rebuild ──────────────────────────────────────────
    box_a["proc"].kill()
    box_a["proc"].wait()
    box_a2, cleanup_a2 = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "legs-box-a2", unclaimed=True,
        extra_env={"FAUNA_DEPLOYMENT_SEED": seed_hex})
    request.addfinalizer(cleanup_a2)
    assert box_a2["nest_id"] == box_a["nest_id"], "the rebuild re-presents the same identity"
    with open(os.path.join(box_a2["tmp_dir"], "claim-code")) as f:
        claim_code = f.read().strip()
    claim_admin(box_a2["port"], claim_code, base_url=box_a2["url"],
                handle=owner.get("handle") or "admin", signing_key=owner["signing_key"])
    assert inbox_count(box_a2, owner) == 0, "pre-state: the rebuilt box holds no mail"

    # ── the re-seed, torn by the seam, then retried ──────────────────────────
    assert _tear(box_a2, "after-journal-adopt")["armed"] == "after-journal-adopt"
    torn = fauna_ffi.harness_backup_pull_back(
        box_a2["url"], secret, destination_id, state_path, rows)
    assert torn["whole"] is False, f"a torn flip is not whole: {torn!r}"
    assert _tear_report(box_a2) == {"armed": None, "torn": 1}, (
        "the seam fired exactly once, on this re-seed's materialize")
    assert inbox_count(box_a2, owner) == 0, "the torn flip serves nothing"

    retried = fauna_ffi.harness_backup_pull_back(
        box_a2["url"], secret, destination_id, state_path, rows)
    assert retried["whole"] is True, f"the retry completes the torn flip: {retried!r}"
    assert inbox_count(box_a2, owner) == 1, "the mail is back on the rebuilt box"
