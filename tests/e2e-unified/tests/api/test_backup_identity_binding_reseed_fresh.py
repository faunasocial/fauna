"""tier_3: re-seeding a replacement box refuses a target nest that is not fresh
(`docs/features/backup-destinations-and-restore.md` outcome 33).

The after-loss re-seed is client Rust — the shared preflight, then
`pull_back_from_destination` and each set's `fauna.backup.custody.materialize`,
whose empty-target rule refuses a target holding live records the ceremony did
not write (`client-device-custodian.md` § Client-device custodian (pull) →
*Empty-target rule*: `target_not_empty` / `target_not_fresh` on the wire) — so,
per `feature-catalog.md` § The two surfaces, it is driven app-less through the
harness library (`harness_backup_pull_back`)::

    box A (owner): one INBOX message
    enroll D (harness); push                     -> D holds A's mail
    A lost; A' rebuilt from the same seed, claimed
    one DIFFERENT message arrives on A' (INBOX)  -> A' is lived-in, not fresh
    harness_backup_pull_back                     -> the mail set answers AlreadyLive (wire: target_not_empty)
    mailboxes on A'                              -> exactly the one arrival, nothing restored

Every wait is a synchronous barrier (the run-now sweep, each leg's own return),
never a sleep (convention 14).
"""

import os

import pytest

import fauna_ffi
from common.auth import claim_admin, register_user
from helpers.backup_seed import import_one_mail, mailbox_counts, push_backup

pytestmark = pytest.mark.tier_3


@pytest.mark.feature("backup-destinations-and-restore")
def test_reseeding_a_replacement_box_refuses_a_target_nest_that_is_not_fresh(
    request, nest_mode, tmp_path_factory, tmp_path
):
    from conftest import _start_dedicated_nest

    box_a, cleanup_a = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "fresh-box-a")
    request.addfinalizer(cleanup_a)
    box_d, cleanup_d = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "fresh-dest")
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
    assert mailbox_counts(box_a, owner).get("INBOX") == 1

    destination_id, rows = fauna_ffi.harness_backup_enroll(
        box_a["url"], secret, box_d["url"], "Off-site")
    push_backup(box_a)

    # ── Total loss, and the rebuild ──────────────────────────────────────────
    box_a["proc"].kill()
    box_a["proc"].wait()
    box_a2, cleanup_a2 = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "fresh-box-a2", unclaimed=True,
        extra_env={"FAUNA_DEPLOYMENT_SEED": seed_hex})
    request.addfinalizer(cleanup_a2)
    with open(os.path.join(box_a2["tmp_dir"], "claim-code")) as f:
        claim_code = f.read().strip()
    claim_admin(box_a2["port"], claim_code, base_url=box_a2["url"],
                handle=owner.get("handle") or "admin", signing_key=owner["signing_key"])

    # ── The replacement is lived-in: mail arrived before the re-seed ─────────
    fauna_ffi.harness_enable_mail(box_a2["url"], secret)
    import_one_mail(box_a2, owner)
    before = mailbox_counts(box_a2, owner)
    assert before.get("INBOX") == 1, f"pre-state on the replacement: {before!r}"

    # ── The re-seed refuses ──────────────────────────────────────────────────
    refused = fauna_ffi.harness_backup_pull_back(
        box_a2["url"], secret, destination_id, state_path, rows)
    # The wire answers the lived-in mail scope `target_not_empty`; the shared
    # driver reports that set as `AlreadyLive` — "the owner pointed at a
    # lived-in nest", nothing written, no force arm (`reseed.rs` SetOutcome).
    assert refused["sets"].get("__landing/mail") == "AlreadyLive", (
        f"the lived-in mail scope must be refused, not materialized: {refused!r}")

    # Nothing was written: the replacement holds exactly what it held.
    after = mailbox_counts(box_a2, owner)
    assert after == before, (
        f"NEST-SIDE: the refused re-seed changed the target: {before!r} -> {after!r}")


# ── Outcome 32: a destination is held to the identity it proved ─────────────
#
# `backup-destinations.md` § Create / edit / remove protocol, Create step 1: the
# enroll binds the id the destination's connection PROVED (its pin, else a
# possession proof over the handshake), refuses a destination whose own
# `fauna.nest.info` claim names another nest, and every later connection to the
# destination is held to that id. Both checks are shared client Rust
# (`fauna_client_backup::trust::proven_destination_id` at the enroll, the
# destination connect behind the audit / recovery / re-seed at every later
# connection), so they are driven app-less through the harness library.


@pytest.mark.feature("backup-destinations-and-restore")
def test_enrolling_a_destination_whose_claim_disagrees_with_its_proof_is_refused(
    request, nest_mode, tmp_path_factory, tmp_path
):
    """box A (owner), destination D claiming A's id while proving its own::

        harness_backup_enroll(A, D)   -> refused, "reports an identity other than ..."
        A's destination list          -> still empty (the audit pass sees no row)
        D's claim cleared; enroll     -> enrolled under D's PROVEN id (the control)
    """
    from conftest import _start_dedicated_nest
    from helpers.admin_wire import post_nest_hook

    box_a, cleanup_a = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "claim-box-a")
    request.addfinalizer(cleanup_a)
    box_d, cleanup_d = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "claim-dest")
    request.addfinalizer(cleanup_d)
    owner = box_a["admin"]
    secret = bytes(owner["signing_key"])
    register_user(
        box_d["port"], owner["actor_id_hex"], admin_signing_key=box_d["admin"]["signing_key"])
    state_path = str(tmp_path / "audit-state.cbor")

    # D claims to be A — a real nest's id, just not the one D's handshake proves
    # (`nest_id_claim_test_hook`, compiled into the e2e nest only, convention 15).
    assert box_a["nest_id"] != box_d["nest_id"]
    post_nest_hook(box_d, "nest-info-claim", {"nest_id": box_a["nest_id"]})

    with pytest.raises(
        RuntimeError, match="reports an identity other than the one its connection proved"
    ):
        fauna_ffi.harness_backup_enroll(box_a["url"], secret, box_d["url"], "Off-site")

    # Nothing was recorded: the owner's list for A holds no destination.
    assert fauna_ffi.harness_backup_audit_pass(box_a["url"], secret, state_path) == [], (
        "a refused enroll must leave no destination row behind")

    # The control: the same destination, claiming its own id again, enrolls.
    post_nest_hook(box_d, "nest-info-claim", {"nest_id": None})
    destination_id, _rows = fauna_ffi.harness_backup_enroll(
        box_a["url"], secret, box_d["url"], "Off-site")
    rows = fauna_ffi.harness_backup_audit_pass(box_a["url"], secret, state_path)
    assert [r["destination_id"] for r in rows] == [destination_id], rows


@pytest.mark.feature("backup-destinations-and-restore")
def test_a_destination_url_answered_by_another_nest_is_refused_at_every_later_connection(
    request, nest_mode, tmp_path_factory, tmp_path
):
    """box A (owner, one INBOX message), destination D1 enrolled and pushed to::

        harness_backup_pull_back(A, D1)       -> reaches D1 (A is lived-in: AlreadyLive)
        D1 stopped; D2 (another deployment) started on D1's port
        harness_backup_pull_back(A, D1's row) -> refused, "proved identity …, not the destination enrolled as …"
        mailboxes on A                        -> unchanged
    """
    from common.nest import stop_nest
    from conftest import _start_dedicated_nest

    box_a, cleanup_a = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "swap-box-a")
    request.addfinalizer(cleanup_a)
    box_d1, cleanup_d1 = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "swap-dest-1")
    request.addfinalizer(cleanup_d1)
    owner = box_a["admin"]
    secret = bytes(owner["signing_key"])
    register_user(
        box_d1["port"], owner["actor_id_hex"], admin_signing_key=box_d1["admin"]["signing_key"])
    state_path = str(tmp_path / "audit-state.cbor")

    fauna_ffi.harness_enable_mail(box_a["url"], secret)
    import_one_mail(box_a, owner)
    destination_id, _rows = fauna_ffi.harness_backup_enroll(
        box_a["url"], secret, box_d1["url"], "Off-site")
    push_backup(box_a)
    before = mailbox_counts(box_a, owner)
    assert before.get("INBOX") == 1, f"pre-state on A: {before!r}"

    # The control: the enrolled destination connects, and the re-seed reaches
    # the set rule (A is lived-in, so the mail set is refused AlreadyLive).
    reached = fauna_ffi.harness_backup_pull_back(box_a["url"], secret, destination_id, state_path)
    assert reached["sets"].get("__landing/mail") == "AlreadyLive", reached

    # ── Another deployment takes over the destination's URL ─────────────────
    stop_nest(box_d1)
    box_d2, cleanup_d2 = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "swap-dest-2", port=box_d1["port"],
        extra_env={"FAUNA_DEPLOYMENT_SEED": os.urandom(32).hex()})
    request.addfinalizer(cleanup_d2)
    assert box_d2["url"] == box_d1["url"]
    assert box_d2["nest_id"] != box_d1["nest_id"]
    # The owner is a user of D2 as well, so the handshake succeeds and the only
    # thing wrong with the connection is the identity it proves.
    register_user(
        box_d2["port"], owner["actor_id_hex"], admin_signing_key=box_d2["admin"]["signing_key"])

    with pytest.raises(RuntimeError, match="not the destination enrolled as") as refused:
        fauna_ffi.harness_backup_pull_back(box_a["url"], secret, destination_id, state_path)
    assert box_d2["nest_id"] in str(refused.value), (
        f"the refusal must name the identity the URL now proves: {refused.value}")

    after = mailbox_counts(box_a, owner)
    assert after == before, (
        f"NEST-SIDE: the refused connection changed A: {before!r} -> {after!r}")
