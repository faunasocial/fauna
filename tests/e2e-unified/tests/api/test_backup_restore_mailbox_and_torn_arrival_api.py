"""tier_3: mail restored from a nest backup destination lands back in the
mailbox it was filed in, and mail that arrives while the restore is torn is
kept when the retry completes it
(`docs/features/backup-destinations-and-restore.md` outcome 27).

The after-loss re-seed is client Rust — the shared preflight, then
`pull_back_from_destination` and each set's `fauna.backup.custody.materialize`
(`client-device-custodian.md` § Implementation status today: the placement
journal's fold gives each message its mailbox; the in-flight marker lets a
retry complete a flip an interruption left standing, over whatever arrived in
between) — so, per `feature-catalog.md` § The two surfaces, it is driven
app-less through the harness library (`harness_backup_pull_back`). The tear is
the nest's test seam (`bins/fauna-nest/src/materialize_tear_test_hook.rs`),
which stops the REAL verb at one of its two named points, so the half-state is
the production path's own; each point is its own case::

    box A (owner): one INBOX message, one filed in "Projects"
    enroll D (harness); push                     -> D holds A's mail
    A lost; A' rebuilt from the same seed, claimed
    arm the tear at <point>
    harness_backup_pull_back                     -> torn: not whole, the seam fired once
    one message arrives on A' (INBOX)            -> the arrival during the torn restore
    harness_backup_pull_back                     -> the retry completes it: whole
    mailboxes on A'                              -> INBOX: restored + arrival;
                                                    Projects: its message, back in it

Every wait is a synchronous barrier (the run-now sweep, each leg's own return),
never a sleep (convention 14).
"""

import os

import pytest
import requests

import fauna_ffi
from common.auth import claim_admin, register_user
from helpers.backup_seed import import_one_mail, mailbox_counts, push_backup

pytestmark = pytest.mark.tier_3

_FILED = "Projects"


def _tear(nest: dict, at: str) -> dict:
    resp = requests.post(
        f"{nest['url']}/api/v1/test/backup/materialize_tear", json={"at": at}, timeout=30)
    assert resp.status_code == 200, f"arm the tear: {resp.status_code} {resp.text}"
    return resp.json()


def _tear_report(nest: dict) -> dict:
    resp = requests.get(f"{nest['url']}/api/v1/test/backup/materialize_tear", timeout=30)
    assert resp.status_code == 200, f"read the tear: {resp.status_code} {resp.text}"
    return resp.json()


@pytest.mark.feature("backup-destinations-and-restore")
@pytest.mark.parametrize("point", ["between-containers-and-halves", "after-journal-adopt"])
def test_restored_mail_lands_in_its_mailbox_and_an_arrival_during_a_torn_restore_is_kept(
    point, request, nest_mode, tmp_path_factory, tmp_path
):
    from conftest import _start_dedicated_nest

    box_a, cleanup_a = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "torn-box-a")
    request.addfinalizer(cleanup_a)
    box_d, cleanup_d = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "torn-dest")
    request.addfinalizer(cleanup_d)
    owner = box_a["admin"]
    secret = bytes(owner["signing_key"])
    seed_hex = owner["deployment_seed"]
    assert seed_hex, "the claim reply must carry the deployment seed the rebuild needs"
    register_user(
        box_d["port"], owner["actor_id_hex"], admin_signing_key=box_d["admin"]["signing_key"])
    state_path = str(tmp_path / "audit-state.cbor")

    fauna_ffi.harness_enable_mail(box_a["url"], secret)
    import_one_mail(box_a, owner)
    import_one_mail(box_a, owner, mailbox=_FILED)
    on_a = mailbox_counts(box_a, owner)
    assert on_a.get("INBOX") == 1 and on_a.get(_FILED) == 1, f"pre-state on A: {on_a!r}"

    destination_id, rows = fauna_ffi.harness_backup_enroll(
        box_a["url"], secret, box_d["url"], "Off-site")
    push_backup(box_a)

    # ── Total loss, and the rebuild ──────────────────────────────────────────
    box_a["proc"].kill()
    box_a["proc"].wait()
    box_a2, cleanup_a2 = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "torn-box-a2", unclaimed=True,
        extra_env={"FAUNA_DEPLOYMENT_SEED": seed_hex})
    request.addfinalizer(cleanup_a2)
    assert box_a2["nest_id"] == box_a["nest_id"], "the rebuild re-presents the same identity"
    with open(os.path.join(box_a2["tmp_dir"], "claim-code")) as f:
        claim_code = f.read().strip()
    claim_admin(box_a2["port"], claim_code, base_url=box_a2["url"],
                handle=owner.get("handle") or "admin", signing_key=owner["signing_key"])

    # ── The restore, torn by the seam ────────────────────────────────────────
    assert _tear(box_a2, point)["armed"] == point
    torn = fauna_ffi.harness_backup_pull_back(
        box_a2["url"], secret, destination_id, state_path, rows)
    assert torn["whole"] is False, f"a torn restore is not whole: {torn!r}"
    assert _tear_report(box_a2) == {"armed": None, "torn": 1}, (
        "the seam fired exactly once, on this restore's materialize")

    # ── Mail arrives while the restore stands torn ───────────────────────────
    fauna_ffi.harness_enable_mail(box_a2["url"], secret)
    import_one_mail(box_a2, owner)

    retried = fauna_ffi.harness_backup_pull_back(
        box_a2["url"], secret, destination_id, state_path, rows)
    assert retried["whole"] is True, f"the retry completes the torn restore: {retried!r}"

    on_a2 = mailbox_counts(box_a2, owner)
    assert on_a2.get(_FILED) == 1, (
        f"NEST-SIDE: the message filed in {_FILED!r} must be back in {_FILED!r}, not "
        f"in the inbox: {on_a2!r}")
    assert on_a2.get("INBOX") == 2, (
        f"NEST-SIDE: the inbox must hold the restored message and the one that "
        f"arrived during the torn restore, each once: {on_a2!r}")
