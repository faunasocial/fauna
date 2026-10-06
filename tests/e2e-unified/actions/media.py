from __future__ import annotations

import os
import time
from typing import TYPE_CHECKING

from drivers.http_bridge import SelectOptionNotOffered
from i18n.strings import S

if TYPE_CHECKING:
    from drivers.base import PlatformDriver

# The `media-folder-filter` value for the all-media default view (vs. a real set
# name). Mirrors the linux/web `FILTER_ALL_VALUE` sentinel — `__`-prefixed so it
# can't collide with a user set name (reserved `__*` sets are excluded from
# `fauna.media.list`). See docs/goal/ui/media.md § O-4.
MEDIA_FILTER_ALL = "__all__"

# Ceiling for a frame to OFFER a `media-folder-filter` option (a named generous
# budget + deadline poll — e2e-conventions.md convention 14), sized far above
# any non-pathological refresh round-trip. A genuinely-unoffered value still
# refuses with the driver's own self-diagnosing message, just after the budget.
FILTER_OFFER_BUDGET_S = 15.0

# Bounded retries for `item_names()`'s count/read TOCTOU race (see its
# docstring) — not a timed budget, since the race resolves on the very next
# consistent snapshot rather than waiting for anything to change.
_ITEM_NAMES_RACE_RETRIES = 5

# tui's `media-thumbnail` text for an item with no picture to show
# (`apps/fauna-tui/src/thumbnail.rs::PLACEHOLDER`), and the half-block glyph its
# painted art is built from (`HALF_BLOCK`, same file). A picture is ANY cell of
# art, so the glyph's presence is the read; the placeholder is the whole text.
_TUI_THUMBNAIL_PLACEHOLDER = "□"
_TUI_HALF_BLOCK = "▀"

#: Ceiling for a folder's reachability change (a source device connecting or
#: dropping) to reach a re-entered Media page — the nest notices a dropped sync
#: socket on its own schedule, then the page must re-read. Generous on purpose
#: (convention 14): a green run returns on the first matching read.
SOURCE_STATUS_BUDGET_S = 60.0


class MediaActions:
    """Cross-app Media page actions.

    The 2026-06-28 sync/folder UI unification (design tracked internally)
    reworked Media into a Windows-Explorer-style cross-set browser
    (``docs/goal/ui/media.md``). The explorer helpers below drive the shared IDs
    (``media-view-toggle`` / ``media-sort-select`` / ``media-folder-filter`` +
    the indexed ``media-item`` component) that all 7 apps converge on; linux
    leads the shape and the other apps mirror it.
    """

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def navigate(self) -> None:
        """Navigate to the media page (top-level on every app)."""
        self.driver.navigate_to("media")

    def reenter(self) -> None:
        """Land on the Media browser and re-fire its one nest read.

        ``MediaMachine::refresh`` pulls ``fauna.media.list`` only when
        navigation ENTERS the page (the nav-edge trigger — tui ``App::apply``
        fires it on ``page != prior`` only; linux on ``connect_map``);
        ``set_filter`` is pure render state. So a listing read while already on
        Media returns the previous visit's snapshot, however long ago that was.
        Any wait for a nest-side change must therefore toggle away and back
        (``feed`` → ``media``) — the "re-enter the page to re-poll" pattern.
        On tui the agent nav AWAITS the nav-edge refresh, so the very next
        read is race-free; GUI apps load asynchronously and callers keep
        their settle/poll loops.

        Closing an open ``media-item-detail`` first is load-bearing, not
        tidiness: the detail owns the pane (tui renders only the detail's
        elements while it is open; linux uses a transient window the page stack
        doesn't own), and navigating away and back closes it on NO client — so
        a re-list with it open renders **zero** ``media-item``s with no error
        banner, a test artifact wearing a product-bug costume (it cost the
        member-decrypt suite five runs)."""
        if self.driver.is_visible("media-item-detail"):
            self.driver.click("media-item-detail-close-button")
        self.driver.navigate_to("feed")
        self.driver.navigate_to("media")

    # ── Explorer chrome ──────────────────────────────────────────────────

    def toggle_view(self) -> None:
        """Flip the list ↔ thumbnail-grid view (``media-view-toggle``)."""
        self.driver.click("media-view-toggle")

    def view_label(self) -> str:
        """The current view-toggle label — reflects the active mode (the
        localized "List" / "Grid"), so a test can assert the toggle flipped."""
        return self.driver.get_text("media-view-toggle")

    def set_sort(self, key: str) -> None:
        """Set the sort key (``media-sort-select``). ``key`` is the stable option
        value ``"name"`` / ``"size"`` / ``"date"`` (not the localized label)."""
        self.driver.select("media-sort-select", key)

    def set_sort_direction(self, direction: str) -> None:
        """Set the sort DIRECTION (``media-sort-direction``): the stable option
        value ``"ascending"`` (the default) or ``"descending"``, applied to
        whatever ``media-sort-select`` key is active.

        Why this exists as its own control rather than a second click on the
        sort key: a toggle's result depends on the current state, so a caller
        that needs a KNOWN order has to read the state first and may still race
        it. Setting a direction is idempotent, which is what makes a
        newest-first read reproducible on a lazy-list client (media.md
        § Layout & flow)."""
        self.driver.select("media-sort-direction", direction)

    def set_filter(
        self, folder: str = MEDIA_FILTER_ALL, *,
        offer_timeout: float = FILTER_OFFER_BUDGET_S,
    ) -> None:
        """Scope the browse (``media-folder-filter``): a folder name, or
        :data:`MEDIA_FILTER_ALL` for the all-media default.

        Retries the frame-anchored ``driver.select`` until the frame offers
        ``folder`` (deadline poll, ``offer_timeout``): ``navigate()`` returns
        while the page's nav-edge refresh is still in flight on the GUI apps,
        so the first frame legitimately paints only the ``__all__`` sentinel —
        a select fired straight after navigate raced that refresh and went red
        on linux (tui's agent nav awaits the refresh, so it
        never saw it). The render that paints the refreshed items rebuilds the
        options in the same pass, so the retry converges with the page. The
        wait lives HERE, not in ``driver.select``: the driver's immediate
        refusal of a value the frame did not offer is convention 11's twin
        rule (pinned by ``test_select_refuses_unoffered_option.py``, which
        drives the driver directly) and stays single-shot. A best-effort
        caller that treats "not offered" as an answer, not a latency, passes
        ``offer_timeout=0`` for exactly one attempt."""
        deadline = time.monotonic() + offer_timeout
        while True:
            try:
                self.driver.select("media-folder-filter", folder)
                return
            except SelectOptionNotOffered as e:
                if time.monotonic() >= deadline:
                    if offer_timeout <= 0:
                        raise
                    # The budget is spent and the frame still does not offer
                    # the value — dump the page facts the diagnosis needs
                    # (convention 6), most importantly whether MORE THAN ONE
                    # `media-folder-filter` exists (a stale previous-session
                    # page still in the widget tree would soak up the select
                    # while the live page renders correctly).
                    raise SelectOptionNotOffered(
                        f"{e}\n  after {offer_timeout:.0f}s of retries; page "
                        f"forensics: media-folder-filter instances="
                        f"{self.driver.count('media-folder-filter')}, "
                        f"media-item count={self.item_count()}, "
                        f"media-empty-state visible="
                        f"{self.driver.is_visible('media-empty-state')}, "
                        f"error present={self._error_present()}, "
                        # ⚠ This is the actor the PATCH ASKED FOR, never the
                        # live one: on linux `get_state`'s session block is the
                        # agent's own `session_override`, so it cannot falsify
                        # the patch that set it (e2e-conventions.md convention
                        # 11 § A HALF-APPLIED command is a dropped command).
                        # It is kept because a mismatch against the fixture's
                        # actor is still worth seeing — but a session that
                        # stayed on the OUTGOING actor reads identical here.
                        # The live surface is Settings → Status'
                        # `account-actor-id` (fed by `DataMessage::AuthSuccess`
                        # — `test_second_login_live_clients.py::_live_actor_id`),
                        # which needs a sub-page nav this failure path must not
                        # take; the nest-side answer is the dispatch beacon's
                        # `caller=` under `RUST_LOG=info,fauna_nest=debug`.
                        f"session.actor_id(requested, not live)="
                        f"{self.driver.get_state('session.actor_id')!r}"
                    ) from e
                time.sleep(0.2)

    # ── Items (indexed ``media-item`` component) ─────────────────────────

    def item_count(self) -> int:
        return self.driver.count("media-item")

    def item_name(self, index: int = 0) -> str:
        return self.driver.get_text("media-item-name", scope=f"media-item[{index}]")

    def item_size(self, index: int = 0) -> str:
        return self.driver.get_text("media-item-size", scope=f"media-item[{index}]")

    def item_date(self, index: int = 0) -> str:
        return self.driver.get_text("media-item-date", scope=f"media-item[{index}]")

    def source_status(self, index: int = 0) -> str:
        return self.driver.get_text("media-source-status", scope=f"media-item[{index}]")

    def item_names(self) -> list[str]:
        """Read every item's name.

        ``item_count()`` and each ``item_name(i)`` are separate round trips, so
        a list that is actively shrinking (mid-delete-propagation) can vanish
        an item between the two: ``item_name`` then 404s as ``LookupError``
        for an index the count no longer covers. Bounded-retry the whole
        read rather than raising — the very next snapshot is consistent by
        construction (the two calls read the same instant), so this converges
        immediately once the mutation settles, exactly the deadline-poll
        callers of this method are already doing at a higher level.
        """
        for _ in range(_ITEM_NAMES_RACE_RETRIES - 1):
            try:
                return [self.item_name(i) for i in range(self.item_count())]
            except LookupError:
                continue
        return [self.item_name(i) for i in range(self.item_count())]

    def wait_for_item_names(self, expected: list[str], timeout: float = 10.0) -> list[str]:
        """Poll until the rendered ``media-item-name`` sequence equals ``expected``,
        ORDER INCLUDED, and return the last read.

        The barrier for a re-sort: ``media-sort-select`` / ``media-sort-direction``
        re-order the snapshot asynchronously, so the frame after the select may still
        show the previous order. An identity match on the whole ordered list is sound
        where a count never could be — every sort shows the same items, so a count
        is satisfied before the re-sort lands (convention 14: poll the app's own
        observable to a generous deadline). On timeout returns the last read, so the
        caller's assertion names the order the page actually showed."""
        deadline = time.monotonic() + timeout
        names = self.item_names()
        while names != expected and time.monotonic() < deadline:
            time.sleep(0.2)
            names = self.item_names()
        return names

    def index_of(self, name: str) -> int:
        """The row index of the item named ``name`` — by identity, never position,
        because the active sort decides where a row lands. Raises ``LookupError``
        naming what IS listed when no row carries that name (convention 6)."""
        names = self.item_names()
        if name not in names:
            raise LookupError(f"no media-item named {name!r}; listed: {names!r}")
        return names.index(name)

    def wait_for_source_status(
        self, name: str, expected: str, timeout: float = SOURCE_STATUS_BUDGET_S
    ) -> str:
        """Re-enter the page until the item named ``name`` reports ``expected`` on its
        ``media-source-status`` — whether the folder it lives in can be reached right
        now (``media.md`` § Source status vs. sync state) — and return the last read.

        Re-entering IS the observation, not a workaround: the page reads
        ``fauna.media.list`` once per nav-edge and never re-polls (see
        :meth:`reenter`), and liveness is a fact about the folder's source device that
        no push announces. A user who wonders whether a folder came back re-opens the
        page; so does this. On timeout returns the last read so the assertion names
        it."""
        deadline = time.monotonic() + timeout
        status = ""
        while True:
            try:
                status = self.source_status(self.index_of(name))
            except LookupError as e:
                status = f"<unreadable: {e}>"
            if status == expected or time.monotonic() >= deadline:
                return status
            time.sleep(0.5)
            self.reenter()
            self.ensure_loaded()

    def wait_for_item_count(self, expected: int, timeout: float = 10.0) -> int:
        """Poll until exactly ``expected`` ``media-item``s are rendered, then
        return the observed count. The ``MediaMachine`` pulls ``fauna.media.list``
        asynchronously, so the count both rises (initial load 0→N) and falls (a
        per-set filter narrowing N→subset) toward its settled value — exact-match
        (not ``>=``) handles both directions. On timeout returns the last observed
        count, so the caller's assertion surfaces the mismatch.

        **Why an exact count is an acceptable post-``set_filter`` barrier here**
        (testing.md convention 14 — a *threshold* would not be; cf.
        ``BackupsActions.wait_for_snapshot_row``): ``set_filter`` switches which
        collection is displayed, so the outgoing set's rows can still be mounted
        when this starts polling. An exact match only false-passes if the stale
        collection has *precisely* the expected size, and the ``seeded_media_app``
        fixture logs in as a DEDICATED actor whose sets have pairwise-distinct
        sizes (photos=2, clips=1, all=3), so no stale count can collide. If a
        future seed plan gives two sets the same size, this stops being a barrier
        — switch those call sites to an identity match on ``item_names()``."""
        deadline = time.monotonic() + timeout
        count = self.item_count()
        while count != expected and time.monotonic() < deadline:
            time.sleep(0.2)
            count = self.item_count()
        return count

    def wait_for_item_size_change(
        self, index: int, previous: str, timeout: float = 10.0
    ) -> str:
        """Poll until ``media-item[index]``'s size text differs from
        ``previous`` (an upload/restore of the same path lands asynchronously —
        the machine refresh re-renders the head's size), returning the settled
        text (or the unchanged one on timeout, so the assertion surfaces it)."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            current = self.item_size(index)
            if current != previous:
                return current
            time.sleep(0.2)
        return self.item_size(index)

    def wait_for_view_label_change(self, previous: str, timeout: float = 5.0) -> str:
        """Click the view toggle already happened — poll until the label differs
        from ``previous`` (the toggle drives the shared machine asynchronously)."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            current = self.view_label()
            if current != previous:
                return current
            time.sleep(0.2)
        return self.view_label()

    # ── Item detail + version history (media-item-detail / file-version-history,
    #    approved 2026-07-09; semantics docs/goal/behavior/file-sync.md § File
    #    Versions / § Restore) ─────────────────────────────────────────────

    def open_item_detail(self, index: int = 0) -> None:
        """Tap/open a ``media-item`` → the ``media-item-detail`` surface
        (``docs/goal/ui/media.md`` § User actions)."""
        if self.driver.is_macos() or self.driver.is_ios():
            # Regression tripwire for the ~467pt controlsBar overflow class
            # (the documented incident that geo-parked media rows at x=-36
            # while they stayed registered/hittable-by-id) —
            # verify the row about to be clicked is actually on screen, not
            # just present in the registry, before is_visible()'s blind spot
            # can mask it again.
            self.driver.assert_on_screen("media-item", scope=f"media-item[{index}]")
        self.driver.click("media-item", index=index)
        self.driver.wait_for("media-item-detail", timeout=5.0)

    def detail_name(self) -> str:
        return self.driver.get_text("media-item-detail-name")

    def close_detail(self) -> None:
        self.driver.click("media-item-detail-close-button")

    def version_count(self) -> int:
        """Rendered ``file-version-item`` rows in the open detail surface."""
        return self.driver.count("file-version-item")

    def wait_for_version_count(self, expected: int, timeout: float = 10.0) -> int:
        """Poll until exactly ``expected`` ``file-version-item`` rows render
        (the list loads via the async shared ``MediaMachine::file_versions``);
        on timeout returns the last observed count so the caller's assertion
        surfaces the mismatch."""
        deadline = time.monotonic() + timeout
        count = self.version_count()
        while count != expected and time.monotonic() < deadline:
            time.sleep(0.2)
            count = self.version_count()
        return count

    def version_size(self, index: int = 0) -> str:
        return self.driver.get_text(
            "file-version-size", scope=f"file-version-item[{index}]"
        )

    def version_timestamp(self, index: int = 0) -> str:
        return self.driver.get_text(
            "file-version-timestamp", scope=f"file-version-item[{index}]"
        )

    def has_version_author(self, index: int = 0) -> bool:
        """``file-version-author`` — every version row names its recorder."""
        return self.driver.is_visible(
            "file-version-author", scope=f"file-version-item[{index}]"
        )

    def version_author(self, index: int = 0) -> str:
        return self.driver.get_text(
            "file-version-author", scope=f"file-version-item[{index}]"
        )

    # ── Recovery browse (file-versions.md § Retention (3)) ──────────────────

    def toggle_show_pruned(self) -> None:
        """Flip the ``file-version-show-pruned-toggle`` recovery-browse switch:
        ON re-lists the open detail's versions with ``include_pruned`` so
        soft-pruned rows appear (badge + undelete per row); OFF returns to the
        live-only listing."""
        self.driver.click("file-version-show-pruned-toggle")

    def has_version_pruned_badge(self, index: int = 0) -> bool:
        """``file-version-pruned-badge`` is present ONLY on a soft-pruned row
        of an ``include_pruned`` listing — a live row never renders it (the
        ``snapshot-undelete-button`` present-only-on-recoverable shape)."""
        return not self.driver.is_absent(
            "file-version-pruned-badge", scope=f"file-version-item[{index}]"
        )

    def undelete_version(self, index: int) -> None:
        """Recover the soft-pruned version at ``index`` back into the live
        population: the per-row ``file-version-undelete-button`` calls
        ``fauna.files.versions.undelete`` and re-lists under the browse's
        current toggle."""
        self.driver.click(
            "file-version-undelete-button", scope=f"file-version-item[{index}]"
        )

    def restore_version(self, index: int) -> None:
        """Restore the version at ``index`` (oldest→newest): the per-row
        ``file-version-restore-button`` opens the lightweight
        ``file-version-restore-confirm-modal`` (restore is reversible — it
        appends a new version; ``file-sync.md`` § Restore), and the confirm
        button records the re-point."""
        self.driver.click(
            "file-version-restore-button", scope=f"file-version-item[{index}]"
        )
        self.driver.wait_for("file-version-restore-confirm-modal", timeout=5.0)
        self.driver.click("file-version-restore-confirm-button")

    # ── Delete the opened file (media-delete-button → media-delete-confirm-modal,
    #    user-approved 2026-07-16) ──────────────────────────────────────────────

    def delete_open_item(self) -> None:
        """Delete the file whose ``media-item-detail`` is open: the
        ``media-delete-button`` opens the lightweight
        ``media-delete-confirm-modal``, and the confirm button runs the shared
        ``MediaMachine::delete`` (→ ``fauna.sync.delete_member``), which records a
        **tombstone** and refreshes the explorer. Single confirm by design — a media
        delete leaves the historical version rows and is not forwarded to backup-type
        destinations, so it deliberately does NOT take the backups typed-id
        immediate-delete ceremony (``file-sync.md`` § File Versions)."""
        self.driver.click("media-delete-button")
        self.driver.wait_for("media-delete-confirm-modal", timeout=5.0)
        self.driver.click("media-delete-confirm-button")

    # ── Download the opened file (media-item-detail-download-button,
    #    user-approved 2026-09-25) ──────────────────────────────────────────

    def download_open_item(self, timeout: float = 10.0) -> bytes:
        """Press ``media-item-detail-download-button`` on the open
        ``media-item-detail`` and return the plaintext bytes the app handed to
        the platform's save path (``docs/goal/ui/media.md`` § Element IDs).

        Waits for the trigger first: it paints once the version rows carry a
        manifest (a followed item's paints at once), so a click before that is
        a race, not a finding. The platform branch lives here, per the e2e
        conventions: web captures the real browser download (Playwright wraps
        the click server-side — ``download_via_click`` — so it can't be
        composed from a plain click); a native app saves dialog-less under e2e
        into ``driver.download_dir()``, named after the file
        (``media-item-detail-name``), and the file is read back from there —
        the backups single-file download's seam
        (``BackupsActions.wait_for_downloaded_file``). A driver that declares
        no ``download_dir()`` has not adopted the ID and fails there with a
        wiring hint. tui's separate ``media-external-open-*`` family (hand an
        audio/video item to the OS player) is a different gesture, not this
        one — ``test_tui_media_external_open.py`` drives it.
        """
        self.driver.wait_for("media-item-detail-download-button", timeout=timeout)
        if self.driver.is_web():
            return self.driver.download_via_click("media-item-detail-download-button")
        from actions.backups import BackupsActions

        name = self.driver.get_text("media-item-detail-name")
        download_dir = self.driver.download_dir()
        if download_dir:
            # The dir is per-launch but a session can download the same name
            # twice (a re-read after a rotation): clear it, so the poll below
            # can only observe THIS press's write.
            stale = os.path.join(download_dir, name)
            if os.path.exists(stale):
                os.remove(stale)
        self.driver.click("media-item-detail-download-button")
        return BackupsActions(self.driver).wait_for_downloaded_file(name, timeout=timeout)

    def cancel_delete_open_item(self) -> None:
        """Open the delete confirm modal and dismiss it. Cancelling is a **pure
        no-op**: no mutation, no error surfaces — the same contract the sibling
        restore/re-auth confirm modals hold."""
        self.driver.click("media-delete-button")
        self.driver.wait_for("media-delete-confirm-modal", timeout=5.0)
        self.driver.click("media-delete-cancel-button")

    # ── Upload (file-upload + upload-button → shared upload_selected gesture) ──

    def upload_file(self, path: str) -> None:
        """Choose ``path`` in the ``file-upload`` picker and submit
        ``upload-button``. On desktop/mobile ``file-upload`` is a text input acting
        as a file picker (``docs/goal/ui/media.md`` § Layout & flow); on web it is a
        real ``<input type=file>`` — a browser cannot read an arbitrary typed path,
        so we set the file on the input (Playwright ``set_input_files``) rather than
        typing it (platform branch lives in the action layer, never the test).
        ``upload-button`` runs the shared ``MediaMachine::upload_selected`` gesture —
        resolve the target set (or the ``media.error_no_set`` policy), seal under the
        owner BackupKey, POST the blob, record the manifest member, then refresh.
        ``path`` is a local filesystem path.

        android's ``file-upload`` opens the system document picker, which no
        agent can drive, and its test agent has no ``compose.file`` arm for this
        page yet (only the feed and conversations composers), so an upload there
        is declared unbuilt rather than typed into a button."""
        if self.driver.is_android():
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="the Media page's file-upload pick seam",
                detail="TestAgent.kt's applyComposePatch has no `file-upload` "
                       "target (MediaScreen's picker result)",
                tracked="",
            )
        if self.driver.is_web():
            self.driver.set_input_files("file-upload", path)
        else:
            # A generous deadline-poll before interacting (testing.md convention
            # 14) — not a wait for THIS call's own render (file-upload is static
            # chrome, never conditionally hidden), but for whatever async UI
            # mutation a PRIOR action on this page may still be settling under
            # load (e.g. a just-completed upload's RenderPage re-layout). Every
            # other post-async-mutation interaction in the shared action layer
            # already guards this way (admin.py, atproto_settings.py, ...);
            # upload_file's native path never did, and a live windows repro
            # (2026-08-02) hit `LookupError: Element 'file-upload' index 0 not
            # found` on the SECOND upload in a row (the only call site that
            # re-invokes this within one test) with no other explanation in the
            # app code (file-upload is never hidden/disabled around an upload).
            self.driver.wait_for("file-upload", timeout=15.0)
            # clear_and_type, not type_text: the entry keeps its previous value
            # (linux `type_text` INSERTS), so a second upload in the same test
            # would concatenate paths into a nonsense one.
            self.driver.clear_and_type("file-upload", path)
        self.driver.click("upload-button")

    def press_upload_with_no_file(self) -> None:
        """Press ``upload-button`` with nothing chosen in ``file-upload`` — the case
        ``media.md`` § User actions says must answer on ``error-message`` rather than
        read as a dead button.

        A native app's path box is emptied first, because it keeps whatever an
        earlier upload in the same session typed there; web's
        ``<input type=file>`` has no "choose nothing" gesture beyond not choosing,
        and a page nobody has picked on is exactly that state."""
        if not self.driver.is_web():
            self.driver.wait_for("file-upload", timeout=15.0)
            self.driver.clear_and_type("file-upload", "")
        self.driver.click("upload-button")

    def no_file_chosen_text(self) -> str:
        """What the no-file guard says: ``media.file_required`` everywhere but tui,
        which uses the typed-path wording ``media.file_path_required`` for the same
        guard because a terminal has no picker to "choose" with (``media.md``
        § User actions names the split). The platform branch lives here, never in a
        test."""
        return S.media.file_path_required if self.driver.is_tui() else S.media.file_required

    def thumbnail_kind(self, index: int = 0) -> str | None:
        """What ``media-item[index]``'s ``media-thumbnail`` shows: ``"picture"`` — the
        item's own image — or ``"placeholder"`` — what an item with nothing to show
        (no thumbnail, art still loading, a fetch that failed) paints instead
        (``media.md`` § Layout & flow; § Thumbnails).

        tui reads the element's own text: its picture IS half-block art (see
        :meth:`painted_thumbnail_count`), and its placeholder is the ``□`` glyph.
        Any other text is returned verbatim so a caller's assertion shows it.

        web reads the DOM: the picture is the ``<img>`` painted inside
        ``media-thumbnail`` once its bytes decoded (``naturalWidth`` > 0); the
        placeholder is the bare ``.thumb`` tile (``routes/media/+page.svelte``).

        linux reads the element's ``state`` attribute — ``painted`` once the
        ``gtk::Image`` holds decoded bytes as its paintable, ``placeholder`` while it
        still shows the icon — the same live-widget read ``post-image`` answers
        (``apps/fauna-linux/src/automation/agent.rs``). macOS and iOS answer the
        same attribute off the card's decoded image (``MediaExplorerContent.swift``),
        and windows off the ``Image``'s own ``Source`` read back after assignment,
        published as ``AutomationProperties.HelpText`` (``Helpers/ImageHashBind.cs``
        ``SetPaintState``) — the Image stays realized over a placeholder glyph, so
        a placeholder item still carries the element. android answers it off the
        node's ``stateDescription`` — ``painted`` on the decoded ``Image``,
        ``placeholder`` on the stand-in ``Icon`` (``MediaScreen.kt``
        ``MediaThumbnail``).

        ``None`` on an app whose driver cannot yet tell the two apart — the other
        GUI apps paint into native image views this layer does not inspect (the
        same gap :meth:`painted_thumbnail_count` documents). A client gains a branch
        here when its driver can read the difference; until then a witness of this
        outcome is not marked for it."""
        scope = f"media-item[{index}]"
        if self.driver.is_web():
            painted = self.driver.eval_js(
                "(() => { const item = document.querySelectorAll("
                "'[data-testid=\"media-item\"]')[%d]; if (!item) return null; "
                "const img = item.querySelector('[data-testid=\"media-thumbnail\"] img'); "
                "return !!(img && img.complete && img.naturalWidth > 0); })()" % index
            )
            if painted is None:
                raise LookupError(
                    f"no media-item[{index}] on the page; listed: {self.item_names()!r}"
                )
            return "picture" if painted else "placeholder"
        if (
            self.driver.is_linux() or self.driver.is_macos() or self.driver.is_ios()
            or self.driver.is_windows() or self.driver.is_android()
        ):
            state = self.driver.get_attr("media-thumbnail", "state", scope=scope)
            return {"painted": "picture", "placeholder": "placeholder"}.get(
                state, f"unrecognized media-thumbnail state {state!r}"
            )
        if not self.driver.is_tui():
            return None
        text = self.driver.get_text("media-thumbnail", scope=scope)
        if _TUI_HALF_BLOCK in text:
            return "picture"
        if text == _TUI_THUMBNAIL_PLACEHOLDER:
            return "placeholder"
        return f"unrecognized media-thumbnail text {text!r}"

    # ── Thumbnail paint (the producer → fetch → decrypt → paint positive path) ──

    def painted_thumbnail_count(self) -> int | None:
        """Number of ``media-thumbnail`` tiles that have PAINTED a real image — an
        item whose upload produced a ``thumbnail_hash`` that then fetched, decrypted
        and rendered (``docs/goal/ui/media.md`` § Thumbnails). This is the positive
        producer→paint path: a null-hash item keeps the placeholder and is NOT
        counted.

        Web reads the DOM — the painted ``<img>`` inside ``media-thumbnail`` exists
        only once the shared ``MediaMachine::fetch_thumbnail`` resolves
        (``routes/media/+page.svelte`` ``loadThumbnail``).

        tui reads the element text: a thumbnail is painted as **half-block art**
        (``apps/tui.md`` § Rendering), so the picture IS the element's own
        characters — a ``▀`` per cell, fg = the top pixel, bg = the bottom. An item
        with no thumbnail, a failed fetch, or art still loading paints the ``□``
        placeholder instead, which is not counted. That makes tui the one app
        able to assert the positive producer→fetch→decrypt→paint path end-to-end
        headlessly alongside web and linux (which reads each tile's ``state``
        attribute off the live ``gtk::Image``, as macOS and iOS do off the card's
        decoded image and windows off the Image's own Source); the other GUI
        apps paint into a
        native image view this layer does not inspect and prove it with per-app
        unit tests today, so this returns ``None`` for them and the caller skips the
        assertion (the upload's count round-trip still runs cross-app). A client
        adds a native-view branch here when it asserts the positive paint
        end-to-end via e2e."""
        if self.driver.is_tui():
            # HALF_BLOCK — apps/fauna-tui/src/thumbnail.rs. Counting the glyph
            # rather than "text is non-empty" is what keeps the placeholder from
            # reading as a paint.
            return sum(
                "▀" in self.driver.get_text("media-thumbnail", scope=f"media-item[{i}]")
                for i in range(self.item_count())
            )
        if (
            self.driver.is_linux() or self.driver.is_macos() or self.driver.is_ios()
            or self.driver.is_windows()
        ):
            # The live `gtk::Image`'s paintable (apple: the card's decoded image;
            # windows: the Image's own Source, as HelpText) — see :meth:`thumbnail_kind`.
            return sum(
                state == "painted"
                for state in self.driver.get_attrs("media-thumbnail", "state")
            )
        if not self.driver.is_web():
            return None
        n = self.driver.eval_js(
            'document.querySelectorAll(\'[data-testid="media-thumbnail"] img\').length'
        )
        return int(n or 0)

    def wait_for_painted_thumbnails(self, expected: int, timeout: float = 15.0) -> int | None:
        """Poll until exactly ``expected`` ``media-thumbnail`` tiles have painted a
        real image, then return the observed count — or ``None`` on a client that
        doesn't inspect the paint (see :meth:`painted_thumbnail_count`), so the
        caller guards the assertion with ``if painted is not None``. The producer
        records the ``thumbnail_hash`` on upload, the machine ``refresh()`` surfaces
        it via ``fauna.media.list``, and the on-appear fetch+decrypt paints it —
        several async hops, so poll. On timeout returns the last count so the
        caller's assertion surfaces the mismatch."""
        if self.painted_thumbnail_count() is None:
            return None
        deadline = time.monotonic() + timeout
        count = self.painted_thumbnail_count()
        while count != expected and time.monotonic() < deadline:
            time.sleep(0.2)
            count = self.painted_thumbnail_count()
        return count

    def has_loaded(self) -> bool:
        """Whether the Media listing has finished its first read.

        The three-state read that `media-empty-state` buys us
        (`media.md` § Default view: cross-set all-media):

        * rows present                  → loaded, non-empty
        * `media-empty-state` present   → loaded, genuinely empty
        * neither                       → still loading
        """
        if self.item_count() > 0:
            return True
        return self.driver.is_visible("media-empty-state")

    #: Generous ceiling for "the first fauna.media.list read came back". Sized far
    #: above any non-pathological load on a loaded box, and a green run never pays
    #: it — the poll returns the moment the app says it loaded (convention 14: a
    #: named budget + deadline poll, never a settle-sleep).
    LOAD_BUDGET_S = 30.0

    def wait_for_loaded(self, timeout: float | None = None) -> bool:
        """Poll until :meth:`has_loaded` is True, returning the final answer."""
        deadline = time.monotonic() + (self.LOAD_BUDGET_S if timeout is None else timeout)
        while True:
            if self.has_loaded():
                return True
            if time.monotonic() >= deadline:
                return self.has_loaded()
            time.sleep(0.2)

    def ensure_loaded(self, *, reenters: int = 2) -> None:
        """Land the page LOADED, re-entering on a failed first read.

        The page pulls ``fauna.media.list`` once per nav-edge and never
        re-polls (see :meth:`reenter`), so a first read that loses to
        auth/connection turbulence stays failed until the page is re-entered:
        an actor switch's fresh session can fire its nav-edge refresh while
        its WS-RPC is still coming up, parking the page on an
        ``rpc disconnected`` error with no items and no filter options —
        measured on linux under machine thrash as both the
        ``test_all_media_cross_set`` 0-item timeout and the 15-second
        ``this render painted [__all__]`` select refusals.
        A user shown that error re-opens the page; this drives the same
        recovery off the app's own observables (loaded vs the page error),
        never wall-clock — within each attempt the poll breaks to a re-entry
        the moment the app SAYS the read failed. Raises loudly if the page
        still has not loaded after ``reenters`` re-entries."""
        for attempt in range(reenters + 1):
            deadline = time.monotonic() + self.LOAD_BUDGET_S
            while time.monotonic() < deadline:
                if self.has_loaded():
                    return
                if self._error_present():
                    break  # the app says the read FAILED — re-enter, don't wait
                time.sleep(0.2)
            if attempt < reenters:
                self.reenter()
        raise AssertionError(
            f"Media page never reported loaded after {reenters} re-entries "
            f"(each budgeted {self.LOAD_BUDGET_S:.0f}s): "
            f"{self.driver.diagnose('error-message')}; "
            f"{self.driver.diagnose('media-empty-state')}"
        )

    def wait_for_error(self, timeout: float = 10.0) -> bool:
        """Poll until a page error is present, returning whether one appeared.

        The upload gesture runs asynchronously off the UI thread, so the error
        (``snapshot.error`` → ``messages.error``) appears a beat after the click.
        Presence is read the SAME way as ``ActionLayer.error_text`` — state first
        (``messages.error``), element only as a fallback — because the windows
        ``error-message`` is an InfoBar whose UIA peer reports *visible* even when
        idle, so ``is_visible`` alone returns True prematurely and races the
        caller's ``error_text()`` read (→ a stale ``""``). The caller then reads
        the text via ``app.error_text()``."""
        deadline = time.monotonic() + timeout
        while True:
            if self._error_present():
                return True
            if time.monotonic() >= deadline:
                return self._error_present()
            time.sleep(0.2)

    def _error_present(self) -> bool:
        """Whether a page error is currently set, preferring the canonical state
        read (``messages.error``) over the unreliable windows InfoBar UIA peer;
        falls back to element visibility for clients that don't serialize
        messages into state. Mirrors ``ActionLayer.error_text`` resolution."""
        err = self.driver.get_state("messages.error")
        if err is not None:
            return bool(err)  # serialized → non-empty string means a real error
        if self.driver.get_state("messages") is not None:
            return False  # messages serialized, no active error
        return self.driver.is_visible("error-message")  # client doesn't serialize
