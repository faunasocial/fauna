from __future__ import annotations

from typing import TYPE_CHECKING

from helpers.waiting import wait_until

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class MailListsActions:
    """Drive the user-facing mail-lists page (docs/goal/behavior/mail-mass-mailing.md
    § mail-lists page UX; tests/e2e-unified/ui.yaml `mail-lists` page).

    A person runs **their own** mailing lists here (a list is a sixth alias kind)
    — create / edit / delete a list — backed by the shared
    `fauna_client_mail_settings::MailListsMachine` over the user-tier
    `fauna.bridges.{list,create,update,delete}_account_list` surface (live
    nest-side since 2026-06-13; the client seam was rewired onto it 2026-07-29).

    The page is reached via the settings/status view on every app; action
    methods are kept uniform across all 7 apps. The add/edit sheet is an inline reveal, not a modal.
    """

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def navigate(self) -> None:
        # mail-lists Settings sub-page (sidebar-swap shell on linux; the sub-id is
        # ignored on single-scroll clients).
        self.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "mail-lists"}]},
        })

    def is_page_visible(self, timeout: float = 10.0) -> bool:
        try:
            self.driver.wait_for("mail-lists-add-button", timeout=timeout)
            return True
        except TimeoutError:
            return False

    def error_text(self) -> str:
        """``error-message`` is built hidden — absent from the registry, not
        merely empty — when there's nothing to show (`ErrorBanner.swift`), so
        an unguarded `get_text` 404s on a clean page. Check visibility first,
        matching `ActionLayer._message_from_element`."""
        if not self.driver.is_visible("error-message"):
            return ""
        try:
            return self.driver.get_text("error-message")
        except Exception:
            return ""

    def row_count(self) -> int:
        return self.driver.count("mail-lists-list-item-name")

    def names(self) -> list[str]:
        return [
            self.driver.get_text("mail-lists-list-item-name", i)
            for i in range(self.row_count())
        ]

    def add_list(
        self,
        name: str,
        local_part: str,
        domain_label: str | None = None,
        *,
        description: str | None = None,
        help_url: str | None = None,
        archive_url: str | None = None,
        per_send_cap: int | None = None,
    ) -> None:
        """Open the add-sheet, fill it (the optional fields only when given),
        submit → MailListsAction::Create."""
        self.driver.wait_for("mail-lists-add-button", timeout=10.0)
        self.driver.click("mail-lists-add-button")
        self.driver.wait_for("mail-lists-add-sheet-name-input", timeout=10.0)
        self.driver.clear_and_type("mail-lists-add-sheet-name-input", name)
        self.driver.clear_and_type("mail-lists-add-sheet-local-part-input", local_part)
        if domain_label is not None:
            self.driver.select("mail-lists-add-sheet-domain-picker", domain_label)
        self._fill_optional(description, help_url, archive_url, per_send_cap)
        self.driver.click("mail-lists-add-sheet-submit-button")

    def _fill_optional(self, description, help_url, archive_url, per_send_cap) -> None:
        """The sheet's optional fields (`mail-mass-mailing.md` § Add list sheet);
        `None` leaves a field as it is."""
        for element_id, value in (
            ("mail-lists-add-sheet-description-input", description),
            ("mail-lists-add-sheet-list-help-url-input", help_url),
            ("mail-lists-add-sheet-list-archive-url-input", archive_url),
            ("mail-lists-add-sheet-per-send-cap-input", per_send_cap),
        ):
            if value is not None:
                self.driver.clear_and_type(element_id, str(value))

    def open_edit(self, index: int) -> None:
        """Open the edit sheet for the row at `index`, pre-populated.

        Waits for the name field to actually hold a value, not merely to be
        visible — convention 14: the sheet's presented content settles via its
        own `onAppear` hydrate, so a caller reading the field the instant it's
        visible can race an unsettled one."""
        self.driver.click("mail-lists-list-item-edit-button", index)
        self.driver.wait_for("mail-lists-add-sheet-name-input", timeout=10.0)
        wait_until(
            lambda: self.driver.get_text("mail-lists-add-sheet-name-input") or None,
            10.0,
            diagnose=lambda: (
                "the edit sheet's name field never hydrated: "
                f"{self.driver.diagnose('mail-lists-add-sheet-name-input')}"
            ),
        )

    def sheet_value(self, element_id: str) -> str:
        """The current text of one of the open sheet's inputs."""
        return self.driver.get_text(element_id)

    def submit_edit(
        self,
        *,
        name: str | None = None,
        description: str | None = None,
        help_url: str | None = None,
        archive_url: str | None = None,
        per_send_cap: int | None = None,
    ) -> None:
        """Change the open edit sheet and submit → MailListsAction::Update."""
        if name is not None:
            self.driver.clear_and_type("mail-lists-add-sheet-name-input", name)
        self._fill_optional(description, help_url, archive_url, per_send_cap)
        self.driver.click("mail-lists-add-sheet-submit-button")

    def row_text(self, element_id: str, index: int) -> str:
        """The text of one row leaf (`-member-count`, `-last-send`, `-quota`,
        `-delete-button`) at row `index`."""
        return self.driver.get_text(element_id, index)

    def arm_delete(self, index: int) -> None:
        """The first click of the two-click delete: arms it, deletes nothing."""
        self.driver.click("mail-lists-list-item-delete-button", index)

    def open_members(self, index: int) -> None:
        self.driver.click("mail-lists-list-item-members-button", index)

    def delete_list(self, index: int) -> None:
        """Two-click inline confirm → MailListsAction::Delete."""
        self.driver.click("mail-lists-list-item-delete-button", index)  # arm
        self.driver.click("mail-lists-list-item-delete-button", index)  # confirm
