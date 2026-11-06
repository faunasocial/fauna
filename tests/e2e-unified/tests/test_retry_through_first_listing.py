"""tier_1: a gesture the first-listing gate refused is repeated, and nothing else is.

Owner: ``docs/goal/architecture/account-client-lifecycle.md`` § The client-side
lifecycle → *The first listing*, clause (4): the refused gesture fails with the
reason every nest-requiring affordance carries, and can be retried. The harness
seam is ``helpers/waiting.py::retry_through_first_listing``, which the mail enable
and the backup enrollment go through. Pure state, no timing (convention 14).
"""

import os
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from helpers.waiting import FIRST_LISTING_REFUSAL, retry_through_first_listing

pytestmark = pytest.mark.tier_1


class _Page:
    """A page whose first ``refusals`` gestures are refused, or which fails with
    ``other`` when it is set."""

    def __init__(self, refusals=0, other=""):
        self.refusals = refusals
        self.other = other
        self.gestures = 0
        self.reopens = 0
        self.ok = False
        self.error = ""

    def gesture(self):
        self.gestures += 1
        if self.gestures <= self.refusals:
            self.error = f"account state load: {FIRST_LISTING_REFUSAL}"
        elif self.other:
            self.error = self.other
        else:
            self.ok = True

    def reopen(self):
        self.reopens += 1
        self.error = ""

    def run(self, settle=0.5):
        retry_through_first_listing(
            self.gesture,
            lambda: self.ok,
            lambda: self.error,
            settle=settle,
            reopen=self.reopen,
            diagnose=lambda: f"error={self.error!r}",
        )


def test_a_gesture_that_lands_is_made_once():
    page = _Page()
    page.run()
    assert (page.gestures, page.reopens) == (1, 0)


def test_a_refused_gesture_is_made_again_from_the_reopened_page():
    page = _Page(refusals=2)
    page.run()
    assert (page.gestures, page.reopens) == (3, 2)


def test_any_other_failure_is_not_retried_and_keeps_its_diagnosis():
    page = _Page(other="the nest refused this credential")
    with pytest.raises(AssertionError, match="the nest refused this credential"):
        page.run(settle=0.1)
    assert (page.gestures, page.reopens) == (1, 0)


def test_a_scope_that_never_lists_ends_at_the_budget(monkeypatch):
    from helpers import budgets

    monkeypatch.setattr(budgets, "ACCOUNT_RUNTIME_ASSEMBLY_S", 0.0)
    page = _Page(refusals=10**6)
    with pytest.raises(AssertionError, match="still refused at the budget's end"):
        page.run()
    assert page.gestures == 1
