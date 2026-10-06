from __future__ import annotations

import time
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class ModerationActions:
    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def navigate(self) -> None:
        """Navigate to the moderation page."""
        self.driver.navigate_to("moderation")

    def require_web_throwaway_sender_local_detection_leg(self) -> None:
        """Skip unless this app is web — the only client with a
        single-real-engine local-detection leg (a throwaway sender engine
        + one real receiving GUI), as opposed to the two-real-engine
        `second_real_faunamls_app` leg linux/tui use (e2e convention 7 — the
        platform check lives in the action layer, not the test body).

        The native FFI clients' equivalent readers are the cross-machine
        slice-4 legs (`test_moderation_local_detection.py`'s module
        docstring) — not yet built for any of them."""
        if not self.driver.is_web():
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="the single-real-engine local-detection leg (web's "
                        "throwaway-sender-engine test shape)",
                detail="only web has this leg today; the native FFI clients' "
                       "equivalent is the cross-machine slice-4 legs, not yet "
                       "built for any of them",
                tracked="test_moderation_local_detection.py module docstring "
                        "(slice-4 legs)",
            )

    def require_second_real_engine_fixture_supported(self, *, flow: str) -> None:
        """Skip unless this app is one of the two the `second_real_faunamls_app`
        fixture supports (e2e convention 7 — the platform check lives in the
        action layer, not the test body).

        These journeys drive `flow` through TWO real GUI engines at once
        (alice's + bob's), so both need a standalone moderation queue AND a
        `second_real_faunamls_app`-capable fixture: linux (the original leg)
        and tui (joined 2026-07-29 with its own moderation queue).

        ⚠ **Widening means widening TWO places.** Each consuming test also
        carries `@pytest.mark.linux`/`@pytest.mark.tui` so collection deselects
        it before any fixture runs — necessary since `real_faunamls_app` became
        a real precondition on the launch-gate apps
        (`helpers/real_rail_control.py`), which would otherwise raise here
        before this guard ever got to speak. This remains the single place the
        supported set is *described*; the markers are its collection-time
        shadow."""
        if not (self.driver.is_linux() or self.driver.is_tui()):
            from helpers.app_surface import skip_unbuilt

            skip_unbuilt(
                self.driver,
                surface="a second_real_faunamls_app-capable standalone "
                        "moderation queue",
                detail=f"drives {flow} through two real GUI engines — only "
                       "linux and tui support the second-real-engine fixture "
                       "today",
                tracked="second_real_faunamls_app fixture (conftest.py)",
            )

    def correction_count(self) -> int:
        """Return the number of train-correction buttons (one per item)."""
        return self.driver.count("train-correction-button")

    def train_correction(self, index: int = 0) -> None:
        """Click the train-correction button at the given index."""
        self.driver.click("train-correction-button", index=index)
        time.sleep(1)

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
