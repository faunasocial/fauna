"""The personalization action layer's failure messages must survive a page that
paints no ``error-message`` element (e2e convention 6: a failure diagnoses
itself).

``get_text`` on an absent element raises ``LookupError``. A diagnostic that
reads it unguarded turns the failure it was written to report into a bare
``LookupError: not found``, which names neither the test's real assertion nor
what the page showed (observed in a ``--app linux`` batch: the engagement
toggle's own "never reached True" was replaced by exactly that).
"""

import pytest

from actions.personalization import PersonalizationActions

pytestmark = pytest.mark.tier_1


class _NoErrorElementDriver:
    """A driver whose toggle never flips and whose page has no error-message."""

    def get_attr(self, *args, **kwargs):
        return "false"

    def click(self, *args, **kwargs):
        return None

    def is_visible(self, *args, **kwargs):
        return False

    def get_text(self, *args, **kwargs):
        raise LookupError("not found")


def test_a_toggle_that_never_flips_reports_itself_not_a_missing_error_element():
    actions = PersonalizationActions(_NoErrorElementDriver())
    with pytest.raises(AssertionError, match="never reached True"):
        actions.set_engagement_toggle(0, True, timeout=0.3)
