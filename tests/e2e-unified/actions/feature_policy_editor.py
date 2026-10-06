from __future__ import annotations

import time
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class FeaturePolicyEditorActions:
    """Drive the shared feature-policy editor (`feature-policy-editor-*`) —
    `docs/goal/architecture/dynamic-features.md` § Authoring surfaces.

    ONE editor, two hosts: the admin tier opens it from a row on the admin Nest
    page (`admin-nest-feature-limits-edit-button`, `AdminActions`), a person's
    own limit from a row of the Settings feature-limits section
    (`feature-limits-own-edit-button`, `SettingsActions`). Once open, the editor
    is the same ID family on both pages, so one action class drives it wherever
    it was opened — the hosts differ only in which tier the save lands at.

    Every read is a deadline poll with a generous budget, never a settle-sleep
    (`e2e-conventions.md` point 14): a save is a real nest round trip followed by
    a re-read, and the editor repaints only once both have landed.
    """

    WAIT_S = 20.0

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    def wait_open(self, timeout: float = WAIT_S) -> None:
        self.driver.wait_for("feature-policy-editor", timeout=timeout)

    def is_open(self) -> bool:
        return self.driver.count("feature-policy-editor") > 0

    def title(self) -> str:
        return self.driver.get_text("feature-policy-editor-title")

    def is_on(self) -> bool:
        """Whether the On radio is the marked one — the uniform
        `get_attr(id, "state")` idiom the other radio pairs read."""
        return self.driver.get_attr("feature-policy-editor-on-radio", "state") == "on"

    def choose_on(self) -> None:
        self.driver.click("feature-policy-editor-on-radio")

    def choose_off(self) -> None:
        self.driver.click("feature-policy-editor-off-radio")

    def cell_count(self) -> int:
        return self.driver.count("feature-policy-editor-cell")

    def cell(self, index: int) -> dict[str, str]:
        """One cell, read scoped to itself — the label, the typed text, and the
        no-effect note when one is painted."""
        scope = f"feature-policy-editor-cell[{index}]"
        cell = {
            "label": self.driver.get_text("feature-policy-editor-cell-label", scope=scope),
            "input": self.driver.get_text("feature-policy-editor-cell-input", scope=scope),
        }
        if self.driver.count("feature-policy-editor-cell-note", scope=scope) > 0:
            cell["note"] = self.driver.get_text("feature-policy-editor-cell-note", scope=scope)
        return cell

    def set_cell(self, index: int, text: str) -> None:
        self.driver.clear_and_type(
            "feature-policy-editor-cell-input",
            text,
            scope=f"feature-policy-editor-cell[{index}]",
        )

    def wait_for_cell_note(
        self, index: int, *, present: bool, timeout: float = WAIT_S
    ) -> dict[str, str]:
        """Poll until cell `index`'s note is present (or absent); return the
        last-seen cell either way so the caller's assert carries it."""
        deadline = time.monotonic() + timeout
        cell = self.cell(index)
        while time.monotonic() < deadline:
            if ("note" in cell) == present:
                return cell
            time.sleep(0.2)
            cell = self.cell(index)
        return cell

    def save(self) -> None:
        self.driver.click("feature-policy-editor-save-button")

    def remove_present(self) -> bool:
        return self.driver.count("feature-policy-editor-remove-button") > 0

    def remove(self) -> None:
        self.driver.click("feature-policy-editor-remove-button")

    def cancel(self) -> None:
        self.driver.click("feature-policy-editor-cancel-button")

    def status(self) -> str:
        if self.driver.count("feature-policy-editor-status") == 0:
            return ""
        return self.driver.get_text("feature-policy-editor-status")

    def wait_for_status(self, expected: str, timeout: float = WAIT_S) -> str:
        """Poll the verdict line until it reads `expected`; return the last read."""
        deadline = time.monotonic() + timeout
        seen = self.status()
        while time.monotonic() < deadline:
            if seen == expected:
                return seen
            time.sleep(0.2)
            seen = self.status()
        return seen

    def close_if_open(self) -> None:
        """Best-effort teardown: a test that failed with the editor open must not
        leak it into its siblings on the session-scoped app."""
        try:
            if self.is_open():
                self.cancel()
        except Exception:
            pass
