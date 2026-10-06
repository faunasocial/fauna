import os
import re
import secrets
import tempfile
import time

import pytest
import requests

from common.auth import (
    create_folder_snapshot,
    register_user,
    user_create_folder,
    user_folder_ref,
)
from helpers.app_surface import app_name, skip_unbuilt
from helpers.harness_writer import HarnessWriter
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]


def _seed_folder_with_snapshot(nest_instance, owner, name=None, with_snapshot=True):
    """Create a folder owned by `owner` plus one snapshot in it.

    Backups operate on a *folder*; a fresh nest has none, so the
    `snapshot-create-button` flow (and the detail / delete-button surfaces the
    tests below exercise) has nothing to act on. This seeds one real folder
    via the canonical Admin WS-RPC surface — `fauna.admin.folders.create`
    (owned by the logged-in actor so it surfaces in that actor's backup-status
    fetch) plus `fauna.filesync.snapshot.create_folder` for an initial
    snapshot — mirroring what device enrollment would create in production.
    Returns `(folder_name, snapshot_id)`.

    The snapshot id is returned, not discarded, because it is the only
    *identity* a test can use as its causal barrier that the list pane has
    finished switching to this set (testing.md convention 14) — see
    `BackupsActions.wait_for_snapshot_row`. A bare row count cannot do that job:
    the previously selected set's rows stay mounted while the new set's fetch is
    in flight, so a count threshold is satisfied by the *stale* rows.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    # `folders.name` is globally UNIQUE and `nest_instance` is session-scoped,
    # so each distinct set needs a distinct name to avoid a `fauna.admin.conflict`.
    if name is None:
        name = f"documents-{secrets.token_hex(4)}"

    admin = nest_instance["admin"]
    admin_sk = admin["signing_key"]
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
    if not with_snapshot:
        # No snapshot: the caller drives the FIRST create through the UI. That is
        # what makes such a test deterministic — `create_snapshot_v2` dedups on
        # `UNIQUE(folder_id, created_at)` at SECONDS granularity and returns the
        # existing row, so a UI create landing in the same wall-clock second as a
        # seeded snapshot legitimately adds NO row (`filesync_handlers.rs`
        # § create_folder → sync_storage.rs's same-second branch). An empty set
        # has nothing to collide with.
        return name, None

    # Snapshot is captured BY THE OWNER: create_folder is owner-scoped (review
    # N1), so signing as admin would now be rejected (not_found).
    owner_client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=owner["actor_id_bytes"],
        signing_key=bytes(owner["signing_key"]),
    )
    with owner_client:
        reply = owner_client.call(
            "fauna.filesync.snapshot.create_folder",
            {"folder": name},
        )
    # `SnapshotCreateFolderReply.id` (filesync_handlers.rs § create_folder) —
    # the id of the snapshot just captured into this set.
    return name, reply["id"]


@pytest.fixture
def fresh_backup_set(nest_instance, test_user):
    """`(folder_name, snapshot_id)` — a folder holding exactly ONE snapshot,
    seeded fresh for THIS test.

    Function-scoped, and that is load-bearing rather than wasteful. Two reasons,
    the second measured the hard way on 2026-07-29:

    1. **The set must be selected explicitly**, because `test_user` is
       session-scoped and other modules seed folders for it too (e.g.
       `test_devices_conflicts` via the wizard). With >1 set the backups page's
       implicit selection (web's `folders.length === 1` auto-select / the linux
       dropdown's default-to-first-item) lands on the wrong set.
    2. **Selecting is necessary but NOT sufficient** — the outgoing set's rows
       stay mounted while the new set's fetch is in flight, so the test needs an
       identity barrier, `wait_for_snapshot_row(snapshot_id)` (testing.md
       convention 14). For that barrier to be *reachable*, the snapshot must be
       RENDERED: macOS lists snapshots in a virtualizing SwiftUI `List`, so only
       the rows that fit the pane enter the automation registry — measured at
       **2** rows at the e2e window size. A module-shared set accumulates a
       snapshot per test, pushing the seeded one out of the registry within two
       tests, and the barrier then times out on a snapshot that genuinely exists
       (`rows listed: [4, 3]` while waiting for `1`). A per-test set holds one
       snapshot, which is therefore both newest-first (index 0) and always
       rendered.

    (`fauna.folders.delete` can't be used for per-test cleanup instead: it
    returns `fauna.folders.internal` on a set that already has a snapshot —
    a non-cascading delete; tracked separately, out of scope for the e2e fix.)
    """
    return _seed_folder_with_snapshot(nest_instance, test_user)


@pytest.fixture
def empty_backup_set(nest_instance, test_user):
    """A folder with NO snapshot, seeded fresh for THIS test — the shape a
    test needs when it drives `snapshot-create-button` itself.

    Returns the set name. Pair it with `wait_for_no_snapshots()`, which is a
    genuine causal barrier here rather than a count-threshold: every other set
    this module can leave mounted holds at least one snapshot, so an observed
    count of ZERO can only be our set. See `fresh_backup_set` for the
    with-a-snapshot variant and why the create must not race a seeded snapshot.
    """
    name, _ = _seed_folder_with_snapshot(
        nest_instance, test_user, with_snapshot=False
    )
    return name


@pytest.fixture
def pruneable_backup_set(nest_instance, test_user):
    """`(folder_name, newest_snapshot_id)` — a set holding FOUR snapshots under
    a resting `max_snapshots: 1` retention policy, so a prune dry run names
    exactly ONE candidate and offers execute.

    Four, not two, and that is the nest's arithmetic rather than a margin: the
    § 8 hard floor is **3 active snapshots**, always enforced even when the
    policy would prune below it (`bins/fauna-nest/src/backup/retention.rs`
    `SNAPSHOT_HARD_FLOOR`). At or under the floor nothing is ever a candidate,
    so a 2-snapshot set under any policy previews "nothing to prune" and the
    test would assert against a vacuous surface. 4 − 3 = one candidate, and
    after the execute the set sits exactly at the floor — which is what makes
    the "no execute offered now" half of the test a real second state rather
    than a repeat of the first.

    The policy rides the set's own resting column via `fauna.folders.update`
    (the wizard's write path; the page itself never supplies a policy —
    § Architectural rules, rule 5). Fixture setup arranging a precondition, so
    the API shortcut is the sanctioned one under e2e convention 8; the
    behavior under test is driven entirely through the app UI.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    name, first_id = _seed_folder_with_snapshot(nest_instance, test_user)
    owner_client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=test_user["actor_id_bytes"],
        signing_key=bytes(test_user["signing_key"]),
    )
    newest_id = first_id
    with owner_client:
        while_ids = {first_id}
        # `create_snapshot_v2` dedups on `UNIQUE(folder_id, created_at)` at
        # SECONDS granularity and returns the EXISTING row, so a same-second
        # retry legitimately adds nothing. Poll for a DISTINCT id rather than
        # sleeping past the second boundary: the new id is the observable that
        # the row landed (convention 14), and the loop paces itself.
        while len(while_ids) < 4:
            deadline = time.monotonic() + 15
            while True:
                got = owner_client.call(
                    "fauna.filesync.snapshot.create_folder", {"folder": name}
                )["id"]
                if got not in while_ids:
                    while_ids.add(got)
                    newest_id = got
                    break
                if time.monotonic() >= deadline:
                    raise AssertionError(
                        f"seeding {name!r}: no new snapshot id within 15s "
                        f"(same-second dedup kept returning {got}); "
                        f"have {sorted(while_ids)}"
                    )
                time.sleep(0.5)
        owner_client.call(
            "fauna.folders.update",
            {"name": name, "retention_policy": '{"max_snapshots":1,"max_age_days":0}'},
        )
    return name, newest_id


def test_navigate_to_backups(logged_in_app):
    """Navigate to backups page and verify UI is visible."""
    logged_in_app.backups.navigate()
    assert logged_in_app.driver.is_visible("snapshot-list"), (
        "backups page should render the snapshot-list after navigate: "
        f"{logged_in_app.driver.diagnose('snapshot-list')} "
        f"error={logged_in_app.error_text()!r}"
    )


def test_backup_controls_visible(logged_in_app):
    """Verify backup action buttons are present."""
    logged_in_app.backups.navigate()
    assert logged_in_app.driver.is_visible("snapshot-create-button"), (
        "backups page should render the snapshot-create-button: "
        f"{logged_in_app.driver.diagnose('snapshot-create-button')} "
        f"error={logged_in_app.error_text()!r}"
    )
    assert logged_in_app.driver.is_visible("snapshot-prune-button"), (
        "backups page should render the snapshot-prune-button: "
        f"{logged_in_app.driver.diagnose('snapshot-prune-button')} "
        f"error={logged_in_app.error_text()!r}"
    )
    assert logged_in_app.driver.is_visible("snapshot-check-button"), (
        "backups page should render the snapshot-check-button: "
        f"{logged_in_app.driver.diagnose('snapshot-check-button')} "
        f"error={logged_in_app.error_text()!r}"
    )


def test_navigate_to_devices(logged_in_app):
    """Navigate to devices page and verify it loads."""
    logged_in_app.backups.navigate_devices()
    count = logged_in_app.backups.device_count()
    assert count >= 0


def test_folder_count_accessible(logged_in_app):
    """Verify folder count is accessible on the folder control plane."""
    # 2026-06-28 unification: the folder list lives on Settings → Folders (web)
    # / the combined Devices page (not-yet-migrated clients) — navigate_folders()
    # reaches the folder-row surface on either.
    logged_in_app.backups.navigate_folders()
    count = logged_in_app.backups.folder_count()
    assert count >= 0


@pytest.mark.feature("snapshots")
def test_snapshot_detail_files_visible(logged_in_app, empty_backup_set):
    """Snapshot detail screen exposes the file list with snapshot-detail-files testTag.

    Also the module's coverage of `snapshot-create-button`: the set starts EMPTY,
    so the create this test drives is the set's first — an unambiguous 0 -> 1,
    with no same-second dedup against a seeded snapshot (see `empty_backup_set`).
    """
    logged_in_app.backups.navigate()
    logged_in_app.backups.select_folder(empty_backup_set)
    # Causal barrier, not a count threshold: zero rows can only be OUR set, since
    # every set this module leaves mounted holds a snapshot (convention 14).
    logged_in_app.backups.wait_for_no_snapshots()
    logged_in_app.backups.create_snapshot()
    # Index 0 is the row just created (newest-first), so it is always rendered
    # even where the list virtualizes — see `fresh_backup_set`.
    logged_in_app.backups.open_snapshot(index=0)
    assert logged_in_app.backups.is_snapshot_detail_visible()


@pytest.mark.feature("snapshots")
def test_last_backed_up_is_the_selected_set_s_newest_snapshot(
    logged_in_app, empty_backup_set, fresh_backup_set
):
    """`last-backed-up` reports the SELECTED set, and says "never" when that set
    has no snapshots.

    The ratified derivation (`ui/backups.md` § Snapshot-list shape,
    *`last-backed-up`* ruling): ONE non-indexed element = the selected set's
    newest snapshot `created_at`, "never" when it has none. The 2026-08-05
    six-app survey found **six different derivations** behind this one element
    and ZERO e2e pressure on any of them — this is that pressure.

    Two halves, and the first is the one that catches the most:

    1. **An empty set reads "never".** This is where a client that derives the
       value from anything other than the selected set's own rows goes red:
       web's `last_change_at` source (the last *file change*, not the last
       snapshot — a live wrong-value bug), and any client that paints the label
       once and never clears it, which is what made linux carry the previous
       set's timestamp onto an empty one.
    2. **A set holding a snapshot does NOT read "never".** Without this half a
       client could pass by hard-coding "never", which is exactly the
       paints-a-default-and-remembers-nothing failure mode that let linux's
       Privacy page report a wrong inbox mode for months behind a green test.

    Both barriers are causal, not timed (convention 14): zero rows can only be
    the empty set (every other set this module leaves mounted holds a
    snapshot), and the seeded snapshot's own id is the identity barrier for the
    switch to the second set.
    """
    fresh_name, snapshot_id = fresh_backup_set
    logged_in_app.backups.navigate()

    logged_in_app.backups.select_folder(empty_backup_set)
    logged_in_app.backups.wait_for_no_snapshots()
    empty_text = logged_in_app.backups.wait_for_last_backed_up(
        lambda t: t == S.backups.last_backed_up_never
    )
    assert empty_text == S.backups.last_backed_up_never, (
        f"a set with no snapshots must read {S.backups.last_backed_up_never!r}; "
        f"{empty_text!r} means the value is derived from something other than "
        f"the selected set's own rows (or the label was never cleared). "
        f"error-message: {logged_in_app.error_text()!r} "
        f"{logged_in_app.driver.diagnose('last-backed-up')}"
    )

    logged_in_app.backups.select_folder(fresh_name)
    logged_in_app.backups.wait_for_snapshot_row(snapshot_id)
    populated_text = logged_in_app.backups.wait_for_last_backed_up(
        lambda t: bool(t) and t != S.backups.last_backed_up_never
    )
    assert populated_text and populated_text != S.backups.last_backed_up_never, (
        f"set {fresh_name!r} holds snapshot {snapshot_id} but `last-backed-up` "
        f"reads {populated_text!r} — a client that always says 'never' passes "
        f"the empty-set half without deriving anything. "
        f"error-message: {logged_in_app.error_text()!r} "
        f"{logged_in_app.driver.diagnose('last-backed-up')}"
    )




@pytest.mark.feature("snapshots")
def test_prune_previews_before_it_deletes(logged_in_app, pruneable_backup_set):
    """Prune is preview-first: the button previews, only *execute* deletes, and
    cancel leaves the candidate exactly where it was.

    The ratified ruling (`ui/backups.md` § Snapshot-list shape, *Prune*): the
    button applies the set's own RESTING policy as a dry run first; execute is
    offered only from a standing preview, and `prune_execute` is structurally a
    no-op without one. The 2026-08-05 survey found ZERO e2e pressure on any of
    this across six apps — this is that pressure, and it is the assertion that
    catches the shape that matters most: an app that wires its prune button
    straight to the destructive call.

    **Every assertion is the visibility of an id, never a count and never text.**
    `snapshot-prune-execute-button` renders only while a preview names
    candidates, so its presence *is* "a candidate still exists" — which makes
    the dry-run/execute distinction observable without reading a single number:

    1. preview stands, execute offered  → the policy found its candidate;
    2. cancel, then preview again, execute STILL offered → the dry run and the
       cancel each deleted nothing (had either pruned, the population would be
       at the floor and there would be no candidate left to offer);
    3. execute, then preview again, execute NOT offered → that click, and only
       that click, applied the policy.

    A row count could not do this job: the nest's `list` deliberately keeps
    soft-deleted rows (the 30-day undelete window), so a pruned set has the same
    number of rows it started with.

    Barriers are causal throughout (convention 14): each is the surface the
    gesture itself produces or clears, never a settle-sleep.
    """
    folder, newest_id = pruneable_backup_set

    logged_in_app.backups.navigate()
    logged_in_app.backups.select_folder(folder)
    # Identity barrier, not a count: the outgoing set's rows stay mounted while
    # this set's fetch is in flight (see `fresh_backup_set`).
    logged_in_app.backups.wait_for_snapshot_row(newest_id)

    assert logged_in_app.backups.prune_until_preview(), (
        "clicking snapshot-prune-button must raise a standing prune preview "
        "(the dry run), never delete directly. "
        f"error-message: {logged_in_app.error_text()!r} "
        f"{logged_in_app.driver.diagnose('snapshot-prune-preview')}"
    )
    assert logged_in_app.backups.has_prune_execute(), (
        f"set {folder!r} holds 4 snapshots under max_snapshots=1, so the "
        "preview must name a candidate (one, floor-clamped) and offer execute. "
        f"preview: {logged_in_app.backups.prune_preview_text()!r} "
        f"error-message: {logged_in_app.error_text()!r} "
        f"{logged_in_app.driver.diagnose('snapshot-prune-execute-button')}"
    )

    logged_in_app.backups.cancel_prune()
    assert logged_in_app.backups.wait_for_no_prune_preview(), (
        "cancel must discard the standing preview. "
        f"{logged_in_app.driver.diagnose('snapshot-prune-preview')}"
    )

    assert logged_in_app.backups.prune_until_preview(), (
        "the page must still be usable after a cancel — cancel discards a dry "
        "run, it does not wedge the machine. "
        f"error-message: {logged_in_app.error_text()!r} "
        f"{logged_in_app.driver.diagnose('snapshot-prune-preview')}"
    )
    assert logged_in_app.backups.has_prune_execute(), (
        "THE load-bearing assertion: after a preview and a cancel, the "
        "candidate must still be there. An execute button that has vanished "
        "means the earlier preview (or the cancel) actually pruned — the exact "
        "bug the preview-first ruling exists to prevent. "
        f"preview: {logged_in_app.backups.prune_preview_text()!r} "
        f"error-message: {logged_in_app.error_text()!r} "
        f"{logged_in_app.driver.diagnose('snapshot-prune-execute-button')}"
    )

    logged_in_app.backups.execute_prune()
    assert logged_in_app.backups.wait_for_no_prune_preview(), (
        "executing consumes the preview it applied. "
        f"error-message: {logged_in_app.error_text()!r} "
        f"{logged_in_app.driver.diagnose('snapshot-prune-preview')}"
    )

    assert logged_in_app.backups.prune_until_preview(), (
        "a fresh dry run must still be available after an execute. "
        f"error-message: {logged_in_app.error_text()!r} "
        f"{logged_in_app.driver.diagnose('snapshot-prune-preview')}"
    )
    assert not logged_in_app.backups.has_prune_execute(), (
        "the execute applied the policy, so the set now sits at the hard floor "
        "(3 active snapshots) with nothing left to prune — the preview must "
        "say so and offer no execute. Still offering one means the execute "
        "never reached the nest, which a preview-only test would have missed. "
        f"preview: {logged_in_app.backups.prune_preview_text()!r} "
        f"error-message: {logged_in_app.error_text()!r} "
        f"{logged_in_app.driver.diagnose('snapshot-prune-execute-button')}"
    )


# `snapshot-undelete-button` was approved 2026-08-14 and tui LED the render
# (lead-app rule). The gesture, both boundary faces and the machine's refusal
# rule are shared, so each remaining app owes a render only. linux/web/android landed the same day;
# apple (macOS+iOS) landed 2026-08-23; windows landed
# last, closing this affordance on ALL SEVEN apps.
_UNDELETE_APPS = ("tui", "linux", "web", "android", "macos", "ios", "windows")


def _require_undelete_affordance(driver):
    """Gate the recovery test on an app that has built the affordance.

    Temporary parity debt, so it declares `skip_unbuilt`: it fails under
    `--strict-app` and is tallied every run, which is what stops "six apps
    still owe this" from reading as coverage.
    """
    if app_name(driver) not in _UNDELETE_APPS:
        skip_unbuilt(
            driver,
            surface="snapshot-undelete-button",
            detail=(
                "the undelete gesture, both boundary faces and the machine's "
                "SoftDeleted-only refusal are shared; each app owes the render "
                "only (id approved 2026-08-14, tui led)"
            ),
            tracked="",
        )


@pytest.mark.feature("snapshots")
def test_a_soft_deleted_snapshot_can_be_recovered_from_the_app(
    logged_in_app, pruneable_backup_set
):
    """A pruned snapshot renders a recovery affordance, and using it returns the
    row to `Active` — `fauna.filesync.snapshot.undelete` is reachable from the
    app.

    The ratified ruling (`ui/backups.md` § Snapshot-list shape, *Soft-deleted
    rows*): recovery is offered ONLY on a `SoftDeleted` row, before its
    `purge_after`. Until the id was approved (2026-08-14) the verb was a
    declared **dark verb** — built, shipped, and reachable from no app — so
    this test is the pressure that keeps it reachable.

    **Every assertion is the visibility of an id, never a count of rows and
    never text.** `snapshot-undelete-button` renders only on a soft-deleted row,
    so its presence *is* that row state:

    1. before the prune, no row offers recovery → nothing is soft-deleted;
    2. after the execute, exactly one does  → the prune soft-deleted its
       candidate rather than hard-deleting it (the 30-day window exists);
    3. after the recovery click, none does  → the row came back `Active`.

    A `snapshot-item` count is blind to all three: the nest's `list` keeps
    soft-deleted rows for the whole window, so the row count never moves.

    Barriers are causal throughout (convention 14) — each is the surface the
    gesture itself produces or clears.
    """
    _require_undelete_affordance(logged_in_app.driver)
    folder, newest_id = pruneable_backup_set

    logged_in_app.backups.navigate()
    logged_in_app.backups.select_folder(folder)
    logged_in_app.backups.wait_for_snapshot_row(newest_id)

    assert logged_in_app.backups.recoverable_row_count() == 0, (
        "precondition: a freshly seeded set holds only Active snapshots, so no "
        "row may offer recovery. A control painted here would mean the "
        "affordance is unconditional — it must render only on a SoftDeleted "
        "row, which is what makes its presence the state observable. "
        f"{logged_in_app.driver.diagnose('snapshot-undelete-button')}"
    )

    # Drive the set into the soft-deleted state through the UI: the prune is
    # preview-first, and executing it soft-deletes the one floor-clamped
    # candidate (`test_prune_previews_before_it_deletes` pins that half).
    assert logged_in_app.backups.prune_until_preview(), (
        "the prune dry run must stand before it can be executed. "
        f"error-message: {logged_in_app.error_text()!r} "
        f"{logged_in_app.driver.diagnose('snapshot-prune-preview')}"
    )
    assert logged_in_app.backups.has_prune_execute(), (
        f"set {folder!r} holds 4 snapshots under max_snapshots=1, so the "
        "preview must name exactly one candidate and offer execute. "
        f"preview: {logged_in_app.backups.prune_preview_text()!r} "
        f"error-message: {logged_in_app.error_text()!r}"
    )
    logged_in_app.backups.execute_prune()

    assert logged_in_app.backups.wait_for_recoverable_rows(1), (
        "executing the prune must SOFT-delete its candidate, so exactly one "
        "row now offers recovery. Zero means either the row was hard-deleted "
        "(the 30-day recovery window the ruling depends on does not exist) or "
        "the app does not paint the affordance on a soft-deleted row. "
        f"rows offering recovery: {logged_in_app.backups.recoverable_row_count()} "
        f"error-message: {logged_in_app.error_text()!r} "
        f"{logged_in_app.driver.diagnose('snapshot-undelete-button')}"
    )

    logged_in_app.backups.undelete_snapshot()

    assert logged_in_app.backups.wait_for_recoverable_rows(0), (
        "THE load-bearing assertion: recovering the row must return it to "
        "Active, so the affordance disappears. A control still standing means "
        "the click never reached fauna.filesync.snapshot.undelete — the verb "
        "is dark again, which is exactly what this id was approved to end. "
        f"error-message: {logged_in_app.error_text()!r} "
        f"{logged_in_app.driver.diagnose('snapshot-undelete-button')}"
    )
    assert not logged_in_app.error_text(), (
        "a successful recovery raises no error banner: "
        f"error-message reads {logged_in_app.error_text()!r}"
    )


@pytest.fixture
def slack_policy_backup_set(nest_instance, test_user):
    """`folder_name` — ONE snapshot under a policy so slack nothing is a prune
    candidate, the `policy_state: applied` + zero-candidates shape.

    The sibling of `pruneable_backup_set`, and the cheap half of the
    nothing-to-prune verdict: a set whose policy is *satisfied* previews "nothing
    to prune", which is a different sentence from a set with no policy at all —
    and telling those two apart is the whole of `ui/backups.md` § Errors & edge
    cases → *Prune with no candidates*. `max_snapshots: 10` over one snapshot
    needs no floor arithmetic at all, so it says nothing about the § 8 hard floor
    and cannot be confused with it.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    name, _ = _seed_folder_with_snapshot(nest_instance, test_user)
    owner_client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=test_user["actor_id_bytes"],
        signing_key=bytes(test_user["signing_key"]),
    )
    with owner_client:
        owner_client.call(
            "fauna.folders.update",
            {"name": name, "retention_policy": '{"max_snapshots":10,"max_age_days":0}'},
        )
    return name


@pytest.fixture
def lifecycle_backup_set(nest_instance, test_user):
    """`(folder_name, newest_snapshot_id)` — FIVE snapshots under
    `max_snapshots: 1`, the population both lifecycle states fit in at once.

    Five, and the arithmetic is the § 7 hard floor's (3 active), not a margin.
    The journey needs one row in EACH non-`Active` state simultaneously, and each
    costs an active snapshot: the queued delete takes the population 5 -> 4
    active (`check_snapshot_delete_allowed` refuses at or under the floor, so 4
    would have left nothing to prune afterwards), then the prune's one
    floor-clamped candidate takes it 4 -> 3 and stops. `pruneable_backup_set`'s
    four cannot do it — after its prune the set sits exactly at the floor and the
    delete is refused, whichever order the two are run in.

    Same same-second dedup pacing as `pruneable_backup_set`: poll for a DISTINCT
    id rather than sleeping past the second boundary (convention 14).
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    name, first_id = _seed_folder_with_snapshot(nest_instance, test_user)
    owner_client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=test_user["actor_id_bytes"],
        signing_key=bytes(test_user["signing_key"]),
    )
    newest_id = first_id
    with owner_client:
        seen = {first_id}
        while len(seen) < 5:
            deadline = time.monotonic() + 15
            while True:
                got = owner_client.call(
                    "fauna.filesync.snapshot.create_folder", {"folder": name}
                )["id"]
                if got not in seen:
                    seen.add(got)
                    newest_id = got
                    break
                if time.monotonic() >= deadline:
                    raise AssertionError(
                        f"seeding {name!r}: no new snapshot id within 15s "
                        f"(same-second dedup kept returning {got}); "
                        f"have {sorted(seen)}"
                    )
                time.sleep(0.5)
        owner_client.call(
            "fauna.folders.update",
            {"name": name, "retention_policy": '{"max_snapshots":1,"max_age_days":0}'},
        )
    return name, newest_id


def _snapshot_rows_from_nest(nest_instance, owner, folder: str) -> list[dict]:
    """The nest's own `fauna.filesync.snapshot.list` rows for `folder`, in wire
    order (newest-first) — the source the page renders from.

    The split verdict for every row-content assertion (e2e convention 5): the
    numbers the row must show come from here, so a red says *the row is wrong*
    rather than leaving "the row is wrong" and "the nest served something else"
    indistinguishable.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=owner["actor_id_bytes"],
        signing_key=bytes(owner["signing_key"]),
    )
    with client:
        return client.call("fauna.filesync.snapshot.list", {"folder": folder})["rows"]


def _expected_size_text(total_bytes: int) -> str:
    """The shared byte scale's rendering of `total_bytes` — sub-KiB branch only.

    Deliberately NOT a second implementation of `fauna_core::format::byte_size`:
    a test that re-derived the scale would pass a client that re-derived it
    wrongly in the same way. Only the branch the seeded (fileless) sets land in
    is spelled out, and anything past it raises here rather than silently
    weakening the assertion into a substring nobody chose.
    """
    assert 0 <= total_bytes < 1024, (
        f"snapshot holds {total_bytes} bytes, past this helper's sub-KiB branch — "
        f"assert the chosen unit against fauna_core::format::byte_size instead of "
        f"re-deriving the scale here"
    )
    return S.size.bytes(value=str(total_bytes))


def _state_prefix(rendered: str, placeholder: str) -> str:
    """The fixed part of a `{when}`-templated lifecycle string.

    The deadline is formatted by the app (locale-aware relative time is app
    glue — `behavior/value-formatting.md` § Relative time), so a test can pin the
    i18n sentence but never the date inside it. Splitting the rendered template
    on a placeholder gives the half that IS the contract, and the tail after it
    is what must be non-empty for the row to carry a deadline at all.
    """
    return rendered.split(placeholder)[0]


_WHEN_PLACEHOLDER = "<<when>>"
_SOFT_DELETED_PREFIX = _state_prefix(
    S.backups.snapshot_state_soft_deleted(when=_WHEN_PLACEHOLDER), _WHEN_PLACEHOLDER
)
_DELETION_PENDING_PREFIX = _state_prefix(
    S.backups.snapshot_state_deletion_pending(when=_WHEN_PLACEHOLDER), _WHEN_PLACEHOLDER
)


def _row_carrying(app, fragment: str, budget_s: float = 45.0) -> str:
    """Deadline-poll the rendered `snapshot-item` texts until one contains
    `fragment`, and return that row's text ("" at the deadline).

    Scans the rendered rows rather than addressing one by index: which row a
    lifecycle transition lands on is the nest's retention arithmetic, not the
    test's, and pinning an index would assert that arithmetic by accident. A
    positive wait with a named ceiling (convention 14) — the nest's `list` keeps
    a deleted row for its whole recovery window, so neither the row count nor the
    row order moves and only the text can be the barrier.
    """
    deadline = time.monotonic() + budget_s
    while True:
        for text in app.driver.get_texts("snapshot-item"):
            if fragment in text:
                return text
        if time.monotonic() >= deadline:
            return ""
        time.sleep(0.5)


@pytest.mark.feature("snapshots")
def test_every_snapshot_row_reads_as_a_snapshot_newest_first(
    logged_in_app, nest_instance, test_user, pruneable_backup_set
):
    """A `snapshot-item` says when it was taken, how many files and how big, in
    the wire's newest-first order — never a raw record dump.

    The ratified § *Row content contract* (`ui/backups.md` § Snapshot-list shape):
    *"A `snapshot-item` visibly renders at least: formatted `created_at`,
    `file_count`, formatted `total_bytes` — never a raw record dump … Order is
    the wire's newest-first; apps do not re-sort."* Every list journey in this
    module asserts ids and counts and no journey has ever read what a row SAYS,
    which is how windows shipped the C# record `ToString()` — raw epoch, raw
    bytes, no i18n — for three months behind green tests; only its own C# unit
    tests caught it, and only after the machine adoption went looking.

    Three assertions, and the third is the one a count can never make:

    1. **Order** — the rendered ids are a PREFIX of the nest's own newest-first
       order. A prefix rather than the whole list because a virtualizing shell
       registers only the rows it realized, and "did not re-sort" is provable
       from whatever it did realize.
    2. **Content** — the row carries the file count and the size the nest
       reports for it, through the shared i18n strings (`backups.file_count`,
       `size.*`), never a number the test spelled by hand.
    3. **Not a dump** — the raw `created_at` epoch does not appear in the row.
       A formatted timestamp cannot contain it, and a record dump always does,
       so this is the windows bug's exact signature.
    """
    folder, newest_id = pruneable_backup_set

    logged_in_app.backups.navigate()
    logged_in_app.backups.select_folder(folder)
    logged_in_app.backups.wait_for_snapshot_row(newest_id)

    nest_rows = _snapshot_rows_from_nest(nest_instance, test_user, folder)
    assert len(nest_rows) >= 2, (
        f"the fixture seeds four snapshots into {folder!r}; the nest lists "
        f"{len(nest_rows)} — newest-first is not a claim about one row"
    )

    rendered = [
        logged_in_app.backups.snapshot_row_id(i)
        for i in range(logged_in_app.driver.count("snapshot-item"))
    ]
    assert len(rendered) >= 2, (
        f"only {len(rendered)} snapshot row(s) rendered for {folder!r}; ordering "
        f"cannot be asserted from one row. "
        f"{logged_in_app.driver.diagnose('snapshot-item')}"
    )
    nest_ids = [r["id"] for r in nest_rows]
    assert rendered == nest_ids[: len(rendered)], (
        f"the page renders snapshot ids {rendered} where the nest serves "
        f"{nest_ids} — the wire order is newest-first and apps do not re-sort "
        f"(§ Snapshot-list shape, *Row content contract*). A client that sorted "
        f"by its own key shows the owner the wrong snapshot at index 0, which "
        f"every indexed per-row control on this page then acts on."
    )

    top = nest_rows[0]
    row_text = logged_in_app.backups.snapshot_row_text(0)
    expected_files = S.backups.file_count(count=str(top["file_count"]))
    assert expected_files in row_text, (
        f"row 0 reads {row_text!r} and does not carry {expected_files!r}, the "
        f"file count the nest reports for snapshot {top['id']}. "
        f"error-message: {logged_in_app.error_text()!r}"
    )
    expected_size = _expected_size_text(top["total_bytes"])
    assert expected_size in row_text, (
        f"row 0 reads {row_text!r} and does not carry {expected_size!r}, the "
        f"formatted size of the {top['total_bytes']} bytes the nest reports."
    )
    assert str(top["created_at"]) not in row_text, (
        f"row 0 reads {row_text!r}, which contains the RAW created_at epoch "
        f"{top['created_at']} — a formatted timestamp cannot, and a raw record "
        f"dump always does. This is the exact shape the row-content contract "
        f"was written against."
    )


@pytest.mark.feature("snapshots")
def test_a_snapshot_on_its_way_out_says_so_on_its_own_row(
    logged_in_app, nest_instance, test_user, lifecycle_backup_set
):
    """A non-`Active` row carries its state AND the deadline the owner can still
    act on — deletion-pending with its `execute_after`, soft-deleted with its
    recoverable-until `purge_after`.

    `ui/backups.md` § Snapshot-list shape, *Row content contract*: *"A
    non-`Active` state renders on the row (i18n: deletion-pending with its
    `execute_after`; soft-deleted with its recoverable-until `purge_after`)"* —
    and § *Where logic lives* records why the deadline is the point: *"that
    deadline is the whole reason the wire gained the lifecycle fields"*. The
    recover journey asserts the undelete CONTROL's visibility and deliberately
    nothing about text, so a row that said only "Deleted", with no date, has
    always passed: the owner would be told their snapshot is going and not how
    long they have to stop it.

    **The two states are reached differently, and that asymmetry is a finding,
    not a shortcut.** The soft delete is driven end to end through the app (prune
    -> execute, the path `test_prune_previews_before_it_deletes` pins). The
    queued 48 h delete is NOT reachable from any automation layer on any app:
    `snapshot-delete-button` opens a per-app confirm that `ui.yaml` scopes no id
    (linux's `present_confirm`, and the same on the other shells), so the gesture
    dead-ends at an untagged dialog. Its row state is therefore a precondition
    seeded over the owner's own `fauna.filesync.snapshot.delete` — the same
    convention-8 fixture exemption the divergence journey uses — and what this
    test asserts is the RENDERING, which is what the outcome is about.
    """
    folder, newest_id = lifecycle_backup_set

    logged_in_app.backups.navigate()
    logged_in_app.backups.select_folder(folder)
    logged_in_app.backups.wait_for_snapshot_row(newest_id)

    # Precondition: five Active rows say nothing about going anywhere. Without
    # it a page that painted a lifecycle suffix unconditionally would satisfy
    # both halves below.
    for text in logged_in_app.driver.get_texts("snapshot-item"):
        assert _SOFT_DELETED_PREFIX not in text and _DELETION_PENDING_PREFIX not in text, (
            f"a freshly seeded set holds only Active snapshots, but a row already "
            f"reads {text!r} — the suffix is unconditional, so its presence says "
            f"nothing about the row's state."
        )

    # -- Deletion pending: "when it will go" --------------------------------
    nest_rows = _snapshot_rows_from_nest(nest_instance, test_user, folder)
    oldest_id = nest_rows[-1]["id"]
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    owner = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=test_user["actor_id_bytes"],
        signing_key=bytes(test_user["signing_key"]),
    )
    with owner:
        queued = owner.call(
            "fauna.filesync.snapshot.delete", {"snapshot_id": oldest_id}
        )
    assert queued["status"] == "pending", (
        f"the 48 h delete did not queue for snapshot {oldest_id}: {queued!r}"
    )
    assert queued["execute_after"], (
        f"a queued delete with no execute_after has no deadline to render: {queued!r}"
    )

    # Re-mount so the page re-reads the list: the machine re-reads on load and
    # after its own gestures, and this state change was not one of its gestures.
    logged_in_app.driver.navigate_to("feed")
    logged_in_app.backups.navigate()
    logged_in_app.backups.select_folder(folder)
    logged_in_app.backups.wait_for_snapshot_row(newest_id)

    pending_text = _row_carrying(logged_in_app, _DELETION_PENDING_PREFIX)
    assert pending_text, (
        f"no row reads {_DELETION_PENDING_PREFIX!r} after snapshot {oldest_id} "
        f"was queued for deletion. The rows read "
        f"{logged_in_app.driver.get_texts('snapshot-item')!r}; a set whose "
        f"snapshot is going in 48 h and does not say so leaves the owner no "
        f"reason to look for the cancel. "
        f"error-message: {logged_in_app.error_text()!r}"
    )
    assert pending_text.split(_DELETION_PENDING_PREFIX, 1)[1].strip(), (
        f"the row reads {pending_text!r} — the state renders but the deadline "
        f"after it is empty. The undated fallback is for a wire that served no "
        f"deadline; this one did ({queued['execute_after']}), so an empty tail "
        f"means the row dropped it (§ *Row content contract*)."
    )

    # -- Soft deleted: "how long you can still get it back" ------------------
    assert logged_in_app.backups.prune_until_preview(), (
        "the prune dry run must stand before it can be executed. "
        f"error-message: {logged_in_app.error_text()!r} "
        f"{logged_in_app.driver.diagnose('snapshot-prune-preview')}"
    )
    assert logged_in_app.backups.has_prune_execute(), (
        f"set {folder!r} holds 4 active snapshots under max_snapshots=1, so the "
        "preview must name one floor-clamped candidate and offer execute. "
        f"preview: {logged_in_app.backups.prune_preview_text()!r} "
        f"error-message: {logged_in_app.error_text()!r}"
    )
    logged_in_app.backups.execute_prune()
    assert logged_in_app.backups.wait_for_recoverable_rows(1), (
        "executing the prune must SOFT-delete its candidate. "
        f"rows offering recovery: {logged_in_app.backups.recoverable_row_count()} "
        f"error-message: {logged_in_app.error_text()!r}"
    )

    soft_text = _row_carrying(logged_in_app, _SOFT_DELETED_PREFIX)
    assert soft_text, (
        f"no row reads {_SOFT_DELETED_PREFIX!r} after the prune soft-deleted its "
        f"candidate. The rows read "
        f"{logged_in_app.driver.get_texts('snapshot-item')!r}; the undelete "
        f"control is painted on that row, so the state is known — it is the "
        f"sentence that is missing. "
        f"error-message: {logged_in_app.error_text()!r}"
    )
    assert soft_text.split(_SOFT_DELETED_PREFIX, 1)[1].strip(), (
        f"the row reads {soft_text!r} — it says the snapshot is deleted and not "
        f"how long it stays recoverable. The 30-day window is the whole promise "
        f"the undelete affordance rests on; a row that names no date makes it "
        f"unactionable."
    )


# The prune VERDICT rides `snapshot-prune-preview`'s own text as "<title>
# <verdict>" on tui, linux, android, macos and ios (each declares it on the id'd
# element; apple's one FaunaKit `PrunePreviewView` serves both targets), and
# web's container's text contains its verdict paragraph. windows (a constant UIA
# name) still reads nothing that says which of the two no-op states the nest
# returned.
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("snapshots")
def test_a_prune_with_nothing_to_remove_says_which_kind_of_nothing(
    logged_in_app, fresh_backup_set, slack_policy_backup_set
):
    """"Nothing to prune" and "no retention policy configured for this set" are
    DIFFERENT answers, and the preview says which one it gave.

    `ui/backups.md` § Errors & edge cases, *Prune with no candidates*: the
    preview renders "nothing to prune" (zero candidates, `remaining` = the active
    count) and offers no execute, *"distinct from `policy_state: not_set` … All
    three states come typed off the set-policy kind's reply; no app infers
    them."* The existing prune journey reaches the first state and reads it only
    through `snapshot-prune-execute-button`'s ABSENCE — which both states share,
    so nothing has ever asserted that the page distinguishes them. An owner who
    never set a policy and one whose policy is satisfied are in completely
    different positions: the first has a thing to go and do in the folder wizard.

    Both halves click the same button on two sets and assert the two sentences,
    so a page that hard-coded either one fails on the other.
    """
    app = logged_in_app
    app.backups.navigate()

    # A set with NO policy: the preview explains that, and names the wizard.
    app.backups.select_folder(fresh_backup_set[0])
    app.backups.wait_for_snapshot_row(fresh_backup_set[1])
    assert app.backups.prune_until_preview(), (
        "clicking snapshot-prune-button must raise a standing preview even with "
        "no policy to apply — a set with no retention configured is a state to "
        "report, not an error. "
        f"error-message: {app.error_text()!r} "
        f"{app.driver.diagnose('snapshot-prune-preview')}"
    )
    no_policy = app.backups.wait_for_prune_preview_text(
        lambda t: S.backups.prune_policy_not_set in t
    )
    assert no_policy, (
        f"the preview on an unpolicied set reads "
        f"{app.backups.prune_preview_text()!r} and not "
        f"{S.backups.prune_policy_not_set!r}. `policy_state: not_set` comes typed "
        f"off the reply, so this is a rendering gap, not an inference the page "
        f"got wrong. error-message: {app.error_text()!r}"
    )
    assert S.backups.prune_preview_nothing not in no_policy, (
        f"the preview reads {no_policy!r} — it says BOTH things. The owner of a "
        f"set with no policy has an action to take (set one); telling them every "
        f"snapshot is within a policy they never configured is the opposite."
    )
    assert not app.backups.has_prune_execute(), (
        "no policy means no candidate and no execute. "
        f"{app.driver.diagnose('snapshot-prune-execute-button')}"
    )
    app.backups.cancel_prune()
    assert app.backups.wait_for_no_prune_preview()

    # A set WITH a policy nothing falls foul of: the other sentence.
    app.backups.select_folder(slack_policy_backup_set)
    assert app.backups.prune_until_preview(), (
        "the same button on a policied set must raise a preview too. "
        f"error-message: {app.error_text()!r} "
        f"{app.driver.diagnose('snapshot-prune-preview')}"
    )
    satisfied = app.backups.wait_for_prune_preview_text(
        lambda t: S.backups.prune_preview_nothing in t
    )
    assert satisfied, (
        f"the preview on a set whose max_snapshots=10 policy is satisfied reads "
        f"{app.backups.prune_preview_text()!r} and not "
        f"{S.backups.prune_preview_nothing!r}. error-message: {app.error_text()!r}"
    )
    assert S.backups.prune_policy_not_set not in satisfied, (
        f"the preview reads {satisfied!r} — a set that HAS a policy is being told "
        f"it has none, which sends the owner to the folder wizard to set one that "
        f"is already there."
    )
    assert not app.backups.has_prune_execute(), (
        "a satisfied policy names no candidate, so no execute is offered. "
        f"{app.driver.diagnose('snapshot-prune-execute-button')}"
    )


@pytest.mark.feature("snapshots")
def test_completed_check_is_a_result_not_an_error(logged_in_app, fresh_backup_set):
    """A completed integrity check renders its verdict in `snapshot-check-result`
    and leaves `error-message` clear.

    `ui/backups.md` § Architectural rules, rule 6 + § Errors: *"a completed
    check reporting errors is a result, not an error"* — only a check that could
    not run (transport failure, `backup_unavailable`) is an error. Two apps
    routed check successes through `error-message` before adopting the machine;
    nothing asserted otherwise, because the verdict surface carried no id.

    Also the module's only pressure on the *actuation* contract: one click on
    the tagged button RUNS the check (§ *Check* — a tagged button that merely
    opens a sheet does not count), so the verdict appearing is itself the proof.
    """
    folder, snapshot_id = fresh_backup_set

    logged_in_app.backups.navigate()
    logged_in_app.backups.select_folder(folder)
    logged_in_app.backups.wait_for_snapshot_row(snapshot_id)

    logged_in_app.backups.check_integrity()
    verdict = logged_in_app.backups.wait_for_check_result()
    assert verdict, (
        "clicking snapshot-check-button must run the check and render its "
        "verdict in snapshot-check-result. An empty read means either the "
        "button only opened a sheet (the actuation contract) or the result was "
        "never surfaced. "
        f"error-message: {logged_in_app.error_text()!r} "
        f"{logged_in_app.driver.diagnose('snapshot-check-result')}"
    )
    assert not logged_in_app.error_text(), (
        "a COMPLETED check is a result, never an error (rule 6): "
        f"error-message reads {logged_in_app.error_text()!r} while the verdict "
        f"reads {verdict!r}. Routing the verdict through error-message is the "
        "pre-adoption bug this pins."
    )


@pytest.mark.feature("snapshots")
def test_backup_folder_selector_visible(logged_in_app):
    """Backups page exposes a folder selector after the dropdown loads."""
    logged_in_app.backups.navigate()
    assert logged_in_app.backups.is_folder_selector_visible()


@pytest.mark.feature("snapshots")
def test_snapshot_delete_button_indexed(logged_in_app, fresh_backup_set):
    """Each snapshot row exposes a delete button (indexed)."""
    folder, snapshot_id = fresh_backup_set
    logged_in_app.backups.navigate()
    logged_in_app.backups.select_folder(folder)
    # Identity barrier, not a count — see `wait_for_snapshot_row`. A bare count
    # is satisfied by the previously selected set's still-mounted rows, and the
    # delete buttons counted below would then be another set's.
    logged_in_app.backups.wait_for_snapshot_row(snapshot_id)
    assert logged_in_app.driver.count("snapshot-delete-button") >= 1


@pytest.mark.feature("backup-destinations-and-restore")
def test_backup_destination_crud(logged_in_app, second_nest, test_user):
    """Add, edit, and remove a backup destination through the Backups page.

    The destination-management surface is being lifted across the 6 clients
    (docs/goal/ui/backups.md § Implementation status today). Landed on linux
    (slice 2), web (slice 3), and windows (native FFI consume); the remaining
    clients still skip until they implement the surface. Adding `second_nest`'s
    URL exercises the full enroll
    path: resolve the destination identity by connecting as the owner's stable
    cross-nest actor (the `fauna.auth.handshake` is the reachability +
    authorization proof — `segment_backup::resolve_destination` natively, the
    wasm resolve on web), reading `fauna.nest.info`, and the row is persisted to
    the `fauna.state.backup` plane. Edit renames it; remove drops it.

    A v1 destination is a nest the owner *administers*, so the handshake only
    succeeds when the owner's identity is registered there (`require_registration`
    is the nest default — a stranger is correctly rejected). Register `test_user`
    on `second_nest` first, the proven shape from `test_link_both_seeds_both_nests`.
    """
    logged_in_app.backups.require_destination_management_supported()

    # Authorize the owner on the destination nest (WS-RPC `fauna.admin.users.create`
    # with its admin key). Idempotent across a re-run of the session-scoped nest.
    try:
        register_user(
            second_nest["port"],
            test_user["actor_id_hex"],
            admin_signing_key=second_nest["admin"]["signing_key"],
        )
    except Exception:
        # Already registered (the session nest persists across this test's reruns).
        pass

    app = logged_in_app
    app.backups.navigate()

    # Add-button is always present; no destinations configured yet.
    assert app.driver.is_visible("backup-destination-add-button"), (
        "backups page should render the backup-destination-add-button: "
        f"{app.driver.diagnose('backup-destination-add-button')} "
        f"error={app.error_text()!r}"
    )
    assert app.backups.destination_count() == 0

    # Add: resolve + record a destination pointing at the second (owned) nest.
    app.backups.add_destination(second_nest["url"], name="Offsite")
    app.backups.wait_for_destination_count(1)
    assert "Offsite" in app.backups.destination_text(0)
    # The status sub-elements render inside the row (scoped query resolves them).
    assert app.driver.is_visible(
        "backup-destination-last-upload-time",
        scope="backup-destination-status-row[0]",
    )
    assert app.driver.is_visible(
        "backup-destination-backlog-count",
        scope="backup-destination-status-row[0]",
    )

    # Edit: rename the destination.
    app.backups.edit_destination(0, name="Offsite-renamed")
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if "Offsite-renamed" in app.backups.destination_text(0):
            break
        time.sleep(0.5)
    assert "Offsite-renamed" in app.backups.destination_text(0)

    # Remove: drop it (a plain config edit; coordinator reconciles offsite).
    # NOTE for anyone tracing state across this module: this does NOT return the
    # shared session owner to "never enrolled". Enrolling granted this owner's
    # `NestBackupKey` to the nest, and `deregister_backup_destination` pointedly
    # leaves that grant standing (`backup_enroll.rs` § deregister — freezing the
    # seal is a separate trust-facet action a user chooses). So `fauna.backup.
    # status` keeps reporting `enrolled: true` for `test_user` for the rest of the
    # run, and the trust facet keeps rendering its seal row. That is correct
    # product behaviour, not a leak to plug here — tests that need an owner who
    # has granted nothing take the `ungranted_app` fixture instead.
    app.backups.remove_destination(0)
    app.backups.wait_for_destination_count(0)


@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.tui
@pytest.mark.web
@pytest.mark.feature("backup-destinations-and-restore")
def test_client_device_custodian_destination(logged_in_app, second_nest, test_user, request):
    """Enroll this device as a client-device custodian destination, and verify
    the third-kind UI (`ui/backups.md` § Third destination kind — client device
    as custodian): the kind select swaps the URL input for a capacity input,
    the row carries a kind badge + usage text, and the sole-client warning
    tracks whether every configured destination is a client device.

    Scoped to macOS + iOS + windows and tui (joined 2026-09-19, its
    same UI in `apps/fauna-tui/src/backups.rs`).
    linux and android have also landed this UI; widen the marks once their
    exact behavior against this same test is confirmed. web joined in its
    catalog trickle-down pass: it DECLARES the kind select and capacity input
    absent (`ui/backups.md` § Implementation status today — enrolment runs on
    the device being enrolled) but owes the badge, the usage line and the
    warning, so its enrolment step is another device's act
    (`BackupsActions.add_custodian_destination`'s web branch) and everything
    asserted after it is web's own render.
    """
    logged_in_app.backups.require_destination_management_supported()
    app = logged_in_app
    app.backups.navigate()
    assert app.backups.destination_count() == 0
    # Registered BEFORE the enroll: `test_user` is session-scoped, so a row this
    # test leaves behind on any failure fails every later test's zero-count
    # precondition.
    request.addfinalizer(lambda: _leave_no_custodian_behind(app))

    # Enroll: no URL at all — the custodian kind has no address.
    app.backups.add_custodian_destination(name="This Mac", capacity="50 GB")
    app.backups.wait_for_destination_count(1)
    assert "This Mac" in app.backups.destination_text(0)

    # Every configured destination is a client device -> the standing warning
    # renders (an owner with only this device backing them up has no
    # off-device copy).
    assert app.backups.sole_client_destination_warning_visible(), (
        "the sole-client-destination warning should render when every "
        f"destination is a client device: error={app.error_text()!r}"
    )

    # The row carries a kind badge (every row does, regardless of kind) and a
    # usage text (client-device rows only) — both over the shared FFI faces,
    # never a re-derived label.
    assert app.driver.is_visible(
        "backup-destination-kind-badge",
        scope="backup-destination-status-row[0]",
    ), (
        "backup-destination-kind-badge should render on every row: "
        f"{app.driver.diagnose('backup-destination-kind-badge')}"
    )
    assert app.backups.destination_kind_badge_text(0) == S.backups.backup_destination_kind_client_device
    assert app.backups.destination_usage_visible(0), (
        "backup-destination-usage should render on a client-device row: "
        f"{app.driver.diagnose('backup-destination-usage')}"
    )
    # This device has never checked in yet (no pull pass has run under e2e),
    # so the usage text is the "nothing held yet" baseline — never a fabricated
    # zero (backups.md § Third destination kind; cap-reached is read from
    # cap_state, never inferred from held >= cap).
    assert app.backups.destination_usage_text(0) == S.backups.backup_destination_usage_unknown

    # Register + add a NEST destination alongside it: the sole-client warning
    # must now disappear (not every destination is a client device anymore).
    try:
        register_user(
            second_nest["port"],
            test_user["actor_id_hex"],
            admin_signing_key=second_nest["admin"]["signing_key"],
        )
    except Exception:
        pass
    app.backups.add_destination(second_nest["url"], name="Offsite")
    app.backups.wait_for_destination_count(2)
    assert not app.backups.sole_client_destination_warning_visible(), (
        "the sole-client-destination warning must clear once a nest "
        f"destination coexists: error={app.error_text()!r}"
    )
    # The nest row's own kind badge reads "Another nest", distinctly.
    nest_row = 0 if app.backups.destination_kind_badge_text(0) != S.backups.backup_destination_kind_client_device else 1
    assert app.backups.destination_kind_badge_text(nest_row) == S.backups.backup_destination_kind_nest
    assert not app.backups.destination_usage_visible(nest_row), (
        "backup-destination-usage must NOT render on a nest-kind row"
    )

    # Cleanup: remove both rows.
    app.backups.remove_destination(0)
    app.backups.wait_for_destination_count(1)
    app.backups.remove_destination(0)
    app.backups.wait_for_destination_count(0)


@pytest.fixture
def custodian_pull_app(request, app, nest_instance, test_user, tmp_path_factory):
    """`logged_in_app`, but on windows the launch's isolated `fauna-sync-agent.exe`
    is confirmed serving the run's pipe BEFORE login.

    Why before login: login's own hydration loop only spawns the agent lazily
    and grants itself a "spawn grace" period before its NEXT convergence tick —
    the app's regular `RefreshBearer`/provisioning traffic tolerates that, but
    `custodian_pull_run_now` is a one-shot IPC call with no such retry, so it
    can race a lazy spawn and fail with "agent unreachable ... The system cannot
    find the file specified" (measured 2026-08-27: intermittent, ~1 in 3 runs,
    whenever login->navigate->enroll finished before the agent's pipe was up).

    WHOSE agent: the launch's own, on the launch's own `--data-dir`
    (`driver.sync_agent_state_base`, the session-scoped
    `isolated_sync_agent_data_dir`) — `serving_agent` adopts it when the app
    already spawned it and spawns it on that same dir when nothing serves the
    pipe yet. ⚠ It once spawned a SECOND agent on a per-test dir and pinned the
    driver to that dir. Since every windows launch brings its own agent
    (convention 10, windows axis (a)), that second agent found the pipe served
    and exited as a duplicate on every test but a file's first; the launch's
    agent hosted the replica in the session dir while the witness read the
    empty per-test one. One agent per launch, one
    dir, whichever process started it.

    The session dir is a short flat `tmp_path_factory.mktemp` one on purpose:
    the custodian store nests an actor hex + "backup-custodian/blobs/
    {2-hex}/{64-hex}" (~155 chars) under the data dir, and a `tmp_path`-shaped
    parent pushed the total past Windows' 260-char `MAX_PATH` — measured
    2026-08-27, a 272-char blob path: `atomic_write_file` failed writing every
    blob, so every kind's `run_once` errored and the pass reported
    `kinds_run: 0`, `checked_in: false` despite `hosting: true`.

    Non-windows apps need no external agent process spawned here — each
    already brings its own, isolated by construction (see
    `sync_live_apply_app`'s docstring) — so this is a plain passthrough to
    `_login_app_as`.
    """
    from conftest import _login_app_as

    if app.driver.is_windows():
        from helpers.windows_sync_agent import serving_agent

        exe = request.getfixturevalue("sync_agent_binary")
        pipe_leaf = request.getfixturevalue("isolated_sync_agent_pipe_name")
        pipe_path = r"\\.\pipe\{}".format(pipe_leaf)
        data_dir = app.driver.sync_agent_state_base
        assert data_dir, (
            "this launch pinned no agent `--data-dir` (`FAUNA_E2E_SYNC_AGENT_DATA_DIR`), "
            "so its agent is not isolated and its custodian store is not ours to read — "
            "the test must carry `isolated_sync_agent` (convention 10)"
        )
        log_path = tmp_path_factory.mktemp("custodian-agent-spawn") / "agent.log"
        with serving_agent(exe, pipe_path, data_dir, log_path):
            _login_app_as(app, request, nest_instance, test_user)
            yield app
        return

    _login_app_as(app, request, nest_instance, test_user)
    yield app


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.windows
# windows only: the sync agent is a SEPARATE process there (unlike tui/linux,
# which run the provisioner in-process), so a real one must be explicitly
# spawned and isolated from the box's installed/per-SID agent — the same pair
# `test_sync_agent_unprovision_windows.py` carries. Inert for tui/linux: the
# env these set is only ever read in conftest's `app_name == "windows"` launch
# branch.
@pytest.mark.real_sync_agent
@pytest.mark.isolated_sync_agent
@pytest.mark.feature("backup-destinations-and-restore")
def test_a_hosted_custodian_pulls_checks_in_and_the_owners_row_reports_it(
    custodian_pull_app, nest_instance, test_user, request
):
    """The enroll -> pull -> check-in -> status round trip, end to end, against a
    real nest with a real agent-hosted replica.

    This is the proof `behavior/backup-destinations.md` § Implementation status
    today has listed as owed since the kind was ratified ("the end-to-end tier_3
    enroll->pull->check-in->status proof driving a real device against a real
    nest ... what remains is the pull side meeting it"). Everything upstream of
    it was already pinned: the pull pass, the custody policy and the local store
    have mutation-verified tier_1 coverage, the nest half has
    `conformance_backup_custodian.rs`, and `test_client_device_custodian_destination`
    covers the enroll UI. What nothing covered is the join — that a device
    enrolled through the app is *found* by the host loop, pulls the owner's real
    segments, and reports bytes the owner's own page then shows.

    Flow under test::

        seed a mail segment  -> the owner's nest-local SegmentManager holds one
        enroll via the UI    -> enroll_client_custodian: registry row carrying
                                kind=client-device + this device's sync id
        poll the pass poke   -> agent's custodian loop re-reads the registry
                                (IDLE_RECHECK_SECS), matches its own device id,
                                opens the sealed store, hosts -> hosting: true
        one poked pass       -> list -> diff -> fetch -> seal -> store -> reclaim
                                -> fauna.backup.custodian.checkin
        nest projection      -> fauna.backup.status inverts the row: held_bytes,
                                backlog from head - high_water
        re-mount the page    -> backup-destination-usage leaves its baseline

    **Latency-independent throughout** (convention 14). The poke is a causal
    barrier, not a settle-sleep: the agent replies only once the pass has written
    its check-in, so when `custodian_pull_run_now` returns, the round trip has
    either happened or definitively failed. The one genuine wait — the host loop
    noticing the new registry row — is a deadline poll over the poke itself,
    whose `hosting: false` arm exists precisely so this needs no sleep.

    tui-marked because tui is the lead app and drives the sync agent directly;
    **linux and windows joined once each grew the `custodian_pull_run_now`
    command arm this poll pokes** (`apps/fauna-linux/src/main.rs`;
    `apps/fauna-windows/FaunaApp/FaunaApp/Testing/TestAgent.cs`) — they host the
    same replica through the same agent. web is a declared absence (a browser
    cannot host the sealed store), and the mobile shells drive their own
    schedulers rather than this agent.

    Rides `custodian_pull_app`, not the plain `logged_in_app`, so that on
    windows the real agent is spawned and its pipe confirmed serving BEFORE
    login — see that fixture's docstring for the race it closes.
    """
    custodian_pull_app.backups.require_destination_management_supported()
    app = custodian_pull_app

    # Precondition only (convention 8 exempts fixture setup): the mutation under
    # test is the enroll, driven through the page below. Without a real segment
    # the pass would mirror an empty manifest and every held-bytes assertion
    # would pass against a device holding nothing.
    _seed_one_mail_segment(app, nest_instance, test_user)

    app.backups.navigate()
    app.backups.wait_for_destination_count(0)

    # Registered BEFORE the enroll, so a failure anywhere below still leaves no
    # row for the file's later zero-count preconditions (it once failed four
    # later tests on "Expected 0 backup-destination rows, got 2").
    request.addfinalizer(lambda: _leave_no_custodian_behind(app))
    app.backups.add_custodian_destination(name="This device", capacity="50 GB")
    app.backups.wait_for_destination_count(1)

    # Pre-state. Without it the post-assert proves nothing: a row already
    # reporting held bytes would satisfy the final check too.
    assert app.backups.destination_usage_text(0) == S.backups.backup_destination_usage_unknown, (
        f"a freshly enrolled custodian must read the 'nothing held yet' baseline "
        f"before any pass — never a fabricated zero. error={app.error_text()!r}"
    )

    # The agent discovers its own assignment on its own cadence; every poll both
    # checks and runs a pass the moment it has.
    first = app.backups.wait_for_custodian_hosting()

    # Split the verdict at the pass itself, before touching the UI (convention 5):
    # a red below must distinguish "the pass never moved bytes" from "the page
    # isn't rendering what the nest returned".
    assert first["checked_in"], (
        f"the pass ran but no kind checked in ({first!r}). The check-in is inside "
        f"`run_once`, and a failed check-in makes it return Err — so a pass with "
        f"kinds_run=0 means every kind failed before reporting."
    )
    assert first["cap_state"] == "ok", (
        f"a 50 GB cap cannot be reached by one seeded mail segment, so a "
        f"cap_state of {first['cap_state']!r} means the verdict is being derived "
        f"rather than read ({first!r})."
    )
    assert first["held_bytes"] > 0, (
        f"the pass reported holding {first['held_bytes']} bytes after pulling a "
        f"seeded segment ({first!r}) — the custodian either listed nothing at the "
        f"source or stored nothing it listed. A zero here is the whole failure "
        f"this proof exists to catch: every upstream tier_1 test passes against a "
        f"pass that moves no bytes."
    )

    # The nest's own projection — the exact source all 7 apps read.
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    status_client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=test_user["actor_id_bytes"],
        signing_key=bytes(test_user["signing_key"]),
    )
    with status_client:
        projection = status_client.call("fauna.backup.status", {})
    rows = projection.get("destinations", [])
    assert rows, (
        f"the nest projects no destination rows after a check-in: {projection!r}. "
        f"The status projection is registry-driven precisely so a pull-only "
        f"custodian — which grants no NestBackupKey by design — is not served an "
        f"empty list forever."
    )
    nest_row = rows[0]
    assert nest_row.get("held_bytes"), (
        f"NEST-SIDE failure: the pass reported held_bytes={first['held_bytes']} "
        f"but the nest projects held_bytes={nest_row.get('held_bytes')!r} "
        f"({nest_row!r}). The check-in either never landed or landed against a "
        f"different destination row."
    )

    # The owner's own page. Navigate away first so the return is a real re-mount:
    # the page re-reads `fauna.backup.status` on mount and after add/edit/remove,
    # never on a timer.
    app.driver.navigate_to("feed")
    app.backups.navigate()
    app.backups.wait_for_destination_count(1)

    USAGE_RENDER_BUDGET_S = 30
    deadline = time.monotonic() + USAGE_RENDER_BUDGET_S
    usage = ""
    while time.monotonic() < deadline:
        usage = app.backups.destination_usage_text(0)
        if usage != S.backups.backup_destination_usage_unknown:
            break
        time.sleep(0.5)  # sleep-ok: poll interval inside the deadline loop above — the pass has already checked in, so this waits only on the page's own re-read

    assert usage != S.backups.backup_destination_usage_unknown, (
        f"CLIENT-SIDE failure: the nest projects held_bytes="
        f"{nest_row.get('held_bytes')!r} for this custodian, but the row still "
        f"reads the 'nothing held yet' baseline {usage!r} after "
        f"{USAGE_RENDER_BUDGET_S}s. The nest-side asserts above already passed, so "
        f"this client is either not re-reading the projection on re-mount or is "
        f"swallowing the read into the baseline. error={app.error_text()!r}"
    )

    # `nest_instance`/`test_user` are session-scoped and a sibling test asserts a
    # zero destination count, so leave the surface as we found it — including the
    # agent's custodian stint. A plain remove KEEPS the copy (the product
    # default), so the agent keeps hosting the old replica until its next
    # registry rediscovery, and the next custodian test's first pass is that
    # stale replica's (hosting, zero kinds run, check-in refused).
    app.backups.remove_destination(0)
    app.backups.wait_for_destination_count(0)
    _reclaim_the_orphaned_store(app)



@pytest.mark.tui
@pytest.mark.feature("backup-destinations-and-restore")
def test_a_custodian_whose_store_rots_reports_a_failing_row_to_its_owner(
    logged_in_app, nest_instance, test_user
):
    """A custodian whose local store fails its **self-audit** raises
    `backup-audit-alert` on the OWNER's device — the second pin
    `behavior/backup-destinations.md` § Implementation status today lists as owed
    ("a custodian whose store fails its self-audit produces a visibly-failing row
    on the OWNER's device", the sentence tui's five render tests cover only the
    app half of).

    Companion to `test_a_hosted_custodian_pulls_checks_in_and_the_owners_row_reports_it`,
    which proves the healthy round trip. This one proves the *alarming* one, and
    the two together are what make the third destination kind observable: a
    client-device custodian is the one destination the owner-side audit loop can
    never sample — it has no address to connect to — so the device's own verdict,
    carried on its check-in, is the ONLY failure signal that exists for it.
    Nothing below the render was ever run end to end: `custodian_pull`'s
    `audit_if_due` has tier_1 coverage, the wire field has conformance coverage,
    and the banner has five render tests, but no test had ever rotted a real
    store and watched the alarm come out the far end.

    Flow under test::

        enroll + pass 1   -> the store is created, audits EMPTY (an empty store
                             passes — a device holding nothing is holding all of
                             it), then pulls the seeded segment. audit_state: ok,
                             no banner.
        rot the store     -> overwrite every sealed blob under the custodian
                             store root. The index still claims those
                             generations, so the store now claims bytes it
                             cannot produce — the exact condition the self-audit
                             exists to find.
        pass 2 at +25 h   -> past AUDIT_MIN_INTERVAL_SECS, so `audit_if_due`
                             actually re-audits: `open()` fails the content
                             verification, `record_audit(now, false)` persists
                             the failure, and `run_once`'s check-in carries
                             audit_state=failed with `last_audit_passed_at` still
                             frozen at pass 1 (a failure never advances it).
        nest projection   -> fauna.backup.status passes the verdict through
        render            -> backup-audit-alert appears, naming this destination

    **Why the corruption is an overwrite and not a delete.** Deleting a blob
    would fail a mere `stat` too; overwriting one fails only if the store really
    verifies the content it claims to hold. If the audit ever weakened to an
    existence check, this goes red.

    ⚠ It does **not** separate the store's two audit arms, and saying so would
    be wrong: `CustodianStore::verify_presence` (the mirror plane's arm) is not
    a stat either — it reads the manifest and every chunk and hash-verifies them
    against their own addresses, so routing the sealed plane to it leaves this
    test green (measured 2026-08-21 by doing exactly that). What separates the
    arms is **decryption**: only `open` opens the sealed bytes under the owner's
    key, so a custodian holding intact bytes it can no longer DECRYPT is a
    failure mode nothing here covers yet — pinning it means corrupting the key,
    not the bytes.

    **Why the clock is the only injected thing.** The finding itself is real:
    real bytes are really unproducible, and the verdict is reached by the
    production `audit_if_due` on the production `run_all_kinds` path. Only the
    *cadence* is faked, and only because the cadence is a day
    (`AUDIT_MIN_INTERVAL_SECS`) — convention 14's fake clock, handed to the pass
    as the same `now` the scheduler supplies, never a sleep. Remove the offset
    and the audit is debounced, no verdict changes, and no banner appears: the
    jump is load-bearing but it manufactures nothing.
    """
    from helpers.sync_agent_config import (
        custodian_store_blobs,
        custodian_store_roots,
        describe_custodian_store,
    )
    from actions.backups import AUDIT_DEBOUNCE_JUMP_S

    logged_in_app.backups.require_destination_management_supported()
    app = logged_in_app

    # Precondition only (convention 8 exempts fixture setup): without a real
    # segment the store holds nothing, and "every blob corrupted" would corrupt
    # nothing — the audit would pass and this test would be green on an empty
    # store forever.
    _seed_one_mail_segment(app, nest_instance, test_user)

    app.backups.navigate()
    app.backups.wait_for_destination_count(0)

    DESTINATION_NAME = "Rotting device"
    app.backups.add_custodian_destination(name=DESTINATION_NAME, capacity="50 GB")
    app.backups.wait_for_destination_count(1)

    # Pass 1 — the healthy baseline. Every poll runs a pass the moment the agent
    # has found its assignment, so this is a deadline poll, never a wait.
    first = app.backups.wait_for_custodian_hosting()
    assert first["held_bytes"] > 0, (
        f"pass 1 pulled nothing ({first!r}), so there is nothing in the store to "
        f"rot and the audit below would pass vacuously. This assert is what keeps "
        f"the test from going green against an empty store."
    )
    assert first["audit_state"] == "ok", (
        f"a fresh custodian's first pass audits an EMPTY store, which passes — a "
        f"device holding nothing is holding all of it. Got {first['audit_state']!r} "
        f"({first!r}); anything else means the baseline this test contrasts against "
        f"does not exist."
    )

    # The banner's pre-state, read through a real re-mount. Without it a red
    # below cannot tell "the rot raised the alarm" from "the alarm was already up".
    app.driver.navigate_to("feed")
    app.backups.navigate()
    app.backups.wait_for_destination_count(1)
    assert app.driver.count("backup-audit-alert") == 0, (
        f"a healthy custodian must raise no audit alert; the page already shows "
        f"{app.driver.count('backup-audit-alert')}. error={app.error_text()!r}"
    )

    # Rot the store. The agent keeps it under its own per-actor state root, which
    # this launch isolated — `sync_agent_state_base` is the driver's answer for a
    # launch that pinned one explicitly (windows), `config_home` derives it
    # otherwise (unix).
    roots = custodian_store_roots(
        app.driver.config_home, getattr(app.driver, "sync_agent_state_base", None)
    )
    assert roots, (
        f"the agent reported hosting a custodian replica ({first!r}) but created no "
        f"store under its own state root. Either the store moved out of "
        f"`custodian_store_root`'s layout (<base>/<actor-hex>/backup-custodian) or "
        f"the agent is writing outside this launch's isolation, which convention 10 "
        f"forbids."
    )
    corrupted = 0
    for root in roots:
        for blob in custodian_store_blobs(root):
            blob.write_bytes(b"rot")
            corrupted += 1
    assert corrupted, (
        f"nothing was corrupted, so the audit below has no reason to fail and a "
        f"green result would prove nothing. "
        + "; ".join(describe_custodian_store(r) for r in roots)
    )

    # Pass 2, a day and an hour later — the first pass whose audit is not
    # debounced, and the first that can therefore see the rot.
    second = app.backups.custodian_pull_run_now(now_offset_secs=AUDIT_DEBOUNCE_JUMP_S)
    assert second["hosting"], (
        f"the agent stopped hosting between the two passes ({second!r}) — the "
        f"stint died rather than the audit failing."
    )
    assert second["audit_state"] == "failed", (
        f"{corrupted} sealed blob(s) were overwritten with garbage the store cannot "
        f"produce, yet the pass reports audit_state={second['audit_state']!r} "
        f"({second!r}). `None` means the audit was still debounced — the clock jump "
        f"did not reach `audit_if_due`. `'ok'` is worse: the audit ran and passed "
        f"over unproducible bytes, which means the sealed plane is being sampled "
        f"with a presence check instead of `CustodianStore::open`."
    )
    assert second["checked_in"], (
        f"a FAILED audit must still check in ({second!r}). Withholding the report "
        f"makes the row go silent, and silence is read by the 30-day intermittency "
        f"rule as a sleeping device — the wrong alarm, thirty days late "
        f"(`backup-destinations.md` § Custodian contract, question 4)."
    )

    # The nest's own projection — the exact source all 7 apps read. Split here so
    # a red below distinguishes a nest that dropped the verdict from a client
    # that did not render it (testing.md § point 6).
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    status_client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=test_user["actor_id_bytes"],
        signing_key=bytes(test_user["signing_key"]),
    )
    with status_client:
        projection = status_client.call("fauna.backup.status", {})
    reporting = [
        r
        for r in projection.get("destinations", [])
        if r.get("audit_state") is not None
    ]
    assert reporting and reporting[0].get("audit_state") == "failed", (
        f"NEST-SIDE failure: the device reported audit_state=failed but the nest "
        f"projects {projection!r}. `put_custodian_checkin` either dropped the "
        f"column or the row was matched to a different destination."
    )

    # The owner's own page. A re-mount, because the page re-reads
    # `fauna.backup.status` on mount and after add/edit/remove, never on a timer.
    app.driver.navigate_to("feed")
    app.backups.navigate()
    app.backups.wait_for_destination_count(1)

    ALERT_RENDER_BUDGET_S = 30
    deadline = time.monotonic() + ALERT_RENDER_BUDGET_S
    alerts = 0
    while time.monotonic() < deadline:
        alerts = app.driver.count("backup-audit-alert")
        if alerts:
            break
        time.sleep(0.5)  # sleep-ok: poll interval inside the deadline loop above — the failing verdict is already at the nest, so this waits only on the page's own re-read

    assert alerts == 1, (
        f"CLIENT-SIDE failure: the nest projects audit_state=failed for this "
        f"custodian, but the page shows {alerts} backup-audit-alert banner(s) after "
        f"{ALERT_RENDER_BUDGET_S}s. error={app.error_text()!r}"
    )
    banner = app.driver.get_text("backup-audit-alert")
    assert banner == S.backups.backup_audit_alert_self_reported(
        destination=DESTINATION_NAME
    ), (
        f"the banner must name the destination that reported the failure and say "
        f"the copy reported its OWN check failing — an owner with several "
        f"destinations cannot act on an unattributed alarm. Got {banner!r}."
    )

    # `nest_instance`/`test_user` are session-scoped and a sibling test asserts a
    # zero destination count, so leave the surface as we found it — including the
    # agent's custodian stint. A plain remove KEEPS the copy (the product
    # default), so the agent keeps hosting the old replica until its next
    # registry rediscovery, and the next custodian test's first pass is that
    # stale replica's (hosting, zero kinds run, check-in refused).
    app.backups.remove_destination(0)
    app.backups.wait_for_destination_count(0)
    _reclaim_the_orphaned_store(app)


def _reclaim_the_orphaned_store(app, budget_s: float = 30.0) -> None:
    """Reclaim the store a plain removal just orphaned, through the orphaned-store
    row, and wait for the row to retire. The agent's reclaim handler stops a live
    custodian stint before it deletes, so this is what ends the removed
    destination's replica. (The remove dialog's reclaim tick is meant to do the
    same in one gesture, but on linux it did not reach the agent in the
    2026-09-22 grading run, and nothing else exercises it.) Raises if no orphaned-store row paints: the custodian
    tests that call it all leave a non-empty store behind."""
    wait_until(app.backups.orphaned_store_visible, budget_s,
               diagnose=lambda: "no orphaned-store row after the removal")
    app.backups.reclaim_orphaned_store()
    wait_until(lambda: not app.backups.orphaned_store_visible(), 60.0,
               diagnose=lambda: f"the reclaim did not retire the row; "
                                f"error={app.error_text()!r}")


def _leave_no_custodian_behind(app) -> None:
    """Teardown for a custodian test that may stop anywhere: remove every
    remaining destination, then reclaim whatever store is left orphaned. A no-op
    on the happy path, which ends with neither."""
    app.backups.navigate()
    remaining = app.backups.destination_count()
    while remaining:
        app.backups.remove_destination(0)
        app.backups.wait_for_destination_count(remaining - 1)
        remaining -= 1
    if app.backups.orphaned_store_visible():
        app.backups.reclaim_orphaned_store()


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
# macos and windows: each spawns its agent only under this marker (without it the
# app binds folders with nothing behind them, and the pass would fall to the
# in-app host, whose store the agent's root never contains). Inert for every other
# app — the env it sets is read only in the macos/windows launch branches.
@pytest.mark.real_sync_agent
# windows only: the agent is a SEPARATE process there (unlike tui/linux, which run
# the provisioner in-process), so `custodian_pull_app` pre-spawns a real one on
# this run's own pipe, off the box's installed/per-SID agent — the same pair the
# sibling round trip above carries. Inert for every other app: the env it sets is
# read only in conftest's `app_name == "windows"` launch branch.
@pytest.mark.isolated_sync_agent
@pytest.mark.feature("backup-destinations-and-restore")
def test_removing_a_custodian_without_the_opt_in_leaves_a_reclaimable_orphaned_store(
    custodian_pull_app, nest_instance, test_user, request
):
    """Remove a client-device destination *without* the reclaim opt-in and the
    bytes stay; the orphaned-store row offers them back, and taking the offer
    actually frees them on disk.

    The proof `behavior/backup-destinations.md` § Implementation status today
    lists as owed for the reclaim affordance ("the tier_3 case that proves the
    whole gesture"). tui's tier_1 rules pin the surface and its guards — that the
    row paints on the shared verdict, that the confirm is plain and cancels
    cleanly, that the opt-in tick never survives its dialog — but nothing had
    ever run the round
    trip against a real agent holding real sealed bytes, so no test observed the
    two halves that only exist across the process boundary: that a removal
    *keeps* the store, and that a reclaim *empties* it.

    Flow under test::

        enroll + one pass -> the agent hosts a replica and pulls the seeded
                             segment into a sealed store on this launch's disk
        remove, opt-in OFF-> the registry row goes; the store deliberately stays,
                             because it is the owner's only offline copy
        the row appears   -> `custodian_store_is_orphaned` says the bytes are
                             claimed by nothing, and the row names how much
        reclaim + confirm -> the agent stops its own stint, then deletes
        the row goes      -> re-measured, not assumed: the agent walks the disk
                             again and reports nothing held

    **Both ends are witnessed off the UI as well as on it** — the blobs are
    counted on disk before the removal, again after it, and again after the
    reclaim.

    ⚠ **What the disk witness actually guards is narrower than it looks, and the
    mutation round is what established that.** The obvious guess — that a reclaim
    which reports success without deleting would sail past the page assertions and
    be caught only on disk — is WRONG, and was measured wrong: stub `reclaim_all`
    to report what it would have freed without freeing it and this test reds at
    the *page* assertion below, never reaching the disk one. `Outcome::Reclaimed`
    re-derives the row from a fresh `footprint` call rather than from the
    reclaim's own report, and `footprint` walks the disk, so the store is still
    visibly orphaned and the row correctly refuses to retire. The page is not
    blind to a lying reclaim.

    So the disk assertions earn their place on one specific failure the page
    cannot see: `footprint` itself regressing to an index-derived answer — the
    regression it walks the disk to avoid (`backup-destinations.md`
    § Implementation status today). A page told "nothing held" retires the row,
    and every page-level assertion here passes over bytes that never went away.

    **Latency-independent throughout** (convention 14). The one real wait — the
    host loop noticing the new registry row — is the same deadline poll over
    `custodian_pull_run_now` the sibling proofs use. The reclaim itself needs no
    wait: the agent stops its stint and deletes *before* it replies, so the reply
    is the causal barrier. The two post-gesture polls are bounded reads of a page
    that has already been told the answer.

    **Which columns this speaks for, and why the rest cannot yet** (measured
    2026-09-20). Every app but web *implements* the gesture — the six IDs sit
    in each `ui-actual-<app>.yaml`, and `ui/backups.md` § Manage backup
    destinations → *Reclaim this device's copy* records it BUILT on tui, linux,
    android, macos, ios and windows — so what separates the columns here is the
    HARNESS, not the product:

    * **tui, linux** — marked. Both run the sync agent in-process and answer
      `driver.config_home`, so both halves of this test work: the
      `custodian_pull_run_now` command arm (`apps/fauna-tui/src/automation.rs`,
      `apps/fauna-linux/src/main.rs`) runs the pass, and `custodian_store_roots`
      finds the store inside the launch's own isolated XDG world. linux's
      sibling round trip
      (`test_a_hosted_custodian_pulls_checks_in_and_the_owners_row_reports_it`)
      already holds a passing linux ledger record, which is what made this
      widening a marker add rather than a lift.
    * **windows** — marked (2026-09-21). The
      command arm (`Testing/TestAgent.cs`) was already in place; what was owed
      was the harness. The agent is a separate process there, so the test rides
      `custodian_pull_app`, which has the launch's own isolated agent serving the
      run's pipe on the launch's `--data-dir` before login. The driver answers
      `sync_agent_state_base` — and deliberately no `config_home`, an XDG concept
      the windows launch does not have — so the disk half finds the store the way
      apple's does. ⚠ The answer is that `--data-dir` (the base the per-actor
      store nests under), **not** the driver's
      `_resolved_store_root` (`<LOCALAPPDATA>/Fauna/sync`): that is the ACCOUNT
      store's root and the custodian never writes there.
    * **macos, ios** — marked (2026-09-21).
      Both answer `custodian_pull_run_now` through ONE shared FaunaKit handler
      (`CustodianPullTestCommand`), whose platform pick is the runtime one
      `FaunaClient` already makes for the store itself: a provisioner means the
      external agent hosts (macos — hence `real_sync_agent` above), none means
      the app process does (ios). Each driver answers `sync_agent_state_base`,
      which is what lets the disk half find the store with no `config_home`:
      macos's is `<launch HOME>/Library/Application Support/Fauna/sync` — the
      agent's user-domain root, **not** the app-group container it lived in before
      2026-08-25 — and ios's is the simulator container's
      `Library/Application Support/Fauna`, the in-app host's unscoped base.
      ⚠ The two replies are not the same six fields: ios's in-app door reports
      only `hosting` / `held_bytes` (plus segment counts) and refuses a non-zero
      `now_offset_secs` by name, so a test that needs the agent's `checked_in` /
      `audit_state` or a shifted clock cannot yet widen to it.
    * **android** — no `custodian_pull_run_now` arm in its test agent at all, so
      `wait_for_custodian_hosting` has nothing to poke, and it hosts in-app like
      ios, leaving no agent path for the store root to be derived from.
    * **web** — not a short column but an absent one: it hosts no custodian and
      so can hold no orphaned store (`ui/backups.md` § Manage backup
      destinations → *Reclaim this device's copy*: "the one remaining leg is
      web's declared absence"). Under `architecture/feature-catalog.md` § The
      coverage contract that is a page split with a page-level absence, never a
      per-outcome annotation — a gated contract move, open with the user.
    """
    from helpers.sync_agent_config import (
        custodian_store_blobs,
        custodian_store_roots,
        describe_agent_state_base,
        describe_custodian_store,
    )

    custodian_pull_app.backups.require_destination_management_supported()
    app = custodian_pull_app

    # Precondition only (convention 8 exempts fixture setup): without a real
    # segment the pass stores nothing, every blob count below is zero, and the
    # whole gesture goes green over an empty store — the vacuous pass this test
    # exists to rule out.
    _seed_one_mail_segment(app, nest_instance, test_user)

    app.backups.navigate()
    app.backups.wait_for_destination_count(0)

    # Whatever fails below — the enroll's own wait included — leave no
    # destination and no custodian store behind: `test_user` is session-scoped,
    # and a leftover row here once failed the next three tests on their
    # "Expected 0 backup-destination rows" precondition.
    request.addfinalizer(lambda: _leave_no_custodian_behind(app))
    app.backups.add_custodian_destination(name="Departing device", capacity="50 GB")
    app.backups.wait_for_destination_count(1)

    first = app.backups.wait_for_custodian_hosting()
    assert first["held_bytes"] > 0, (
        f"the pass stored nothing ({first!r}), so there are no bytes for the "
        f"removal to keep or the reclaim to free, and every assertion below "
        f"would pass vacuously against an empty store."
    )

    # Pre-state on the PAGE: a store a destination row still claims is not
    # orphaned. Without this, a row that painted unconditionally would satisfy
    # the post-removal assert too and the shared verdict would go untested.
    assert not app.backups.orphaned_store_visible(), (
        f"a store claimed by a live destination row must not be offered for "
        f"reclaim — `custodian_store_is_orphaned` answers `None` while any row "
        f"names this device. error={app.error_text()!r}"
    )

    # Pre-state on DISK: the independent witness. `sync_agent_state_base` is the
    # driver's answer for a launch whose store root is not derivable from an XDG
    # config home — windows (an explicit `--data-dir`) and apple (a pinned HOME;
    # no `config_home` at all, hence the `getattr`) — and `config_home` derives it
    # otherwise (unix). The same pair the rot proof above reads, minus its
    # unconditional `config_home` access.
    roots = custodian_store_roots(
        getattr(app.driver, "config_home", None),
        getattr(app.driver, "sync_agent_state_base", None),
    )
    assert roots, (
        f"the agent reported hosting a replica ({first!r}) but created no store "
        f"under its own state root — either the layout moved out of "
        f"`custodian_store_root` or the agent is writing outside this launch's "
        f"isolation, which convention 10 forbids. "
        + describe_agent_state_base(
            getattr(app.driver, "config_home", None),
            getattr(app.driver, "sync_agent_state_base", None),
        )
    )
    blobs_before = [b for root in roots for b in custodian_store_blobs(root)]
    assert blobs_before, (
        f"the pass reported held_bytes={first['held_bytes']} but wrote no blob "
        f"files this test can see. "
        + "; ".join(describe_custodian_store(r) for r in roots)
    )

    # The gesture: remove WITHOUT the opt-in. The product rule under test is that
    # this KEEPS the copy — it is the owner's only offline one.
    app.backups.remove_destination(0, reclaim=False)
    app.backups.wait_for_destination_count(0)

    # Half one, on disk: removal is not deletion. Stated as containment rather
    # than an equal count, because the rule under test is that nothing was
    # DELETED — a count would also go red if a scheduled pass happened to add a
    # blob in this window, which is not a failure of anything.
    blobs_after_removal = [b for root in roots for b in custodian_store_blobs(root)]
    deleted = sorted(set(blobs_before) - set(blobs_after_removal))
    assert not deleted, (
        f"removing a client-device destination WITHOUT the reclaim opt-in deleted "
        f"{len(deleted)} of {len(blobs_before)} sealed blob(s) anyway: "
        f"{[str(b) for b in deleted[:5]]}. The default keeps the copy on purpose — "
        f"it is the owner's only offline one — which is the whole reason the opt-in "
        f"is a separate tick the removal must not imply."
    )

    # Half one, on the page: the row that offers the bytes back. The removal op
    # carries a handle to re-measure with, so this repaints without a re-mount —
    # but the measurement is an IPC round trip, so it is a deadline poll.
    ORPHAN_ROW_BUDGET_S = 30
    deadline = time.monotonic() + ORPHAN_ROW_BUDGET_S
    while time.monotonic() < deadline:
        if app.backups.orphaned_store_visible():
            break
        time.sleep(0.5)  # sleep-ok: poll interval inside the deadline loop above — the removal has already returned, so this waits only on the page's own re-measure

    assert app.backups.orphaned_store_visible(), (
        f"{len(blobs_after_removal)} sealed blob(s) are still on disk with no "
        f"destination row claiming them, but the page offers no "
        f"`backup-orphaned-store-row` after {ORPHAN_ROW_BUDGET_S}s. Those bytes "
        f"are then unreclaimable from the app for the life of the install — the "
        f"failure this affordance exists to prevent. error={app.error_text()!r}"
    )

    # The row must name what the device is actually holding. Read the sentence
    # off the generated i18n module rather than retyping it (the copy has drifted
    # under a hardcoded literal before), and split on a sentinel so the assertion
    # survives a re-wording that keeps the placeholder.
    prefix, suffix = S.backups.backup_orphaned_store_row(held="\x00").split("\x00", 1)
    row_text = app.backups.orphaned_store_text()
    # Containment, in order — NOT anchored to the ends of `row_text`.
    # `backup-orphaned-store-row` is a CONTAINER: ui.yaml puts
    # `backup-destination-reclaim-button` `.within` it, so on any driver whose
    # `get_text` folds in descendant text the button's own label trails the
    # sentence (linux: a `gtk::Box` yields the label AND "Free up this space" —
    # measured 2026-09-20 when this test was widened past tui). Anchoring made
    # the assertion a tui-shaped accident rather than the check it describes;
    # this is the same containment shape the `destination_text` assertions in
    # this module already use for rows that carry buttons. What it still
    # discriminates is exactly what the message claims: the row resolving the
    # shared `orphaned_store_text` versus composing its own copy — a re-worded
    # or hand-built sentence fails on the prefix or the suffix either way.
    start = row_text.find(prefix)
    end = row_text.find(suffix, start + len(prefix)) if suffix else len(row_text)
    assert start != -1 and end != -1, (
        f"the orphaned-store row reads {row_text!r}, which does not contain the "
        f"shared `backups.backup_orphaned_store_row` sentence — the row is "
        f"composing its own copy instead of resolving "
        f"`fauna_core::format::orphaned_store_text`."
    )
    held = row_text[start + len(prefix) : end]
    assert not re.match(r"^0(\.0+)?\s", held), (
        f"the row offers back {held!r} while {len(blobs_after_removal)} sealed "
        f"blob(s) sit on disk. A zero means the row is painting off the verdict "
        f"alone and quoting a measurement that came back empty — the offer would "
        f"read as worthless and the user would decline it."
    )

    # The reclaim. The agent stops its own custodian stint before deleting, so a
    # `still_hosting` refusal is a reported outcome the page shows rather than an
    # exception — which is why the assert below reads the page's error text.
    app.backups.reclaim_orphaned_store()

    RECLAIM_BUDGET_S = 60
    deadline = time.monotonic() + RECLAIM_BUDGET_S
    while time.monotonic() < deadline:
        if not app.backups.orphaned_store_visible():
            break
        time.sleep(0.5)  # sleep-ok: poll interval inside the deadline loop above — the agent deletes before it replies, so this waits only on the page's own repaint

    assert not app.backups.orphaned_store_visible(), (
        f"the reclaim did not retire `backup-orphaned-store-row` within "
        f"{RECLAIM_BUDGET_S}s. The row re-derives from a FRESH measurement the "
        f"agent takes after deleting, so a row still standing means the agent "
        f"refused (a `still_hosting` teardown that outran its 30s budget) or the "
        f"delete failed. error={app.error_text()!r}"
    )

    # Half two, on disk: the bytes are actually gone. The assertion the UI cannot
    # make — a retired row proves the app stopped offering, never that the disk
    # space came back.
    blobs_after_reclaim = [b for root in roots for b in custodian_store_blobs(root)]
    assert not blobs_after_reclaim, (
        f"the page retired the orphaned-store row, but {len(blobs_after_reclaim)} "
        f"of {len(blobs_before)} sealed blob(s) are still on disk: "
        f"{[str(b) for b in blobs_after_reclaim[:5]]}. The user was told their "
        f"space came back and it did not. "
        + "; ".join(describe_custodian_store(r) for r in roots)
    )

    # `nest_instance`/`test_user` are session-scoped and a sibling test asserts a
    # zero destination count; the destination is already gone and the store is
    # empty, so the surface is as we found it.


# The "nothing has ever synced" row copy. Both branches of
# `fauna_core::format::backup_last_upload_label` render into
# `backup-destination-last-upload-time`, so this is what the row shows until a
# pass mirrors a manifest. Read off the generated i18n module rather than
# retyped: the literal drifted silently when the copy was re-worded to "Last
# synced" (2026-07-29), which a hardcoded string would not have caught.
_NEVER_UPLOAD_TEXT = S.backups.backup_destination_last_upload_never

# The audit twin — `backups.backup_destination_last_audit_never`. Rendered into
# `backup-destination-last-audit-time` until this client's own audit first
# *passes* against the destination.
_NEVER_AUDIT_TEXT = S.backups.backup_destination_last_audit_never


def _backlog_count(row_text: str) -> int:
    """Parse `backup-destination-backlog-count` ("{count} queued").

    Copy is `backups.backup_destination_backlog` in `i18n/strings/en.yaml`.
    """
    m = re.match(r"\s*(\d+)\s+queued\s*$", row_text)
    assert m, f"unrecognised backlog row text {row_text!r} (expected '<n> queued')"
    return int(m.group(1))


def _seed_one_mail_segment(app, nest_instance, owner) -> None:
    """Put one real **mail** segment in `owner`'s nest-local segment store.

    ``app`` must be signed in as ``owner``: the owner's mailbox is enabled through it
    first, so the seeded message is sealed to the owner's OWN standing key.

    Precondition-only (e2e convention 8 explicitly exempts fixture setup from the
    drive-it-through-the-UI rule): the mutation this test is about is the enroll —
    driven through the Backups page below — plus the nest's own backup sweep.

    Why a segment must exist at all: it is what makes the *backlog* assertions in the
    test meaningful. `last_upload_time` now reads `last_content_synced_at`, which only a
    real `record_custody` (inside the `diff.to_upload` loop, `segment_backup.rs:395`)
    advances — a bookkeeping-only manifest write (an owner's first pass with zero
    segments) no longer moves it (`docs/goal/ui/backups.md` § Copy/semantic mismatch,
    fixed 2026-07-29). The backlog check stays as independent, defense-in-depth proof
    that a segment actually moved rather than trusting the timestamp alone.

    Called once per parametrized client against a **session-scoped** nest + user, so it
    must be re-entrant: the source descriptor and dedup key are unique per call (a repeat
    of either trips `fauna.bridges.import_source_locked` / a dedup skip), and the session
    is finalized rather than left open.

    Why the **import** surface rather than the MTA/MDA bridge chain: it reaches the same
    write path (`bridge_import_handlers.rs` → `insert_appended_mail` → the mail
    `SegmentManager`) with three User-class, caller-scoped WS-RPC calls and no extra
    binaries — imports are "just APPENDs with a source-tracking annotation".

    ⚠ **The seal key is the owner's own, never a stand-in.** The import seals fail-closed
    to a key the owner has published, so a mailbox must exist; it is enabled the way a
    user enables one, through the app (idempotent across the session). This seed used to
    register a fixed test key instead, on the grounds that nothing here opens the blob.
    But the owner is the shared session ``test_user``, whose app DOES open it. Nothing
    enables that actor's mail before this module in a linux sweep, so the stand-in was
    its only key and the seeded record was sealed to it. The first real enable later in
    the run minted a fresh key, the record could never open, and every later module's
    relaunch counted it again. That raised the unopenable-mail floor on the conversations
    page (``ui/conversations.md`` § Errors & edge cases → *A fifth truth*), which no
    gesture clears, so every later "page shows no error" assertion in the run failed on
    it. (With a mailbox already enabled the stand-in happened to be harmless: the import
    seals to the owner's published epoch key, and the app's next sign-in re-registers its
    own standing key.) e2e convention 10: a test must not poison the shared identity.
    """
    from clients.ws_rpc_admin_client import WsRpcAdminClient
    from helpers.mail_envelope_key import envelope_key

    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()

    nonce = secrets.token_hex(6)
    message_id = f"backup-upload-proof-{nonce}@example.org"
    body = (
        f"Message-ID: <{message_id}>\r\n"
        "From: alice@example.org\r\n"
        "To: bob@example.net\r\n"
        "Subject: segment for the backup upload proof\r\n"
        "Date: Thu, 24 Jul 2026 12:00:00 +0000\r\n"
        "\r\n"
        "one message is all the sweep needs to have something to upload\r\n"
    ).encode()

    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=owner["actor_id_bytes"],
        signing_key=bytes(owner["signing_key"]),
    )
    with client:
        session_id = client.call(
            "fauna.bridges.start_import_session",
            {"source_descriptor": f"imap://imap.example.org/backup-proof-{nonce}",
             "total_count": 1},
        )["session_id"]
        reply = client.call(
            "fauna.bridges.import_message",
            {
                "session_id": session_id,
                "message": {
                    "mailbox": "INBOX",
                    "flags": ["\\Seen"],
                    "body": body,
                    "timestamp": 1_783_000_000,
                    "body_size": len(body),
                    "sender_domain": "example.org",
                    "source_uid": 1,
                    "source_uid_validity": 42,
                    "dedup_key": f"msgid:v1:<{message_id}>",
                    "envelope_key": envelope_key(body),
                },
                "skip_dedup": False,
            },
        )
        # Release the source lock so a second parametrized client can seed too.
        client.call("fauna.bridges.finalize_import_session", {"session_id": session_id})

    outcome = reply["outcome"]["outcome"]
    assert outcome == "imported", (
        f"seeding a mail segment failed: outcome={reply['outcome']!r}. Without a segment "
        f"the owner's backlog stays empty and nothing can be uploaded."
    )


@pytest.mark.feature("backup-destinations-and-restore")
def test_backup_destination_last_upload_time_reflects_a_nest_side_pass(
    logged_in_app, nest_instance, second_nest, test_user
):
    """`backup-destination-last-upload-time` advances off a **nest-side** upload.

    This is the proof leg (d) owed. `test_backup_destination_crud` above asserts the
    element is *visible*; nothing asserted it ever *advances*, and an assertion-by-
    existence passes just as happily against a page wired to a permanently-"never"
    projection. Leg (d) repointed all 7 apps off their local `FfiBackupCoordinator`
    onto the nest's `fauna.backup.status` projection precisely so an owner on web or
    mobile — who runs no local coordinator at all — sees a truthful page
    (`docs/goal/ui/backups.md` § Per-destination status read). The timestamp asserted
    here can only have been written by a pass the *nest* ran: this client has no backup
    driver on this path.

    Flow under test::

        import a mail  → nest mail SegmentManager holds one segment
        enroll via UI  → backup_enroll.rs: seal-key grant at source,
                         writer grant at destination, destination registered at source
        run-now poke   → NestBackupWorker::run_once → run_all_tuples → run_once(dest,"mail")
                       → upload_bytes + record_custody  (segment_backup.rs:395)
        re-mount page  → fauna.backup.status → max_manifest_synced_at_for_dest
                       → "Last synced: <when>" replaces "Last synced: never"

    **Latency-independent throughout** (e2e convention 14). The poke is a causal
    barrier, not a settle-sleep: `POST /api/v1/test/backup/run-now` runs exactly one
    sweep *synchronously* and only then replies, so when it returns the upload has
    either happened or definitively failed. Waiting for the production 15-minute
    `PERIODIC_INTERVAL` tick (`bins/fauna-nest/src/main.rs:1322`) is what the hook
    exists to avoid. The single remaining wait is the page's own re-read after the
    re-mount, which takes a named generous budget + deadline poll.
    """
    logged_in_app.backups.require_destination_management_supported()

    # The owner must be authorized on the destination nest for the enroll handshake
    # (`require_registration` is the nest default). Idempotent across re-runs of the
    # session-scoped nest, exactly as in `test_backup_destination_crud`.
    try:
        register_user(
            second_nest["port"],
            test_user["actor_id_hex"],
            admin_signing_key=second_nest["admin"]["signing_key"],
        )
    except Exception:
        pass

    app = logged_in_app
    _seed_one_mail_segment(app, nest_instance, test_user)

    app.backups.navigate()
    app.backups.wait_for_destination_count(0)

    app.backups.add_destination(second_nest["url"], name="Upload-proof")
    app.backups.wait_for_destination_count(1)

    # Pre-state. Without this the post-assert proves nothing: a row that read
    # "Last synced: 3 seconds ago" before the sweep would pass the final check too.
    before = app.driver.get_text(
        "backup-destination-last-upload-time",
        scope="backup-destination-status-row[0]",
    )
    assert before.strip() == _NEVER_UPLOAD_TEXT, (
        f"a freshly enrolled destination must read {_NEVER_UPLOAD_TEXT!r} before any "
        f"pass has run, got {before!r} — the post-poke assertion below is only "
        f"meaningful against that baseline. error={app.error_text()!r}"
    )
    # The backlog is what makes the seeded segment load-bearing, as independent
    # proof alongside the timestamp: `last_upload_time` now reads
    # `last_content_synced_at`, which only advances on a real upload/drop — a
    # bookkeeping-only manifest write (an owner's first pass with zero segments)
    # no longer moves it (`docs/goal/ui/backups.md` § Copy/semantic mismatch).
    # Pinning "1 queued" → "0 queued" around the sweep is still the assertion
    # only a real segment upload can satisfy. (Verified by mutation: dropping the
    # seeding leaves both counts at 0 and fails here.)
    backlog_before = _backlog_count(app.driver.get_text(
        "backup-destination-backlog-count",
        scope="backup-destination-status-row[0]",
    ))
    # `>= 1`, not `== 1`: `nest_instance`/`test_user` are session-scoped, so an
    # earlier parametrized client may have left segments in the owner's store —
    # and every one of them is backlog for this brand-new destination.
    assert backlog_before >= 1, (
        f"the seeded mail segment must show as queued before the sweep, got "
        f"{backlog_before} queued. Without a real backlog this test would pass on "
        f"the manifest-mirror write alone and prove nothing about segment upload. "
        f"error={app.error_text()!r}"
    )

    # Navigate away so the return trip below is a real re-mount: both linux and web
    # re-read `fauna.backup.status` on mount and after add/edit/remove, never on a
    # timer, so staying on the page would assert against the pre-sweep render.
    app.driver.navigate_to("feed")

    # The causal barrier: one synchronous sweep, then the reply.
    resp = requests.post(
        f"{nest_instance['url']}/api/v1/test/backup/run-now",
        json={},
        timeout=60,
    )
    assert resp.status_code == 200, (
        f"test-hooks backup run-now returned {resp.status_code}: {resp.text}"
    )
    payload = resp.json()
    assert payload.get("ok") is True, payload
    # `owners_run == 0` is the specific diagnostic for "the owner has no granted
    # NestBackupKey or no registered destination" — i.e. the enroll above did not
    # reach the nest. Asserting it here separates that from a rendering failure
    # rather than letting a silent no-op read downstream as a product bug.
    assert payload.get("owners_run", 0) >= 1, (
        f"the sweep ran no owners ({payload!r}): the enroll did not leave this owner "
        f"with both a granted NestBackupKey and a registered destination, so nothing "
        f"could have been uploaded."
    )

    # Split the verdict before touching the UI again (e2e convention 5): read the
    # nest's own `fauna.backup.status` projection — the exact source the page reads.
    # Without this, a red below is ambiguous between "the nest never recorded the
    # upload" and "the page isn't rendering what the nest returned", and the client
    # side degrades *silently* to the "never" baseline on a failed read
    # (`unwrap_or_default()` on linux, `catch { destStatus = {} }` on web), so the
    # two failures look identical on screen.
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    status_client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=test_user["actor_id_bytes"],
        signing_key=bytes(test_user["signing_key"]),
    )
    with status_client:
        projection = status_client.call("fauna.backup.status", {})
    assert projection.get("enrolled") is True, (
        f"the nest does not consider this owner enrolled after the UI enroll: {projection!r}"
    )
    nest_rows = {r["destination_id"]: r for r in projection.get("destinations", [])}
    assert nest_rows, f"the nest projects no destination rows after the enroll: {projection!r}"
    nest_row = next(iter(nest_rows.values()))
    nest_last_upload = nest_row["last_upload_time"]
    # The segment really moved: an owner whose backlog drained to 0 had its
    # `diff.to_upload` loop run, which is the only caller of `record_custody`
    # (`segment_backup.rs:395`) and the only way a segment reaches the destination.
    assert nest_row["backlog_count"] == 0, (
        f"the sweep reported {payload!r} but the nest still projects "
        f"backlog_count={nest_row['backlog_count']!r} (was {backlog_before} before) — "
        f"the seeded segment was not uploaded, so any timestamp below reflects only "
        f"the manifest-mirror write."
    )
    assert nest_last_upload, (
        f"NEST-SIDE failure: the sweep reported {payload!r} but "
        f"fauna.backup.status still projects last_upload_time={nest_last_upload!r} "
        f"({projection!r}). `max_manifest_synced_at_for_dest` maxes over "
        f"segment_backup_manifest_state, which only `put_segment_backup_manifest_state` "
        f"writes — reached only when a segment was actually uploaded."
    )

    app.backups.navigate()
    app.backups.wait_for_destination_count(1)

    # Generous named budget + deadline poll (convention 14): the upload is already
    # done, so a green run pays only the client's own re-read latency.
    STATUS_RENDER_BUDGET_S = 30
    deadline = time.monotonic() + STATUS_RENDER_BUDGET_S
    after = before
    while time.monotonic() < deadline:
        after = app.driver.get_text(
            "backup-destination-last-upload-time",
            scope="backup-destination-status-row[0]",
        )
        if after.strip() != _NEVER_UPLOAD_TEXT:
            break
        time.sleep(0.5)

    assert after.strip() != _NEVER_UPLOAD_TEXT, (
        f"CLIENT-SIDE failure: the nest projects last_upload_time="
        f"{nest_last_upload!r} for this destination, but the row still reads "
        f"{after!r} after {STATUS_RENDER_BUDGET_S}s. The nest-side asserts above "
        f"already passed, so the upload happened and the projection is correct — "
        f"this client is either not re-reading fauna.backup.status on re-mount or "
        f"is swallowing the read failure into the 'never' baseline. "
        f"error={app.error_text()!r}"
    )

    # THE BACKLOG HALF, on the app's own row. `test_backup_destination_crud`
    # asserts `backup-destination-backlog-count` is *present*; the pre-state above
    # asserts it reports a real queue. Neither watches the value MOVE, and an
    # element wired to a constant — or to a client-side count that no pass ever
    # touches — satisfies both. This is the assertion only a drained queue can
    # satisfy, and it is the app's row saying so, not the projection: the
    # nest-side `backlog_count == 0` above is the split verdict (convention 5),
    # so a red here is unambiguously client-side.
    BACKLOG_RENDER_BUDGET_S = 30
    deadline = time.monotonic() + BACKLOG_RENDER_BUDGET_S
    backlog_after = backlog_before
    while time.monotonic() < deadline:
        backlog_after = _backlog_count(app.driver.get_text(
            "backup-destination-backlog-count",
            scope="backup-destination-status-row[0]",
        ))
        if backlog_after == 0:
            break
        time.sleep(0.5)
    assert backlog_after == 0, (
        f"CLIENT-SIDE failure: the nest projects backlog_count=0 for this "
        f"destination after the sweep, but the row still reads "
        f"{backlog_after} queued (was {backlog_before} before) after "
        f"{BACKLOG_RENDER_BUDGET_S}s. The owner is being told segments are still "
        f"waiting to reach a destination that already holds them — the page's one "
        f"job is to say whether their data got there. "
        f"error={app.error_text()!r}"
    )

    # `nest_instance`/`test_user` are session-scoped and `test_backup_destination_crud`
    # asserts a zero destination count, so leave the surface as we found it.
    app.backups.remove_destination(0)
    app.backups.wait_for_destination_count(0)


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("backup-destinations-and-restore")
def test_backup_audit_pass_advances_the_last_checked_row(
    logged_in_app, second_nest, test_user
):
    """A real client-side audit pass runs against a real destination and renders.

    `docs/goal/ui/backups.md` § Audit-alert surface owns the two elements. Before
    this slice neither rendered on any client, and the loop's verdicts had only
    tier_1 coverage — nothing proved a client ever *ran* a pass against a live
    destination, nor that the result reached the page.

    Flow under test::

        enroll via UI  → backup_enroll.rs (seal grant, writer grant, register)
        run one pass   → the CLIENT opens its own authenticated session to the
                         DESTINATION (never through the source nest), calls
                         fauna.backup.custody.list, reaches a verdict, and
                         persists it under the account's backup state dir
        render         → "Last checked: never" advances, and no banner shows

    **What makes the assertion load-bearing.** The row can only advance on a
    `Passed` verdict, and `audit_destination` only reaches one after
    `custody_list()` returns `Ok` — an unreachable or unauthorized destination
    stays `Unreachable`, which deliberately never advances the last-passed clock.
    So the advance is evidence the whole chain ran: a second-origin authenticated
    session against a nest this client has never talked to on this page, the
    destination-side kind, the verdict, the store write, and the render. Mutation
    check: reverting the enroll (or pointing the connector at the source nest)
    leaves the row at "never".

    linux led these elements; **tui lifted them 2026-07-29** over the same
    shared `run_audit_pass` in direct Rust, and **web 2026-07-30** over the wasm
    twins of the same three seams (`wasm_backup_destination_connector` /
    `wasm_backup_inclusion_source` / a `localStorage` `AuditStateStore`). Four
    shells still owe them (windows, macos/ios, android), and their first step is
    the FFI face the audit pass has nowhere yet — `backups.md` § Audit-alert
    surface, *Implementation status*.

    **Not covered here — the alerting arm, and why.** Making a *freshness* failure
    happen needs the destination's high-water to lag the client's own by more than
    `FRESHNESS_SLACK` (48 h). `evaluate_freshness` floors that high-water at
    `BackupDestination.added_at`, which shared Rust stamps from the real system
    clock at enroll (`fauna_client_config::backup_enroll`), so a destination
    enrolled seconds ago is *correctly* never stale, and the client-side clock
    poke this test could use shifts both sides of the comparison equally. The
    `Overdue` arm is reachable — a destination that cannot be confirmed for over a
    week — but needs a destination the test can take offline mid-run, i.e. a
    test-scoped nest stopped through the bridge API rather than the session-scoped
    `second_nest` other tests depend on. Both are ordinary testable mechanisms,
    not "needs a human": the banner's own render path is covered by
    `render_alerts` + the mutation-verified `alert_reason`/`merge_outcomes` tier_1
    tests, and the missing piece was fixture reach —
    `test_backup_audit_alerts_when_the_destination_goes_dark` below closed it
    with the `stoppable_nest` fixture.
    """
    # The owner must be authorized on the destination for the enroll handshake.
    # Idempotent across re-runs of the session-scoped nest.
    try:
        register_user(
            second_nest["port"],
            test_user["actor_id_hex"],
            admin_signing_key=second_nest["admin"]["signing_key"],
        )
    except Exception:
        pass

    app = logged_in_app
    app.backups.navigate()
    # The baseline is the ABSENCE of the row, not a "never" reading in it: enroll
    # itself triggers a pass (`apply` → `refresh_audit`), so "Last checked: never"
    # is real but transient and racing it would be exactly the wall-clock
    # dependence convention 14 forbids. A row that does not exist cannot be
    # showing a stale timestamp, which is the property the assertion needs.
    app.backups.wait_for_destination_count(0)
    assert app.driver.count("backup-destination-last-audit-time") == 0, (
        "no destination is configured, so no audit row may exist yet"
    )

    app.backups.add_destination(second_nest["url"], name="Audit-proof")
    app.backups.wait_for_destination_count(1)

    # Generous named budget + deadline poll: a green run pays only the real
    # connect + handshake + custody read against the destination.
    AUDIT_RENDER_BUDGET_S = 60
    deadline = time.monotonic() + AUDIT_RENDER_BUDGET_S
    after = _NEVER_AUDIT_TEXT
    while time.monotonic() < deadline:
        after = app.driver.get_text(
            "backup-destination-last-audit-time",
            scope="backup-destination-status-row[0]",
        )
        if after.strip() != _NEVER_AUDIT_TEXT:
            break
        time.sleep(0.5)

    assert after.strip() != _NEVER_AUDIT_TEXT, (
        f"the audit never passed: the row still reads {after!r} after "
        f"{AUDIT_RENDER_BUDGET_S}s. A pass requires this client's OWN authenticated "
        f"session to the destination AND a successful fauna.backup.custody.list — an "
        f"unreachable or unauthorized destination stays Unreachable, which by design "
        f"never advances this clock. error={app.error_text()!r}"
    )
    assert app.driver.count("backup-audit-alert") == 0, (
        f"a passing audit must raise no alert, got "
        f"{app.driver.count('backup-audit-alert')}. error={app.error_text()!r}"
    )

    # Drive a second pass explicitly, past the 24 h `AUDIT_MIN_INTERVAL` so it is
    # not debounced away. Two things ride on this: the agent command actually
    # reaches the page's production audit path (it fails loudly on the page's
    # `error-message` if the page is not built — convention 11), and a healthy
    # destination stays healthy across passes rather than alerting on the second.
    app.driver.call_command(
        "backup_audit_run_now", {"now_offset_secs": 25 * 60 * 60}, timeout=60
    )
    assert app.error_text() == "", (
        f"the audit re-run command was not honoured: {app.error_text()!r}"
    )
    assert app.driver.count("backup-audit-alert") == 0, (
        f"a second passing audit must still raise no alert, got "
        f"{app.driver.count('backup-audit-alert')}. error={app.error_text()!r}"
    )
    still = app.driver.get_text(
        "backup-destination-last-audit-time",
        scope="backup-destination-status-row[0]",
    )
    assert still.strip() != _NEVER_AUDIT_TEXT, (
        f"the second pass must not reset the last-passed clock: {still!r}"
    )

    # `second_nest`/`test_user` are session-scoped and `test_backup_destination_crud`
    # asserts a zero destination count, so leave the surface as we found it.
    app.backups.remove_destination(0)
    app.backups.wait_for_destination_count(0)
    # The audit clock offset is a process-wide static that nothing resets, so the
    # 25 h set above would otherwise be inherited by the next audit test and
    # silently subtract from *its* elapsed time. Put it back.
    app.driver.call_command("backup_audit_run_now", {"now_offset_secs": 0}, timeout=60)


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("backup-destinations-and-restore")
def test_backup_audit_alerts_when_the_destination_goes_dark(
    logged_in_app, test_user, stoppable_nest
):
    """The **alerting** arm end to end: a real verdict reaches a rendered banner.

    Companion to `test_backup_audit_pass_advances_the_last_checked_row`, which
    covers only the passing arm. Until this landed, nothing proved a client ever
    rendered `backup-audit-alert` from a verdict its own audit reached — the
    banner's existence rested on tier_1 coverage of `alert_reason` plus the
    render code being read rather than run.

    Flow under test::

        enroll + pass   → the row advances off "never" (a REAL pass: the
                          destination is up, authorizes this owner, and answers
                          fauna.backup.custody.list)
        destination dies→ stoppable_nest.stop() — a causal barrier, not a sleep
        re-run at +8 d  → the connect fails, escalate_unreachable consults
                          evaluate_overdue, and 8 d > AUDIT_OVERDUE (7 d) turns a
                          quiet Unreachable into a loud Overdue
        render          → a backup-audit-alert banner appears

    **Why `Overdue` and not freshness.** `evaluate_freshness` floors the
    destination's high-water at `BackupDestination.added_at`, stamped from the
    real system clock in shared Rust at enroll, so shifting the client's `now`
    moves *both* sides of that comparison and cannot manufacture lag; faking the
    observation instead would be faking the finding. Overdue is honestly
    reachable because it is measured against the client's own last-passed clock,
    which this test establishes for real in step 1.

    **What makes it load-bearing rather than a banner-count assertion.** The
    baseline is a *passed* audit against a live destination, so the alert cannot
    come from a destination that was never reachable. And the pass→alert
    transition is driven entirely by killing the real nest: with the `stop()`
    removed the destination stays up, the re-run passes again, and no banner
    appears. Nothing about the finding is injectable — only the clock is.

    No wall-clock dependence: every wait is a deadline poll on state, and the
    only time value is an explicit offset handed to the audit (convention 14).
    """
    # The owner must be authorized on the destination for the enroll handshake.
    register_user(
        stoppable_nest["port"],
        test_user["actor_id_hex"],
        admin_signing_key=stoppable_nest["admin"]["signing_key"],
    )

    app = logged_in_app
    app.backups.navigate()

    # Zero the audit clock offset FIRST. It is a process-wide static that the
    # `backup_audit_run_now` command sets and nothing resets, so a sibling audit
    # test earlier in the session leaves its own offset behind — and `added_at` /
    # `last_passed_at` are stamped through that same shifted clock at enroll.
    # Without this the elapsed time below is (my offset − the previous test's),
    # which is how this test first failed: a leftover 25 h offset turned the
    # intended 8 d into 167 h and missed AUDIT_OVERDUE (168 h) by one hour.
    # Resetting makes the assertion depend only on this test's own actions.
    app.driver.call_command("backup_audit_run_now", {"now_offset_secs": 0}, timeout=60)
    app.backups.wait_for_destination_count(0)
    assert app.driver.count("backup-audit-alert") == 0, (
        "no destination is configured, so no alert may stand"
    )

    app.backups.add_destination(stoppable_nest["url"], name="Going-dark")
    app.backups.wait_for_destination_count(1)

    # Step 1 — establish a REAL passed audit, so the alert below cannot be
    # explained by a destination that was never reachable in the first place.
    AUDIT_RENDER_BUDGET_S = 60
    deadline = time.monotonic() + AUDIT_RENDER_BUDGET_S
    passed_text = _NEVER_AUDIT_TEXT
    while time.monotonic() < deadline:
        passed_text = app.driver.get_text(
            "backup-destination-last-audit-time",
            scope="backup-destination-status-row[0]",
        )
        if passed_text.strip() != _NEVER_AUDIT_TEXT:
            break
        time.sleep(0.5)
    assert passed_text.strip() != _NEVER_AUDIT_TEXT, (
        f"the baseline audit never passed against a live destination — the row "
        f"still reads {passed_text!r} after {AUDIT_RENDER_BUDGET_S}s, so this test "
        f"cannot distinguish 'went dark' from 'was never up'. "
        f"error={app.error_text()!r}"
    )
    assert app.driver.count("backup-audit-alert") == 0, (
        f"a passing audit must raise no alert, got "
        f"{app.driver.count('backup-audit-alert')}. error={app.error_text()!r}"
    )

    # Step 2 — the destination goes dark. `stop()` returns only once the process
    # is gone, so this is a barrier: everything after it is causally downstream.
    stoppable_nest["stop"]()

    # Step 3 — one pass, eight days on. Past AUDIT_MIN_INTERVAL (24 h) so it is
    # not debounced, and past AUDIT_OVERDUE (7 d) since the pass in step 1, so
    # the unreachable read escalates instead of staying quiet.
    app.driver.call_command(
        "backup_audit_run_now", {"now_offset_secs": 8 * 24 * 60 * 60}, timeout=120
    )
    assert app.error_text() == "", (
        f"the audit re-run command was not honoured: {app.error_text()!r}"
    )

    ALERT_RENDER_BUDGET_S = 60
    deadline = time.monotonic() + ALERT_RENDER_BUDGET_S
    alerts = 0
    while time.monotonic() < deadline:
        alerts = app.driver.count("backup-audit-alert")
        if alerts >= 1:
            break
        time.sleep(0.5)

    assert alerts >= 1, (
        f"the destination has been dead and unconfirmable for 8 simulated days — "
        f"past AUDIT_OVERDUE (7 d) — but no backup-audit-alert rendered. Either the "
        f"verdict never escalated from Unreachable to Overdue (escalate_unreachable "
        f"→ evaluate_overdue), or the banner is not wired to "
        f"DestinationAuditRecord::alert_reason(). last-checked row reads "
        f"{app.driver.get_text('backup-destination-last-audit-time', scope='backup-destination-status-row[0]')!r}, "
        f"error={app.error_text()!r}"
    )

    # Leave the surface as we found it — `test_user` is session-scoped and
    # `test_backup_destination_crud` asserts a zero destination count.
    app.backups.remove_destination(0)
    app.backups.wait_for_destination_count(0)
    # Removing the destination must also clear its banner (`merge_outcomes` rule
    # 2: an alarm the user cannot dismiss is noise).
    assert app.driver.count("backup-audit-alert") == 0, (
        f"removing the destination must clear its alert, got "
        f"{app.driver.count('backup-audit-alert')}"
    )
    # Put the shared clock back, so this test leaks no offset into the next one.
    app.driver.call_command("backup_audit_run_now", {"now_offset_secs": 0}, timeout=60)


@pytest.mark.windows
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.linux
# ios joined 2026-08-09 with the Backups machine adoption. The marker was held
# back since 2026-06-29 because iOS's page could render the modal but not
# complete the dispatch round-trip this test asserts. Both apple targets now
# drive the SAME shared `BackupsMachineVM` over a configured `BackupsMachine`,
# and the confirm button's enable flag is the machine's own
# `immediate_delete_enabled` predicate rather than a per-app copy — so the
# friction-bar invariant and the hard-floor-breach round-trip are one
# implementation on both.
@pytest.mark.ios
# tui joined 2026-08-21 (the tui-excluding-marker audit): the friction-bar
# modal, its two buffers, and the owner-only delete_immediate dispatch are all
# implemented (apps/fauna-tui/src/backups.rs) and `snapshot_row_id` already
# special-cases tui alongside linux (actions/backups.py — both stamp the id as
# a snapshot-item test-attr). Nothing was missing; the marker set had simply
# stopped growing.
@pytest.mark.tui
@pytest.mark.feature("snapshots")
def test_snapshot_immediate_delete(
    logged_in_app, fresh_backup_set
):
    """Immediate-delete modal: the friction-bar invariant + the dispatch round-trip.

    The behavioural invariant (backups.md § User actions, line 309): NEVER a
    one-click affordance — the confirm button enables ONLY when BOTH the re-typed
    snapshot id AND the exact acknowledge phrase match. We verify that, then that
    confirm dispatches the owner-only `fauna.filesync.snapshot.delete_immediate`
    over the new shared seam end-to-end: this folder is below the nest hard floor
    (`count_active > 3`), so the nest rejects with `hard_floor_breach`, the error
    surfaces (state protocol), and the modal stays open with no row removed —
    proving the client→nest→client round-trip.

    The happy-path *removal* is not e2e-reachable here: a snapshot delete needs >3
    DISTINCT active snapshots, and snapshots of an unchanged/empty folder dedupe
    by content (the same reason the soft-delete e2e never actually deletes). That
    path is unit-covered — `BackupsViewModel` `ConfirmImmediateDelete` (closes +
    reloads on success) + the nest `delete_immediate_handler` tests + the shared
    `fauna-client-snapshots` wire-contract test. windows leads this cross-app
    surface; web lifts it over the wasm twin (`snapshotDeleteImmediate` +
    `immediateDeleteAckText`); macOS via FaunaKit's shared
    `SnapshotImmediateDeleteModal` (the friction bar + a new page-level
    `error-message` ErrorBanner the hard-floor rejection writes to). linux/android
    still owe the lift (priority #1/#2); iOS render-landed the modal but its
    `SnapshotListView` VM isn't `configure(api:)`'d, so the dispatch round-trip
    isn't e2e-reachable (unit/shared-covered) — the marker grows one app at a
    time.
    """
    folder, snapshot_id = fresh_backup_set
    app = logged_in_app
    app.backups.navigate()
    app.backups.select_folder(folder)
    # Identity barrier, not a count — see `wait_for_snapshot_row`. A bare count
    # is satisfied by the previously selected set's still-mounted rows, and the
    # friction bar would then be driven against ANOTHER set's snapshot: the
    # assertions still pass (the id is read back off the same row), so the
    # contamination is silent.
    row = app.backups.wait_for_snapshot_row(snapshot_id)

    # Read the id back off the row we are about to target. This is the per-app
    # `snapshot_row_id` exposure contract (web data-snapshot-id / apple automation
    # value / windows row text / linux test-attr), so assert it agrees with the id
    # the nest handed us at seed time rather than trusting either alone.
    target_id = app.backups.snapshot_row_id(row)
    assert target_id == snapshot_id, (
        f"row {row} should expose the seeded snapshot id "
        f"{snapshot_id}, got {target_id}"
    )
    app.backups.open_immediate_delete(row)
    try:
        # Behavioural invariant: never a one-click affordance — confirm stays disabled
        # until BOTH inputs match exactly.
        assert not app.backups.is_immediate_delete_confirm_enabled(), (
            "confirm must start disabled (no input typed): "
            f"{app.driver.diagnose('immediate-delete-confirm-button')}"
        )
        app.backups.type_immediate_delete_confirm(str(target_id))
        assert not app.backups.is_immediate_delete_confirm_enabled(), (
            "the snapshot id alone must not enable confirm — the acknowledge phrase is required too"
        )
        app.backups.type_immediate_delete_acknowledge(app.backups.IMMEDIATE_DELETE_ACK_TEXT)

        # Both match → confirm enables (the recompute rides the client's reactive
        # binding — windows TextChanged→VM→bind, web the $derived friction bar).
        deadline = time.time() + 5
        while time.time() < deadline and not app.backups.is_immediate_delete_confirm_enabled():
            time.sleep(0.3)
        assert app.backups.is_immediate_delete_confirm_enabled(), (
            "confirm should enable once both inputs match exactly: "
            f"{app.driver.diagnose('immediate-delete-confirm-button')} error={app.error_text()!r}"
        )

        # Dispatch: confirm → the owner-only delete_immediate RPC reaches the nest,
        # which rejects (below the >3-active hard floor). The error surfaces (state
        # protocol; the dialog covers the page ErrorBar), proving the round-trip.
        app.backups.confirm_immediate_delete()
        deadline = time.time() + 10
        while time.time() < deadline and not app.error_text():
            time.sleep(0.5)
        assert app.error_text(), (
            "confirm should dispatch delete_immediate and surface the nest's "
            "hard-floor rejection (proving the round-trip)"
        )
        # The rejection left the modal open for a retry (no Hide on the error path) —
        # which also means no snapshot was removed (the nest declined the delete).
        assert app.backups.is_immediate_delete_modal_visible(), (
            "modal stays open after a rejected confirm"
        )
    finally:
        # Close the modal this test opened. The assertion directly above is that it
        # STAYS open on the error path, so nothing in the flow ever dismisses it —
        # and the `app` fixture is session-scoped, with a `reset()` that is not
        # guaranteed to dismiss a native modal (conftest's `app` documents the
        # "previous test left an unclosed modal dialog" case explicitly). A dialog
        # left up therefore covers the Backups page for the NEXT test in this module:
        # that is precisely what broke
        # `test_snapshot_file_download_button_downloads_sealed_bytes` — zero
        # `snapshot-file-download-button` rows, reproducible with just these two
        # tests in one command, green with either one alone.
        # `finally`, not a trailing call: a failure mid-ceremony must not cascade
        # into the next test as a second, misleading red.
        app.backups.cancel_immediate_delete()


@pytest.mark.web
@pytest.mark.windows
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
# tui joined 2026-08-21 (the tui-excluding-marker audit): the same journey
# as `test_backups_download.py::test_download_single_file_bytes_roundtrip`
# (already tui-marked and green) — `snapshot-file-download-button`,
# `download_dir()`, and every action-layer method this test drives are generic
# across web/native, and `drivers/tui.py::download_dir` mirrors linux's.
@pytest.mark.tui
@pytest.mark.feature("snapshots")
def test_snapshot_file_download_button_downloads_sealed_bytes(
    logged_in_app, nest_instance, test_user
):
    """`snapshot-file-download-button` downloads the REAL sealed bytes of a
    snapshot file — the browser leg could not close (`docs/goal/ui/backups.md` § Where logic lives -> Single-file byte
    download). `_seed_folder_with_snapshot`/`fresh_backup_set` seed a snapshot
    with ZERO files (an admin-only `create_folder` call on an empty set), so
    the button never renders there — this seeds a snapshot containing ONE real
    file, uploaded through the actual client-side walk (the shared engine's
    `SyncEngine::upload_file`: hash -> compress -> seal -> POST chunks +
    manifest -> signed record), which is the only way to produce a genuinely
    SEALED file (a plaintext-only file would green on the legacy server-route
    too and prove nothing — see the LEAD's own note). The plaintext arm of the
    same journey is `test_backups_download.py` (raw seeded chunks,
    `stored_hashes: None`); together they cover both branches of the walk's seal
    discriminator.

    The upload is the harness's signed writer (`helpers.harness_writer`) — not
    the app's own bound folder, because this journey runs on web and ios, which
    bind no local folder — driven as the SAME `test_user` identity
    `logged_in_app` is logged in as (`test_user["signing_key"].encode().hex()`),
    so the file's owner-only sealed chunks are readable under the session the
    client already holds. It used to be a `fauna-sync run` daemon, whose
    unsigned records the nest refuses.
    """
    secret_key = test_user["signing_key"].encode().hex()
    folder = f"download-proof-{secrets.token_hex(4)}"
    user_create_folder(
        nest_instance["port"], folder,
        secret_key=secret_key,
        base_url=nest_instance["url"],
    )
    writer = HarnessWriter(
        nest_instance["url"], secret_key,
        user_folder_ref(
            nest_instance["port"], folder, secret_key=secret_key,
            base_url=nest_instance["url"],
        ),
        tempfile.mkdtemp(prefix="fauna-snapshot-download-"),
    )

    file_content = f"sealed download proof {secrets.token_hex(16)}\n".encode()
    file_name = "proof.txt"
    writer.write(file_name, file_content)
    # One signed seat pass: it returns once the record is on the nest, so the
    # snapshot below is taken after the file exists — no poll (convention 14).
    writer.sync(expect=[file_name])

    snap = create_folder_snapshot(
        nest_instance["port"], folder, secret_key=secret_key,
        base_url=nest_instance["url"],
    )
    assert snap is not None and snap.get("file_count", 0) >= 1, (
        f"the writer recorded {file_name!r}, so the snapshot should capture it: {snap!r}"
    )

    # ── Drive the client-side download through the real button ──
    logged_in_app.backups.navigate()
    logged_in_app.backups.select_folder(folder)
    # Wait for THIS snapshot by id, not for "some row exists" — the set just
    # selected renders behind the previously selected set's still-mounted rows,
    # so a bare count opens the wrong set's (empty) snapshot. See
    # `wait_for_snapshot_row`.
    row = logged_in_app.backups.wait_for_snapshot_row(snap["id"])
    logged_in_app.backups.open_snapshot(index=row)
    assert logged_in_app.backups.is_snapshot_detail_visible(), (
        f"snapshot detail should render the file list: "
        f"{logged_in_app.driver.diagnose('snapshot-detail-files')}"
    )
    # A bare count IS sufficient here: this test opens exactly ONE snapshot after
    # a fresh `navigate()`, so no earlier snapshot's file rows can be mounted to
    # satisfy the threshold (the stale-collection hazard `wait_for_snapshot_row`
    # guards needs a *previous* open of a different collection).
    assert logged_in_app.backups.wait_for_snapshot_files(min_count=1) >= 1, (
        "the seeded snapshot has a real file — snapshot-file-download-button "
        f"should render: {logged_in_app.driver.diagnose('snapshot-file-download-button')}"
    )

    downloaded = logged_in_app.backups.download_file_and_read(0, file_name)
    assert downloaded == file_content, (
        f"downloaded bytes should match the uploaded (sealed) file content: "
        f"expected {file_content!r}, got {downloaded!r}"
    )


def _inbox_count(nest, owner) -> int:
    """How many INBOX messages `owner` has on `nest`, read over the owner's own
    authenticated WS-RPC (convention 5: the nest-side half of a restore verdict,
    independent of what any page renders)."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    client = WsRpcAdminClient(
        nest["url"],
        actor_id=owner["actor_id_bytes"],
        signing_key=bytes(owner["signing_key"]),
    )
    with client:
        reply = client.call("fauna.email.inbox.fetch", {"after_uid": 0, "limit": 50})
    return len(reply.get("messages", []))


def _cover_a_folder_on_this_device(app, nest, owner, mail_only) -> tuple[str, dict]:
    """One ordinary folder holding one signed file, attached to this device's
    custodian destination through the folder's own row, and held by it: a pass
    is poked until the device holds more than ``mail_only`` (its mail-only
    report). Returns the folder's display name and that pass's report.

    The file is the harness's signed writer (`helpers.harness_writer`), not a
    bound location: this journey also runs on ios, which binds no local folder.
    The attach is the user's gesture, so it goes through the app (convention 8)."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    secret_key = bytes(owner["signing_key"]).hex()
    name = f"restored-{secrets.token_hex(4)}"
    user_create_folder(nest["port"], name, secret_key=secret_key, base_url=nest["url"])
    writer = HarnessWriter(
        nest["url"], secret_key,
        user_folder_ref(nest["port"], name, secret_key=secret_key, base_url=nest["url"]),
        tempfile.mkdtemp(prefix="fauna-reseed-folder-"),
    )
    writer.write("kept.txt", f"restored with the box {secrets.token_hex(8)}\n".encode())
    # One signed seat pass: it returns once the record is on the nest.
    writer.sync(expect=["kept.txt"])

    with WsRpcAdminClient(
        nest["url"],
        actor_id=owner["actor_id_bytes"],
        signing_key=bytes(owner["signing_key"]),
    ) as client:
        listed = client.call("fauna.backup.destination.list", {})["destinations"]
    assert len(listed) == 1, f"one destination, this device's: {listed!r}"
    destination_id = listed[0]["destination_id"]

    b = app.backups
    b.navigate_folders()
    b.find_and_expand_folder(name)
    app.driver.wait_for("folder-destination-attach-select", timeout=15.0)
    app.driver.select("folder-destination-attach-select", destination_id)
    app.driver.click("folder-destination-attach-button")
    app.driver.wait_for("folder-destination-row", timeout=15.0)

    # The folder axis runs before the kinds' check-ins, so a pass that mirrored
    # the folder reports its bytes in `held_bytes` (`custodian_pull.rs`
    # `run_all_kinds`). Each poll RUNS a pass (convention 14).
    reports: list[dict] = []

    def held_more():
        reports.append(b.custodian_pull_run_now())
        return reports[-1].get("held_bytes", 0) > mail_only["held_bytes"]

    wait_until(held_more, 120.0, interval=2.0,
               diagnose=lambda: f"the device never held the covered folder: "
                                f"mail-only {mail_only!r}, last {reports[-1:]!r}")
    b.navigate()
    return name, reports[-1]


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.windows
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.android
# web is a declared absence (a browser cannot hold the sealed store), so it is
# never marked; android, still owed its shell, skips through
# `require_reseed_supported`.
@pytest.mark.real_sync_agent
# windows only: its agent is a separate process, so this pins it to the run's own
# pipe rather than the box's installed one — the orphaned-store test's pair.
@pytest.mark.isolated_sync_agent
@pytest.mark.feature("backup-destinations-and-restore")
@pytest.mark.parametrize("with_folder", [
    pytest.param(False, id="mail"),
    pytest.param(True, id="mail-and-folder"),
])
def test_after_losing_the_nest_the_devices_copy_restores_the_mail_onto_the_rebuilt_one(
    app, request, nest_mode, tmp_path_factory, with_folder
):
    """**After losing the nest, the owner restores from the copy one of their own
    devices holds, from the app** (`backup-destinations-and-restore` outcome 12;
    `ui/backups.md` § Restore after losing the nest; the ceremony
    `behavior/backup-destinations.md` § Re-seed).

    The box is really lost: its process is killed and the rebuilt box starts from
    a brand-new data directory, carrying nothing across but the deployment seed
    (`nest/box-recovery.md`), so no nest-alive restore path can satisfy it.

    Flow under test::

        box A claimed by the owner  -> one mail message in the owner's INBOX
        enroll this device (UI)     -> custodian row; one poked pull pass holds it
        attach a folder to it (UI)  -> one signed file in an ordinary folder; a
                                       poked pass holds its mirror too
        kill A; provision B         -> same FAUNA_DEPLOYMENT_SEED, empty /data
        owner re-claims B           -> same identity, INBOX empty (pre-state)
        the app signs in to B       -> the list survives -> this device's own row
                                       carries the gesture (the heal re-registers
                                       it at B; its pull refuses, never tombstones)
        press "Restore my data..."  -> agent job: grant, re-seal + deliver,
                                       materialize (the shared driver)
        result view                 -> the driver's whole verdict
        INBOX on B                  -> the message is back
        the folder on B             -> listed under its name, its file live — the
                                       app pre-created the set, the device signed
                                       every re-homed row (ruling (7)(a))
        destination list            -> this device re-enrolled (post-ceremony duty)

    Latency-independent (convention 14): every wait is a deadline poll on the
    state it waits for (the pass poke's own reply, the result view reaching a
    verdict, the destination row appearing), never a settle-sleep.

    tui led; linux followed over the same two shared calls; macOS (through the
    agent) and iOS (in-process) followed over the shared FaunaKit view; windows
    followed through the same agent FFI face as macOS. android skips as unbuilt
    until its shell lands. web is a declared absence (a browser cannot hold the
    sealed store).
    """
    from common.auth import claim_admin
    from conftest import _login_app_as, _start_dedicated_nest

    # ── Box A: the owner's nest, with mail and a device holding a copy ─────────
    box_a, cleanup_a = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "reseed-box-a")
    request.addfinalizer(cleanup_a)
    owner = box_a["admin"]
    seed_hex = owner["deployment_seed"]
    assert seed_hex, "the claim reply must carry the deployment seed the rebuild needs"

    _login_app_as(app, request, box_a, owner)
    app.backups.require_destination_management_supported()
    app.backups.require_reseed_supported()
    _seed_one_mail_segment(app, box_a, owner)
    assert _inbox_count(box_a, owner) == 1

    app.backups.navigate()
    app.backups.wait_for_destination_count(0)
    app.backups.add_custodian_destination(name="This device", capacity="50 GB")
    app.backups.wait_for_destination_count(1)
    mail_only = app.backups.wait_for_custodian_hosting()
    assert mail_only["held_bytes"] > 0, (
        f"the device must hold the mail before the box is lost, or the rebuilt "
        f"box has nothing to come back from: {mail_only!r}"
    )
    folder, first = (
        _cover_a_folder_on_this_device(app, box_a, owner, mail_only)
        if with_folder else (None, mail_only)
    )

    # ── Total loss, and the rebuild ──────────────────────────────────────────
    box_a["proc"].kill()
    box_a["proc"].wait()
    box_b, cleanup_b = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "reseed-box-b", unclaimed=True,
        extra_env={"FAUNA_DEPLOYMENT_SEED": seed_hex})
    request.addfinalizer(cleanup_b)
    assert box_b["nest_id"] == box_a["nest_id"], "the rebuild re-presents the same identity"
    with open(os.path.join(box_b["tmp_dir"], "claim-code")) as f:
        claim_code = f.read().strip()
    claim_admin(box_b["port"], claim_code, base_url=box_b["url"],
                handle=owner.get("handle") or "admin", signing_key=owner["signing_key"])
    assert _inbox_count(box_b, owner) == 0, "pre-state: the rebuilt box holds no mail"

    # The device keeps its copy across the app's relaunch onto the new address.
    assert app.driver.preserve_state_across_relaunch()
    _login_app_as(app, request, box_b, owner)

    # The destination list is an account-plane row keyed on the box's identity,
    # which the rebuild re-presents, so the list survives the loss
    # (`behavior/backup-destinations.md` § Destination data model → *A rebuilt
    # box keeps its list too*): this device's own custodian row carries the
    # gesture, and nothing reads orphaned.
    app.backups.navigate()
    app.backups.wait_for_destination_count(1)
    wait_until(lambda: app.backups.reseed_button_on_row_visible(0), 60.0,
               diagnose=lambda: f"this device's custodian row must carry the re-seed "
                                f"gesture on the rebuilt box; error={app.error_text()!r}")
    assert not app.backups.orphaned_store_visible(), (
        "the list survives the box, so this device's copy is claimed by its own "
        "row and must not read orphaned"
    )

    # The heal re-registered this device at the empty rebuilt box; its pull must
    # REFUSE a source below its copy, never tombstone it (`segment-backup-protocol.md`
    # § Implementation status today, the custodian pull's regression arm).
    # `>=`, not `==`: the refused kind keeps every byte it held, while a kind
    # the rebuilt box already serves at or above this device's copy may pull
    # new bytes in the same pass. A tombstoned mail corpus would read BELOW.
    before = app.backups.wait_for_custodian_hosting_report()
    assert before["held_bytes"] >= first["held_bytes"], (
        f"the device's copy must survive a pass against the rebuilt, empty box: "
        f"held {first['held_bytes']} before the loss, {before['held_bytes']} now "
        f"({before!r})"
    )

    # ── The gesture under test ───────────────────────────────────────────────
    app.backups.reseed_from_custodian_row(0)
    wait_until(
        lambda: app.backups.reseed_result_text() not in ("", S.backups.backup_reseed_running)
        or bool(app.error_text()),
        300.0,
        diagnose=lambda: f"the re-seed never reached a verdict; "
                         f"result={app.backups.reseed_result_text()!r} "
                         f"error={app.error_text()!r}",
    )
    result = app.backups.reseed_result_text()
    assert result.startswith(S.backups.reseed_result_whole), (
        f"the restore must report the corpus whole: result={result!r} "
        f"error={app.error_text()!r}"
    )

    # Nest-side, independent of the page (convention 5): the owner's mail scope
    # on the rebuilt box now holds live records, which is exactly what makes a
    # second materialize answer `target_not_empty` before it reads a byte.
    from clients._ws_rpc_core import RpcCallError
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    with WsRpcAdminClient(
        box_b["url"],
        actor_id=owner["actor_id_bytes"],
        signing_key=bytes(owner["signing_key"]),
    ) as client:
        with pytest.raises(RpcCallError) as refused:
            client.call("fauna.backup.custody.materialize", {"set_name": "__mail"})
    assert refused.value.code == "fauna.backup.target_not_empty", (
        f"NEST-SIDE failure: the page says {result!r}, but the rebuilt box's mail "
        f"scope does not read as live: {refused.value!r}"
    )

    # The covered folder is back under its name, its file live on the rebuilt
    # box (`writer-signed-change-records.md` ruling (7)(a)): the app created the
    # target set, the device signed every re-homed row, and the nest minted only
    # rows whose signatures verified — so a reader that admits no unsigned row
    # lists it. Nest-side first (convention 5), then the app's own list.
    if folder is not None:
        snap = create_folder_snapshot(
            box_b["port"], folder, secret_key=bytes(owner["signing_key"]).hex(),
            base_url=box_b["url"],
        )
        assert snap is not None and snap.get("file_count", 0) >= 1, (
            f"NEST-SIDE failure: the page says {result!r}, but the restored folder "
            f"{folder!r} holds no live file on the rebuilt box: {snap!r}"
        )
        app.backups.navigate_folders()
        app.backups.wait_for_folder_row(folder)
        app.backups.navigate()

    # The post-ceremony duty: this device is the rebuilt box's custodian again.
    app.backups.wait_for_destination_count(1)
    assert not app.backups.orphaned_store_visible(), (
        "a re-enrolled device's copy is claimed again, so the orphaned row retires"
    )

    # Leave nothing hosting behind for the next custodian test on this launch.
    _leave_no_custodian_behind(app)

    # LAST: the restored message is back in the mailbox it was in, because the
    # materialize adopts the placement journal with the content
    # (`behavior/backup-destinations.md` § Re-seed → *Where restored mail lands*).
    restored = _inbox_count(box_b, owner)
    assert restored == 1, (
        f"the restored mail must land back in the owner's INBOX on the rebuilt "
        f"box; INBOX holds {restored} message(s). A live record in no mailbox "
        f"means the placement journal was not restored with the content."
    )
