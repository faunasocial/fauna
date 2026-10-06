"""The shared report sheet (`report-sheet` — moderation.md § User-initiated
reporting → App surface).

One component on every page that opens it (feed ⋯, message ⋯, an OTHER
profile), so one action group: a test opens the sheet through the page's own
verb (the three ``open_from_*`` helpers), then drives the members here. The
sheet paints once and flat, so its members are read unscoped.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class ReportActions:
    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    # ── The three entry verbs ──

    def open_from_post(self, feed, index: int) -> None:
        """Open the sheet on the feed post at ``index`` (its ⋯ menu →
        `feed-post-report-button`). ``feed`` is the caller's FeedActions."""
        feed.open_post_actions(index)
        self.driver.wait_for("feed-post-report-button", timeout=10)
        self.driver.click("feed-post-report-button")
        self.driver.wait_for("report-sheet", timeout=10)

    def open_from_profile(self) -> None:
        """Open the sheet on the OTHER profile open now (`profile-report-button`)."""
        self.driver.wait_for("profile-report-button", timeout=10)
        self.driver.click("profile-report-button")
        self.driver.wait_for("report-sheet", timeout=10)

    def open_from_message_menu(self) -> None:
        """Open the sheet from an already-open `dm-message-actions-menu`."""
        self.driver.wait_for("dm-message-report-button", timeout=10)
        self.driver.click("dm-message-report-button")
        self.driver.wait_for("report-sheet", timeout=10)

    # ── The sheet ──

    def is_open(self) -> bool:
        return self.driver.count("report-sheet") > 0

    def choose_reason(self, token: str) -> None:
        """Pick a reason by its shared token (`spam`, `harassment`, `hate`,
        `violence`, `sexual`, `illegal`, `impersonation`, `other`)."""
        self.driver.select("report-reason-select", token)

    def fill_note(self, note: str) -> None:
        self.driver.fill("report-note-input", note)

    def has_include_text(self) -> bool:
        """Whether the excerpt checkbox renders — a sealed subject only."""
        return self.driver.count("report-include-text-checkbox") > 0

    def tick_include_text(self) -> None:
        self.driver.click("report-include-text-checkbox")

    def tick_block_author(self) -> None:
        self.driver.click("report-block-author-checkbox")

    def submit_enabled(self) -> bool:
        return self.driver.is_enabled("report-submit-button")

    def submit(self) -> None:
        self.driver.click("report-submit-button")

    def cancel(self) -> None:
        self.driver.click("report-cancel-button")

    def has_status(self) -> bool:
        return self.driver.count("report-status") > 0

    def status_text(self) -> str:
        """The acknowledgement naming where the report went."""
        return self.driver.get_text("report-status")
