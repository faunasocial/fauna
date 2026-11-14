from __future__ import annotations

import time
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class ModerationActions:
    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    #: How long a fresh visit may show a loading-state error before it settles.
    _SETTLE_BUDGET_S = 30.0

    def navigate(self) -> None:
        """Navigate to the moderation page and let its first reads settle.

        Web hosts the queue on its Privacy settings page, which paints the
        inbox-mode-unknown sentence on ``error-message`` while the inbox-mode
        read is still out (``privacy_page.inbox_mode_unknown``) — a true
        loading state, not a failure. A caller asserting "no page error" right
        after the visit would race that read, so the visit polls (a deadline,
        never a fixed sleep — convention 14) until the error clears; one that
        persists past the budget is left showing, for the caller's own
        assertion to name.
        """
        from actions import ActionLayer

        self.driver.navigate_to("moderation")
        page = ActionLayer(self.driver)
        deadline = time.monotonic() + self._SETTLE_BUDGET_S
        while time.monotonic() < deadline and page.has_error():  # deadline-ok: settle only; a persistent error is the caller's assertion to name
            time.sleep(0.25)

    def require_web_throwaway_sender_local_detection_leg(self) -> None:
        """Skip unless this app is web — the only client with a
        single-real-engine local-detection leg (a throwaway sender engine
        + one real receiving GUI), as opposed to the two-real-engine
        `second_real_faunamls_app` leg linux/tui use (e2e convention 7 — the
        platform check lives in the action layer, not the test body).

        The native FFI clients' equivalent readers are the cross-machine
        slice-4 legs (`test_moderation_local_detection.py`'s module
        docstring) — not yet built for any of them."""
        from helpers.app_surface import require_surface

        require_surface(self.driver, "single-engine-local-detection")

    def require_second_real_engine_fixture_supported(self, *, flow: str) -> None:
        """Skip unless this app is one of those the `second_real_faunamls_app`
        fixture supports (e2e convention 7 — the platform check lives in the
        action layer, not the test body).

        These journeys drive `flow` through TWO real GUI engines at once
        (alice's + bob's), so both need a standalone moderation queue AND a
        `second_real_faunamls_app`-capable fixture: linux (the original leg),
        tui (joined 2026-07-29 with its own moderation queue), macos, ios and
        windows (joined 2026-11-11 — its fixture seat and its queue's
        local-detection row were already built).

        ⚠ **Widening means widening TWO places.** Each consuming test also
        carries its app markers so collection deselects
        it before any fixture runs — necessary since `real_faunamls_app` became
        a real precondition on the launch-gate apps
        (`helpers/real_rail_control.py`), which would otherwise raise here
        before this guard ever got to speak. This remains the single place the
        supported set is *described*; the markers are its collection-time
        shadow."""
        from helpers.app_surface import require_surface

        require_surface(self.driver, "moderation-queue-second-real-engine")

    def correction_count(self) -> int:
        """Return the number of train-correction buttons (one per item)."""
        return self.driver.count("train-correction-button")

    def train_correction(self, index: int = 0) -> None:
        """Click the train-correction button at the given index."""
        self.driver.click("train-correction-button", index=index)
        time.sleep(1)

    # ── A row's facts (moderation.md § Element IDs → *Queue row facts*): its
    #    category is the `content-label-badge` text, and the badge carries the
    #    confidence, the action and a delivered row's issuer the row paints as
    #    `confidence` / `action` / `issuer` attributes — the row's other text
    #    has no id of its own. ──

    def row_category(self, index: int = 0) -> str:
        """The category label the row at `index` paints on its badge."""
        return self.driver.get_text("content-label-badge", index=index)

    def row_confidence(self, index: int = 0) -> str | None:
        """The confidence the row at `index` shows (e.g. ``"100%"``)."""
        return self.driver.get_attr("content-label-badge", "confidence", index=index)

    def row_action(self, index: int = 0) -> str | None:
        """The action label the row at `index` shows, or ``None`` when the row
        carries no action (a local detection)."""
        return self.driver.get_attr("content-label-badge", "action", index=index)

    def row_issuer(self, index: int = 0) -> str | None:
        """The domain of the nest whose admin decided the row at `index`, or
        ``None`` on this nest's own decision and on a local detection (the
        ``issuer`` fact — a row another nest delivered, ``moderation.md``
        § Legal takedown → *The author homed on another nest*)."""
        return self.driver.get_attr("content-label-badge", "issuer", index=index)

    # ── Appeal (moderation.md § Legal takedown — the transparency triple's
    #    second leg). `appeal-button` is per-row and FLAT-indexed like
    #    `train-correction-button`; the form's ids are unindexed because only
    #    one appeal is open at a time. ──

    def appeal_count(self) -> int:
        """The number of appeal buttons — one per nest-issued enforcement row.

        A local detection paints none (there is no nest-side decision behind it
        to appeal), so this is NOT the same as `correction_count()`; the gap
        between them is exactly the local half of the queue.
        """
        return self.driver.count("appeal-button")

    def open_appeal(self, index: int = 0) -> None:
        """Open the appeal form on the row at the given index."""
        self.driver.click("appeal-button", index=index)

    def appeal_form_open(self) -> bool:
        """Whether the appeal form is painted."""
        return self.driver.count("appeal-reason-input") > 0

    def appeal_submit_enabled(self) -> bool:
        """Whether the submit control is live — the shared fold's `can_submit`
        rendered (a reasonless appeal is never submittable)."""
        return self.driver.is_enabled("appeal-submit-button")

    def fill_appeal_reason(self, reason: str) -> None:
        """Type the appeal's reason."""
        self.driver.fill("appeal-reason-input", reason)

    def submit_appeal(self) -> None:
        """Dispatch the open appeal (`fauna.moderation.appeal`)."""
        self.driver.click("appeal-submit-button")

    def cancel_appeal(self) -> None:
        """Drop the draft without dispatching."""
        self.driver.click("appeal-cancel-button")

    def appeal_status_text(self) -> str:
        """The appeal's outcome line (present only after an attempt)."""
        return self.driver.get_text("appeal-status")

    def has_appeal_status(self) -> bool:
        """Whether an outcome line is painted at all."""
        return self.driver.count("appeal-status") > 0

    # ── The reporter's own ledger (`moderation-reports-section` —
    #    moderation.md § User-initiated reporting → What the reporter is told) ──

    def report_rows(self) -> list[str]:
        """Every ledger row's text (`moderation-report-item`, flat indexed)."""
        return [
            self.driver.get_text("moderation-report-item", index=i)
            for i in range(self.driver.count("moderation-report-item"))
        ]

    def withdraw_count(self) -> int:
        """How many open rows offer withdraw."""
        return self.driver.count("moderation-report-withdraw-button")

    def withdraw_report(self, index: int = 0) -> None:
        """Withdraw the open report whose button is at ``index``."""
        self.driver.click("moderation-report-withdraw-button", index=index)
