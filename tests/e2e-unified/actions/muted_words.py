from __future__ import annotations

import time
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class MutedWordsActions:
    """Drive the user-facing "Muted words" Settings sub-page
    (docs/goal/behavior/moderation.md § Muted keywords; tests/e2e-unified/ui.yaml
    `muted-words` page).

    A person manages their single user-global muted-keyword list here — add a
    term (`muted-word-input` + `muted-word-add-button`), see the terms
    (`muted-word-item` rows: `muted-word-text` + `muted-word-remove-button`),
    empty state (`muted-word-empty`) — backed by the shared sealed
    `fauna.state.moderation` `muted_keywords` list over the client-config seam (linux
    calls `fauna_client_config::{muted_keywords,set_muted_keywords}` directly; web
    the wasm `mutedKeywords{List,Set}`; natives the UniFFI `muted_keywords_{list,
    set}`). No new WS-RPC kind — the list is sealed client-side, nest-opaque.

    Reached via the settings sidebar-swap shell on every app (the sub-id is
    ignored on single-scroll clients). linux is the lead; the other five lift this
    shape (priority #1). Structurally the mail-aliases sub-page CRUD list, only
    simpler (the input + add-button sit directly on the page — no add-sheet
    reveal, no kind picker).
    """

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    # A settings sub-page nav is a fire-and-forget `set_state` patch: the page still
    # has to construct, bind and render before any of its elements exist. Sized far
    # above any non-pathological render rather than to a quiet machine — testing.md
    # § convention 14: a generous POSITIVE budget costs a green run nothing (the
    # waits below are deadline polls that exit the moment the page is up), while a
    # tight one turns ordinary load into a fake product failure. The old 10 s was
    # exactly that bet, and it lost: `muted-word-input` "not found" after a loaded
    # batch is this navigation not having landed yet, not a missing control.
    PAGE_READY_BUDGET_S = 60.0

    def navigate(self, *, timeout: float | None = None) -> None:
        """Navigate to the muted-words Settings sub-page (sidebar-swap shell on
        linux; the sub-id is ignored on single-scroll clients).

        Blocks until the page has actually rendered. That barrier is the point: a
        caller that navigates and immediately types (``add`` below) would otherwise
        be racing the page's construction, which is a *causal* race, not a slow one —
        no amount of waiting inside ``add`` fixes navigating to a page that is not up.
        """
        self.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "muted-words"}]},
        })
        self.driver.wait_for(
            "muted-word-add-button", timeout=timeout or self.PAGE_READY_BUDGET_S
        )

    def is_page_visible(self, timeout: float | None = None) -> bool:
        """True when the muted-words page is reachable (add-button present)."""
        try:
            self.driver.wait_for(
                "muted-word-add-button", timeout=timeout or self.PAGE_READY_BUDGET_S
            )
            return True
        except TimeoutError:
            return False

    # ── list reads ────────────────────────────────────────────────────────
    def row_count(self) -> int:
        """How many muted-word rows the list currently renders."""
        return self.driver.count("muted-word-item")

    def words(self) -> list[str]:
        """The rendered term of every muted-word row, in order."""
        return [
            self.driver.get_text("muted-word-text", scope=f"muted-word-item[{i}]")
            for i in range(self.row_count())
        ]

    def is_empty_state_visible(self) -> bool:
        """True when the `muted-word-empty` placeholder shows (no terms).

        ⚠ An immediate read after `navigate()` is a race: the placeholder now
        paints only once the muted-keyword read has RESOLVED (the page's shared
        `loaded` bit — `docs/goal/ui/README.md` § *List pages: loading is not
        empty*), so a caller asserting "the list is empty" wants
        `wait_for_empty_state()`, which polls to that deadline. This bare read
        stays for the negative direction (is it gone yet).
        """
        return not self.driver.is_absent("muted-word-empty")

    # The empty state is now gated on a nest round trip (config get → unseal →
    # project), not on the page's construction. Sized far above any
    # non-pathological read rather than to a quiet machine — testing.md
    # convention 14: a generous POSITIVE budget costs a green run nothing (the
    # poll exits the moment the placeholder appears), while a tight one turns
    # ordinary load into a fake product failure.
    LOAD_BUDGET_S = 60.0

    def wait_for_empty_state(self, timeout: float | None = None) -> bool:
        """Poll until `muted-word-empty` paints — i.e. until the page has read
        the list AND found nothing.

        This is the loaded-and-empty barrier: a page whose read has not resolved
        paints neither rows nor the placeholder (three states off one id, no
        `*-loading` id to wait on), so "no rows yet" is not evidence of an empty
        list. Returns False on timeout — the caller says what that means.
        """
        deadline = time.time() + (timeout or self.LOAD_BUDGET_S)
        while time.time() < deadline:
            if self.driver.is_visible("muted-word-empty"):
                return True
            time.sleep(0.3)
        return self.driver.is_visible("muted-word-empty")

    def has_loaded(self) -> bool:
        """True once the page's read has resolved: rows, or `muted-word-empty`."""
        return self.row_count() > 0 or self.driver.is_visible("muted-word-empty")

    def revisit(self) -> list[str]:
        """Leave the page, enter it again, wait for its read, return the terms.

        On an app that does not consume the store-change notice yet (windows,
        macOS, iOS; a web tab that hosts no runtime) the page reads the stored
        list once per visit, so a list that changes under an open page —
        another device's edit walked in, or a successor's inherited terms
        carried by its account runtime's first pass — reaches the screen at the
        next visit, not before. A caller that must hold on every app polls
        THIS, never `words()` on a page it entered once; the no-re-visit
        journeys (`test_store_change_notice.py`) are for the apps that do
        consume the notice.

        Leaves through the feed rather than the settings root, so the visit is
        a real nav edge on the single-scroll shells too. Waits for the read
        before returning, so the next call cannot tear an in-flight load down;
        an unresolved read returns what is painted and the caller's own
        diagnosis reads the error surface.
        """
        self.driver.set_state({"nav": {"stack": [{"view": "feed"}]}})
        self.navigate()
        deadline = time.monotonic() + self.LOAD_BUDGET_S
        while not self.has_loaded() and time.monotonic() < deadline:
            time.sleep(0.3)
        try:
            return self.words()
        except LookupError:
            return []  # a row the list rebuilt away between the count and the read

    def wait_for_row_count(self, expected: int, timeout: float = 12.0) -> bool:
        """Poll until the list holds exactly `expected` rows (the dispatch →
        re-seal → nest write-back → reload round-trip is async)."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.row_count() == expected:
                return True
            time.sleep(0.3)
        return self.row_count() == expected

    def wait_for_word(self, word: str, timeout: float = 12.0) -> bool:
        """Poll until `word` is one of the listed terms.

        The add's own verdict. A row count cannot say WHICH term landed: on the
        session-scoped account a term an earlier test's add left behind (one that
        landed after that test gave up on it) satisfies "one row" while the term
        just added is still in flight — and the mute the test then relies on is
        not in force (the 2026-09-22 linux sweep: `muted=0 text=2`)."""
        deadline = time.time() + timeout
        while True:
            try:
                if word in self.words():
                    return True
            except LookupError:
                pass  # a row the list rebuilt away between the count and the read
            if time.time() >= deadline:
                return False
            time.sleep(0.3)

    # ── mutations ─────────────────────────────────────────────────────────
    def add(self, word: str) -> None:
        """Type a term into `muted-word-input` and click `muted-word-add-button`."""
        self.driver.wait_for("muted-word-input", timeout=self.PAGE_READY_BUDGET_S)
        self.driver.clear_and_type("muted-word-input", word)
        self.driver.click("muted-word-add-button")

    def remove(self, index: int = 0) -> None:
        """Click the `muted-word-remove-button` of the `index`-th row (set the
        stored list minus that term)."""
        self.driver.click("muted-word-remove-button", scope=f"muted-word-item[{index}]")
