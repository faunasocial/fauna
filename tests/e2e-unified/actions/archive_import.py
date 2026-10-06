from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class ArchiveImportActions:
    """Drive the user-facing archive-import wizard (docs/goal/behavior/
    archive-import.md § The wizard and its machine; tests/e2e-unified/ui.yaml
    `archive-import` page, IDs user-approved 2026-09-08).

    Six steps (Source → Archive → Scope → Confirm → Progress → Done) over the
    shared `fauna_archive_import_machine::ArchiveImportMachine`. tui is the lead
    app; the archive is a PATH there (tui.md § Declared platform absences item 4).
    Category rows are the indexed `archive-import-scope-category-item` whose
    text is the row label and whose ``state`` attribute is ``on``/``off``/``kept``
    (the mail-import mailbox-row shape).
    """

    _CATEGORY_ITEM = "archive-import-scope-category-item"

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def navigate(self) -> None:
        self.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "archive-import"}]},
        })

    def is_page_visible(self, timeout: float = 10.0) -> bool:
        try:
            self.driver.wait_for("archive-import-source-picker", timeout=timeout)
            return True
        except TimeoutError:
            return False

    def error_text(self) -> str:
        """The page-level error-message — built hidden when there is nothing
        to show (the mail-import reasoning), so check visibility first."""
        if not self.driver.is_visible("error-message"):
            return ""
        try:
            return self.driver.get_text("error-message")
        except Exception:
            return ""

    def _text_or_empty(self, element_id: str) -> str:
        """``""`` while the wizard has not painted the element — every reader
        here is polled across a step transition (the `MailImportActions`
        reasoning: a missing element 404s, and a bare get_text would die on
        the first poll)."""
        try:
            return self.driver.get_text(element_id)
        except LookupError:
            return ""

    # ── step 1 — source ──────────────────────────────────────────────────

    def select_source(self, label: str) -> None:
        self.driver.select("archive-import-source-picker", label)

    def next(self) -> None:
        self.driver.wait_for("wizard-next-button", timeout=10.0)
        self.driver.click("wizard-next-button")

    def back(self) -> None:
        self.driver.wait_for("wizard-back-button", timeout=10.0)
        self.driver.click("wizard-back-button")

    # ── step 2 — archive ─────────────────────────────────────────────────

    def set_archive_path(self, path: str) -> None:
        self.driver.clear_and_type("archive-import-archive-path", path)

    def open_archive(self) -> None:
        self.driver.wait_for("archive-import-archive-open-button", timeout=10.0)
        self.driver.click("archive-import-archive-open-button")

    def archive_summary(self) -> str:
        return self._text_or_empty("archive-import-archive-summary")

    # ── step 3 — scope ───────────────────────────────────────────────────

    def category_labels(self) -> list[str]:
        return self.driver.get_texts(self._CATEGORY_ITEM)

    def _category_index(self, label_prefix: str) -> int:
        labels = self.category_labels()
        for i, label in enumerate(labels):
            if label.startswith(label_prefix):
                return i
        raise AssertionError(f"no category row starting {label_prefix!r}; rows: {labels}")

    def category_state(self, label_prefix: str) -> str:
        """``on`` / ``off`` / ``kept`` — the row's ``state`` attribute."""
        return self.driver.get_attr(self._CATEGORY_ITEM, "state", index=self._category_index(label_prefix))

    def toggle_category(self, label_prefix: str) -> None:
        self.driver.click(self._CATEGORY_ITEM, index=self._category_index(label_prefix))

    def audience_summary(self) -> str:
        return self._text_or_empty("archive-import-scope-audience-summary")

    def select_audience_mode(self, label: str) -> None:
        self.driver.select("archive-import-scope-audience-mode", label)

    # ── step 4 — confirm / commit ────────────────────────────────────────

    def confirm_summary(self) -> str:
        return self._text_or_empty("archive-import-confirm-summary")

    def start(self) -> None:
        self.driver.wait_for("archive-import-start-button", timeout=10.0)
        self.driver.click("archive-import-start-button")

    # ── step 5 — progress ────────────────────────────────────────────────

    def progress_summary(self) -> str:
        return self._text_or_empty("archive-import-progress-summary")

    def error_log(self) -> str:
        return self._text_or_empty("archive-import-error-log")

    def pause(self) -> None:
        self.driver.click("archive-import-pause-button")

    def resume(self) -> None:
        self.driver.wait_for("archive-import-resume-button", timeout=10.0)
        self.driver.click("archive-import-resume-button")

    def cancel(self) -> None:
        self.driver.click("archive-import-cancel-button")  # arm
        self.driver.click("archive-import-cancel-button")  # confirm

    def arm_pause_after(self, records: int) -> None:
        """Arm the machine's one-shot pause after ``records`` settled records —
        the restart-resume journey's causal anchor (convention 14). A test-agent
        command, never user-facing."""
        reply = self.driver.call_command("archive_import_pause_after", {"records": records})
        assert not (isinstance(reply, dict) and reply.get("error")), reply

    # ── step 6 — done ────────────────────────────────────────────────────

    def done_summary(self) -> str:
        return self._text_or_empty("archive-import-done-summary")

    def folder_link_text(self) -> str:
        return self._text_or_empty("archive-import-folder-link")

    def view_imported(self) -> None:
        self.driver.click("archive-import-view-imported-button")

    def review_skipped(self) -> None:
        self.driver.click("archive-import-review-skipped-button")
