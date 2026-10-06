from __future__ import annotations

import time
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class LabelerCatalogActions:
    """Drive the unified Personalization home + Community-labelers catalog
    Settings sub-pages (docs/goal/architecture/content-moderation-and-ranking.md
    § Tier-3 community models; tests/e2e-unified/ui.yaml `personalization` +
    `labeler-catalog` pages).

    Both pages render the SAME `labeler-catalog-item` component off one shared
    `LabelerCatalogMachine` snapshot (`libs/fauna-labeler-catalog-machine`):
    `labeler-catalog` lists every published labeler (inspect + exactly one of
    subscribe/unsubscribe per row, gated on `entry.subscribed`);
    `personalization` filters to the caller's SUBSCRIBED rows only
    (unsubscribe-button only, no inspect/subscribe). Only one page is
    navigated to at a time, so the indexed `labeler-catalog-item[i]` scope
    (mirrors `media-item[i]`) needs no outer container disambiguation.
    """

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    # ── navigation ───────────────────────────────────────────────────────

    def navigate_home(self) -> None:
        """Navigate to the Personalization home Settings sub-page."""
        self.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "personalization"}]},
        })

    def navigate_catalog(self) -> None:
        """Navigate to the Community-labelers catalog Settings sub-page."""
        self.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "labeler-catalog"}]},
        })

    def browse_catalog(self) -> None:
        """From the personalization home, follow the browse-catalog link."""
        self.driver.click("personalization-browse-catalog-button")

    # ── personalization home (subscribed-only facet) ────────────────────

    def subscribed_count(self) -> int:
        return self.driver.count("labeler-catalog-item")

    def is_subscribed_empty_visible(self) -> bool:
        return not self.driver.is_absent("personalization-labelers-empty")

    def wait_for_subscribed_count(self, expected: int, timeout: float = 10.0) -> int:
        deadline = time.monotonic() + timeout
        count = self.subscribed_count()
        while count != expected and time.monotonic() < deadline:
            time.sleep(0.2)
            count = self.subscribed_count()
        return count

    # ── the loaded-vs-empty read (shared by both pages) ──────────────────

    def has_loaded(self, empty_element: str) -> bool:
        """Whether the shared catalog read has RESOLVED on the page in view.

        The three-state read both empty-state elements buy
        (`docs/goal/ui/README.md` § *List pages: loading is not empty*):

        * rows present          → loaded, non-empty
        * `empty_element` shown → loaded, genuinely empty
        * neither               → still loading
        """
        if self.driver.count("labeler-catalog-item") > 0:
            return True
        return self.driver.is_visible(empty_element)

    #: Generous ceiling for "the first `fauna.labelers.list` read came back".
    #: Sized far above any non-pathological load, and a green run never pays it —
    #: the poll returns the moment the app says it loaded (convention 14: a named
    #: budget + deadline poll, never a settle-sleep).
    LOAD_BUDGET_S = 30.0

    def wait_for_loaded(self, empty_element: str, timeout: float | None = None) -> bool:
        """Poll until :meth:`has_loaded` is True, returning the final answer."""
        deadline = time.monotonic() + (self.LOAD_BUDGET_S if timeout is None else timeout)
        while True:
            if self.has_loaded(empty_element):
                return True
            if time.monotonic() >= deadline:
                return self.has_loaded(empty_element)
            time.sleep(0.2)

    # ── labeler-catalog page (full catalog) ─────────────────────────────

    def catalog_count(self) -> int:
        return self.driver.count("labeler-catalog-item")

    def is_catalog_empty_visible(self) -> bool:
        return not self.driver.is_absent("labeler-catalog-empty")

    def wait_for_catalog_count(self, expected: int, timeout: float = 10.0) -> int:
        deadline = time.monotonic() + timeout
        count = self.catalog_count()
        while count != expected and time.monotonic() < deadline:
            time.sleep(0.2)
            count = self.catalog_count()
        return count

    def find_index_by_factor(self, factor: str, timeout: float = 10.0) -> int | None:
        """The row index whose `labeler-catalog-item-factor` equals `factor`
        (``"labeler:<hex>"``), on whichever page is currently navigated. The
        catalog is nest-global (not per-actor), so a caller should locate its
        own row by factor rather than assume a position — polls because a
        just-published labeler needs the list to (re)load first."""
        deadline = time.monotonic() + timeout
        while True:
            for i in range(self.catalog_count()):
                if self.item_factor(i) == factor:
                    return i
            if time.monotonic() >= deadline:
                return None
            time.sleep(0.2)

    # ── row fields (indexed `labeler-catalog-item`) ─────────────────────

    def item_publisher(self, index: int = 0) -> str:
        return self.driver.get_text(
            "labeler-catalog-item-publisher", scope=f"labeler-catalog-item[{index}]"
        )

    def item_content_kind(self, index: int = 0) -> str:
        return self.driver.get_text(
            "labeler-catalog-item-content-kind", scope=f"labeler-catalog-item[{index}]"
        )

    def item_version(self, index: int = 0) -> str:
        return self.driver.get_text(
            "labeler-catalog-item-version", scope=f"labeler-catalog-item[{index}]"
        )

    def item_factor(self, index: int = 0) -> str:
        return self.driver.get_text(
            "labeler-catalog-item-factor", scope=f"labeler-catalog-item[{index}]"
        )

    def is_subscribe_visible(self, index: int = 0) -> bool:
        return not self.driver.is_absent(
            "labeler-catalog-item-subscribe-button", scope=f"labeler-catalog-item[{index}]"
        )

    def is_unsubscribe_visible(self, index: int = 0) -> bool:
        return not self.driver.is_absent(
            "labeler-catalog-item-unsubscribe-button", scope=f"labeler-catalog-item[{index}]"
        )

    def subscribe(self, index: int = 0) -> None:
        """Click `labeler-catalog-item-subscribe-button` on row `index`
        (labeler-catalog page only — v1 always mints no capability grant,
        `grant_id: None`, so this is inert-but-honest for a restricted kind;
        a public kind needs no grant at all)."""
        self.driver.click(
            "labeler-catalog-item-subscribe-button", scope=f"labeler-catalog-item[{index}]"
        )

    def unsubscribe(self, index: int = 0) -> None:
        """Click `labeler-catalog-item-unsubscribe-button` on row `index`
        (present on both pages)."""
        self.driver.click(
            "labeler-catalog-item-unsubscribe-button", scope=f"labeler-catalog-item[{index}]"
        )

    def wait_for_subscribed_state(
        self, index: int = 0, *, subscribed: bool, timeout: float = 10.0
    ) -> bool:
        """Poll row `index` until its subscribe/unsubscribe toggle reflects
        `subscribed` — the gesture round-trips async (RPC → nest write →
        `fauna.labelers.list` refresh)."""
        deadline = time.monotonic() + timeout
        while True:
            if (
                self.is_unsubscribe_visible(index) == subscribed
                and self.is_subscribe_visible(index) == (not subscribed)
            ):
                return True
            if time.monotonic() >= deadline:
                return (
                    self.is_unsubscribe_visible(index) == subscribed
                    and self.is_subscribe_visible(index) == (not subscribed)
                )
            time.sleep(0.2)

    # ── inspect-before-subscribe panel ──────────────────────────────────

    def inspect(self, index: int = 0) -> None:
        self.driver.click(
            "labeler-catalog-item-inspect-button", scope=f"labeler-catalog-item[{index}]"
        )

    def is_inspect_panel_visible(self) -> bool:
        return self.driver.is_visible("labeler-inspect-panel")

    def inspect_metadata_text(self) -> str:
        return self.driver.get_text("labeler-inspect-metadata")

    def close_inspect(self) -> None:
        self.driver.click("labeler-inspect-close-button")

    def wait_for_inspect_panel(self, visible: bool, timeout: float = 10.0) -> bool:
        deadline = time.monotonic() + timeout
        while True:
            if self.is_inspect_panel_visible() == visible:
                return True
            if time.monotonic() >= deadline:
                return self.is_inspect_panel_visible() == visible
            time.sleep(0.2)

    # ── list-kind rows + inspect section (frame § Tier-3 artifact kinds;
    # IDs user-approved 2026-07-16) ──────────────────────────────────────

    def item_kind(self, index: int = 0) -> str:
        """The row's artifact kind (`list` | `wasm` | `text-model`; the machine
        normalizes an absent/empty wire value to `wasm`).

        One case does NOT return a raw discriminator: a `text-model` artifact
        whose `version` this build does not implement substitutes the localized
        "needs a newer app" sentence here (`S.labeler_catalog.kind_needs_newer_app`)
        — the *says so* half of the unknown-version contract. Every ordinary row
        still paints the stable value ui.yaml pins."""
        return self.driver.get_text(
            "labeler-catalog-item-kind", scope=f"labeler-catalog-item[{index}]"
        )

    def inspect_list_name_text(self) -> str:
        """The decoded publisher-chosen name line (list-kind inspect only)."""
        return self.driver.get_text("labeler-inspect-list-name")

    def inspect_list_entry_count_text(self) -> str:
        return self.driver.get_text("labeler-inspect-list-entry-count")

    def inspect_list_entry_rows(self) -> int:
        return self.driver.count("labeler-inspect-list-entry")

    def inspect_list_entry_id(self, index: int = 0) -> str:
        return self.driver.get_text(
            "labeler-inspect-list-entry-id", scope=f"labeler-inspect-list-entry[{index}]"
        )

    def inspect_list_entry_score(self, index: int = 0) -> str:
        return self.driver.get_text(
            "labeler-inspect-list-entry-score", scope=f"labeler-inspect-list-entry[{index}]"
        )

    def wait_for_inspect_list_entries(self, expected: int, timeout: float = 10.0) -> int:
        """Poll until the inspect panel renders `expected` list-entry rows
        (inspect round-trips fauna.labelers.inspect before the panel fills)."""
        deadline = time.monotonic() + timeout
        count = self.inspect_list_entry_rows()
        while count != expected and time.monotonic() < deadline:
            time.sleep(0.2)
            count = self.inspect_list_entry_rows()
        return count

    # ── text-model-kind inspect section (frame § Tier-3 artifact kinds:
    # inspect renders the FULL vocabulary before subscribing; IDs
    # user-approved 2026-08-13) ──────────────────────────────────────────

    def inspect_model_name_text(self) -> str:
        """The decoded publisher-chosen name line (text-model-kind inspect
        only). The name rides inside the artifact, so inspect is where it first
        becomes visible."""
        return self.driver.get_text("labeler-inspect-model-name")

    def inspect_model_ngram_count_text(self) -> str:
        return self.driver.get_text("labeler-inspect-model-ngram-count")

    def inspect_model_entry_rows(self) -> int:
        return self.driver.count("labeler-inspect-model-entry")

    def inspect_model_entry_text(self, index: int = 0) -> str:
        return self.driver.get_text(
            "labeler-inspect-model-entry-text",
            scope=f"labeler-inspect-model-entry[{index}]",
        )

    def inspect_model_entry_direction(self, index: int = 0) -> str:
        return self.driver.get_text(
            "labeler-inspect-model-entry-direction",
            scope=f"labeler-inspect-model-entry[{index}]",
        )

    def inspect_model_entry_count(self, index: int = 0) -> str:
        return self.driver.get_text(
            "labeler-inspect-model-entry-count",
            scope=f"labeler-inspect-model-entry[{index}]",
        )

    def inspect_model_entry_texts(self) -> list[str]:
        """Every rendered n-gram, in panel order — the whole vocabulary, which
        is what a subscriber is being asked to trust."""
        return [
            self.inspect_model_entry_text(i)
            for i in range(self.inspect_model_entry_rows())
        ]

    def wait_for_inspect_model_entries(self, minimum: int = 1, timeout: float = 10.0) -> int:
        """Poll until the panel renders at least `minimum` vocabulary rows."""
        deadline = time.monotonic() + timeout
        count = self.inspect_model_entry_rows()
        while count < minimum and time.monotonic() < deadline:
            time.sleep(0.2)
            count = self.inspect_model_entry_rows()
        return count
