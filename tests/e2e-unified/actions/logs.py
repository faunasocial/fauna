"""Settings → Logs — the client's durable log record
(docs/goal/architecture/apps/observability.md § Surfaces).

A Settings rail sub-page that renders the process-global `fauna_log` ring
(`snapshot()`) newest-first with a severity filter (`log-level-filter`), a
copy-to-clipboard affordance (`log-copy-button`), and a clear button
(`log-clear-button`). No client handle is needed — the ring is a process global,
so the page self-wires. Same IDs + flows on all seven apps (priority #1); linux
leads, the rest lift this shape.

The level filter maps onto `fauna_log::snapshot_at_least(level)`: selecting a
severity shows that level and everything more severe; "All" shows `snapshot()`.
"""
from __future__ import annotations

import time
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class LogsActions:
    """Drive the Settings → Logs sub-page (observability.md § Surfaces)."""

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def navigate(self) -> None:
        """Navigate to the Logs Settings sub-page.

        Sidebar-swap shell on linux/windows/web; the sub-id is ignored on
        single-scroll clients (same two-element nav the other settings sub-pages
        use). The ring renders synchronously from the process-global snapshot, so
        no async hydrate wait is needed beyond letting GTK map the page.
        """
        self.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "logs"}]},
        })
        time.sleep(1)

    def is_page_visible(self, timeout: float = 10.0) -> bool:
        """True once the Logs page landmark (`settings-logs`) is present."""
        try:
            self.driver.wait_for("settings-logs", timeout=timeout)
            return True
        except TimeoutError:
            return False

    def entry_count(self) -> int:
        """How many log lines the view currently renders (`log-entry`)."""
        return self.driver.count("log-entry")

    def entry_texts(self) -> list[str]:
        """The rendered text of each log line (`log-entry`, indexed) — a
        "level · time · target · message" string. Lets a test assert a specific
        captured event is present (e.g. a producer-logged displayed error,
        observability.md § Log on the *event*, not the *paint*)."""
        return [
            self.driver.get_text("log-entry", index=i) or ""
            for i in range(self.entry_count())
        ]

    def any_entry_contains(self, needle: str) -> bool:
        """True if some rendered log line contains `needle` (substring)."""
        return any(needle in t for t in self.entry_texts())

    def wait_for_min_entries(self, minimum: int = 1, timeout: float = 10.0) -> bool:
        """Poll until at least `minimum` log lines are rendered.

        The ring captures `tracing` events; the client logs a startup line at
        `main()` start (`install_logging`), so a settled, logged-in client always
        has at least one entry.
        """
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.entry_count() >= minimum:
                return True
            time.sleep(0.3)
        return self.entry_count() >= minimum

    def set_level(self, level: str) -> None:
        """Set the severity filter (`log-level-filter`).

        `level` is one of the displayed option labels: "All", "Error", "Warn",
        "Info", "Debug", "Trace". Re-renders the list from
        `fauna_log::snapshot_at_least`.
        """
        self.driver.select("log-level-filter", level)
        time.sleep(0.5)

    def copy(self) -> None:
        """Click the copy-to-clipboard affordance (`log-copy-button`)."""
        self.driver.click("log-copy-button")
        time.sleep(0.3)

    def clear(self) -> None:
        """Clear the in-memory ring (`log-clear-button` → `fauna_log::clear`).
        Does not touch the on-disk rolling file; the list re-renders empty."""
        self.driver.click("log-clear-button")
        time.sleep(0.5)
