from __future__ import annotations

import time
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from drivers.base import PlatformDriver


class RuleEditorActions:
    """Drive the shared rule editor (`rule-editor-*`) —
    `docs/goal/behavior/nest-rules.md` § The shared rule editor.

    ONE editor, several hosts: the admin's mounting in `admin-nest-rules-section`
    on the admin Nest page (`AdminActions.navigate_nest`), the guardian's in
    `family-policy-rules-section` for the selected ward. The component is the
    same ID family on every host, so one action class drives it wherever it is
    mounted; the hosts differ only in where the save lands.

    Rows are indexed: every per-rule control is scoped in `rule-editor-item[i]`.
    Pickers are LABEL pickers — a select takes the label the shared crate paints
    (`S.rule_editor.kind.keywords`, …). Every wait is a deadline poll (convention
    14): a save is a nest round trip, and the status repaints when it lands.
    """

    WAIT_S = 30.0

    def __init__(self, driver: PlatformDriver):
        self.driver = driver

    @staticmethod
    def _row(index: int) -> str:
        return f"rule-editor-item[{index}]"

    def wait_loaded(self, timeout: float = WAIT_S) -> None:
        """Wait until the host's read has landed — the add button paints only
        over a loaded document."""
        self.driver.wait_for("rule-editor-add-button", timeout=timeout)

    def rule_count(self) -> int:
        return self.driver.count("rule-editor-item")

    def add_rule(self) -> int:
        """Append a rule; its row index."""
        before = self.rule_count()
        self.driver.click("rule-editor-add-button")
        deadline = time.monotonic() + self.WAIT_S
        while time.monotonic() < deadline and self.rule_count() <= before:
            time.sleep(0.1)
        assert self.rule_count() == before + 1, (
            f"add did not append a rule: {self.rule_count()} rows, "
            f"status={self.status()!r}"
        )
        return before

    def pick(self, row: int, picker: str, label: str) -> None:
        """Pick ``label`` on the row's `rule-editor-item-<picker>-select`."""
        self.driver.select(f"rule-editor-item-{picker}-select", label, scope=self._row(row))

    def picked(self, row: int, picker: str) -> str:
        return self.driver.get_text(f"rule-editor-item-{picker}-select", scope=self._row(row))

    def type_input(self, row: int, field: str, text: str) -> None:
        """Type into the row's `rule-editor-item-<field>-input`."""
        self.driver.clear_and_type(f"rule-editor-item-{field}-input", text, scope=self._row(row))

    def has_control(self, row: int, element: str) -> bool:
        return self.driver.count(element, scope=self._row(row)) > 0

    def save_enabled(self) -> bool:
        return self.driver.is_enabled("rule-editor-save-button")

    def save(self) -> None:
        self.driver.click("rule-editor-save-button")

    def status(self) -> str:
        if self.driver.count("rule-editor-status") == 0:
            return ""
        return self.driver.get_text("rule-editor-status")

    def wait_status(self, expected: str, timeout: float = WAIT_S) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.status() == expected:
                return
            time.sleep(0.25)
        raise AssertionError(
            f"rule-editor-status never read {expected!r} within {timeout}s; "
            f"saw {self.status()!r}"
        )
