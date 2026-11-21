"""The Backups page offers only what is possible: each journey makes a
precondition unmet through the page itself and reads what the page offers then.

Goal: ``docs/goal/ui/README.md`` § Offer only what is possible (rule 2: what
the person can unblock is disabled with its reason within eyeshot; what they
can never do on this app is not shown). The page's instance:
``docs/goal/ui/backups.md`` § Errors & edge cases → *What the page offers* —
every gated control paints from one shared answer in
``fauna_backups_machine::possibility``, its reason beside it.

**The witness rule.** ``docs/goal/architecture/feature-catalog.md`` § The
coverage contract: a possibility outcome is witnessed by a journey on its own
page, which fails when the control is enabled while its precondition is unmet,
when its reason is not within eyeshot, or when a control this person can never
use is shown. The machine tests in ``fauna-backups-machine`` and tui's
in-process walk invariant are not that witness.

**The reason is read as painted** (``PlatformDriver.blocked_reason``) and
compared with the ``S.`` string of the predicate's reason key, never a literal;
an app whose driver cannot read it yet takes the ``blocked-reason-read``
surface gate.

**What no journey can make unmet.** ``set_action_possibility`` blocks create,
prune and check while no folder is selected, but the machine selects the first
entry of a list that always carries the message-kind entries
(``fauna_backups_machine`` ``load``), so after the first read a selection always
stands; the only unmet state left is the single-flight gate's, a window a
journey could only race (convention 14). Outcome 17 therefore names the delete
and restore halves alone. A restore *running* (``snapshot_restore_possibility``'s
first arm, ``restore_possibility``'s second) is that same race and is left to
the machine tests.
"""

from __future__ import annotations

import os
import secrets
import tempfile

import pytest

from helpers.app_surface import app_name, require_surface
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.waiting import wait_until
from i18n.strings import S
from tests.test_backups import MAIL_KIND_ENTRY, _grow_set_to, _seed_folder_with_snapshot

pytestmark = [pytest.mark.tier_3]

# Apps on which the whole-snapshot restore is a declared absence — they keep the
# per-file download instead (`ui/backups.md` § Whole-snapshot restore;
# `test_backups.py::test_snapshot_restore_writes_the_whole_tree_byte_identically`).
_NO_WHOLE_SNAPSHOT_RESTORE = {"web", "ios", "android"}


def _assert_blocked_with(driver, element_id: str, reason: str, *, scope: str | None = None,
                         index: int = 0, what: str) -> None:
    """``element_id`` is shown, not live, and the words ``reason`` are painted
    beside it. One message names every part that failed (convention 6)."""
    diag = driver.diagnose(element_id, attrs=("enabled", "reason", "title"), scope=scope)
    assert not driver.is_absent(element_id, scope=scope), (
        f"{what}: {element_id} must stay visible while the person can unblock it "
        f"(ui/README.md § Offer only what is possible, rule 2): {diag}"
    )
    assert not driver.is_enabled(element_id, scope=scope), (
        f"{what}: {element_id} is live while its precondition is unmet: {diag}"
    )
    painted = driver.blocked_reason(element_id, index, scope=scope)
    assert painted == reason, (
        f"{what}: the reason beside {element_id} should read {reason!r}, "
        f"painted {painted!r}: {diag}"
    )


def _wait_reason(driver, element_id: str, reason: str, *, scope: str | None = None,
                 index: int = 0, budget: float = RPC_ROUNDTRIP_S) -> None:
    """Wait for ``reason`` to be painted beside ``element_id`` — the page's reads
    resolve asynchronously, so the painted reason is the causal barrier."""
    wait_until(
        lambda: not driver.is_absent(element_id, scope=scope)
        and driver.blocked_reason(element_id, index, scope=scope) == reason,
        budget,
        diagnose=lambda: (
            f"{element_id} never painted {reason!r}: "
            f"{driver.diagnose(element_id, attrs=('enabled', 'reason', 'title'), scope=scope)} "
            f"error-message={driver.get_text('error-message') if driver.is_visible('error-message') else ''!r}"
        ),
    )


def _kind_snapshot_ids(nest_instance, owner, kind: str) -> set[int]:
    """The ids the nest's own message-kind list serves for ``kind`` — the split
    verdict (convention 5) for which row a create added."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=owner["actor_id_bytes"],
        signing_key=bytes(owner["signing_key"]),
    )
    with client:
        rows = client.call("fauna.filesync.snapshot.list", {"message_kind": kind})["rows"]
    return {row["id"] for row in rows}


# ── snapshots 17 ──────────────────────────────────────────────────────────────


@pytest.mark.feature("snapshots")
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_restore_and_delete_say_why_until_their_snapshot_allows_them(
    dedicated_actor_app, nest_instance,
):
    """``backups.md`` § Errors & edge cases → *What the page offers*:

    * the restore kinds, ``restore-confirm-input`` and ``restore-confirm-button``
      wait, with their reason, for a snapshot to exist
      (``restore_inputs_possibility``) — a fresh actor holds no message-kind
      snapshot, so the restore list is empty;
    * once one exists, ``restore-confirm-button`` waits for its id to be
      re-typed, and again for its kind to be ticked (``restore_possibility``);
    * ``snapshot-delete-button`` is not offered on a row already waiting for
      deletion (``delete_possibility``), while ``snapshot-immediate-delete-button``
      stays live there — ending the row now is a real choice.

    A dedicated actor because the shared ``test_user`` collects message-kind
    snapshots from other modules, and "no snapshot to restore" holds only on an
    account nobody else touches. The 48 h delete is queued over the owner's own
    ``fauna.filesync.snapshot.delete``: ``snapshot-delete-button`` opens a confirm
    no ui.yaml id reaches (the convention-8 fixture exemption
    ``test_a_snapshot_on_its_way_out_says_so_on_its_own_row`` documents), and the
    act under test is what the row offers afterwards."""
    app, user = dedicated_actor_app
    driver = app.driver
    require_surface(driver, "blocked-reason-read")
    backups = app.backups
    backups.navigate()

    # ── 1. No snapshot to restore: every restore input says so ─────────────
    no_snapshots = S.backups.restore_no_snapshots
    _wait_reason(driver, "restore-confirm-button", no_snapshots)
    _assert_blocked_with(driver, "restore-confirm-button", no_snapshots,
                         what="the restore with no snapshot to restore")
    _assert_blocked_with(driver, "restore-confirm-input", no_snapshots,
                         what="the re-type box with no snapshot to restore")
    _assert_blocked_with(driver, "restore-kind-checkbox", no_snapshots,
                         what="the Mail kind with no snapshot to restore")

    # ── 2. A mail snapshot, taken from its kind entry through the page ─────
    backups.select_folder(MAIL_KIND_ENTRY)
    backups.wait_for_no_snapshots()
    backups.create_snapshot()
    added = wait_until(
        lambda: _kind_snapshot_ids(nest_instance, user, "mail") or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"snapshot-create-button on the Mail entry added no mail snapshot; "
            f"error-message: {app.error_text()!r}"
        ),
    )
    snapshot_id = max(added)
    marker = f"(#{snapshot_id})"
    wait_until(
        lambda: any(marker in option for option in (driver.option_texts("restore-snapshot-select") or [])),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"restore-snapshot-select never offered snapshot {snapshot_id}: "
            f"{driver.option_texts('restore-snapshot-select')!r} error={app.error_text()!r}"
        ),
    )

    # Nothing re-typed yet: the button names the step.
    _wait_reason(driver, "restore-confirm-button", S.backups.restore_progress_idle)
    _assert_blocked_with(driver, "restore-confirm-button", S.backups.restore_progress_idle,
                         what=f"snapshot {snapshot_id} with its id not re-typed")
    assert driver.is_enabled("restore-confirm-input"), (
        "once a snapshot exists the re-type box must be live: "
        f"{driver.diagnose('restore-confirm-input', attrs=('enabled', 'reason'))}"
    )
    backups.type_restore_confirm(str(snapshot_id))
    driver.wait_until_enabled("restore-confirm-button", timeout=10.0)

    # Its kind unticked: the button says to tick it. Mail is the first box.
    driver.click("restore-kind-checkbox", index=0)
    try:
        _wait_reason(driver, "restore-confirm-button", S.backups.restore_kind_unticked, budget=10.0)
        _assert_blocked_with(driver, "restore-confirm-button", S.backups.restore_kind_unticked,
                             what=f"mail snapshot {snapshot_id} with Mail unticked")
    finally:
        driver.click("restore-kind-checkbox", index=0)
    driver.wait_until_enabled("restore-confirm-button", timeout=10.0)
    backups.type_restore_confirm("")

    # ── 3. A row waiting for deletion offers no second delete ──────────────
    # Four snapshots, not one: the nest refuses a delete at or under the hard
    # floor of 3 active snapshots (`check_snapshot_delete_allowed`), so a
    # one-snapshot set answers `hard_floor_breach` and nothing is queued. The
    # NEWEST is queued so its row is index 0 — always rendered, even in a
    # virtualizing list (`test_backups.py` `fresh_backup_set`).
    folder, first_id = _seed_folder_with_snapshot(
        nest_instance, user, name=f"possible-{secrets.token_hex(4)}"
    )
    seeded_id = _grow_set_to(nest_instance, user, folder, first_id, 4)
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    owner = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    )
    with owner:
        queued = owner.call("fauna.filesync.snapshot.delete", {"snapshot_id": seeded_id})
    assert queued["status"] == "pending", (
        f"the 48 h delete did not queue for snapshot {seeded_id}: {queued!r}"
    )
    # Re-mount so the page re-reads: the queued delete was not its own gesture.
    driver.navigate_to("feed")
    backups.navigate()
    backups.select_folder(folder)
    row = backups.wait_for_snapshot_row(seeded_id)
    scope = f"snapshot-item[{row}]"
    _wait_reason(driver, "snapshot-delete-button", S.backups.snapshot_delete_already_pending,
                 scope=scope)
    _assert_blocked_with(driver, "snapshot-delete-button", S.backups.snapshot_delete_already_pending,
                         scope=scope, what=f"snapshot {seeded_id}, already waiting for deletion")
    assert driver.is_enabled("snapshot-immediate-delete-button", scope=scope), (
        "ending a row that waits for deletion now is a real choice, so the immediate "
        "delete must stay live there: "
        f"{driver.diagnose('snapshot-immediate-delete-button', attrs=('enabled', 'reason'), scope=scope)}"
    )


# ── backup-destinations-and-restore 34 ────────────────────────────────────────


@pytest.mark.feature("backup-destinations-and-restore")
@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_the_destination_dialog_and_the_snapshot_restore_say_what_they_still_need(
    kept_login_app, nest_instance, test_user,
):
    """``backups.md`` § Errors & edge cases → *What the page offers*:

    * the destination dialog's confirm (``destination_form_possibility``) is
      blocked with its reason while a nest destination has no address, and on
      the add-device form while a typed capacity does not read as a size — a
      blank capacity is uncapped, a real choice;
    * ``snapshot-restore-button`` (``snapshot_restore_possibility``) is blocked
      with its reason until a destination folder is typed.

    Nothing is confirmed or restored: each leg reads the control live once its
    input is filled, and the dialog is cancelled. On web the add-device form's
    kind select (enrolment runs on the device being enrolled) and the
    whole-snapshot restore are declared absences, so there the journey asserts
    they are not shown."""
    app = kept_login_app
    driver = app.driver
    require_surface(driver, "blocked-reason-read")
    backups = app.backups
    platform = app_name(driver)
    confirm = "backup-destination-add-confirm-button"

    # The set leg 3 restores from is seeded BEFORE the page mounts: the folder
    # selector lists the sets its first read returned, so a set seeded after
    # the mount is never offered (the page re-reads only after its own gestures).
    folder, snapshot_id = _seed_folder_with_snapshot(nest_instance, test_user)

    # ── 1. A nest destination needs its address ────────────────────────────
    # Leave first: the kept app may still stand on Backups from an earlier
    # test, and only a fresh mount reads the set seeded above.
    driver.navigate_to("feed")
    backups.navigate()
    backups.open_add_destination()
    try:
        _wait_reason(driver, confirm, S.backups.backup_destination_url_required, budget=10.0)
        _assert_blocked_with(driver, confirm, S.backups.backup_destination_url_required,
                             what="the add-destination dialog with no address")
        driver.clear_and_type("backup-destination-url-input",
                              f"https://possible-{secrets.token_hex(3)}.invalid")
        driver.wait_until_enabled(confirm, timeout=10.0)

        # ── 2. The add-device form: a capacity must read as a size ─────────
        if platform == "web":
            assert driver.is_absent("backup-destination-kind-select"), (
                "web declares the add-device form absent (enrolment runs on the "
                "device being enrolled), so it must not offer the kind select: "
                f"{driver.diagnose('backup-destination-kind-select')}"
            )
        else:
            backups.select_destination_kind("client-device")
            driver.clear_and_type("backup-destination-capacity-input", "a lot")
            _wait_reason(driver, confirm, S.backups.backup_destination_capacity_invalid,
                         budget=10.0)
            _assert_blocked_with(driver, confirm, S.backups.backup_destination_capacity_invalid,
                                 what="the add-device form with an unreadable capacity")
            driver.clear_and_type("backup-destination-capacity-input", "")
            driver.wait_until_enabled(confirm, timeout=10.0)
    finally:
        if not driver.is_absent("backup-destination-add-cancel-button"):
            driver.click("backup-destination-add-cancel-button")

    # ── 3. The whole-snapshot restore needs a folder to restore into ───────
    backups.select_folder(folder)
    row = backups.wait_for_snapshot_row(snapshot_id)
    backups.open_snapshot(row)
    if platform in _NO_WHOLE_SNAPSHOT_RESTORE:
        assert driver.is_absent("snapshot-restore-button"), (
            f"{platform} declares the whole-snapshot restore absent "
            "(ui/backups.md § Whole-snapshot restore), so it must not show it: "
            f"{driver.diagnose('snapshot-restore-button')}"
        )
        return
    driver.clear_and_type("snapshot-restore-destination-input", "")
    reason = S.backups.snapshot_restore_destination_required
    _wait_reason(driver, "snapshot-restore-button", reason, budget=10.0)
    _assert_blocked_with(driver, "snapshot-restore-button", reason,
                         what=f"snapshot {snapshot_id} with no folder typed to restore into")
    driver.clear_and_type("snapshot-restore-destination-input",
                          os.path.join(tempfile.gettempdir(), f"possible-{secrets.token_hex(3)}"))
    driver.wait_until_enabled("snapshot-restore-button", timeout=10.0)
    driver.clear_and_type("snapshot-restore-destination-input", "")
