from __future__ import annotations

import time
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class MailSpamActions:
    """Drive the user-facing mail-spam page (docs/goal/behavior/mail-spam.md
    § Reset / § Cold start Path 2 / § Undo; tests/e2e-unified/ui.yaml `mail-spam`
    page).

    A person manages **their own** per-account spam classifier here — reset the
    per-user Bayesian model, opt in/out of the deployment-baseline contribution,
    and undo individual training events — backed by the shared
    `fauna_client_mail_settings::MailSpamMachine` over the user-tier
    `fauna.bridges.{reset_spam_model,list_spam_training_history,put_spam_model}`
    surface. Every history row is sealed to the user's own key, so the app
    unwraps its subject for display and undoes it client-side (the inverse delta
    and the row delete ride one `put_spam_model` write).

    The page is reached via the settings/status view on every app (the
    adw::PreferencesWindow modal is not reachable through the state protocol), so
    navigating to "settings" surfaces the mail-spam IDs. Action methods are kept
    uniform across all 7 apps — linux is the lead;
    the other five lift this shape.

    Reset is a two-click inline confirm (no modal — the linux state protocol can't
    open a separate window, same idiom as the mail-aliases destructive controls).
    """

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def navigate(self) -> None:
        """Navigate to the mail-spam Settings sub-page (sidebar-swap shell on
        linux; the sub-id is ignored on single-scroll clients)."""
        self.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "mail-spam"}]},
        })

    def is_page_visible(self, timeout: float = 10.0) -> bool:
        """True when the mail-spam page is reachable (reset button present).

        Uses wait_for so the driver scrolls the button into the viewport — the
        spam page sits below the mail-settings/aliases sections in the embedded
        settings view.
        """
        try:
            self.driver.wait_for("mail-spam-reset-model-button", timeout=timeout)
            return True
        except TimeoutError:
            return False

    def error_text(self) -> str:
        """The page-level error-message text (the unbuilt-backend explanation
        until the per-user spam loop lands).

        ``error-message`` is built hidden — absent from the registry, not
        merely empty — when there's nothing to show (`ErrorBanner.swift`), so
        an unguarded `get_text` 404s on a clean page. Check visibility first,
        matching `ActionLayer._message_from_element`.
        """
        if not self.driver.is_visible("error-message"):
            return ""
        try:
            return self.driver.get_text("error-message")
        except Exception:
            return ""

    # ── training-history reads ────────────────────────────────────────────

    def history_count(self) -> int:
        """How many training-history rows the list currently renders."""
        return self.driver.count("mail-spam-training-history-list-item-message")

    def history_messages(self) -> list[str]:
        """The rendered message of every training-history row, in order."""
        return [
            self.driver.get_text("mail-spam-training-history-list-item-message", i)
            for i in range(self.history_count())
        ]

    def history_message(self, index: int) -> str:
        return self.driver.get_text("mail-spam-training-history-list-item-message", index)

    def history_label(self, index: int) -> str:
        return self.driver.get_text("mail-spam-training-history-list-item-label", index)

    def history_source(self, index: int) -> str:
        return self.driver.get_text("mail-spam-training-history-list-item-source", index)

    # ── mutations ─────────────────────────────────────────────────────────

    def reset_classifier(self) -> None:
        """Two-click inline confirm → MailSpamAction::ResetModel."""
        self.driver.wait_for("mail-spam-reset-model-button", timeout=10.0)
        self.driver.click("mail-spam-reset-model-button")  # arm
        self.driver.click("mail-spam-reset-model-button")  # confirm

    def arm_reset(self) -> None:
        """The first click only — arms the two-click confirm without resetting."""
        self.driver.wait_for("mail-spam-reset-model-button", timeout=10.0)
        self.driver.click("mail-spam-reset-model-button")

    def reset_button_text(self) -> str:
        """The reset button's label — it relabels once armed, which is the
        confirm's whole visible affordance (no confirm id on this page)."""
        return self.driver.get_text("mail-spam-reset-model-button")

    def is_contribute_baseline_on(self) -> bool:
        """The contribute-baseline toggle's on/off, via the uniform `state` attr
        (the same idiom as `is_share_reports_on`)."""
        self.driver.wait_for("mail-spam-contribute-baseline-toggle", timeout=10.0)
        return (
            self.driver.get_attr("mail-spam-contribute-baseline-toggle", "state") or ""
        ).strip().lower() == "on"

    def set_threshold_override(self, value: str) -> None:
        """Type the account's spam-folder threshold (0–15, or blank to follow the
        deployment default) and commit it with Enter →
        `fauna.bridges.set_spam_threshold_override`."""
        self.driver.wait_for("mail-spam-threshold-override-input", timeout=10.0)
        self.driver.clear_and_type("mail-spam-threshold-override-input", value)
        self.driver.press_key("mail-spam-threshold-override-input", "Enter")

    def toggle_contribute_baseline(self) -> None:
        """Flip the deployment-baseline contribution toggle →
        MailSpamAction::SetContributeBaseline."""
        self.driver.wait_for("mail-spam-contribute-baseline-toggle", timeout=10.0)
        self.driver.click("mail-spam-contribute-baseline-toggle")

    # ── distributed report sharing (report-sharing.md § Client wire +
    # transparency surface) — a small dedicated flow over
    # fauna.moderation.report_share.{set,status}, NOT the MailSpamMachine
    # (a bool + read-only list, not a state machine). Sibling of the
    # contribute-baseline toggle above. ────────────────────────────────────

    def toggle_share_reports(self) -> None:
        """Flip the report-share opt-in → fauna.moderation.report_share.set."""
        self.driver.wait_for("mail-spam-share-reports-toggle", timeout=10.0)
        self.driver.click("mail-spam-share-reports-toggle")

    def is_share_reports_on(self) -> bool:
        """The report-share toggle's on/off, read via the uniform `state` attr
        idiom (driver.get_attr(id, "state") — mirrors mail_settings.serve_here_state)."""
        return (self.driver.get_attr("mail-spam-share-reports-toggle", "state") or "").strip().lower() == "on"

    def published_count(self) -> int:
        """How many `report-share-published-list-item` rows are rendered — the
        ≥k aggregates this nest currently exports to peers."""
        return self.driver.count("report-share-published-list-item-hash")

    #: Registry ids the published-list dump reports on — the section's own
    #: presence marker, the row component, and the leaf the test asserts through.
    _PUBLISHED_IDS = (
        "report-share-published-list",
        "report-share-published-list-item",
        "report-share-published-list-item-hash",
    )

    def published_diagnosis(self) -> str:
        """Failure-path only: why does the published list read empty?

        `published_count() == 0` conflates three different failures, and the
        cross-app `diagnose` probe alone cannot split them: the nest returned no
        aggregate (the VM hydrated an empty list — the section renders its
        `published_empty` marker, so `report-share-published-list` IS
        registered); the VM never hydrated (no marker either, but an error is
        usually surfaced); or the section was never realized at all, which on an
        iOS lazy `Form` means nothing below the fold ever `.onAppear`-registers
        (apple-e2e-automation.md § Registration rules, rule 6). Apple's
        in-process registry keeps every slot it ever placed, tagged with why it
        currently reads visible or hidden (live sentinel geometry vs. the two
        off-screen votes), so its `/tree` dump separates exactly those — see
        `AutomationRegistry.debugDump`. Mirrors
        `EventsActions._registry_dump`, the same probe for the same class of
        never-built-row failure.

        Empty on every app whose bridge has no `/tree` (`tree()` returns ""),
        which is the honest answer there rather than a fabricated one.
        """
        parts = [
            self.driver.diagnose("report-share-published-list"),
            self.driver.diagnose("mail-spam-reset-model-button"),
        ]
        try:
            dump = getattr(self.driver, "tree", lambda: "")()
        except Exception as exc:  # noqa: BLE001 — diagnostic only; never mask the failure
            parts.append(f"<registry dump failed: {type(exc).__name__}: {exc}>")
            return "; ".join(parts)
        if not dump:
            parts.append("<no /tree on this bridge>")
            return "; ".join(parts)
        kept: list[str] = []
        keeping = False
        for line in dump.splitlines():
            if line.startswith(" "):
                if keeping:
                    kept.append(line.strip())
                continue
            keeping = line.split(" ")[0] in self._PUBLISHED_IDS
            if keeping:
                kept.append(line)
        parts.append(
            "REGISTRY: " + (" | ".join(kept) if kept else "<no published-list slots registered>")
        )
        return "; ".join(parts)

    def published_hash(self, index: int) -> str:
        """The content-hash of the published aggregate at `index`."""
        return self.driver.get_text("report-share-published-list-item-hash", index)

    def published_factor(self, index: int) -> str:
        """The scoring factor (e.g. `report:spam`) of the published aggregate at `index`."""
        return self.driver.get_text("report-share-published-list-item-factor", index)

    def published_reporter_count(self, index: int) -> str:
        """The local reporter count (always ≥ k) of the published aggregate at `index`."""
        return self.driver.get_text("report-share-published-list-item-count", index)

    def undo_training(self, index: int) -> None:
        """Undo one training event → MailSpamAction::UndoTraining."""
        self.driver.click("mail-spam-training-history-list-item-undo-button", index)

    def wait_for_history_count(self, expected: int, timeout: float = 12.0) -> bool:
        """Poll until the list holds exactly `expected` rows (the dispatch →
        snapshot → re-render round-trip is async)."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.history_count() == expected:
                return True
            time.sleep(0.3)
        return self.history_count() == expected
