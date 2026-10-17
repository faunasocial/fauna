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

import re
import time
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


#: A rendered row's ``LEVEL · HH:MM:SS · target`` head (`fauna_log::format`).
ROW_HEAD = re.compile(r"(TRACE|DEBUG|INFO|WARN|ERROR) · (\d\d:\d\d:\d\d) · (\S+)")

#: Severity rank, most severe first — the order `snapshot_at_least` filters on.
LEVELS = ("ERROR", "WARN", "INFO", "DEBUG", "TRACE")


def parse_row(text: str) -> tuple[str, str, str, str] | None:
    """One rendered row (or one copied line) as ``(level, time, target,
    message)``, or None when it carries no row head.

    Two row shapes, both from `fauna_log::format`: the one-line `format_line`
    (``level · time · target · message`` — tui and linux rows, and every app's
    copied text) and the two-line `LogRow` (the message, then its
    ``level · time · target`` subtitle — web)."""
    m = ROW_HEAD.search(text)
    if not m:
        return None
    if m.start() == 0:
        message = text[m.end():].removeprefix(" · ").strip()
    else:
        message = text[: m.start()].strip()
    return m.group(1), m.group(2), m.group(3), message


class LogsActions:
    """Drive the Settings → Logs sub-page (observability.md § Surfaces)."""

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def navigate(self) -> None:
        """Navigate to the Logs Settings sub-page.

        Sidebar-swap shell on linux/windows/web; the sub-id is ignored on
        single-scroll clients (same two-element nav the other settings sub-pages
        use). Returns once the page landmark is up (or its budget is spent —
        callers assert `is_page_visible()`); the rows render from the
        process-global snapshot, and a caller reading them polls.
        """
        self.driver.set_state({
            "nav": {"stack": [{"view": "settings"}, {"view": "settings", "id": "logs"}]},
        })
        self.is_page_visible()

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

    def rows(self) -> list[tuple[str, str, str, str]]:
        """The rendered rows as ``(level, time, target, message)``, top to
        bottom — see :func:`parse_row`. Rows without a head are skipped."""
        return [r for r in map(parse_row, self.entry_texts()) if r]

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
        `fauna_log::snapshot_at_least`; a caller reading the narrowed list polls
        for it (convention 14 — no fixed settle).
        """
        self.driver.select("log-level-filter", level)

    def copy(self) -> None:
        """Click the copy-to-clipboard affordance (`log-copy-button`). A caller
        reading the clipboard polls `driver.get_clipboard_text()`."""
        self.driver.click("log-copy-button")

    def clear(self) -> None:
        """Clear the in-memory ring (`log-clear-button` → `fauna_log::clear`).
        Does not touch the on-disk rolling file; the list re-renders empty, and
        a caller asserting that polls."""
        self.driver.click("log-clear-button")
