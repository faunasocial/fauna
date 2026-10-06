"""Tests for error message display across all apps.

Every app must implement an 'error-message' element that shows error text
to the user. These tests verify that error messages appear correctly for
invalid inputs, can be read by the test framework, and match the canonical
i18n strings.
"""
import pytest

pytestmark = [pytest.mark.tier1, pytest.mark.tier_3]


# The former `test_login_invalid_url_shows_error` / `test_login_unreachable_url_shows_error`
# here were empty `pytest.skip("Covered by test_onboarding_errors…")` stubs — the
# real assertions live in `test_onboarding_errors.py` (invalid/unreachable URL →
# i18n `nest_unreachable`). Removed rather than left as permanently-skipped
# duplicates. (On linux those canonical tests xfail pending a manual node-URL
# screen — tracked internally; on macos/ios they
# run and pass.)


def test_error_message_readable(logged_in_app):
    """Verify that error_text() returns a string (not crash) on any page."""
    error = logged_in_app.error_text()
    assert isinstance(error, str)
