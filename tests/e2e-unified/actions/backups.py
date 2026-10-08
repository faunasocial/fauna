from __future__ import annotations

import re
import secrets
import time
from typing import TYPE_CHECKING

from helpers import budgets
from helpers.waiting import wait_until

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


# Wall-clock budgets for a real backend round trip (nest/agent state changing
# over WS-RPC, then the client re-rendering) or a quick local UI transition
# (a dialog/modal/wizard step becoming visible) — testing.md convention 14:
# named generous budgets + deadline polls, never a settle-sleep. Green runs
# pay nothing; sized to survive real fleet contention, not just the no-load
# case — measured 2026-08-16: a bare 20s budget reds under a ~64% memory-stall
# build queue while an unrelated app's SAME assertion passes in the same run
# (`test_last_backed_up_is_the_selected_set_s_newest_snapshot[linux]`).
# The sync agent's custodian host re-reads the destination registry every
# IDLE_RECHECK_SECS (60), so a freshly enrolled device becomes hosted within
# roughly one such tick plus a registry round trip. Ceiling, not a wait: the
# poll below returns the instant it flips.
CUSTODIAN_HOSTING_WAIT_S = 180      # agent discovers its own custodian assignment
CUSTODIAN_PASS_WAIT_S = 120         # ONE pull pass completes and checks in

# How far a poked pass must move the clock to get PAST the custodian self-audit's
# own debounce (`fauna_client_backup::audit::AUDIT_MIN_INTERVAL_SECS` = 24 h) so
# the audit actually re-runs. Not a wall-clock budget at all — nothing waits this
# long; it is convention 14's fake clock, the value handed to
# `custodian_pull_run_now(now_offset_secs=...)`. A day plus an hour rather than a
# day exactly, so the comparison is never decided by which side of a `<` a
# same-second boundary falls on.
AUDIT_DEBOUNCE_JUMP_S = 25 * 60 * 60
FOLDER_SELECTABLE_WAIT_S = 25       # folder becomes selectable in the dropdown
SNAPSHOT_LIST_WAIT_S = 40           # snapshot rows render (count threshold)
SNAPSHOT_LIST_EMPTY_WAIT_S = 40     # snapshot list drains to empty
SNAPSHOT_ROW_WAIT_S = 45            # a SPECIFIC snapshot id appears in the list
SNAPSHOT_CREATE_WAIT_S = 30         # a new snapshot row appears after create
SNAPSHOT_DETAIL_OPEN_WAIT_S = 25    # the opened snapshot's detail pane mounts
SNAPSHOT_FILES_WAIT_S = 30          # per-file rows render inside the open detail
SNAPSHOT_DOWNLOAD_WAIT_S = 40       # a downloaded file's bytes finish writing
LAST_BACKED_UP_WAIT_S = 40          # the last-backed-up label re-derives
PRUNE_PREVIEW_WAIT_S = 40           # the dry-run prune preview renders
PRUNE_PREVIEW_GONE_WAIT_S = 40      # the prune preview clears
PRUNE_UNTIL_PREVIEW_WAIT_S = 40     # retry-click until a preview stands
CHECK_RESULT_WAIT_S = 60            # the integrity check's verdict renders
RECOVERABLE_ROWS_WAIT_S = 40        # soft-deleted-row count settles
SNAPSHOT_ROW_TEXT_WAIT_S = 45       # a row's own text re-renders after a lifecycle change
RESTORE_PROGRESS_WAIT_S = 60        # restore-progress reaches its terminal state
DESTINATION_COUNT_WAIT_S = 40       # destination row count settles after add/remove
DESTINATION_DIALOG_WAIT_S = 15      # the add/edit/remove destination dialog opens
IMMEDIATE_DELETE_MODAL_WAIT_S = 15  # the immediate-delete confirm modal opens
WIZARD_STEP_WAIT_S = 20             # a wizard step's next element becomes visible
FOLDER_CREATE_WAIT_S = 30           # a new folder row appears after wizard create
PENDING_SHARES_WAIT_S = 45          # cross-user pending-share count settles


class BackupsActions:
    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def navigate(self) -> None:
        """Navigate to backups page."""
        self.driver.navigate_to("backups")

    def require_destination_management_supported(self) -> None:
        """Skip unless this app renders the Backups destination-management
        surface (add/edit/remove a destination, the per-destination status
        rows) — e2e convention 7, the platform check lives in the action
        layer, not the test body.

        Landed on linux, web, windows (native FFI consume), and tui
        (2026-07-24, `apps/fauna-tui/src/backups.rs`; its snapshot half
        followed 2026-08-05 over the shared `fauna-backups-machine`, so the
        page's former per-test snapshot-surface skip is gone).
        macOS GREEN as of 2026-06-18: the full add+edit+remove CRUD
        passes in-process — the earlier registration gap was fixed by the
        BackupSplitView layout fix, and the deeper EDIT/rename
        blocker (the row's automationValue closure captured a value-type
        snapshot at `.onAppear`, so a rename keeping the same `ForEach`
        identity never re-read the fresh name) was fixed — the
        row now reads live by stable id via `BackupDestinationsVM.label(forId:)`.
        iOS GREEN as of 2026-07-29: the SnapshotListView backups surface
        renders the same shared `BackupDestinationsView` macOS uses, and the
        full CRUD passes with no client-side fix needed — the prior "not yet
        confirmed" claim was stale, blocked only by a fleet-wide FaunaKit
        Swift compile break (a UniFFI flat-module type collision) that
        predated this test ever getting a real build to run against on iOS.
        android is the one app still owed this surface."""
        if self.driver.is_android():
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="the Backups destination-management surface",
                detail="the other 6 apps have landed it; android is the "
                       "remaining client",
                tracked="ui/backups.md § Implementation status today",
            )

    def select_folder(self, name: str, timeout: float = FOLDER_SELECTABLE_WAIT_S) -> None:
        """Select the folder named `name` so its snapshots load below.

        Per-set backups tests must pick their seeded set explicitly rather than
        relying on the machine's default-to-first-row selection: when an earlier
        module grows the session-scoped `test_user` past one folder, the
        implicit selection lands on the wrong set and the snapshot list is empty
        (tracked internally).

        **Uniform since 2026-08-05** — every app renders ONE picker under
        `backup-folder-selector`, so this is one `driver.select` by value on all
        seven. Web's N-indexed-buttons-under-one-non-indexed-id shape (and the
        branch that clicked them) died with its machine adoption; `ui/backups.md`
        § Snapshot-list shape → *Reconciliation ledger*, web row.

        Retries until the set appears — the list populates asynchronously after
        `navigate()`, and `select` raises until the value is in the model.
        """
        deadline = time.monotonic() + timeout
        last: Exception | None = None
        while time.monotonic() < deadline:
            try:
                self.driver.select("backup-folder-selector", name)
                return
            except (LookupError, RuntimeError) as e:
                # 404 / bridge error == the set is not in the dropdown model yet
                # (still loading); retry. BridgeDead is not caught — it means the
                # app crashed, which must surface.
                last = e
                time.sleep(0.5)
        raise TimeoutError(
            f"folder {name!r} not selectable on the dropdown within {timeout}s "
            f"(last answer: {last!r})"
        )

    def snapshot_count(self) -> int:
        return self.driver.count("snapshot-item")

    def wait_for_snapshots(self, min_count: int = 1, timeout: float = SNAPSHOT_LIST_WAIT_S) -> int:
        """Wait until at least `min_count` snapshot rows are listed.

        ⚠ **This is NOT a gate that the list pane is showing any particular file
        set** — it only says "some rows are rendered". The docstring used to
        claim otherwise, and that claim cost four sessions of misdiagnosis: after
        `select_folder()` the outgoing set's rows are still mounted, so the
        threshold is met by *stale* rows and the caller then acts on the wrong
        set (full story in `wait_for_snapshot_row`, testing.md convention 14).

        **After any action that switches which collection is displayed, use
        `wait_for_snapshot_row(snapshot_id)` instead.** This method is safe only
        where no switch precedes it — e.g. a plain "backups page finished
        loading and has at least one row" check, on a client whose list cannot
        already be showing another set. It currently has no callers; it is kept
        as that (narrower) primitive rather than deleted.

        Returns the observed count.
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            count = self.snapshot_count()
            if count >= min_count:
                return count
            time.sleep(0.5)
        # Self-diagnosing timeout (e2e rule 6): report visibility + final count
        # so the failure classifies itself — not visible => the list pane / set
        # never loaded; visible with count 0 => set loaded but has no snapshots.
        raise TimeoutError(
            f"Expected >= {min_count} snapshot-item rows after {timeout}s, "
            f"{self.driver.diagnose('snapshot-item')}"
        )

    def wait_for_no_snapshots(self, timeout: float = SNAPSHOT_LIST_EMPTY_WAIT_S) -> None:
        """Wait until the snapshot list is EMPTY.

        The causal barrier for "the pane has finished switching to the empty set
        I just selected" (testing.md convention 14). Unlike a `min_count`
        threshold this cannot be satisfied by the outgoing set's still-mounted
        rows — those are non-empty, which is exactly what makes zero a sound
        identity here. Use it with a set seeded WITHOUT a snapshot
        (`empty_backup_set`) before driving `snapshot-create-button`, so the
        subsequent 0 -> 1 growth is unambiguous.
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.snapshot_count() == 0:
                return
            time.sleep(0.5)
        raise TimeoutError(
            f"snapshot list never emptied within {timeout}s — the pane is still "
            f"showing another folder's rows (the selection never took, or its "
            f"fetch never landed). {self.driver.diagnose('snapshot-item')}"
        )

    def create_snapshot(self) -> None:
        # The button is disabled while any op holds the machine's single-flight
        # slot (`ui/backups.md` § Snapshot-list shape, *Create*) — and a pick's
        # own load holds it. `wait_for_no_snapshots()` cannot stand in for this
        # wait: a pick clears the outgoing rows the moment it is recorded, so
        # "zero rows" is true while the pick's load is still in flight, and a
        # click there is refused (bridge 409, a linux sweep 2026-10-06).
        self.driver.wait_until_enabled(
            "snapshot-create-button", timeout=SNAPSHOT_CREATE_WAIT_S
        )
        initial = self.snapshot_count()
        self.driver.click("snapshot-create-button")
        deadline = time.monotonic() + SNAPSHOT_CREATE_WAIT_S
        while time.monotonic() < deadline:
            if self.driver.count("snapshot-item") > initial:
                return
            time.sleep(0.5)
        # Self-diagnosing timeout (e2e rule 6): count unchanged from initial =>
        # the create button click produced no new snapshot row.
        raise TimeoutError(
            f"Snapshot did not appear after creation: initial_count={initial}, "
            f"{self.driver.diagnose('snapshot-item')}"
        )

    def delete_snapshot(self, index: int = 0) -> None:
        """Delete a snapshot by index."""
        self.driver.click("snapshot-delete-button", index=index)
        time.sleep(2)

    # --- Immediate-delete modal (backups.md § User actions; the friction bar) ---

    # The exact acknowledge string the modal requires, pinned to the protocol
    # constant fauna_protocol::filesync::IMMEDIATE_DELETE_ACK_TEXT (the shared
    # Rust ack_text_matches_protocol_const test guards it can't drift).
    IMMEDIATE_DELETE_ACK_TEXT = "I understand this is immediate and irreversible."

    def snapshot_row_id(self, index: int = 0) -> int:
        """Read the snapshot id from the row at `index`.

        The immediate-delete friction bar requires the user to re-type the exact
        snapshot id, so the test reads it back from the row it targets. The id is
        exposed differently per client:
          * web tags the per-row snapshot-immediate-delete-button with
            data-snapshot-id (the visible row text is the formatted date/size,
            not the raw id) — read it via get_attr scoped to the row;
          * apple (macOS/iOS) exposes the id as the snapshot-item row's automation
            value (the apple twin of web's data-snapshot-id) — macOS via the row's
            automationActivate `value:`, iOS via its automationValue — read the
            indexed row's value attr, scoped to the row;
          * windows takes web's shape for the same reason web has it: the id is on
            the per-row snapshot-immediate-delete-button, as the
            AutomationProperties.HelpText the FlaUI bridge's get_attr reads. It
            can NOT ride the row container the way linux/tui's does — a scoped
            FlaUI read resolves by FindAllDescendants from the scope element, and
            a container is not its own descendant;
          * linux and tui stamp the id as a test-attr on the snapshot-item row
            itself (the in-process automation bridge's twin of web's
            data-snapshot-id — linux via testid::set_test_attr, tui via
            `Element::attr`) — read it via get_attr scoped to the row.

        ⚠ There is deliberately NO text-parsing fallback for a native app. An app
        that reaches the final branch without stamping the attr fails with "could
        not parse snapshot id", which reads like a selection bug and is not one —
        it is a missing attr (tui hit exactly that on 2026-08-05, with the row
        visibly rendered and its id unreadable).

        windows used to be the one exception, parsing a trailing `Id = N` out of
        the raw `SnapshotInfo` record `ToString()` it bound into the row. Its
        machine adoption replaced that with the formatted row line the § Row
        content contract requires, so the id moved to the attr where every other
        native app already had it — the parse and its shape died together.
        """
        if self.driver.is_web() or self.driver.is_windows():
            app = "web" if self.driver.is_web() else "windows"
            val = self.driver.get_attr(
                "snapshot-immediate-delete-button",
                "snapshot-id",
                scope=f"snapshot-item[{index}]",
            )
            if val is None or not val.strip():
                raise AssertionError(
                    f"{app} snapshot row {index} has no per-row snapshot id "
                    f"(web: data-snapshot-id; windows: the button's HelpText): "
                    f"{self.driver.diagnose('snapshot-immediate-delete-button')}"
                )
            return int(val)
        if self.driver.is_linux() or self.driver.is_tui():
            app = "linux" if self.driver.is_linux() else "tui"
            val = self.driver.get_attr(
                "snapshot-item",
                "snapshot-id",
                scope=f"snapshot-item[{index}]",
            )
            if val is None or not val.strip():
                raise AssertionError(
                    f"{app} snapshot row {index} has no snapshot-id test-attr: "
                    f"{self.driver.diagnose('snapshot-item')}"
                )
            return int(val)
        if self.driver.is_macos() or self.driver.is_ios():
            # The apple snapshot-item row carries the snapshot id as its automation
            # value (FaunaKit MacSnapshotTimelineView / SnapshotListView). NOTE:
            # the apple per-row scoped value read is being verified end-to-end by
            # the macOS e2e harness (which owns the mac e2e build window); the marker is
            # added there once green (tracked internally).
            val = self.driver.get_attr(
                "snapshot-item", "value", scope=f"snapshot-item[{index}]"
            )
            if val is None or not val.strip():
                raise AssertionError(
                    f"apple snapshot row {index} has no readable snapshot id: "
                    f"{self.driver.diagnose('snapshot-item')}"
                )
            return int(val)
        text = self.driver.get_text("snapshot-item", index=index)
        m = re.search(r"Id = (\d+)", text)
        if not m:
            raise AssertionError(f"could not parse snapshot id from row text {text!r}")
        return int(m.group(1))

    def snapshot_row_text(self, index: int = 0) -> str:
        """The rendered text of the `snapshot-item` row at `index`.

        The § *Row content contract* surface: formatted `created_at`, the file
        count and the formatted total bytes, plus a non-`Active` state's
        lifecycle suffix and (once a check has run this session) the integrity
        suffix. Deliberately one read of the row's OWN text rather than a walk of
        per-part ids — the contract is about what the row visibly says, and the
        parts carry no ids of their own on any app.
        """
        return self.driver.get_text("snapshot-item", index=index)

    def wait_for_snapshot_row_text(
        self, index: int, predicate, timeout: float = SNAPSHOT_ROW_TEXT_WAIT_S
    ) -> str:
        """Deadline-poll row `index`'s text until `predicate` holds, returning it
        ("" at the deadline).

        The barrier for a LIFECYCLE transition that leaves the row in place: the
        nest's `list` keeps a deleted row for its whole recovery window, so
        neither the row count nor the row's identity moves when it goes
        deletion-pending or soft-deleted — the row's own text is the observable
        (convention 14: the surface the gesture produces, never a settle-sleep).
        """
        deadline = time.monotonic() + timeout
        while True:
            try:
                text = self.snapshot_row_text(index)
            except Exception:
                text = ""
            if predicate(text):
                return text
            if time.monotonic() >= deadline:
                return ""
            time.sleep(0.5)

    def wait_for_snapshot_row(self, snapshot_id: int, timeout: float = SNAPSHOT_ROW_WAIT_S) -> int:
        """Wait until the snapshot `snapshot_id` is listed, and return its row index.

        **Use this, not `wait_for_snapshots(min_count=…)` + `open_snapshot(0)`,
        whenever the test knows which snapshot it seeded.** A bare row COUNT is not
        a causal barrier for "the list has reloaded for the set I just selected"
        (testing.md convention 14 — assert latency-independent state). After
        `select_folder()` switches sets, the previously selected set's rows are
        still mounted while the new set's fetch is in flight, so `min_count=1` is
        satisfied *immediately by the stale rows*; the caller then opens a snapshot
        belonging to the OTHER folder. Because that other snapshot is usually one
        seeded empty, the symptom lands far away — zero
        `snapshot-file-download-button` rows, reading exactly like a rendering or
        FFI bug in the download affordance.

        This is what made the apple download tests order-dependent: green alone
        (no other set exists, so no stale row can satisfy the count), red as soon
        as any earlier test seeds a second folder with snapshots. In a combined
        `--app macos,ios` run it also crosses PARAMETRIZATIONS — `[ios]` opened the
        `[macos]` run's snapshot and downloaded that file's bytes, which reads as
        "the download directory was reused across runs" and is not (each launch
        mints a fresh `mkdtemp`; measured 2026-07-29).

        Matching on the id makes the wait independent of list ORDER too, so it
        keeps working if the newest-first ordering (`sync_storage.rs` — `list_
        snapshots` is `ORDER BY created_at DESC`) ever changes.

        ⚠ **The snapshot must be RENDERED for this to find it — seed the set so
        that it is.** This scans `snapshot_count()` rows, which counts only what
        the client put in its automation registry, and macOS renders the list in
        a virtualizing SwiftUI `List`: only the rows that fit the pane register
        (measured at **2** at the e2e window size, 2026-07-29). So waiting for an
        OLD snapshot in a set that has accumulated several is a guaranteed
        timeout on macOS even though the snapshot exists on the nest — it fails
        `rows listed: [4, 3]` while waiting for `1`, which reads like a selection
        bug and is not one. **Give the test its own folder holding the one
        snapshot it waits on** (`fresh_backup_set` in `test_backups.py`); then
        the target is newest-first, index 0, and always rendered. iOS and web do
        not clip as aggressively, so this is invisible until a macOS run.
        """
        deadline = time.monotonic() + timeout
        seen: list[int] = []
        while time.monotonic() < deadline:
            seen = []
            for i in range(self.snapshot_count()):
                try:
                    row_id = self.snapshot_row_id(i)
                except AssertionError:
                    # A row mid-render has no readable id yet; re-poll rather than
                    # fail — the deadline below is the real bound.
                    continue
                if row_id == snapshot_id:
                    return i
                seen.append(row_id)
            time.sleep(0.5)
        raise TimeoutError(
            f"snapshot {snapshot_id} never appeared in the list within {timeout}s; "
            f"rows listed: {seen}. A non-empty list of OTHER ids means the pane is "
            f"still showing a different folder's snapshots (the selection never "
            f"took, or its fetch never landed); an empty list means the set's "
            f"snapshots never loaded at all. "
            f"{self.driver.diagnose('snapshot-item')} "
            # Which set the picker shows at the timeout, so a dropped
            # `select_folder` (the machine refuses a selection while another op
            # is in flight, and the render snaps back to the first folder by
            # name) names itself instead of reading as a fetch that never
            # landed. `text` is the selected option; `options` the whole list.
            f"{self.driver.diagnose('backup-folder-selector', attrs=('options',))}"
        )

    def open_immediate_delete(self, index: int = 0) -> None:
        """Click the per-row immediate-delete button and wait for the modal."""
        self.driver.click("snapshot-immediate-delete-button", index=index)
        self.driver.wait_for("immediate-delete-confirm-input", timeout=IMMEDIATE_DELETE_MODAL_WAIT_S)

    def type_immediate_delete_confirm(self, text: str) -> None:
        self.driver.clear_and_type("immediate-delete-confirm-input", text)

    def type_immediate_delete_acknowledge(self, text: str) -> None:
        self.driver.clear_and_type("immediate-delete-acknowledge-input", text)

    def is_immediate_delete_confirm_enabled(self) -> bool:
        return self.driver.is_enabled("immediate-delete-confirm-button")

    def is_immediate_delete_modal_visible(self) -> bool:
        return self.driver.is_visible("immediate-delete-confirm-modal")

    def confirm_immediate_delete(self) -> None:
        self.driver.click("immediate-delete-confirm-button")

    def cancel_immediate_delete(self) -> None:
        self.driver.click("immediate-delete-cancel-button")

    def prune(self) -> None:
        """Click `snapshot-prune-button`.

        On an app that has adopted `fauna-backups-machine` this OPENS the
        dry-run preview and deletes nothing (`ui/backups.md` § Snapshot-list
        shape, *Prune* ruling — execute is offered only from the preview).

        ⚠ Deliberately barrier-free: the caller supplies the barrier for
        whatever it is about to assert. This used to `time.sleep(2)`, which
        testing.md convention 14 declares defunct — a settle-sleep gives no
        verdict under load, and there is nothing here for one to wait *for*
        that the caller does not already know better.
        """
        self.driver.click("snapshot-prune-button")

    def check_integrity(self) -> None:
        """Click `snapshot-check-button` — which RUNS the check (§ *Check*
        ruling: a tagged button that only opens a sheet violates the actuation
        contract).

        ⚠ Barrier-free for the same reason as [`prune`]; the settle-sleep it
        used to carry is retired per convention 14.
        """
        self.driver.click("snapshot-check-button")

    # --- Prune preview + check verdict (the two result surfaces) ---
    #
    # Both surfaces were untagged chrome on every app until 2026-08-13: the
    # apps painted them, but no id meant no e2e reach, which is exactly where
    # the six implementations had diverged most (`ui/backups.md`
    # § Snapshot-list shape). The ids below are `optional_elements` on the
    # backups page, user-approved 2026-08-13.

    def wait_for_prune_preview(self, timeout: float = PRUNE_PREVIEW_WAIT_S) -> bool:
        """Deadline-poll until the standing prune preview renders.

        This is the causal barrier for `prune()`, which is deliberately
        barrier-free: the preview element is `Some(prune_preview)` on the shared
        machine and renders only while one stands, so its appearance IS the
        round trip completing (convention 14 — never a settle-sleep, and the two
        two-second settle-sleeps that used to sit in `prune()` /
        `check_integrity()` must not come back).

        ⚠ Spell that pair in prose, never as source: the cheap-tier sleep
        ratchet is a regex over the file, so a docstring quoting the call it
        replaced counts as one and holds the baseline up. Measured here.
        """
        return self._visible_within("snapshot-prune-preview", timeout)

    def wait_for_no_prune_preview(self, timeout: float = PRUNE_PREVIEW_GONE_WAIT_S) -> bool:
        """Deadline-poll until no prune preview stands — the causal barrier for
        cancel (and for execute, which clears the preview it consumed).

        Returns True once the surface is gone. A negative assert with its own
        barrier, not a settle-sleep: the machine drops `prune_preview` in the
        same state write that answers the gesture.
        """
        deadline = time.monotonic() + timeout
        while True:
            if not self.driver.is_visible("snapshot-prune-preview"):
                return True
            if time.monotonic() >= deadline:
                return False
            time.sleep(0.5)

    def prune_preview_text(self) -> str:
        """The standing preview's text — counts + candidates, or the typed
        `not_set` / `unparseable` explanation of why nothing would be pruned."""
        if not self.driver.is_visible("snapshot-prune-preview"):
            return ""
        return self.driver.get_text("snapshot-prune-preview")

    def wait_for_prune_preview_text(
        self, predicate, timeout: float = PRUNE_PREVIEW_WAIT_S
    ) -> str:
        """Deadline-poll the STANDING preview's text until `predicate` holds,
        returning it ("" at the deadline). Does not click — the caller raises the
        preview with `prune_until_preview()` first.

        The verdict is what separates the two states the execute button cannot:
        "nothing to prune" and "no retention policy configured for this set" both
        stand a preview and both offer no execute, so only the text says which
        one the nest returned (`ui/backups.md` § Errors & edge cases — *Prune
        with no candidates*, all three states typed off the reply). A poll rather
        than a one-shot read because a shell may register the surface a frame
        before it paints the verdict into it — a positive wait with a named
        ceiling (convention 14), never a settle-sleep.
        """
        deadline = time.monotonic() + timeout
        while True:
            text = self.prune_preview_text()
            if predicate(text):
                return text
            if time.monotonic() >= deadline:
                return ""
            time.sleep(0.5)

    def has_prune_execute(self) -> bool:
        """Is execute offered? Present ONLY with a standing preview that names
        candidates (`ui/backups.md` § Snapshot-list shape, *Prune* — an armed
        button over zero candidates would promise an effect it cannot have)."""
        return not self.driver.is_absent("snapshot-prune-execute-button")

    def execute_prune(self) -> None:
        """Click `snapshot-prune-execute-button` — applies the STANDING preview.

        Barrier-free by the same rule as `prune()`: the caller barriers on what
        it is about to assert (the pruned row leaving the list, or the preview
        clearing).
        """
        self.driver.click("snapshot-prune-execute-button")

    def cancel_prune(self) -> None:
        """Click `snapshot-prune-cancel-button` — discards the preview with no
        side effect (nothing was deleted; the preview was a dry run)."""
        self.driver.click("snapshot-prune-cancel-button")

    def prune_until_preview(self, timeout: float = PRUNE_UNTIL_PREVIEW_WAIT_S) -> bool:
        """Click `snapshot-prune-button` until a preview stands, then return.

        Why a retry loop rather than one click plus a wait: every mutating
        control is disabled while the machine's `in_progress_op` is `Some`, and
        the preview clears *before* the op ends — `prune_execute` sets
        `prune_preview = None`, then `load()`s (a nest round trip), and only
        then `end_op`s (`libs/fauna-backups-machine/src/machine.rs`). So a
        caller that barriers on "no preview standing" and clicks immediately can
        land its click on a still-disabled button and lose it. Re-clicking is
        safe by construction: `begin_op` refuses a second concurrent op, so an
        extra click during the window is a no-op rather than a double prune.

        This is a deadline poll over a retryable gesture (the `select_folder`
        idiom), not a settle-sleep — a green run pays only as long as the round
        trip actually takes.
        """
        deadline = time.monotonic() + timeout
        while True:
            self.driver.click("snapshot-prune-button")
            if self._visible_within("snapshot-prune-preview", 2):
                return True
            if time.monotonic() >= deadline:
                return False

    def wait_for_check_result(self, timeout: float = CHECK_RESULT_WAIT_S) -> str:
        """Deadline-poll until the completed check's verdict renders, returning
        its text ("" at the deadline).

        The causal barrier for `check_integrity()`. A completed check is a
        RESULT, never an error (`ui/backups.md` § Architectural rules, rule 6),
        so this surface — not `error-message` — is where a verdict lands, pass
        or fail.
        """
        deadline = time.monotonic() + timeout
        while True:
            if self.driver.is_visible("snapshot-check-result"):
                return self.driver.get_text("snapshot-check-result")
            if time.monotonic() >= deadline:
                return ""
            time.sleep(0.5)

    # --- Undelete (backups.md § Snapshot-list shape, *Soft-deleted rows*) ---

    def recoverable_row_count(self) -> int:
        """How many listed rows currently offer recovery.

        `snapshot-undelete-button` renders ONLY on a `SoftDeleted` row, so its
        count is the number of soft-deleted rows — the one observable that
        distinguishes soft-deleted from active *without* reading row text. A
        `snapshot-item` count cannot: the nest's `list` deliberately keeps
        soft-deleted rows for the 30-day recovery window, so the row count is
        identical either side of a prune (the same reason the prune test asserts
        on the execute button's presence rather than on a count).
        """
        return self.driver.count("snapshot-undelete-button")

    def wait_for_recoverable_rows(self, count: int, timeout: float = RECOVERABLE_ROWS_WAIT_S) -> bool:
        """Deadline-poll until exactly `count` rows offer recovery.

        The causal barrier for both directions of the transition: a prune
        execute soft-deletes its candidate (0 -> 1) and an undelete returns that
        row to `Active` (1 -> 0). Each is the surface the gesture itself
        produces or clears, never a settle-sleep (convention 14), and the
        machine re-reads the list before it releases the single-flight slot, so
        the count is the state the nest just served.
        """
        deadline = time.monotonic() + timeout
        while True:
            if self.recoverable_row_count() == count:
                return True
            if time.monotonic() >= deadline:
                return False
            time.sleep(0.5)

    def undelete_snapshot(self, index: int = 0) -> None:
        """Click the `index`-th `snapshot-undelete-button` — recover that row.

        Barrier-free by the same rule as `execute_prune()`: the caller barriers
        on what it is about to assert (here, the control disappearing as the row
        returns to `Active`).
        """
        self.driver.click("snapshot-undelete-button", index=index)

    def _visible_within(self, element_id: str, timeout: float) -> bool:
        """Deadline-poll for an element, returning whether it appeared.

        ⚠ Deliberately NOT named `_wait_visible`: this class already has one
        (further down, the wizard's), and that one RAISES on timeout and returns
        None. Python takes the last definition, so a same-named helper here is
        silently shadowed — `if self._wait_visible(...)` then raises where it
        was written to return False. Measured, not theorised: it is how this
        method got its name.
        """
        deadline = time.monotonic() + timeout
        while True:
            if self.driver.is_visible(element_id):
                return True
            if time.monotonic() >= deadline:
                return False
            time.sleep(0.5)

    def open_snapshot(self, index: int = 0) -> None:
        """Open the snapshot at the given index from the list.

        Opening fetches the snapshot detail (file listing) — on web a
        `fauna.filesync.snapshot.get` WS-RPC round trip, so the detail
        view appears asynchronously. Poll until the file-list surface is
        visible (no-op wait on clients that render it synchronously).
        """
        self.driver.click("snapshot-item", index=index)
        deadline = time.monotonic() + SNAPSHOT_DETAIL_OPEN_WAIT_S
        while time.monotonic() < deadline:
            if self.driver.is_visible("snapshot-detail-files"):
                return
            time.sleep(0.5)
        # Raise rather than fall through: the detail surface never appearing means
        # the click did not open a snapshot (e.g. it landed on a row whose UIA
        # element exposes no selection), and every later assertion would then be
        # about a pane that was never opened — a confusing "element missing"
        # failure far from the real cause.
        raise TimeoutError(
            f"snapshot {index} did not open within {SNAPSHOT_DETAIL_OPEN_WAIT_S}s: "
            f"{self.driver.diagnose('snapshot-detail-files')} "
            f"{self.driver.diagnose('snapshot-item')}"
        )

    def snapshot_file_count(self) -> int:
        """Count the per-file download buttons inside the open snapshot detail
        (`snapshot-file-download-button`, indexed — one per file row)."""
        return self.driver.count("snapshot-file-download-button")

    def wait_for_snapshot_files(self, min_count: int = 1, timeout: float = SNAPSHOT_FILES_WAIT_S) -> int:
        """Wait until at least `min_count` `snapshot-file-download-button` rows
        are listed inside the open snapshot detail.

        `open_snapshot()` only waits for the `snapshot-detail-files` CONTAINER
        to mount, which happens synchronously with the view appearing — the
        actual file listing is a separate async fetch (macOS/iOS:
        `BackupManagementVM.loadSnapshotFiles`, kicked off from the view's
        `.task`; the other apps have their own async fetch behind the same
        container). Reading the file count immediately after `open_snapshot()`
        races that fetch and reads 0 rows even though the snapshot genuinely
        has files (found via `test_snapshot_file_download_button_downloads_sealed_bytes`/
        `test_download_single_file_bytes_roundtrip` failing reproducibly on a
        real seeded file, 2/2, macOS). Returns the observed count.
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            count = self.snapshot_file_count()
            if count >= min_count:
                return count
            time.sleep(0.5)
        raise TimeoutError(
            f"Expected >= {min_count} snapshot-file-download-button rows after "
            f"{timeout}s, {self.driver.diagnose('snapshot-file-download-button')}"
        )

    def download_file(self, index: int = 0) -> None:
        """Click the per-file download button at `index` inside the open
        snapshot detail (single-file restore, backup-restore.md § 3). Where the
        bytes land is platform glue: a native save dialog for a real user; under
        e2e the client saves without a dialog into `driver.download_dir()`
        (see `wait_for_downloaded_file`)."""
        self.driver.click("snapshot-file-download-button", index=index)

    def download_file_and_read(self, index: int, filename: str) -> bytes:
        """Click the per-file download button at `index` and return the saved
        bytes, whichever way the platform exposes them (the platform branch
        lives here in the action layer, per the e2e conventions): web captures
        the real browser download (Playwright wraps the click server-side, so
        it can't be composed from a plain click + a directory read); native
        apps save dialog-less into `driver.download_dir()` (the
        `ISnapshotFileSaver`-family seam) and the file is read back from there.
        """
        if self.driver.is_web():
            return self.driver.download_via_click(
                "snapshot-file-download-button", index=index
            )
        self.download_file(index=index)
        return self.wait_for_downloaded_file(filename)

    def wait_for_downloaded_file(self, filename: str, timeout: float = SNAPSHOT_DOWNLOAD_WAIT_S) -> bytes:
        """Wait for the client to finish writing `filename` into the driver's
        e2e download directory and return its bytes.

        `driver.download_dir()` is the per-platform observation seam for the
        save step (the native save dialog is not e2e-driveable — clients bypass
        it under e2e and write here). Raises AssertionError on timeout, naming
        the dir and what IS there so failures diagnose themselves.
        """
        import os

        download_dir = self.driver.download_dir()
        assert download_dir, (
            f"driver {type(self.driver).__name__} does not expose an e2e "
            "download dir — wire download_dir() before using this action"
        )
        target = os.path.join(download_dir, filename)
        deadline = time.monotonic() + timeout
        last_size = -1
        while time.monotonic() < deadline:
            if os.path.exists(target):
                size = os.path.getsize(target)
                # Two consecutive polls at the same non-zero size = write done
                # (the client writes once, but don't race a partial write).
                if size > 0 and size == last_size:
                    with open(target, "rb") as f:
                        return f.read()
                last_size = size
            time.sleep(0.5)
        present = (
            sorted(os.listdir(download_dir))
            if os.path.isdir(download_dir) else "<dir missing>"
        )
        # The click is fire-and-forget (the apps dispatch the fetch into a
        # task), so a failing download NEVER fails the click — it lands in the
        # page error element moments later. Reading it here is e2e convention 6:
        # this exact timeout hid `snapshot N has no file at {path}` — the
        # keyless-read bug fixed — behind a bare "dir
        # contains: []" across three sessions of apple row 41, each of which
        # went looking at the harness save path instead.
        err = ""
        if self.driver.is_visible("error-message"):
            err = self.driver.get_text("error-message")
        raise AssertionError(
            f"downloaded file {filename!r} did not appear in {download_dir} "
            f"within {timeout}s; dir contains: {present}. "
            f"error-message: {err!r}"
        )

    def last_backed_up(self) -> str:
        """Get the last backup timestamp text."""
        if not self.driver.is_visible("last-backed-up"):
            return ""
        return self.driver.get_text("last-backed-up")

    def wait_for_last_backed_up(self, predicate, timeout: float = LAST_BACKED_UP_WAIT_S) -> str:
        """Deadline-poll `last-backed-up` until `predicate(text)` is true,
        returning the last-read text (matching or not) at the deadline.

        A single-shot `last_backed_up()` read right after `wait_for_no_snapshots()`
        / `wait_for_snapshot_row()` races this label: those barrier on the
        snapshot LIST re-rendering, not on `last-backed-up` specifically, so
        under load the two can repaint on separate ticks even though the VM
        derives both from the same `Snapshot()` read — the same class of gap
        `wait_for_post_state_by_text` fixed for feed state reads (testing.md
        convention 14; measured flaky on `--app windows` under a cold-build-load
        first run, green on an immediate re-run). A deadline poll, never a
        settle-sleep: green runs pay nothing, only a genuinely broken
        derivation times out. The caller does the assertion + diagnose(), same
        division as `wait_for_post_state_by_text`.
        """
        deadline = time.monotonic() + timeout
        seen = ""
        while True:
            seen = self.last_backed_up()
            if predicate(seen):
                return seen
            if time.monotonic() >= deadline:
                return seen
            time.sleep(0.5)

    def is_folder_selector_visible(self) -> bool:
        """Check if the folder selector is visible."""
        return self.driver.is_visible("backup-folder-selector")

    def is_snapshot_detail_visible(self) -> bool:
        """Check if snapshot detail file list is visible."""
        return self.driver.is_visible("snapshot-detail-files")

    # --- Restore history (read surface) ---

    def restore_history_count(self) -> int:
        """Count restore-history rows."""
        return self.driver.count("restore-history-item")

    def restore_history_item_text(self, index: int = 0) -> str:
        """Text of a restore-history row (completed-at + kinds + source)."""
        return self.driver.get_text("restore-history-item", index=index)

    def has_divergence_banner(self, index: int = 0) -> bool:
        """Whether the restore-history row at `index` shows a divergence banner."""
        return self.driver.is_visible(
            "restore-divergence-banner",
            scope=f"restore-history-item[{index}]",
        )

    def divergence_banner_text(self, index: int = 0) -> str:
        return self.driver.get_text(
            "restore-divergence-banner",
            scope=f"restore-history-item[{index}]",
        )

    def open_divergence_modal(self, index: int = 0) -> None:
        """Click the divergence banner on the row at `index` to open the modal."""
        self.driver.click(
            "restore-divergence-banner",
            scope=f"restore-history-item[{index}]",
        )

    def is_divergence_modal_visible(self) -> bool:
        return self.driver.is_visible("restore-divergence-details-modal")

    def divergence_detail_count(self) -> int:
        return self.driver.count("restore-divergence-details-item")

    def divergence_detail_text(self, index: int = 0) -> str:
        return self.driver.get_text("restore-divergence-details-item", index=index)

    # --- Local restore action ---

    def restore_snapshot_option_count(self) -> int:
        """How many local snapshots the restore picker offers."""
        return self.driver.count("restore-snapshot-select")

    def select_restore_snapshot(self, index: int = 0) -> None:
        self.driver.click("restore-snapshot-select", index=index)

    def type_restore_confirm(self, text: str) -> None:
        self.driver.clear_and_type("restore-confirm-input", text)

    def is_restore_button_enabled(self) -> bool:
        return self.driver.is_enabled("restore-confirm-button")

    def click_restore(self) -> None:
        self.driver.click("restore-confirm-button")

    def restore_progress_text(self) -> str:
        if not self.driver.is_visible("restore-progress"):
            return ""
        return self.driver.get_text("restore-progress")

    def restore_warning_text(self) -> str:
        """`restore-warning`'s text, or "" when it is absent — the element
        renders only after a restore whose reply said `config_present == false`
        (`ui/backups.md` § Restore from backup destination)."""
        if not self.driver.is_visible("restore-warning"):
            return ""
        return self.driver.get_text("restore-warning")

    def wait_for_restore_progress(
        self, predicate, timeout: float = RESTORE_PROGRESS_WAIT_S
    ) -> str:
        """Deadline-poll `restore-progress` until `predicate` holds, returning
        the text ("" at the deadline).

        The restore's own terminal-state barrier (`ui/backups.md` § Restore from
        backup destination). **Assert the terminal state, never the transient
        one**: "Restoring…" is the state the click arms and the reply clears, so
        a test that waits to *observe* it is asserting a wall-clock window and is
        defunct by convention 14. What is latency-independent is the pair the
        journey can pin — the idle prompt before the click, and the done text
        after it.
        """
        deadline = time.monotonic() + timeout
        while True:
            text = self.restore_progress_text()
            if predicate(text):
                return text
            if time.monotonic() >= deadline:
                return ""
            time.sleep(0.5)

    # --- Backup destinations (management) ---

    def destination_count(self) -> int:
        """How many configured backup destinations are listed."""
        return self.driver.count("backup-destination-status-row")

    def open_add_destination(self) -> None:
        self.driver.click("backup-destination-add-button")
        self.driver.wait_for("backup-destination-url-input", timeout=DESTINATION_DIALOG_WAIT_S)

    def add_destination(self, url: str, name: str = "") -> None:
        """Open the add dialog, enter url (+ optional name), and confirm.

        Confirm runs the shared enroll flow: resolve the destination identity
        over an authed connection (`segment_backup::resolve_destination`) then
        persist the row to the `fauna.state.backup` plane. The caller waits on the row count
        (the round-trip is async).
        """
        self.open_add_destination()
        self.driver.clear_and_type("backup-destination-url-input", url)
        if name:
            self.driver.clear_and_type("backup-destination-name-input", name)
        self.driver.click("backup-destination-add-confirm-button")

    def edit_destination(self, index: int, name: str) -> None:
        """Open the edit dialog for the row at `index`, set a new name, confirm."""
        self.driver.click("backup-destination-edit-button", index=index)
        self.driver.wait_for("backup-destination-name-input", timeout=DESTINATION_DIALOG_WAIT_S)
        self.driver.clear_and_type("backup-destination-name-input", name)
        self.driver.click("backup-destination-add-confirm-button")

    # --- Post-succession review: the per-row Keep/Remove pair
    # (succession-aftermath.md § Re-key scope → *Adjudicating what the aftermath
    # carries across*) ---

    def destination_unattested_mark_visible(self, index: int = 0) -> bool:
        """Whether the row at `index` carries `backup-destination-unattested-mark`.

        ⚠ **Absence, not emptiness, is the un-raised state.** The mark renders
        only while the row is actually raised — a permanently-present element
        would train the user straight past the one succession that matters
        (`ui.yaml`'s registry entry states this). So a caller asserting "not
        raised" must assert this returns ``False``, never that its text is "".

        Scoped to the row rather than read flat: the pair renders only on raised
        rows, so a flat index would walk a raised-rows list `0..n` against a
        destination list `0..m` and the two would silently disagree about which
        destination is being asked about (the same reason tui scopes it
        `.within("backup-destination-status-row", i)`).
        """
        return not self.driver.is_absent(
            "backup-destination-unattested-mark",
            scope=f"backup-destination-status-row[{index}]",
        )

    def destination_unattested_mark_text(self, index: int = 0) -> str:
        """The raised row's review copy, `""` when the row is genuinely not
        raised, or `<unreadable: ...>` when the mark is present but its text
        could not be read.

        ⚠ **Diagnostic-only — no assertion reads this** (assert
        `destination_unattested_mark_visible` instead). Twin of
        `NestTrustActions.grant_unattested_mark_text` — same reasoning: `""`
        for a failed read would read identically to "never raised", exactly
        the misleading kind convention 6 forbids
        (`e2e-self-diagnosing-failures.md` § The convention).
        """
        scope = f"backup-destination-status-row[{index}]"
        try:
            return self.driver.get_text(
                "backup-destination-unattested-mark",
                scope=scope,
            )
        except Exception as exc:
            if self.driver.is_absent("backup-destination-unattested-mark", scope=scope):
                return ""
            return f"<unreadable: {type(exc).__name__}>"

    def destination_keep_visible(self, index: int = 0) -> bool:
        """Whether the row at `index` offers `backup-destination-keep-button`."""
        return not self.driver.is_absent(
            "backup-destination-keep-button",
            scope=f"backup-destination-status-row[{index}]",
        )

    def keep_destination(self, index: int = 0) -> None:
        """Press **Keep** on the raised row at `index`.

        Clears that row's mark in the `fauna.state.backup` plane (`keep_backup_destination`) and
        re-reads the destination list from the nest, so a caller polls on the
        mark's *absence* rather than sleeping. The Remove half deliberately has
        no twin here — `backup-destination-remove-button` above already is it.
        """
        self.driver.click(
            "backup-destination-keep-button",
            scope=f"backup-destination-status-row[{index}]",
        )

    # --- Third destination kind: client-device custodian (backups.md § Third
    # destination kind — client device as custodian) ---

    def select_destination_kind(self, wire_value: str) -> None:
        """Pick a destination kind in the (already-open) add/edit dialog's
        `backup-destination-kind-select` — the wire value (`"nest"` /
        `"client-device"`), never the localized label."""
        self.driver.select("backup-destination-kind-select", wire_value)

    def custodian_pull_run_now(
        self, timeout: float = CUSTODIAN_PASS_WAIT_S, now_offset_secs: int = 0
    ) -> dict:
        """Run **one** custodian pull pass on the sync agent's hosted replica and
        return its report.

        The causal barrier the enroll→pull→check-in→status proof rests on: the
        agent replies only once the pass has pulled, sealed, stored,
        audited-if-due and written its check-in, so a caller asserts state and
        never timing (convention 14). Without it the first production pass is
        `PERIODIC_INTERVAL` (15 min) away — `CustodianPull::run_loop`
        deliberately mutes the interval's immediate first tick.

        The reply's `hosting` is `False` while the agent has not yet re-read the
        registry and found this device's row (up to `IDLE_RECHECK_SECS` after
        enrollment). That is a **report, not an error**, which is what lets
        `wait_for_custodian_hosting` below poll instead of sleeping.

        `now_offset_secs` shifts the clock **this one pass** runs at — convention
        14's fake clock, the same knob `backup_audit_run_now` takes. It is not a
        convenience: a pass contains cadences slower than any test may wait for,
        the slowest being the self-audit's 24-hour debounce
        (`AUDIT_MIN_INTERVAL_SECS`), so a store's *second* audit — the first one
        that can observe rot appearing after enrollment — is unreachable at the
        real clock. Prefer `AUDIT_DEBOUNCE_JUMP_S` below over a hand-written
        number.
        """
        import json as _json
        raw = self.driver.call_command(
            "custodian_pull_run_now",
            payload={"now_offset_secs": int(now_offset_secs)},
            timeout=timeout,
        )
        if raw is None:
            raise AssertionError(
                "custodian_pull_run_now returned no report — the app refused the "
                f"command. error={self.driver.error_text() if hasattr(self.driver, 'error_text') else '<n/a>'!r}"
            )
        return _json.loads(raw) if isinstance(raw, str) else raw

    def wait_for_custodian_hosting(
        self, timeout: float = CUSTODIAN_HOSTING_WAIT_S
    ) -> dict:
        """Poll `custodian_pull_run_now` until the agent reports it is hosting
        this device's replica AND that pass checked in at the nest, and return
        that first such report.

        Polling the poke itself is what keeps this latency-independent: the host
        loop re-reads the destination registry on its own `IDLE_RECHECK_SECS`
        cadence, and every poll both *checks* whether it has and *runs* a pass
        the moment it has — so the budget below is a ceiling, never a wait.

        ``hosting`` alone is not enough: a replica for a destination removed a
        moment ago still reports ``hosting: True`` for the one pass whose
        check-in the nest refuses as not assigned (``checked_in: False``). That
        refusal ends the stint at once (`sync-agent.md` § A7), and the next
        destination is hosted at the agent's idle recheck. Before 2026-09-29
        the stale stint ran on until the next registry rediscovery, up to 15
        minutes, and the 2026-09-22 linux sweep's custodian-removal test took
        its stale report as its own first pass.
        """
        deadline = time.monotonic() + timeout
        last = {}
        while time.monotonic() < deadline:
            last = self.custodian_pull_run_now()
            if last.get("hosting") and last.get("checked_in"):
                return last
            time.sleep(2)  # sleep-ok: poll interval inside the deadline loop above, not a settle-sleep — each tick RUNS a pass and returns the instant the agent reports hosting (convention 14's deadline-poll mechanism)
        raise AssertionError(
            f"the sync agent never began hosting a custodian replica within "
            f"{timeout}s; last report={last!r}. Either enrollment never reached "
            f"the nest's destination registry, or the agent holds no capability "
            f"(it needs device_id + backup_key to host at all)."
        )

    def wait_for_custodian_hosting_report(
        self, timeout: float = CUSTODIAN_HOSTING_WAIT_S
    ) -> dict:
        """Poll `custodian_pull_run_now` until the agent reports it is hosting
        this device's replica, whether or not the pass checked in, and return
        that report.

        Use this, not `wait_for_custodian_hosting`, where a pass is EXPECTED to
        be refused: a pass against a source below its copy (a rebuilt, empty
        box) is refused whole and never checks in, yet still reports the store
        as it stands (`held_bytes`).
        """
        deadline = time.monotonic() + timeout
        last = {}
        while time.monotonic() < deadline:
            last = self.custodian_pull_run_now()
            if last.get("hosting"):
                return last
            time.sleep(2)  # sleep-ok: poll interval inside the deadline loop above, not a settle-sleep — each tick RUNS a pass (convention 14's deadline-poll mechanism)
        raise AssertionError(
            f"the sync agent never began hosting a custodian replica within "
            f"{timeout}s; last report={last!r}"
        )

    def add_custodian_destination(self, name: str = "", capacity: str = "") -> None:
        """Open the add dialog, pick the client-device kind, optionally set a
        capacity cap + name, and confirm.

        No URL at all — the custodian kind has no address
        (`enroll_client_custodian`, three steps, no `NestBackupKey` grant, no
        resolve round-trip). A blank `capacity` is a real choice (uncapped),
        not a placeholder for "don't set one".

        **web** declares the kind select and capacity input absent: enrolment
        runs on the device being enrolled (`ui/backups.md` § Implementation
        status today), while the row's kind badge, usage line and the
        sole-client warning are still web's to show. So on web the enrolment
        is ANOTHER device's act — `backup_enroll_custodian_for_test` runs the
        shared `enroll_client_custodian` under a stand-in device id — and the
        page is then re-mounted (it reads the destination list on mount) so
        what it renders is the product's own read.
        """
        if self.driver.is_web():
            self.driver.call_command(
                "backup_enroll_custodian_for_test",
                {
                    "device_id": secrets.token_hex(16),
                    "name": name,
                    "capacity": capacity,
                },
            )
            self.driver.navigate_to("settings")
            self.navigate()
            return
        self.open_add_destination()
        self.select_destination_kind("client-device")
        if capacity:
            self.driver.clear_and_type("backup-destination-capacity-input", capacity)
        if name:
            self.driver.clear_and_type("backup-destination-name-input", name)
        self.driver.click("backup-destination-add-confirm-button")

    def destination_kind_badge_text(self, index: int = 0) -> str:
        """`backup-destination-kind-badge` text for the row at `index` — every
        row carries one, regardless of kind."""
        return self.driver.get_text(
            "backup-destination-kind-badge",
            scope=f"backup-destination-status-row[{index}]",
        )

    def destination_usage_visible(self, index: int = 0) -> bool:
        """Whether `backup-destination-usage` renders on the row at `index` —
        client-device rows only."""
        return not self.driver.is_absent(
            "backup-destination-usage",
            scope=f"backup-destination-status-row[{index}]",
        )

    def destination_usage_text(self, index: int = 0) -> str:
        return self.driver.get_text(
            "backup-destination-usage",
            scope=f"backup-destination-status-row[{index}]",
        )

    def sole_client_destination_warning_visible(self) -> bool:
        """Whether the page-level `backup-sole-client-destination-warning`
        renders — standing warning while EVERY configured destination is a
        client device (an `Inert` row does not count as one)."""
        return not self.driver.is_absent("backup-sole-client-destination-warning")

    def remove_destination(self, index: int = 0, reclaim: bool = False) -> None:
        """Open the remove-confirm dialog for the row at `index` and confirm.

        `reclaim` ticks `backup-destination-remove-reclaim-checkbox` — the
        client-device-only opt-in to *also* free this device's sealed copy in
        the same gesture (`ui/backups.md` § Manage backup destinations →
        *Remove*). It defaults to `False` because that is the product default
        and the one that matters: removing a client-device destination
        deliberately KEEPS the local store, since it is the owner's only offline
        copy, and a test that ticked it by default could never observe the
        orphaned-store row at all.
        """
        self.driver.click("backup-destination-remove-button", index=index)
        self.driver.wait_for("backup-destination-remove-confirm-button", timeout=DESTINATION_DIALOG_WAIT_S)
        if reclaim:
            self.driver.click("backup-destination-remove-reclaim-checkbox")
        self.driver.click("backup-destination-remove-confirm-button")

    def remove_every_destination(self) -> None:
        """Start from no configured destination: remove each one through the
        page, one confirmed remove at a time.

        For a test that counts destinations or addresses their rows by index on
        the session-scoped `test_user`, where an earlier module's leftover
        would otherwise be part of what it counts. Each remove is waited on
        before the next, so a remove that does not take fails HERE, naming the
        count it stuck at, instead of as a wrong count several steps later.
        """
        self.navigate()
        remaining = self.destination_count()
        while remaining:
            self.remove_destination(0)
            self.wait_for_destination_count(remaining - 1)
            remaining -= 1

    def orphaned_store_visible(self) -> bool:
        """Whether `backup-orphaned-store-row` renders — this device is holding
        a sealed custodian store that NO destination row claims.

        The steady state after a client-device destination is removed without
        the reclaim opt-in, and the only place the reclaim gesture is reachable
        from once that row is gone.
        """
        return not self.driver.is_absent("backup-orphaned-store-row")

    def orphaned_store_text(self) -> str:
        """The orphaned-store row's sentence — it names how much this device is
        still holding, which is the number that makes the offer worth taking."""
        return self.driver.get_text("backup-orphaned-store-row")

    def reclaim_orphaned_store(self) -> None:
        """Free this device's whole sealed copy: press `backup-destination-reclaim-button`
        on the orphaned-store row and confirm.

        A **plain** confirm — no re-typed id, unlike the immediate-delete
        friction bar — because the store is re-buildable from a fresh pull
        whenever the device re-enrolls; the modal exists because reclaiming ends
        this device's ability to restore with no nest reachable.
        """
        self.driver.click("backup-destination-reclaim-button")
        self.driver.wait_for("backup-reclaim-confirm-button", timeout=DESTINATION_DIALOG_WAIT_S)
        self.driver.click("backup-reclaim-confirm-button")

    def cancel_reclaim(self) -> None:
        """Close the reclaim-confirm dialog without freeing anything."""
        self.driver.click("backup-reclaim-cancel-button")

    def require_reseed_supported(self) -> None:
        """Skip unless this app paints the re-seed gesture (`ui/backups.md`
        § Restore after losing the nest) — e2e convention 7.

        Built on tui (the lead app, 2026-09-26), linux (2026-09-29), both over
        the shared `fauna_client_sync::reseed_wire::await_agent_reseed` +
        `fauna_client_config::reenroll_custodian_after_reseed`, macOS + iOS
        (2026-09-29, the shared FaunaKit view: macOS through the agent's FFI
        face, iOS in-process), and windows (2026-09-30, through the same agent
        FFI face as macOS). android owes its shell (the batched trickle-down).
        web is a declared absence, never marked: a browser cannot hold the
        sealed store (`behavior/backup-destinations.md` § Implementation status
        today).
        """
        if self.driver.is_android():
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="backup-destination-reseed-button",
                detail="tui, linux, apple and windows have landed the re-seed "
                       "gesture; this app's shell is the batched trickle-down",
                tracked="behavior/backup-destinations.md § Implementation status today",
            )

    def reseed_from_orphaned_store(self) -> None:
        """Restore the signed-in nest from this device's copy: press
        `backup-destination-reseed-button` inside the orphaned-store row and
        confirm (`ui/backups.md` § Restore after losing the nest). Returns once
        the confirm is pressed; read the outcome with `reseed_result_text`."""
        self.driver.click(
            "backup-destination-reseed-button", scope="backup-orphaned-store-row[0]"
        )
        self.driver.wait_for(
            "backup-destination-reseed-confirm-button", timeout=DESTINATION_DIALOG_WAIT_S
        )
        self.driver.click("backup-destination-reseed-confirm-button")

    def reseed_button_on_row_visible(self, index: int = 0) -> bool:
        """Whether `backup-destination-reseed-button` renders inside the
        destination status row at `index` — the re-seed gesture on a destination
        row, as opposed to the orphaned-store row's."""
        return not self.driver.is_absent(
            "backup-destination-reseed-button",
            scope=f"backup-destination-status-row[{index}]",
        )

    def reseed_from_custodian_row(self, index: int = 0) -> None:
        """Restore the signed-in nest from this device's copy through its own
        custodian destination row: press `backup-destination-reseed-button`
        inside `backup-destination-status-row[index]` and confirm — the same
        confirm as `reseed_from_orphaned_store` (`ui/backups.md` § Restore after
        losing the nest). Returns once the confirm is pressed; read the outcome
        with `reseed_result_text`."""
        self.driver.click(
            "backup-destination-reseed-button",
            scope=f"backup-destination-status-row[{index}]",
        )
        self.driver.wait_for(
            "backup-destination-reseed-confirm-button", timeout=DESTINATION_DIALOG_WAIT_S
        )
        self.driver.click("backup-destination-reseed-confirm-button")

    def reseed_result_text(self) -> str:
        """`backup-destination-reseed-result`'s text, or '' while it is absent."""
        if self.driver.is_absent("backup-destination-reseed-result"):
            return ""
        return self.driver.get_text("backup-destination-reseed-result")

    def destination_text(self, index: int = 0) -> str:
        """Text of the destination status row at `index` (its display name)."""
        return self.driver.get_text("backup-destination-status-row", index=index)

    def wait_for_destination_count(self, expected: int, timeout: float = DESTINATION_COUNT_WAIT_S) -> None:
        """Wait until exactly `expected` destination rows are listed.

        The add/remove round-trips persist to the `fauna.state.backup` plane and re-render
        from a reload, so the row count settles asynchronously.
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.destination_count() == expected:
                return
            # An enroll failure surfaces in the page error element — surface it
            # in the timeout rather than waiting blind.
            time.sleep(0.5)
        err = ""
        if self.driver.is_visible("error-message"):
            err = self.driver.get_text("error-message")
        raise TimeoutError(
            f"Expected {expected} backup-destination rows, got "
            f"{self.destination_count()}. error-message: {err!r}"
        )

    # --- Devices / Folders pages ---

    # Clients whose folder UI has split into the Settings → Devices / Folders
    # sub-pages (2026-06-28 unification). web led; linux migrated 2026-06-28;
    # macos+ios migrated via — macos
    # e2e-confirmed N+40, ios N+41; windows migrated 2026-06-29 (Settings →
    # Devices roster + Settings → Folders control plane, top-level
    # Sync/Conflicts retired). tui built both the Devices roster AND the
    # Folders CORE control plane — tests
    # exercising the still-unbuilt folders remainders (local-folder binding,
    # sharing, webdav, paywall, page-level default-conflict-policy) carry
    # their own narrow tui skip instead of relying on this set. android adds
    # itself here once its folders UI restructure lands; drop this set once
    # every app has split.
    def _folders_in_settings(self) -> bool:
        return (
            self.driver.is_web()
            or self.driver.is_linux()
            or self.driver.is_macos()
            or self.driver.is_ios()
            or self.driver.is_windows()
            or self.driver.is_tui()
        )

    def navigate_devices(self) -> None:
        """Navigate to the device ROSTER.

        2026-06-28 sync/folder UI unification (design tracked internally):
        migrated clients (``_folders_in_settings``) split the former combined
        top-level Devices/Peers page into two Settings sub-pages — Settings →
        Devices (roster) + Settings → Folders (wizard/list/conflicts). The other
        apps still render the combined top-level page (their per-app
        ``NEXT-<client>-folders-ui`` restructure is pending), so they keep the
        ``{"view":"devices"}`` nav until they migrate.
        """
        if self._folders_in_settings():
            self.driver.set_state(
                {"nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "devices"}]}}
            )
        else:
            self.driver.navigate_to("devices")

    def navigate_folders(self) -> None:
        """Navigate to the folder control plane — list + create wizard +
        conflict resolution (the ``folder-*`` / ``wizard-*`` / ``conflict-*`` IDs).

        2026-06-28 unification: migrated clients (``_folders_in_settings``) render
        these on Settings → Folders; the other apps still surface them on the
        combined top-level Devices/Peers page (restructure pending), so they reuse
        the ``{"view":"devices"}`` nav.

        tui's folders page landed 2026-07-22 — the CORE control
        plane only (list / in-place config / conflict review / create wizard).
        `_folders_in_settings()` already includes tui (it was true before this
        page existed, since tui's Devices roster already used the Settings-shell
        nav), so no tui-specific branch is needed here anymore; tests exercising
        local-folder binding / sharing / webdav / paywall / the page-level
        default-conflict-policy select carry their own narrow tui skip instead
        (those aren't built yet).
        """
        if self._folders_in_settings():
            self.driver.set_state(
                {"nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "folders"}]}}
            )
        else:
            self.driver.navigate_to("devices")

    def device_count(self) -> int:
        """Count visible device cards."""
        return self.driver.count("device-card")

    def device_name(self, index: int = 0) -> str:
        """Get the name of a device at the given index."""
        return self.driver.get_text("device-name", index=index)

    def device_status(self, index: int = 0) -> str:
        """Get the status of a device at the given index."""
        return self.driver.get_text("device-status", index=index)

    def remove_device(self, index: int = 0) -> None:
        """Remove a device by index."""
        from helpers.waiting import await_device_removal_ready

        await_device_removal_ready(self.driver)
        self.driver.click("device-remove-button", index=index)
        time.sleep(1)

    # --- Folder management ---

    def folder_count(self) -> int:
        """Count visible folder rows."""
        return self.driver.count("folder-row")

    def add_folder(self) -> None:
        """Open the new folder wizard."""
        self.driver.click("folder-add-button")

    def folder_title(self, index: int = 0) -> str:
        """The visible text of the folder row at `index` (includes its name)."""
        return self.driver.get_text("folder-row", index=index)

    def expand_folder(self, index: int = 0) -> None:
        """Toggle a folder `adw::ExpanderRow` open so its body (member roster,
        path editors, delete button) becomes reachable."""
        self.driver.click("folder-row", index=index)
        time.sleep(0.5)

    def folder_titles(self) -> list[str]:
        """Every folder row's visible text, in list order. A row the list
        rebuilt away between the count and the read reads as ``""``."""
        titles = []
        for i in range(self.folder_count()):
            try:
                titles.append(self.folder_title(i))
            except LookupError:
                titles.append("")
        return titles

    def wait_for_folder_row(self, name: str, timeout: float = FOLDER_CREATE_WAIT_S) -> int:
        """The index of the row whose title contains `name`, polled until it
        paints; ``AssertionError`` naming every listed row if it never does.

        A row is never there the instant after the gesture that made it: the
        list re-reads the nest on its own refresh (linux rebuilds it on every
        ``DevicesMachine`` snapshot tick, which a create fires asynchronously),
        so a set made a moment ago — by the wizard, or straight at the nest —
        can be missing from a read taken as the page is shown. A one-shot scan
        made that race the verdict (convention 14). Nor is "the count grew"
        proof that THIS row arrived: linux's list also carries offline-share
        group-scope rows from a separate listing, so only the name is."""
        deadline = time.monotonic() + timeout
        while True:
            titles = self.folder_titles()
            for i, title in enumerate(titles):
                if name in title:
                    return i
            if time.monotonic() >= deadline:
                raise AssertionError(
                    f"folder {name!r} not found among the rows within {timeout}s; "
                    f"rows listed: {titles}; {self._folder_page_diagnosis()}"
                )
            time.sleep(0.5)

    def revisit_folders(self, settle: float = FOLDER_CREATE_WAIT_S) -> list[str]:
        """Leave the Folders page, enter it again, and return the rows this
        visit paints — as soon as it paints one, or whatever it shows at
        ``settle`` (an empty list included).

        The Folders twin of ``MutedWordsActions.revisit``, for the same reason:
        on an app that does not consume the store-change notice yet (windows,
        macOS, iOS; a web tab that hosts no runtime —
        ``account-runtime.md`` § Implementation status today) the page renders
        its sets once per visit, through folder-key custody. A set whose
        custody lands under an open page — a successor's carry of its
        predecessor's folder keys, finished after the actor id switched — stays
        omitted from that visit's paint and reaches the screen at the next
        visit. A caller that must hold on every app polls THIS, never
        ``folder_count()`` on a page it entered once.

        Leaves through the feed, so the visit is a real nav edge, and gives the
        visit's own read ``settle`` to paint before handing back, so the
        caller's next poll does not tear an in-flight refresh down."""
        self.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
        self.navigate_folders()
        deadline = time.monotonic() + settle
        while True:
            titles = self.folder_titles()
            if titles or time.monotonic() >= deadline:
                return titles
            time.sleep(0.5)

    def _folder_page_diagnosis(self) -> str:
        """What the Folders page is painting instead of the row (convention 6).

        ``rows listed: []`` alone cannot tell its causes apart: a wizard still
        open is the WHOLE page on tui (no rows paint beside it), a list that
        painted empty shows its add button, and a refused create rides
        ``error-message``. Read all three so a red names which it was."""

        def read(label: str, fn) -> str:
            try:
                return f"{label}={fn()!r}"
            except Exception as e:  # noqa: BLE001 — diagnostic only; never mask the timeout
                return f"{label}=<{type(e).__name__}>"

        return "; ".join([
            read("wizard open", lambda: self.driver.is_visible("wizard-create-button")
                 or self.driver.is_visible("wizard-name-input")),
            read("list painted", lambda: self.driver.is_visible("folder-add-button")),
            read("error-message", lambda: self.driver.get_text("error-message")),
        ])

    def wait_for_folder_row_gone(self, name: str, timeout: float = FOLDER_CREATE_WAIT_S) -> None:
        """Poll until no row's title contains `name`; ``AssertionError`` naming
        every listed row if one still does at the deadline."""
        deadline = time.monotonic() + timeout
        while True:
            titles = self.folder_titles()
            if not any(name in title for title in titles):
                return
            if time.monotonic() >= deadline:
                raise AssertionError(
                    f"row {name!r} still present after {timeout}s; rows listed: {titles}"
                )
            time.sleep(0.5)

    def find_and_expand_folder(self, name: str) -> int:
        """Locate the folder row whose title contains `name` (waiting for it to
        paint — :meth:`wait_for_folder_row`), expand it, and return its index.
        Only one row should be expanded at a time so the unindexed body widgets
        (`folder-include-paths`, `folder-delete-button`, …) resolve uniquely to
        it."""
        i = self.wait_for_folder_row(name)
        self.expand_folder(i)
        return i

    def find_and_expand_folder_until(
        self, name: str, element_id: str, timeout: float = 15.0
    ) -> int:
        """Like :meth:`find_and_expand_folder`, but retries the expand until
        `element_id` (a body widget scoped to the row) is visible, instead of
        expanding once and trusting it to stay open.

        linux fully rebuilds the whole folder list on every ``DevicesMachine``
        observer tick, collapsing every expander closed independent of
        whatever a caller is waiting to observe next — a write (a save, a
        toggle) is exactly what triggers a tick, so a caller that just wrote
        and now needs to read the freshly-expanded body is the common case
        this hits (`webdav_toggle_visible`'s docstring names the same quirk
        for the toggle specifically; this generalizes it to any body widget).
        A single expand-then-wait can sit on a row a tick collapsed moments
        later and never re-open within the budget; re-expanding on every poll
        is the fix — cheap and idempotent when the row is already open and
        nothing is ticking."""
        deadline = time.monotonic() + timeout
        idx = self.find_and_expand_folder(name)
        while True:
            if self.driver.is_visible(element_id):
                return idx
            if time.monotonic() >= deadline:
                # One more real wait_for so a genuine non-render (the row IS
                # open, the widget just never paints) raises the driver's own
                # self-diagnosing TimeoutError rather than a bare "gave up".
                self.driver.wait_for(element_id, timeout=0.1)
                return idx
            idx = self.find_and_expand_folder(name)

    def delete_folder(self) -> None:
        """Click the delete button on the (single) expanded folder row — opens
        the destructive confirmation dialog. Call `confirm_folder_delete()`
        to commit."""
        self.driver.click("folder-delete-button")
        time.sleep(0.5)

    def confirm_folder_delete(self) -> None:
        """Click the destructive response in the delete-confirmation dialog."""
        self.driver.click("folder-delete-confirm")
        time.sleep(1)

    # --- Cross-user sharing (owner side) — folders.md § Sharing ---

    def share_button_visible(self) -> bool:
        """Whether the (single expanded row's) owner-side Share… button renders.

        ``is_visible_scrolled``, not a bare ``is_visible``: on windows the
        expanded row's body has grown (device-activity + place-editor
        sections landed 2026-08-21..29), pushing the share button below the
        600 DIP e2e window's fold — the same below-the-fold trap
        ``is_visible_scrolled``'s own docstring names (windows' ``is_visible``
        reads UIA ``IsOffscreen``, which a below-the-fold-but-real element
        also reports, precedent
        ``writer_uncapped_warning_visible`` below)."""
        return self.driver.is_visible_scrolled("folder-share-button")

    def shared_badge_visible(self) -> bool:
        """Whether the expanded row's "Shared · N" badge is shown (i.e. shared)."""
        return not self.driver.is_absent("folder-shared-badge")

    def shared_member_count(self) -> int:
        """Count the actors the set is shared with (the ``folder-member-item`` rows)."""
        return self.driver.count("folder-member-item")

    # --- Per-set device activity (ordinary sync change signal) ---

    def device_activity_item_count(self) -> int:
        """Count devices with recorded sync activity on the (single expanded)
        row (the ``folder-device-activity-item`` rows)."""
        return self.driver.count("folder-device-activity-item")

    def device_activity_change_count(self, index: int = 0) -> int:
        """The ``change_count`` shown for the device-activity row at ``index``."""
        return int(self.driver.get_text("folder-device-activity-count", index=index))

    def device_activity_labels(self) -> list[str]:
        """The device label of every device-activity row, in order — who the
        expanded set's recorded changes came from."""
        return [
            self.driver.get_text("folder-device-activity-label", index=k)
            for k in range(self.device_activity_item_count())
        ]

    def open_share_dialog(self) -> None:
        """Click the expanded row's Share… button, opening the reused
        recipient-picker dialog (``recipient-picker-input`` + ``folder-share-confirm``)."""
        self.driver.click("folder-share-button")
        time.sleep(0.5)

    def share_dialog_open(self) -> bool:
        """Whether the share dialog's reused recipient-picker input is visible."""
        return self.driver.is_visible("recipient-picker-input")

    def share_recipient(self, *, handle: str, actor_id_hex: str) -> None:
        """In the open share dialog, type a recipient identifier and confirm the
        share (``folder-share-confirm`` → ``FoldersAuthor::share_set``).

        ⚠ Per-app recipient-INPUT-KIND divergence (found 2026-07-12, NOT fixed
        here — the picker isn't unified across clients, priority #1; tracked
        internally (§ Gotchas) for the full trace):

        - linux's ``do_share_folder`` calls ``ConversationsClient::
          actor_by_handle`` UNCONDITIONALLY — it has NO actor-id path at all, so
          it needs the bare local-part ``handle`` (``domain: None`` always, no
          ``@``-parsing; a qualified string would fail to resolve).
        - apple (and every other UniFFI app sharing ``resolveRecipient`` /
          shared-Rust ``classify_recipient``) instead requires either
          ``local@domain`` (network round-trip: ``resolve_nest``'s SRV/DNS
          lookup for the domain, THEN ``resolve_handle`` against whatever node
          that returns) or a raw 64-hex actor id (classified locally, NO
          network call at all — ``resolveRecipient``'s `"actor_id"` branch
          returns immediately). A synthetic `handled_nest` test domain (e.g.
          "fauna.test") has no real DNS, so the handle path 404s at the OS
          resolver; the actor-id path sidesteps that entirely, hence
          ``actor_id_hex`` here.
        - tui accepts EITHER (it classifies a bare 64-hex string as an actor id
          locally and otherwise resolves the bare local-part handle), so the
          handle branch below covers it; it is the superset of both shapes above
          rather than a third divergence.
        """
        recipient = handle if (self.driver.is_linux() or self.driver.is_tui()) else actor_id_hex
        self.driver.clear_and_type("recipient-picker-input", recipient)
        self.driver.click("folder-share-confirm")
        time.sleep(1)

    def remove_shared_member(self, index: int = 0, row: int | None = None) -> None:
        """Remove the shared-with actor at ``index`` (rotates the content key).

        ⚠ **Pass ``row``** (the ``folder-row`` index :meth:`find_and_expand_folder`
        returns) once more than one folder row exists on the page — same
        reason as :meth:`set_conflict_policy`. ``FolderMemberRow`` lives inside the
        expanded row's ``.automationScope("folder-row", index:)`` container
        (the track-5/8 retrofit), so a bare ``folder-member-item[index]`` scope
        only resolves while it happens to be the ONLY expanded row on the page
        (found 2026-07-12 driving this for the first time on apple — a bare
        scope 404s there even with a single row, since a multi-level-scoped
        entry needs the FULL registered path, not just its innermost segment;
        tracked internally, § Gotchas). ``row=None`` is fine for a
        single-folder-row test (the common case) and preserves old behavior
        for clients not yet on the ``folder-row`` scope retrofit.
        """
        if row is not None and not self.driver.is_web():
            self.driver.click(
                "folder-member-remove-button",
                scope=self._member_scope(index, row),
            )
        else:
            # row=None, or web (see `_member_scope`'s carve-out: the expanded body
            # is a SIBLING <tr>, so a folder-row subtree query cannot reach it, and
            # at most one row is expanded so the flat index is unambiguous).
            self.driver.click("folder-member-remove-button", index=index)
        time.sleep(1)

    def member_handle(self, index: int = 0, row: int | None = None) -> str:
        """The shared-with actor's handle (or actor-id when handle-less) shown on
        the ``folder-member-item`` at ``index``. Scoped so it reads THAT member's
        ``folder-member-handle``, not the first one globally.

        ⚠ **Pass ``row``** — same reason and caveats as :meth:`remove_shared_member`.
        """
        scope = self._member_scope(index, row)
        return self.driver.get_text("folder-member-handle", scope=scope)

    def member_status(self, index: int = 0, row: int | None = None) -> str:
        """The shared-with actor's status ("Active") on the ``folder-member-item``
        at ``index`` (scoped read).

        ⚠ **Pass ``row``** — same reason and caveats as :meth:`remove_shared_member`.
        """
        scope = self._member_scope(index, row)
        return self.driver.get_text("folder-member-status", scope=scope)

    def _member_scope(self, index: int = 0, row: int | None = None) -> str:
        """The scoped path to one ``folder-member-item`` — same ``row`` caveats
        as :meth:`remove_shared_member`.

        ⚠ **Web takes the same carve-out as :meth:`set_conflict_policy`** — the
        ``folder-row`` prefix is DROPPED there, whatever ``row`` the caller passed.
        Web renders the folder list as an HTML ``<table>`` and the expanded body is
        a SIBLING ``<tr class="expanded-row">``, not a descendant of the ``<tr
        data-testid="folder-row">`` (a ``<tr>`` cannot contain another ``<tr>``), so
        a ``folder-row[i]/…`` Playwright subtree query can never resolve an element
        that structurally lives outside that row's own subtree.

        Dropping the prefix loses no precision on web, for the reason
        :meth:`set_conflict_policy` sets out: ``expandedFs`` is a single value
        (``FoldersSection.svelte::toggleExpand``), so at most ONE row is ever
        expanded and the member items of exactly one folder are in the DOM —
        ``folder-member-item[index]`` addresses the intended member unambiguously.

        Added 2026-09-20, after the first web-owner run of
        `test_folder_bound_flip_back.py` timed out here: the web carve-out existed
        for the conflict-policy picker since the ``row`` retrofit landed, but the
        member-scope builder never got it, so every ``row=``-scoped member action
        (:meth:`set_member_access`, :meth:`set_member_cap`, :meth:`member_access`)
        was structurally unrunnable on web — invisibly, because no web test drove
        one until then.
        """
        if row is not None and not self.driver.is_web():
            return f"folder-row[{row}]/folder-member-item[{index}]"
        return f"folder-member-item[{index}]"

    def member_access(self, index: int = 0, row: int | None = None) -> str:
        """The member row's access value (``'reader'``/``'writer'``) — the
        ``folder-member-role-select`` model value (multi-writer Phase 1;
        the select's MODEL strings are the wire values — a stable contract)."""
        return self.driver.get_text(
            "folder-member-role-select", scope=self._member_scope(index, row)
        )

    def set_member_access(self, value: str, index: int = 0, row: int | None = None) -> None:
        """Pick ``'reader'``/``'writer'`` on the member row's role select —
        drives ``fauna.folders.members.set_access`` (+ a roster re-read that
        repaints the row from the nest's authoritative role row)."""
        self.driver.select(
            "folder-member-role-select", value, scope=self._member_scope(index, row)
        )
        time.sleep(1)

    def set_member_cap(self, cap: str, index: int = 0, row: int | None = None) -> None:
        """Type a byte cap (or ``""`` = uncapped) into ``folder-member-cap-input``
        and commit it. The entry commits on activate (Enter); the agent's
        ``click`` on an editable emits exactly that activation, mirroring the
        SpinButton commit idiom."""
        scope = self._member_scope(index, row)
        self.driver.clear_and_type("folder-member-cap-input", cap, scope=scope)
        self.driver.click("folder-member-cap-input", scope=scope)
        time.sleep(1)

    def writer_uncapped_warning_visible(self, index: int = 0, row: int | None = None) -> bool:
        """Whether the member row shows the uncapped-writer warning
        (``folder-writer-uncapped-warning`` — visible iff access==writer with a
        blank cap; advisory, user-approved 2026-07-19).

        ``is_visible_scrolled``, not a bare ``is_visible``: the warning line
        adds height to an already-deep member row (folder row → member item →
        access row → warning), so a fresh promote-to-writer repaint can land it
        below the fold of the share dialog's scroll viewport — the same
        below-the-fold trap ``is_visible_scrolled``'s own docstring names for
        the admin-tab/family-tab nav rows (windows' ``is_visible`` reads UIA
        ``IsOffscreen``, which a below-the-fold-but-real element also reports)."""
        return self.driver.is_visible_scrolled(
            "folder-writer-uncapped-warning", scope=self._member_scope(index, row)
        )

    def pick_share_role(self, value: str) -> None:
        """Pick ``'reader'``/``'writer'`` on the OPEN share dialog's
        ``folder-share-role-select`` — the access granted to the recipient about
        to be confirmed (Reader is the default). No sleep: the dialog's own
        warnings repaint on the pick, and callers poll for the state they need."""
        self.driver.select("folder-share-role-select", value)

    def share_dialog_published_warning_visible(self) -> bool:
        """Whether the OPEN share dialog shows ``folder-writer-published-warning``.

        The share dialog is the only surface carrying an unscoped one: a member
        row's copy hangs under ``folder-member-item[i]`` and is read through
        :meth:`writer_published_warning_visible`. ``is_visible_scrolled`` for the
        same below-the-fold reason as :meth:`writer_uncapped_warning_visible` —
        the role select, then up to two warnings, stack inside the dialog."""
        return self.driver.is_visible_scrolled("folder-writer-published-warning")

    def writer_published_warning_visible(self, index: int = 0, row: int | None = None) -> bool:
        """Whether the member row shows the published-folder writer warning
        (``folder-writer-published-warning`` — visible iff access==writer AND the
        folder's content is readable beyond its members, i.e. audience ``public``
        or paywalled; advisory, user-approved 2026-09-25).

        State-based, so it is independent of the byte cap — unlike
        :meth:`writer_uncapped_warning_visible` — and the two stack.
        ``is_visible_scrolled`` for the same below-the-fold reason."""
        return self.driver.is_visible_scrolled(
            "folder-writer-published-warning", scope=self._member_scope(index, row)
        )

    def writer_published_warning_absent(self, index: int = 0, row: int | None = None) -> bool:
        """Whether the member row is ABSENT the published-folder writer warning — the
        honest NEGATIVE read (``driver.is_absent``): ``not writer_published_warning_visible``
        would run web's Playwright scroll-into-view wait to its bridge timeout on an
        element that is genuinely not there."""
        return self.driver.is_absent(
            "folder-writer-published-warning", scope=self._member_scope(index, row)
        )

    def share_dialog_published_warning_absent(self) -> bool:
        """The negative twin of :meth:`share_dialog_published_warning_visible` — the
        OPEN share dialog carries no published-folder writer warning (``is_absent``,
        for the same web scroll-wait reason as :meth:`writer_published_warning_absent`)."""
        return self.driver.is_absent("folder-writer-published-warning")

    def writer_published_warning_text(self, index: int = 0, row: int | None = None) -> str:
        """The member row's published-folder writer warning copy — which of the two
        ratified sentences (public ⇒ "anyone", paywalled ⇒ "subscribers") the
        shared decision picked."""
        return self.driver.get_text(
            "folder-writer-published-warning", scope=self._member_scope(index, row)
        )

    def shared_badge_text(self) -> str:
        """The expanded row's "Shared · N" badge text (the badge is an ExpanderRow
        suffix, visible even collapsed once populated)."""
        return self.driver.get_text("folder-shared-badge")

    # --- Cross-user sharing (recipient side) — folders.md § Sharing ---

    def pending_share_count(self) -> int:
        """Count the staged ("knocked") cross-user shares in the page-level "Shared
        with you" section (the ``folder-pending-share`` rows). A CONTACT's share
        auto-joins off the chat rail (the gate) and never appears here; only a
        STRANGER's share knocks."""
        return self.driver.count("folder-pending-share")

    def pending_share_text(self, index: int = 0) -> str:
        """The full text of the ``folder-pending-share`` row at ``index`` — the
        "Shared by ‹handle›" sharer-identity label (plus the accept/decline button
        captions the container joins in). A same-nest sharer shows their resolved
        handle; a cross-nest / handle-less sharer falls back to the shortened
        ``shared_by`` hex. Substring reads (``handle in pending_share_text()``)
        match the identity regardless of the surrounding button captions."""
        return self.driver.get_text("folder-pending-share", index=index)

    def wait_for_pending_shares(self, expected: int, timeout: float = PENDING_SHARES_WAIT_S) -> int:
        """Poll until the pending-share count reaches ``expected`` (or timeout),
        re-firing the Folders page's ``connect_map`` fetch each iteration by
        toggling Devices→Folders (the section is fetched on page-visible, not
        pushed). Returns the final observed count."""
        deadline = time.monotonic() + timeout
        count = self.pending_share_count()
        while count != expected and time.monotonic() < deadline:
            time.sleep(1)
            self.navigate_devices()
            self.navigate_folders()
            count = self.pending_share_count()
        return count

    def accept_pending_share(self, index: int = 0) -> None:
        """Accept the staged share at ``index`` (``folder-share-accept-button`` →
        join the MLS group off the chat rail + ack). The row then disappears."""
        self.driver.click("folder-share-accept-button", index=index)
        time.sleep(1)

    def decline_pending_share(self, index: int = 0) -> None:
        """Decline the staged share at ``index`` (``folder-share-decline-button`` →
        bare ack; the Welcome is dropped unprocessed, never joins). The row then
        disappears."""
        self.driver.click("folder-share-decline-button", index=index)
        time.sleep(1)

    def leave_shared_set(self, index: int = 0) -> None:
        """Leave the read-only shared-with-me set at ``index``
        (``folder-leave-button`` → the self-scoped ``fauna.folders.leave`` nest
        roster-drop + local ``MlsEngine::forget_group``). The row then disappears from
        the folders list (off the roster, ``list_owned_and_shared`` stops unioning
        it). The button is a suffix on the read-only member ``folder-row``."""
        self.driver.click("folder-leave-button", index=index)
        time.sleep(1)

    def get_include_paths(self) -> str:
        """Read back the include-paths entry on the expanded row."""
        return self.driver.get_text("folder-include-paths")

    def get_exclude_paths(self) -> str:
        """Read back the exclude-paths entry on the expanded row."""
        return self.driver.get_text("folder-exclude-paths")

    def set_include_paths(self, paths: str) -> None:
        """Set selective sync include paths."""
        self.driver.clear_and_type("folder-include-paths", paths)

    def set_exclude_paths(self, paths: str) -> None:
        """Set selective sync exclude paths."""
        self.driver.clear_and_type("folder-exclude-paths", paths)

    def save_paths(self) -> None:
        """Save selective sync path configuration."""
        self.driver.click("folder-save-paths")
        time.sleep(1)

    # ``set_frequency`` / ``get_frequency`` (the per-row ``folder-frequency-select``)
    # retired with phase 5 of the folders re-model (2026-08-20): the reconcile
    # cadence is a hard-coded constant, not a per-folder choice (file-sync.md
    # § Config, the phase-5 block).

    def set_conflict_policy(self, policy: str, index: int = 0, *, row: int | None = None) -> None:
        """Pick the per-set conflict policy on the folder row
        (``folder-conflict-policy-select``; file-sync.md § Conflicts).

        ``policy`` is the wire value — ``"auto"`` or ``"latest_wins_always"`` — which
        the resolving device reads off the authoritative ``folders.conflict_policy``
        row, so the write is observable in ``fauna.folders.list``.

        ⚠ **Pass ``row``** — the ``folder-row`` index :meth:`find_and_expand_folder`
        returns — **on every platform except web.** It scopes the pick to that row
        by real subtree containment (``scope=f"folder-row[{row}]"``), so the write
        can never land on another row regardless
        of what else is rendered. This is the durable fix for the ordering-dependent
        bug the legacy ``index`` param carried: this picker renders on EVERY sync-type
        row (backup-type rows render none), so a flat *occurrence* index only agrees
        with the ``folder-row`` index while no backup-type set sorts ahead of the
        target — AND, on platforms whose accessibility tree unmaps a collapsed row's
        body (linux `AdwExpanderRow`, windows expanded-only rendering), a leftover
        row left expanded by an earlier test changes *which* occurrences are even
        visible, so the flat index silently disagrees with what's actually
        selectable — an ordering-dependent 404 caught between two folders tests.
        ``scope`` sidesteps the whole class: it addresses the target row's subtree
        directly, never counts occurrences elsewhere.

        Windows renders this picker EXPANDED-ONLY (in the open row's body), so it
        rides the same `row`/scope path as linux — strictly more precise than
        the previous "always index 0" assumption (still correct when exactly one row
        is expanded; also correct if a leftover expanded row from an earlier test
        changes what's globally visible, since scope no longer counts globally).

        **Apple rides the same `row`/scope path too** (flipped 2026-08-25, row 153):
        the premise the old carve-out was written under — "no `.automationScope`
        container registered for `folder-row` on apple" — was stale.
        `FoldersContent.swift` registers `.automationScope(Ids.folderRow, index:)`
        on every row, and `test_automation_registry_lifecycle.py` had already been
        driving this very picker by ``scope="folder-row[i]"`` on apple directly,
        bypassing this method, for exactly that reason.

        ⚠ **Web carve-out — pass the flat ``index`` (always 0), never ``row``.**
        Web renders the folder list as an HTML `<table>`; the expanded body is a
        SIBLING `<tr class="expanded-row">`, not a descendant of the `<tr
        data-testid="folder-row">` (a `<tr>` cannot contain another `<tr>`), so
        `scope="folder-row[i]"` — a Playwright subtree query — can never resolve an
        element that structurally lives outside that row's own subtree. Safe
        unscoped: web's `expandedFs` is a single value
        (`FoldersSection.svelte::toggleExpand`), so at most ONE row is ever
        expanded — `folder-conflict-policy-select` therefore has 0 or 1 matches
        globally, and `index=0` always addresses the (only) visible one.
        """
        if row is not None and not self.driver.is_web():
            self.driver.select("folder-conflict-policy-select", policy,
                                scope=f"folder-row[{row}]")
        else:
            idx = 0 if self.driver.is_web() else index
            self.driver.select("folder-conflict-policy-select", policy, index=idx)
        time.sleep(1)

    def get_conflict_policy(self, index: int = 0, *, row: int | None = None) -> str:
        """Read back the per-set conflict policy. See the ``row``/``index`` and
        web-carve-out warnings on :meth:`set_conflict_policy`."""
        if row is not None and not self.driver.is_web():
            return self.driver.get_text("folder-conflict-policy-select",
                                         scope=f"folder-row[{row}]")
        idx = 0 if self.driver.is_web() else index
        return self.driver.get_text("folder-conflict-policy-select", idx)

    def conflict_policy_count(self) -> int:
        """How many rows render a ``folder-conflict-policy-select`` — one per
        folder (backup-type sets have no conflict policy, so this is *not*
        the same as :meth:`folder_count`)."""
        return self.driver.count("folder-conflict-policy-select")

    def set_default_conflict_policy(self, policy: str) -> None:
        """Pick the page-level default conflict policy in the "Sync defaults" section
        (``sync-default-conflict-policy-select``, unindexed — one control per page).

        This is the ``fauna.state.sync-prefs`` ``default_conflict_policy``: it is stamped onto
        **newly created** sets and does not retro-edit existing ones."""
        self.driver.select("sync-default-conflict-policy-select", policy)
        time.sleep(1)

    def get_default_conflict_policy(self) -> str:
        """Read back the page-level default conflict policy."""
        return self.driver.get_text("sync-default-conflict-policy-select")

    def toggle_webdav(self) -> None:
        """Flip the per-set ``folder-webdav-toggle`` on the expanded folder row (webdav-server.md § Independent enablement point 2).

        The checkbox's onchange drives the full serve orchestration —
        ``FoldersAuthor::serve_set`` (serve_enable/disable + the MSEK-sealed
        ``WebdavKeysBlob`` re-provision) through the conversations-rail wasm/FFI
        face — then refreshes the snapshot so the toggle reflects the persisted
        nest ``folders.webdav_enabled`` flag. A single click flips based on the
        checkbox's current state. Async — callers poll the nest ground truth
        (``fauna.folders.list``)."""
        self.driver.click("folder-webdav-toggle")
        time.sleep(1)

    def webdav_toggle_visible(self) -> bool:
        """Whether the expanded row's ``folder-webdav-toggle`` is currently
        reachable. Some clients (e.g. linux) fully rebuild the folder list on
        every `DevicesMachine` observer tick, collapsing the expander back
        closed after a write — so a caller doing a second action on the same
        row should re-expand it (`find_and_expand_folder`) when this reads
        `False`, rather than assume the expand state survives the refresh.

        ``is_visible_scrolled``, not a bare ``is_visible`` — same
        below-the-fold trap as ``share_button_visible`` above: windows' body
        has grown enough (include/exclude/conflict/residency/audience/website
        rows above the toggle) to push it past the 600 DIP e2e window's fold
        on a freshly-created folder. A
        collapsed row still reads False here — ``is_visible_scrolled`` falls
        through to a plain ``is_visible`` on a scroll-to `LookupError`/
        `TimeoutError`, so the re-expand branch above keeps working."""
        return self.driver.is_visible_scrolled("folder-webdav-toggle")

    def webdav_toggle_enabled(self) -> bool:
        """Whether the expanded row's ``folder-webdav-toggle`` is *interactive*.

        Serving a set over WebDAV seals the ``WebdavKeysBlob`` under the actor's
        MSEK, which is minted when mail is first enabled — so an actor with no
        mail sees the toggle **disabled with a "set up mail first" hint** rather
        than clicking into a ``NoMsek`` failure (webdav-server.md § Independent
        enablement point 2). Reads the uniform ``/element/enabled`` contract (web
        ``disabled`` attr, GTK ``!SENSITIVE``, WinUI ``!IsEnabled``, …).

        Query it only on a row you know is rendered — a *missing* element also
        reads as not-enabled, so assert :meth:`webdav_toggle_visible` first if
        that distinction matters."""
        return self.driver.is_enabled("folder-webdav-toggle")

    # --- Apple on-demand presence (`folder-on-demand-toggle`) -----------------
    #
    # macos+ios `platform_elements` (ui.yaml § folders): apple's on-demand
    # affordance is SET-level, not location-level — an FP domain cannot take over
    # an arbitrary user directory, so there is exactly one local presence per set
    # per device (on-demand-files.md § Apple File Provider binding). The other
    # five apps render nothing here, which is why the covering test carries only
    # the macos/ios app markers rather than an app-gated skip.

    def on_demand_toggle_visible(self) -> bool:
        """Whether the expanded row renders ``folder-on-demand-toggle``.

        Absent on a row whose set has a bound always-resident folder: the binding
        IS that set's one local presence, so the row renders the arbitration
        caption instead of the toggle (bound folder > FP domain,
        `on-demand-files.md` § Apple File Provider binding). Also absent on the
        five apps that declare the element off their surface, and on a non-Sync
        row.
        """
        return not self.driver.is_absent("folder-on-demand-toggle")

    def on_demand_toggle_state(self) -> str | None:
        """``"on"`` / ``"off"`` — the toggle's live checked state.

        Read over the uniform ``/element/attr?attr=value`` contract, which the
        apple bridge answers from the Entry's `value` closure (the same single
        registration that carries the click — the `folder-webdav-toggle`
        template). Returns ``None`` when the element is absent, so a caller that
        needs to tell "off" from "not rendered" asserts
        :meth:`on_demand_toggle_visible` first.
        """
        return self.driver.get_attr("folder-on-demand-toggle", "value")

    def toggle_on_demand(self, *, timeout: float = 10.0) -> str | None:
        """Flip the per-set on-demand presence on the expanded owner row,
        returning the settled ``"on"``/``"off"`` state.

        Persists to the device-local ``FileProviderDomainPrefs`` store (sanctioned
        device-local config — never nest state, so there is no nest ground truth
        to poll) and re-drives the app's FP reconcile through the platform hook.

        Convention 14: the wait is a deadline poll on the toggle's OWN state, not
        a settle-sleep. The store write and the `isOn` assignment both happen on
        the main actor before `/element/click` answers, but the Entry's `value`
        closure is re-registered by the SwiftUI view update that follows, so the
        read can briefly still answer from the pre-flip registration. Polling for
        "it no longer reads what it read before" is latency-independent: it
        returns the instant the new registration lands and cannot pass by merely
        waiting long enough.
        """
        before = self.on_demand_toggle_state()
        self.driver.click("folder-on-demand-toggle")
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            settled = self.on_demand_toggle_state()
            if settled is not None and settled != before:
                return settled
            # sleep-ok: poll interval inside the deadline loop above, not a
            # settle-sleep — each tick re-reads the live registration and returns
            # the instant the flip is visible (convention 14's deadline-poll
            # mechanism).
            time.sleep(0.2)
        return self.on_demand_toggle_state()

    # --- Audience + website serving (folders re-model phase 4 slice 4d) ---
    #
    # Both render on the expanded OWNER row of ANY folder — audience is the
    # folder's identity, not a per-mode serving option, and the website toggle is
    # the only door to a website folder since the wizard's mode step retired.
    # UX contract: `ui/folders.md` § Audience and website serving.

    def audience_select_visible(self) -> bool:
        """Whether the expanded row's ``folder-audience-select`` renders.

        ``is_visible_scrolled`` for the same below-the-fold reason as
        :meth:`website_toggle_visible` — the audience row sits directly above
        the website row — and with the same stake: :meth:`make_public` and the
        flip-back journeys expand on a False read, and a toggling expand on an
        already-open row closes it."""
        return self.driver.is_visible_scrolled("folder-audience-select")

    def audience_current_value(self) -> str:
        """The audience select's current model value on the expanded row — the
        wire spelling (``private`` / ``shared`` / ``public``).

        Note this is the NORMALIZED reading, not the raw column: a folder
        with no audience set sends nothing (the nullable column), and the apps
        resolve that fail-closed to ``private`` so the select's value is always one of the
        options it offers."""
        return self.driver.get_text("folder-audience-select")

    def set_audience(self, value: str) -> None:
        """Pick ``value`` on the expanded row's ``folder-audience-select``.

        ⚠ **Picking ``public`` does NOT publish the folder** — it arms the
        declassify confirm and writes nothing. Use :meth:`make_public` for the
        whole gesture, or follow this with :meth:`confirm_public`. Every other
        value commits directly.

        The option set is per-row and excludes what the nest would refuse: an
        unbound folder offers ``private``/``public``, a group-bound one offers
        ``shared`` (its current state, not selectable) and ``public``."""
        self.driver.select("folder-audience-select", value)

    def declassify_confirm_visible(self) -> bool:
        """Whether the declassify confirm (``folder-audience-public-confirm``) is
        currently armed and painted. Present only between picking ``public`` and
        answering it — the ``folder-delete-confirm`` pattern."""
        return self.driver.is_visible("folder-audience-public-confirm")

    def audience_hint_text(self) -> str:
        """The expanded row's ``folder-audience-hint`` — the line
        ``audience_hint(bound, current)`` resolves to, riding the same inputs as
        the option set (a shared folder's line says the sharing has to go
        first)."""
        return self.driver.get_text("folder-audience-hint")

    def audience_options(self) -> list[str] | None:
        """The option set ``folder-audience-select`` offers on the expanded row
        (``audience_options(bound, current)``), or None where the driver cannot
        read a picker's options yet."""
        return self.driver.option_texts("folder-audience-select")

    def declassify_warnings(self) -> tuple[str, str]:
        """The armed declassify dialog's two consequence lines, in order:
        ``folder-audience-public-names-warning`` (names and paths go public
        too) and ``folder-audience-public-reseal-warning`` (going private again
        protects only what is added afterwards). Present exactly while the
        confirm is."""
        return (
            self.driver.get_text("folder-audience-public-names-warning"),
            self.driver.get_text("folder-audience-public-reseal-warning"),
        )

    def audience_unattested_visible(self) -> bool:
        """Whether the expanded row paints ``folder-audience-unattested`` — the
        owner's "public, but this app can't confirm you made it public" status.
        Present exactly while the nest reports the folder public and its owner
        attestation does not verify under this seat's own actor id
        (``FolderSummary::is_public_unverified_for``)."""
        return self.driver.is_visible("folder-audience-unattested")

    def reconfirm_public(self) -> None:
        """Press ``folder-audience-reconfirm-button``. Like picking ``public`` on
        the select it only ARMS the declassify confirm — follow with
        :meth:`confirm_public`, whose answer re-mints the attestation."""
        self.driver.click("folder-audience-reconfirm-button")

    def confirm_public(self) -> None:
        """Answer the armed declassify confirm — the explicit owner confirm the
        public audience is gated on (`principles.md` § The user always controls
        their data). This is what actually flips the audience.

        Async — callers poll the nest ground truth (``fauna.folders.list`` →
        ``audience``), or use :meth:`await_audience`."""
        self.driver.click("folder-audience-public-confirm")

    def make_public(self, folder: str) -> int:
        """The whole user gesture: expand ``folder``, pick ``public``, and answer
        the confirm.

        Asserts the confirm actually appeared, because that arming is the
        product invariant this control exists for — a regression that published
        on the bare select would otherwise sail through a test that only checked
        the end state.

        ⚠ Expands only if the row's body is not already showing.
        ``find_and_expand_folder`` TOGGLES, so calling it on an already-expanded
        row closes the body and every gesture below then addresses nothing —
        the trap ``helpers/folder_content.bind_location_under_set`` documents,
        and the one this helper cost a run to rediscover."""
        idx = next(
            (i for i in range(self.folder_count()) if folder in self.folder_title(i)),
            None,
        )
        if idx is None:
            raise AssertionError(f"folder {folder!r} not found among the rows")
        if not self.audience_select_visible():
            self.expand_folder(idx)
        self.set_audience("public")
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if self.declassify_confirm_visible():
                break
            time.sleep(0.2)
        else:
            raise AssertionError(
                "picking `public` did not arm folder-audience-public-confirm — "
                "the declassify confirm is the gate on the one audience that "
                "rests unsealed, and it must not be bypassable"
            )
        self.confirm_public()
        return idx

    def website_toggle_visible(self) -> bool:
        """Whether the expanded row's ``folder-website-toggle`` renders.

        ``is_visible_scrolled``, not a bare ``is_visible`` — the same
        below-the-fold trap as ``share_button_visible`` and
        ``webdav_toggle_visible``: on windows the website row sits under the
        include/exclude/residency/audience rows, past the 600 DIP e2e window's
        fold, and windows' ``is_visible`` reads UIA ``IsOffscreen``, which a
        real but scrolled-out-of-view toggle also reports.
        A false read is not harmless here: callers re-expand on False, and
        ``find_and_expand_folder`` TOGGLES, so the re-expand closes the very
        body the toggle is in. A collapsed row still reads False —
        ``is_visible_scrolled`` falls through to a plain ``is_visible`` on a
        scroll-to `LookupError`/`TimeoutError`."""
        return self.driver.is_visible_scrolled("folder-website-toggle")

    def website_toggle_enabled(self) -> bool:
        """Whether the website toggle is interactive.

        It stays ENABLED even while the folder is neither public nor paywalled —
        the flag publishes the folder's head and the audience decides who may
        read it, so the setting is real and merely inert there (the row hints
        instead of disabling). A test asserting "disabled until public" would be
        asserting the opposite of the ratified UX."""
        return self.driver.is_enabled("folder-website-toggle")

    def website_hint_text(self) -> str:
        """The expanded row's ``folder-website-hint`` — the tri-state
        ``website_serve_hint(audience, paywalled, address_enabled)`` resolves
        to: no readable audience, web address off (naming Settings → Web), or
        served; the combined hedge while the address flag is unknown."""
        return self.driver.get_text("folder-website-hint")

    def toggle_website(self) -> None:
        """Flip the expanded row's ``folder-website-toggle`` — "serve this folder
        as your website" (``FoldersClient::set_website_enabled``, persisted as
        ``folders.website_enabled``).

        A single click flips based on the current state. Async — callers poll the
        nest ground truth (``fauna.folders.list`` → ``website_enabled``)."""
        self.driver.click("folder-website-toggle")

    # --- Web paywall (website-enabled rows only) — folders.md § Web paywall ---

    def paywall_select_visible(self) -> bool:
        """Whether the expanded row's ``folder-paywall-tier-select`` renders.
        Only website-enabled rows carry it — keyed on ``folder-website-toggle``'s
        state, never on the retired ``mode = "web"`` spelling (the structural
        sibling of the webdav toggle)."""
        return self.driver.is_visible("folder-paywall-tier-select")

    def paywall_select_enabled(self) -> bool:
        """Whether the paywall select is interactive. A creator with no
        subscription tiers sees it DISABLED with a "create a tier first" hint —
        there is nothing to paywall to. Reads the uniform ``/element/enabled``
        contract. Assert :meth:`paywall_select_visible` first if the
        missing-vs-disabled distinction matters."""
        return self.driver.is_enabled("folder-paywall-tier-select")

    def set_paywall_tier(self, tier: str) -> None:
        """Pick ``tier`` on the expanded website-enabled row's
        ``folder-paywall-tier-select`` (folders.md § Web paywall).

        The select's onchange drives the full paywall orchestration —
        ``FoldersAuthor::paywall_set`` (content-key genesis/re-seal + the
        web-serve-holder ``content.read{folder:set}`` grant mint + the nest
        ``folders.web_paywall_tier`` flag) — then refreshes the snapshot. The
        model value is the tier NAME (the value/label DropDown split the
        conflict select uses), so pass the tier's name. Async — callers
        poll the nest ground truth (``fauna.folders.list`` → ``web_paywall_tier``)."""
        self.driver.select("folder-paywall-tier-select", tier)
        time.sleep(1)

    def paywall_current_value(self) -> str:
        """The paywall select's current model value on the expanded row — a tier
        name once paywalled, or the empty-string placeholder sentinel while the
        set is still public (v1 set-only)."""
        return self.driver.get_text("folder-paywall-tier-select")

    # --- The nest place's snapshot policy (folders re-model phase 2 slice e) ---
    #
    # Four controls in the EXPANDED row's body, on any folder, saved together —
    # the nest applies the policy whole, so a knob left blank clears to unset.
    # Behavior owner: `docs/goal/behavior/backup-restore.md` § 8b.

    # All five resolve against the ONE expanded row, like every other body
    # control here (`find_and_expand_folder`'s contract: only one row is
    # expanded at a time, so the unindexed body widgets resolve uniquely).

    def set_nest_snapshots(self, value: str) -> None:
        """Pick the nest place's keeps-snapshots knob: `default` | `on` | `off`.

        `default` is UNSET — "nothing authoritative said", the nest-wide
        behavior — and is where the knob returns, not a synonym for `off`.
        """
        self.driver.select("folder-nest-snapshots-select", value)

    def set_nest_quiet(self, secs: str) -> None:
        """Type the nest place's quiet period. Empty string = unset."""
        self.driver.clear_and_type("folder-nest-quiet-input", secs)

    def set_nest_retention(self, snapshots: str = "", days: str = "") -> None:
        """Type the two retention bounds. Empty = that bound unset; both empty
        clears the policy to keep-everything (never "keep zero")."""
        self.driver.clear_and_type("folder-nest-retention-snapshots", snapshots)
        self.driver.clear_and_type("folder-nest-retention-days", days)

    def set_version_retention(self, count: str = "", days: str = "") -> None:
        """Type the version-retention pair (`folders.version_retention`, the
        SIBLING policy family — bounds file-version history, never snapshots;
        `file-versions.md` § Retention). Empty = that bound unset; both empty
        clears the policy to keep-everything (never "keep zero")."""
        self.driver.clear_and_type("folder-version-retention-count", count)
        self.driver.clear_and_type("folder-version-retention-days", days)

    def save_nest_place(self) -> None:
        """Commit the nest-place + version-retention controls with one
        `fauna.folders.update` (each policy family sent whole)."""
        self.driver.click("folder-nest-save-button")

    def nest_snapshots_value(self) -> str:
        """The keeps-snapshots knob's current wire value."""
        return self.driver.get_text("folder-nest-snapshots-select")

    # --- Content residency (folders re-model phase 5) ---
    #
    # The nest-place editor block's consent-gated sibling. Unlike the four knobs
    # above it applies ON CHANGE rather than on the save button — its own
    # `fauna.folders.update` field — and the flip to Metadata-only is the one
    # that deletes the nest's copy of the folder's content, so it is armed
    # rather than committed. UX contract: `behavior/file-sync.md` § Content
    # residency, which states it follows the `folder-audience-public-confirm`
    # pattern exactly.

    def residency_current_value(self) -> str:
        """The residency select's current model value on the expanded row — the
        wire spelling (``full`` / ``metadata_only``).

        NORMALIZED, not the raw column: an empty
        residency is the live default, and the apps resolve it fail-closed to ``full`` (never
        stop bytes resting on an unparseable value), so the select's value is
        always one of the options it offers."""
        return self.driver.get_text("folder-nest-residency-select")

    def set_residency(self, value: str) -> None:
        """Pick ``value`` on the expanded row's ``folder-nest-residency-select``.

        ⚠ **Picking ``metadata_only`` does NOT flip the folder** — it arms the
        confirm and writes nothing. Follow it with :meth:`confirm_residency`.
        ``full`` (the flip back) commits directly."""
        self.driver.select("folder-nest-residency-select", value)

    def residency_confirm_visible(self) -> bool:
        """Whether the residency confirm (``folder-residency-confirm``) is armed
        and painted. Present only between picking ``metadata_only`` and
        answering it — the ``folder-audience-public-confirm`` pattern."""
        return not self.driver.is_absent("folder-residency-confirm")

    def confirm_residency(self) -> None:
        """Answer the armed residency confirm — the explicit owner consent the
        metadata-only flip is gated on (v1 has no custody-inferred eviction, so
        the owner's consent is the ONLY gate). This is what actually flips it,
        and what makes the nest drop its chunk bytes for the folder.

        Async — callers poll the nest ground truth (``fauna.folders.list`` →
        ``residency``)."""
        self.driver.click("folder-residency-confirm")

    # --- Exclusive editing (folder-exclusive-editing-toggle + folder-lease-status) ---
    #
    # `ui/folders.md` § Exclusive editing. The toggle is the owner's, on the
    # expanded body; the status line rides the row HEADER (a reader member has
    # no body) and is present only while exclusive editing is on.

    def exclusive_editing_state(self) -> str | None:
        """``"on"`` / ``"off"`` — the expanded row's toggle as painted, which is
        the nest's value (the toggle is non-optimistic). ``None`` when absent."""
        return self.driver.get_attr("folder-exclusive-editing-toggle", "state")

    def toggle_exclusive_editing(self) -> None:
        """Flip the expanded owner row's exclusive editing
        (``FoldersClient::set_exclusive_editing``). Async — callers poll the
        nest (``fauna.folders.list`` → ``exclusive_editing``)."""
        self.driver.click("folder-exclusive-editing-toggle")

    def lease_status_text(self, row: int) -> str | None:
        """The ``folder-lease-status`` line on ``folder-row[row]``, or ``None``
        while it is absent (exclusive editing off)."""
        if self.driver.is_absent("folder-lease-status", scope=f"folder-row[{row}]"):
            return None
        return self.driver.get_text("folder-lease-status", scope=f"folder-row[{row}]")

    # --- Folder wizard ---

    def wizard_set_name(self, name: str) -> None:
        """Set the folder name in the wizard."""
        self.driver.clear_and_type("wizard-name-input", name)

    # The wizard has no type step and no per-device role picker (folders re-model
    # phase 2 slice e — a folder has no type). The transitional `wizard_has_mode_step`
    # probe and the `wizard_select_sync/backup/web` + `wizard_set_device_role`
    # gestures that spanned the six apps' trickle-down window were deleted when the
    # last app (windows, 2026-08-21) dropped the step: a probe that outlives its
    # window silently makes a real regression look like lag. What a mode used to
    # imply is a seat's place flags (`wizard_set_device_flag`) plus the row's nest
    # place policy (`set_nest_*`); website serving is the row's audience/website
    # controls (`test_folder_audience_control.py`).

    def wizard_check_device(self, index: int = 0) -> None:
        """Toggle a device enrollment checkbox in the wizard."""
        self.driver.click("wizard-device-check", index=index)

    def wizard_set_device_flag(self, flag: str, on: bool, index: int = 0) -> None:
        """Set one of device `index`'s three place flags in the wizard.

        `flag` is `originates` | `accepts` | `applies-deletes`
        (`fauna_protocol::folders::PlaceFlags`; folders re-model § Places). These
        REPLACED the role picker: a live user called the bare Source/Sync/Backup/
        Mirror labels "a completely incomprehensible list of things" (2026-08-05),
        and the design's answer is checkboxes that each say what they do.

        The boxes are toggles, so this reads the current state and clicks only
        when it differs — driving it twice must not undo the caller's intent.
        """
        eid = f"wizard-device-{flag}"
        if self.driver.get_attr(eid, "state", index=index) != ("on" if on else "off"):
            self.driver.click(eid, index=index)

    # ``wizard_select_frequency`` (``wizard-frequency-option``) retired with phase 5
    # of the folders re-model (2026-08-20) — the wizard has no cadence step; the
    # ``wizard_set_retention_*`` helpers (``wizard-retention-*``) left with it, since
    # the retired backup-type retention inputs lived inside that step on the last
    # app still painting them (windows). Retention is the nest place's policy,
    # edited on the row via :meth:`set_nest_place`.

    def wizard_next(self) -> None:
        """Click the wizard next button, once the wizard will accept it.

        ⚠ `wait_until_enabled`, not a bare `click`. Next on the name step is
        sensitive only while `name_snapshot().continue_enabled` — a
        non-empty name — and the app learns the name through its own render
        loop: on linux the entry's `connect_changed` calls
        `FolderWizardMachine::set_name`, and the button's `set_sensitive` runs
        one observer tick later in `wizard.rs::render`. So the gesture pair
        `wizard_set_name(...)` + `wizard_next()` races that tick, and the click
        lands on a control the user could not yet have pressed.

        That is a TEST bug, not a registration bug: the predicate is correct —
        a user genuinely cannot advance with an empty name — and the same
        `create_folder_via_wizard` call passed in
        `test_folder_conflict_policy_round_trip` in the very sweep where nine
        siblings 409'd on it,
        which is what proves the disabled state transient rather than stuck.
        Before convention 11's actuation gate went default-refusing the click
        was simply swallowed, so the race was invisible; now it is a 409, which
        is the gate telling the truth. Same cause and same fix as apple's four
        violations.

        The wait is on STATE (`is_enabled` polled to a generous deadline), never
        on elapsed time — convention 14; a healthy run returns on the first poll.
        """
        self.driver.wait_until_enabled("wizard-next-button")
        self.driver.click("wizard-next-button")

    def wizard_back(self) -> None:
        """Click the wizard back button."""
        self.driver.click("wizard-back-button")

    def wizard_create(self) -> None:
        """Click the wizard create button to finish.

        Same enabled-wait as :meth:`wizard_next`, for the same reason: Create is
        sensitive only while `review_snapshot().create_enabled`, set one observer
        tick after the Review step paints (`wizard.rs::render`). Waiting for
        `wizard-create-button` to be *visible* — which
        `create_folder_via_wizard` does — proves the step swapped, not that the
        machine has finished deciding it can submit.
        """
        self.driver.wait_until_enabled("wizard-create-button")
        self.driver.click("wizard-create-button")
        time.sleep(1)

    def _wait_visible(self, element_id: str, timeout: float = WIZARD_STEP_WAIT_S) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.driver.is_visible(element_id):
                return
            time.sleep(0.3)
        raise TimeoutError(f"{element_id} did not appear within {timeout}s")

    def create_folder_via_wizard(
        self,
        name: str,
        *,
        device_indices: list[int] | None = None,
    ) -> int:
        """Drive the folder wizard end-to-end and return the resulting count.

        Walks Name → Devices → Review → create, waiting on each step's
        anchor element so the page-swap (observer-driven on Linux) has settled
        before the next gesture. On `submit()` success the dialog closes and the
        folder list refreshes; this polls until a row carrying `name` appears
        (folder names are globally unique) and returns the count then. Waiting
        on "the count grew" instead let another row's arrival — a lagging
        refresh, or linux's offline-share group-scope rows — stand in for this
        one, and the caller's next read of `name` then missed.

        Three steps since phase 5 (2026-08-20) retired the scan-frequency step
        (file-sync.md § Config, the phase-5 block). No ``mode`` parameter
        either: a folder has no type
        (phase 2 slice e, on every app since 2026-08-21) — the machine creates
        every folder ``sync``, and what a mode used to imply is the seats' place
        flags plus the row's nest-place policy, set after create.
        """
        self.add_folder()
        self._wait_visible("wizard-name-input")

        # Step 1 — the name.
        self.wizard_set_name(name)
        self.wizard_next()

        # Step 2 — device places (optional enrollment).
        for i in device_indices or []:
            self.wizard_check_device(i)
        self.wizard_next()

        # Step 3 — review + create. (Retention is the nest place's policy since
        # slice e, edited on the row via `set_nest_place`; the cadence step that
        # used to sit here retired with phase 5.)
        self._wait_visible("wizard-create-button")
        self.wizard_create()

        try:
            self.wait_for_folder_row(name)
        except AssertionError as e:
            raise TimeoutError(f"Folder {name!r} did not appear after create: {e}") from e
        return self.folder_count()

    # ── The co-present offline share-initiation affordance ────────────────────
    # `p2p.md` § Offline share initiation, contract point 1. Page-level on
    # Folders, directly under the "Shared with you" knock list. Present only
    # where the plane is compiled in AND rule 7's `p2p-share` nest brake is
    # off — `offline_share_available()` is how a test asks, rather than
    # skipping on a driver type (convention 7).

    def offline_share_available(self, timeout: float = 10.0) -> bool:
        """Whether this app renders the co-present share affordance at all.

        `False` is a legitimate answer, not a failure: the section is absent
        when the app has no usable identity secret or when the nest does not
        advertise `p2p-share`. Callers gate on it rather than on which app
        they are, so the day another app lifts the affordance the test covers
        it with no edit.

        ⚠ **Polls to a deadline before answering `False`.** A bare
        `is_visible` here would read a page that is merely still painting as
        "the affordance is absent", and the caller's response to `False` is to
        SKIP — so the whole file would pass vacuously on a slow machine while
        looking like a legitimate capability skip. The budget is generous and
        the assertion is on state, never on elapsed time (convention 14); the
        `True` path returns as soon as the element appears, so a healthy run
        pays nothing."""
        deadline = time.monotonic() + timeout
        while True:
            if self.driver.is_visible("offline-share-button"):
                return True
            if time.monotonic() >= deadline:
                return False
            time.sleep(0.3)

    def advance_offline_share_clock(self, offset_secs: int) -> None:
        """Move THIS seat's ceremony ADMISSION clock — the `now` a receive-act
        expectation is minted and judged against — by `offset_secs`.

        The window a receive act opens is a 15-minute Rust constant and never a
        knob (`GROUP_CEREMONY_EXPECTATION_TTL_SECS`), so this is the only way a
        journey can reach "someone arriving after it has lapsed is refused like
        a stranger" (`p2p.md` § Offline share initiation) — convention 14's
        fake clock, never a sleep out of a real quarter hour.

        Per-PROCESS, so it is called on the seat whose admission is under test
        (the recipient); the initiator's own clock is untouched. Pass `0` to
        reset — the offset is process-wide and nothing auto-resets it, so a
        leftover value would lapse the next expectation this app mints and read
        as an unrelated ceremony being mysteriously refused.
        """
        self.driver.call_command(
            "offline_share_advance_clock",
            {"now_offset_secs": int(offset_secs)},
        )

    def drop_offline_share_connections(self) -> int:
        """Drop every connection a counterpart has open to THIS seat's ceremony
        listener, keeping the listener up, and return how many were dropped.

        This is the link between two devices failing part-way through a
        ceremony, which a journey has no other way to cause. It is how "the
        share picks up again without either person entering the code a second
        time" (`p2p.md` § Offline share initiation) gets a witness. Call it on
        the seat whose connections should go (the recipient, which is what the
        initiator dialed). A seat with nothing bound fails loudly on the app's
        own error element.
        """
        result = self.driver.call_command("offline_share_drop_connections")
        return int(result or 0)

    def hold_share_serves(self, hold: bool) -> None:
        """Turn THIS seat's share-plane SERVE hold on or off.

        While it is on, the next manifest a peer asks this seat for parks
        unanswered, and ``share_serve_tally()["parked"]`` rises above zero.
        That makes "part-way through a transfer" a state a journey can wait for
        and then cut with :meth:`drop_offline_share_connections` (convention 14,
        no race against a millisecond transfer). Lifting the hold fails the
        parked request, which is never counted as served. Process-wide, and
        nothing auto-resets it, so always lift it before asserting arrival.
        """
        self.driver.call_command("offline_share_hold_serves", {"hold": bool(hold)})

    def share_serve_tally(self) -> dict:
        """What THIS seat's share plane has served, as
        ``{"manifests": {path: n}, "chunks": {path: n}, "parked": n, "held": b}``
        (``fauna_e2e_agent::SHARE_SERVE_TALLY_KEY``).

        Raises when the app does not publish the key: an app without the leg
        is a refusal (convention 11), never an empty tally that a "served once"
        assertion could read as success.

        Forces a fresh state push first (an empty ``set_state`` patch — the
        ``list_threads`` idiom). A serve happens on the listener's own task and
        raises no app event, so without the push the read returns whatever the
        last command ack published: the first run of the resume journey read a
        tally from BEFORE the resumed files were served and saw them at zero.
        """
        self.driver.set_state({})
        value = self.driver.get_state("share_serve_tally")
        if not isinstance(value, dict):
            raise AssertionError(
                "this app does not publish 'share_serve_tally' (the share-plane "
                f"serve tally); got {value!r}"
            )
        return value

    def probe_shared_set(
        self, peer_code: str, group_id_hex: str, manifest_hashes=()
    ) -> dict:
        """Read one shared set straight off another seat's share plane, as THIS
        seat's identity, and return what came back
        (``fauna_sync_engine::share_probe::ShareProbeReport``): ``dialed``,
        ``admitted``, ``admit_error``, ``rows``, ``paths``, ``rows_error``,
        ``manifest_hashes``, ``manifests``, ``manifest_errors``.

        The peer is named by its compare code (``offline-share-own-code``) and
        the set by its raw MLS group id. The probe keeps asking after a refused
        admission, as a hostile client would, and asks for every one of
        ``manifest_hashes`` plus any the served rows name. This seat's listener
        must already be bound (either co-present panel binds it); without one
        the app fails loudly and this raises.
        """
        import json

        result = self.driver.call_command(
            "offline_share_probe_set",
            {
                "peer_code": peer_code,
                "group_id_hex": group_id_hex,
                "manifest_hashes": list(manifest_hashes),
            },
        )
        # The agent publishes a JSON machine result already parsed; a raw string
        # is the same report one decode earlier.
        if isinstance(result, str):
            result = json.loads(result)
        if not isinstance(result, dict):
            raise AssertionError(
                f"offline_share_probe_set returned no report ({result!r}); "
                f"error-message={self.driver.get_text('error-message')!r}"
            )
        return result

    def open_offline_share(self) -> None:
        """`offline-share-button` — the INITIATOR panel. Binds this session's
        ceremony listener on first open (where the brake is read), so allow it
        a moment to settle before reading the panel."""
        self.driver.click("offline-share-button")
        self._wait_visible("offline-share-own-code")

    def open_offline_receive(self) -> None:
        """`offline-receive-button` — the RECIPIENT panel, same bind."""
        self.driver.click("offline-receive-button")
        self._wait_visible("offline-share-own-code")

    def offline_share_own_code(self) -> str:
        """`offline-share-own-code` — this device's compare code, which IS its
        actor key. The whole security of the ceremony is that a human reads
        this aloud and the other person checks it, so a test that asserts
        anything here should assert it equals the actor id, never merely that
        it is non-empty."""
        return self.driver.get_text("offline-share-own-code").strip()

    def type_offline_peer_code(self, code: str) -> None:
        """`offline-share-peer-code-input` — the counterpart's code. Kept
        verbatim; the whitespace-forgiving parse happens at commit."""
        self.driver.clear_and_type("offline-share-peer-code-input", code)

    def offline_share_begin_enabled(self) -> bool:
        """Whether `offline-share-begin-button` is interactive — it is gated on
        a code that parses AND on no ceremony being in flight (a second Begin
        would mint a competing scope).

        Query it only on an open initiator panel: a *missing* element also
        reads as not-enabled."""
        return self.driver.is_enabled("offline-share-begin-button")

    def offline_receive_expect_enabled(self) -> bool:
        """The recipient panel's mirror of :meth:`offline_share_begin_enabled`."""
        return self.driver.is_enabled("offline-receive-expect-button")

    def wait_offline_share_begin_enabled(self) -> None:
        """Wait for `offline-share-begin-button` to BECOME enabled — the positive
        form of :meth:`offline_share_begin_enabled`, for after a counterpart's
        code has been typed.

        Begin's gate is derived, not typed: the app recomputes it in the
        peer-code field's change handler, so a bare read straight after
        :meth:`type_offline_peer_code` can land before that repaint. The state is
        what a test means, not the instant — asserting the instant is the
        latency-dependent shape convention 14 retires (a bare read went red once
        in a whole-set windows run and green alone, 2026-09-21; the cause is
        unproven, which is why this waits on the state AND classifies a miss).
        The *refusal* reads keep the bare form: waiting for a button to STAY
        disabled proves nothing."""
        wait_until(
            self.offline_share_begin_enabled,
            budgets.UI_SETTLE_S,
            diagnose=lambda: self._offline_gate_snapshot("offline-share-begin-button"),
        )

    def wait_offline_receive_expect_enabled(self) -> None:
        """The recipient panel's mirror of :meth:`wait_offline_share_begin_enabled`."""
        wait_until(
            self.offline_receive_expect_enabled,
            budgets.UI_SETTLE_S,
            diagnose=lambda: self._offline_gate_snapshot("offline-receive-expect-button"),
        )

    def _offline_gate_snapshot(self, act_id: str) -> str:
        """Why an act button is still disabled — read on the failure path only.
        A parse refusal (the field holds something that is not a usable code) and
        a gate that never repainted (the field holds a good code) look identical
        from the button alone, so this reads the field, the status and the error
        bar beside it (convention 6)."""

        def read(label: str, fn) -> str:
            try:
                return f"{label}={fn()!r}"
            except Exception as e:  # noqa: BLE001 — diagnostic only; never mask the timeout
                return f"{label}=<{type(e).__name__}>"

        return "; ".join([
            f"{act_id} never enabled although a counterpart's code was typed "
            f"({self.driver.diagnose(act_id)})",
            read("peer-code-input", lambda: self.driver.get_text("offline-share-peer-code-input")),
            read("status", self.offline_share_status),
            read("error-message", lambda: self.driver.get_text("error-message")),
        ])

    def offline_share_status(self) -> str:
        """`offline-share-status` — the ceremony's progress reading. The
        REASON a ceremony stopped rides `error-message`, never this element."""
        return self.driver.get_text("offline-share-status").strip()

    def cancel_offline_share(self) -> None:
        """`offline-share-cancel-button` — close the panel; on the recipient
        side it also withdraws the expectation (wormability rule 6)."""
        self.driver.click("offline-share-cancel-button")
        self._wait_visible("offline-share-button")

