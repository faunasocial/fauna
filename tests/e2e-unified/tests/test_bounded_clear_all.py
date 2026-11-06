"""tier_1: a cleanup that clears a list until it is empty is bounded.

The per-test timeout fires ONCE (pytest-timeout's signal method): a test whose
body times out runs its ``finally``, and a ``while count() > 0: clear(0)`` loop
there whose clear the app never applies spins for ever and hangs the whole run
(a docker-mode whole-suite run sat 46 min in ``test_nest_retire.py``'s cleanup,
the app's account store refusing every DNS-state write). So the two clear-all
actions stop at their budget and fail naming what is left
(e2e-conventions.md convention 9: the harness is self-terminating; convention 6:
the failure says why).
"""

from __future__ import annotations

import pytest

from actions.admin import AdminActions
from actions.muted_words import MutedWordsActions

pytestmark = pytest.mark.tier_1


class _StuckDriver:
    """A list whose removals never land, or land, as `sticks` says."""

    def __init__(self, rows: int, *, sticks: bool):
        self.rows, self.sticks, self.clicks = rows, sticks, 0

    def count(self, element_id: str, **_kw) -> int:
        return self.rows

    def click(self, element_id: str, **_kw) -> None:
        self.clicks += 1
        if not self.sticks:
            self.rows -= 1

    def diagnose(self, element_id: str, **_kw) -> str:
        return "error-message: the account store is not ready yet"


def _admin(driver) -> AdminActions:
    actions = AdminActions(driver)
    return actions


def test_clearing_dns_credentials_that_never_go_fails_within_its_budget(monkeypatch):
    monkeypatch.setattr("actions.admin.time.sleep", lambda s: None)
    driver = _StuckDriver(1, sticks=True)
    with pytest.raises(AssertionError) as failure:
        _admin(driver).clear_all_dns_credentials(budget_s=0.05)
    assert "1 held DNS credential(s) still listed" in str(failure.value)
    assert "account store is not ready yet" in str(failure.value)
    assert driver.clicks >= 1


def test_clearing_dns_credentials_that_go_returns_with_none_left(monkeypatch):
    monkeypatch.setattr("actions.admin.time.sleep", lambda s: None)
    driver = _StuckDriver(3, sticks=False)
    _admin(driver).clear_all_dns_credentials(budget_s=5)
    assert driver.rows == 0 and driver.clicks == 3


def test_removing_muted_words_that_never_go_fails_within_its_budget(monkeypatch):
    actions = MutedWordsActions(_StuckDriver(2, sticks=True))
    monkeypatch.setattr(actions, "row_count", lambda: actions.driver.rows)
    with pytest.raises(AssertionError) as failure:
        actions.remove_all(budget_s=0.05)
    assert "2 muted word(s) still listed" in str(failure.value)


def test_removing_muted_words_that_go_returns_with_none_left(monkeypatch):
    actions = MutedWordsActions(_StuckDriver(2, sticks=False))
    monkeypatch.setattr(actions, "row_count", lambda: actions.driver.rows)
    actions.remove_all(budget_s=5)
    assert actions.driver.rows == 0
