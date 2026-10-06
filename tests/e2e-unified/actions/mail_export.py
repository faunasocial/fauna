from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class MailExportActions:
    """Drive the user-facing mail-export wizard (docs/goal/behavior/mail-export.md
    § UX shape; tests/e2e-unified/ui.yaml `mail-export` page).

    A person exports **their own** mailbox here — a five-step wizard (Format →
    Scope → Confirm → Progress → Done) backed by the shared
    `fauna_client_mail_settings::MailExportMachine` over the (currently unbuilt)
    user-tier `fauna.bridges.{start,list,pause,resume,cancel,finalize}_export_session`
    + `discard_export_blob` surface + the `GET /api/v1/export/<id>` download path.

    The page is reached via the settings/status view on every app (the
    adw::PreferencesWindow modal is not reachable through the state protocol), so
    navigating to "settings" surfaces the mail-export IDs. Action methods are kept
    uniform across all 7 apps — linux is the lead; the
    other five lift this shape.

    Steps 1–3 (Format / Scope / Confirm) are client-side. Format↔Scope↔Confirm
    navigation uses the shared `wizard-next-button` / `wizard-back-button` ids,
    and each mailbox row is an indexed `mail-export-scope-mailbox-item` whose
    text is the mailbox name and whose ``state`` attribute is ``on``/``off``
    (all user-approved 2026-08-29, mirroring `MailImportActions`'s identical
    shape approved 2026-08-28). The durable commit is the tagged
    mail-export-start-button.
    """

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def navigate(self) -> None:
        """Navigate to the mail-export Settings sub-page (sidebar-swap shell on
        linux; the sub-id is ignored on single-scroll clients)."""
        self.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "mail-export"}]},
        })

    def is_page_visible(self, timeout: float = 10.0) -> bool:
        """True when the mail-export wizard is reachable (format picker present)."""
        try:
            self.driver.wait_for("mail-export-format-picker", timeout=timeout)
            return True
        except TimeoutError:
            return False

    def error_text(self) -> str:
        """The page-level error-message text (the unbuilt-backend explanation
        until the export job model lands).

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

    # ── step 1 — format ───────────────────────────────────────────────────

    def select_format(self, label: str) -> None:
        """Pick the export format from the picker (mbox / Maildir++ / EML zip)."""
        self.driver.select("mail-export-format-picker", label)

    def next(self) -> None:
        """Step 1→2 or step 2→3 — the shared wizard Next (same id both steps)."""
        self.driver.wait_for("wizard-next-button", timeout=10.0)
        self.driver.click("wizard-next-button")

    def back(self) -> None:
        """Step 2→1 or step 3→2 — the shared wizard Back."""
        self.driver.wait_for("wizard-back-button", timeout=10.0)
        self.driver.click("wizard-back-button")

    # ── step 2 — scope ────────────────────────────────────────────────────

    _MAILBOX_ITEM = "mail-export-scope-mailbox-item"

    def mailbox_names(self) -> list[str]:
        """The source mailboxes offered on the Scope step, in painted order.

        The rows are an *indexed* element (convention 1), so the name is the
        row's text and position is the only handle a driver has on it.
        """
        return self.driver.get_texts(self._MAILBOX_ITEM)

    def _mailbox_index(self, name: str) -> int:
        names = self.mailbox_names()
        if name not in names:
            raise AssertionError(
                f"no mailbox row named {name!r} on the Scope step; offered: {names}"
            )
        return names.index(name)

    def mailbox_selected(self, name: str) -> bool:
        """Whether `name` is selected for export.

        Read from the row's ``state`` attribute rather than its glyph: the
        checkbox mark is a per-app rendering detail, the attribute is the
        contract every app serves.
        """
        return (
            self.driver.get_attr(
                self._MAILBOX_ITEM, "state", index=self._mailbox_index(name)
            )
            == "on"
        )

    def toggle_mailbox(self, name: str) -> None:
        self.driver.click(self._MAILBOX_ITEM, index=self._mailbox_index(name))

    def set_date_from(self, value: str) -> None:
        self.driver.clear_and_type("mail-export-scope-date-from", value)

    def set_date_to(self, value: str) -> None:
        self.driver.clear_and_type("mail-export-scope-date-to", value)

    def toggle_strip_headers(self) -> None:
        self.driver.click("mail-export-scope-strip-headers-toggle")

    # ── step 3 — confirm / commit ─────────────────────────────────────────

    def confirm_summary(self) -> str:
        return self.driver.get_text("mail-export-confirm-summary")

    def start(self) -> None:
        """Durable commit → MailExportAction::Start (opens the export_sessions row)."""
        self.driver.wait_for("mail-export-start-button", timeout=10.0)
        self.driver.click("mail-export-start-button")

    # ── step 4 — progress ─────────────────────────────────────────────────

    def progress_summary(self) -> str:
        return self.driver.get_text("mail-export-progress-summary")

    def pause(self) -> None:
        self.driver.click("mail-export-pause-button")

    def resume(self) -> None:
        self.driver.click("mail-export-resume-button")

    def cancel(self) -> None:
        """Two-click inline confirm → MailExportAction::Cancel."""
        self.driver.click("mail-export-cancel-button")  # arm
        self.driver.click("mail-export-cancel-button")  # confirm

    def mailbox_progress_count(self) -> int:
        return self.driver.count("mail-export-mailbox-progress-list-item-name")

    # ── step 5 — done ─────────────────────────────────────────────────────

    def done_summary(self) -> str:
        return self.driver.get_text("mail-export-done-summary")

    def download(self) -> None:
        self.driver.click("mail-export-download-button")

    def discard(self) -> None:
        self.driver.click("mail-export-discard-button")
