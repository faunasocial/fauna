from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class NotificationsActions:
    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def navigate(self) -> None:
        """Navigate to notifications page."""
        self.driver.navigate_to("notifications")

    def notification_count(self) -> int:
        return self.driver.count("notification-item")

    def mark_all_read(self) -> None:
        self.driver.click("notification-mark-read")

    def first_notification_text(self) -> str:
        if self.driver.count("notification-item") == 0:
            return ""
        return self.driver.get_text("notification-item", index=0)

    def open_notification(self, index: int = 0) -> None:
        """Activate ``notification-item[index]`` — the row's "take me to what
        this is about" gesture (``behavior/notifications.md`` § User actions).

        A row whose shared-Rust destination is ``None`` renders **inert** (a
        label, not a control — § Deep-link destinations), so this is a no-op
        there rather than an error: the caller asserts on where it landed, not
        on the click.
        """
        self.driver.click("notification-item", index=index)

    def unread_badge_text(self) -> str:
        if not self.driver.is_visible("notification-count-badge"):
            return ""
        return self.driver.get_text("notification-count-badge")
