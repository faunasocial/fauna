from __future__ import annotations

import time
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class MailListMembersActions:
    """Drive the user-facing mail-list-members page
    (docs/goal/behavior/mail-mass-mailing.md § mail-list-members page;
    tests/e2e-unified/ui.yaml `mail-list-members` page).

    A person manages the members of **one** of their mailing lists here — the
    subscribed/unsubscribed summary, add a member, batch-import addresses, and
    per-member unsubscribe / resubscribe — backed by the shared
    `fauna_client_mail_settings::MailListMembersMachine` over the user-tier
    `fauna.bridges.{list,add,batch_import,unsubscribe,resubscribe}_list_member`
    surface (live nest-side since 2026-06-13; the client seam was rewired onto
    it 2026-07-29).

    Action methods are kept uniform across all 7 apps. The add + import sheets are inline reveals, not modals.
    """

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def navigate(self) -> None:
        # mail-list-members Settings sub-page (sidebar-swap shell on linux; the
        # sub-id is ignored on single-scroll clients).
        self.driver.set_state({
            "nav": {
                "stack": [{"view": "settings"}, {"view": "settings", "id": "mail-list-members"}]
            },
        })

    def is_page_visible(self, timeout: float = 10.0) -> bool:
        try:
            self.driver.wait_for("mail-list-members-add-button", timeout=timeout)
            return True
        except TimeoutError:
            return False

    def summary(self) -> str:
        return self.driver.get_text("mail-list-members-summary")

    def row_count(self) -> int:
        return self.driver.count("mail-list-members-list-item-address")

    def addresses(self) -> list[str]:
        return [
            self.driver.get_text("mail-list-members-list-item-address", i)
            for i in range(self.row_count())
        ]

    def add_member(self, address: str) -> None:
        self.driver.wait_for("mail-list-members-add-button", timeout=10.0)
        self.driver.click("mail-list-members-add-button")
        self.driver.wait_for("mail-list-members-add-sheet-address-input", timeout=10.0)
        self.driver.clear_and_type("mail-list-members-add-sheet-address-input", address)
        self.driver.click("mail-list-members-add-sheet-submit-button")

    def batch_import(self, addresses: str) -> None:
        self.driver.click("mail-list-members-import-button")
        self.driver.wait_for("mail-list-members-import-sheet-input", timeout=10.0)
        self.driver.clear_and_type("mail-list-members-import-sheet-input", addresses)
        self.driver.click("mail-list-members-import-sheet-submit-button")

    def read_import_result(self, timeout: float = 10.0) -> str:
        """Return the rendered `mail-list-members-import-result` tally once it
        appears, else "" (poll — the import round-trip is async; the
        `mail-aliases-import-result` twin's idiom)."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.driver.is_visible("mail-list-members-import-result"):
                return self.driver.get_text("mail-list-members-import-result")
            time.sleep(0.3)
        return ""

    def unsubscribe(self, index: int) -> None:
        self.driver.click("mail-list-members-list-item-unsubscribe-button", index)

    def resubscribe(self, index: int) -> None:
        self.driver.click("mail-list-members-list-item-resubscribe-button", index)

    def status(self, index: int) -> str:
        """The rendered status of the member row at `index`."""
        return self.driver.get_text("mail-list-members-list-item-status", index)
