"""E2E test: Email filter CRUD — create and delete email filters.

The web twin of `tests/test_settings.py::test_email_filter_crud`, stricter in
the three things that test leaves out: the empty state's text, the action a row
states, and which row survives a delete.
"""

import pytest

# Drives the web SPA — a web-client test, not client-independent. The `web`
# marker is authoritative in conftest's --client deselection, so this is dropped
# under a non-web run (e.g. `--client windows --include-independent`) instead of
# running a web test inside it.
pytestmark = [pytest.mark.tier_3, pytest.mark.web]

NO_FILTERS_TEXT = "No email filters configured"


def _shows_empty_state(app) -> bool:
    return NO_FILTERS_TEXT in str(app.driver.eval_js("document.body.innerText"))


@pytest.mark.feature("mail-filter-rules")
def test_email_filter_create_and_delete(logged_in_app):
    """Alice creates an email filter and then deletes it.

    Verifies:
    1. With no filters the page says "No email filters configured"
    2. Creating a filter adds it to the list with correct name and action
    3. Creating a second filter shows both
    4. Deleting the first filter removes it, second remains
    5. Deleting the last filter returns to the "No email filters" message
    """
    app = logged_in_app
    settings = app.settings
    settings.navigate()

    # The seat is shared across the session, so an earlier test's filter may
    # still be listed; the empty state is what this test starts from.
    while settings.filter_count() > 0:
        settings.delete_filter(0)
    assert _shows_empty_state(app), f"no filters listed, no empty state; error={app.error_text()!r}"

    settings.create_email_filter(
        name="Block spam sender", rule_type="SenderIs",
        rule_value="spammer@evil.test", action="Discard",
    )
    assert settings.filter_names() == ["Block spam sender"]
    assert "Discard" in settings.filter_action(0)
    assert not _shows_empty_state(app), "the empty state must go once a filter is listed"

    settings.create_email_filter(
        name="Allow trusted", rule_type="SenderDomain",
        rule_value="trusted.test", action="Allow",
    )
    assert settings.filter_names() == ["Block spam sender", "Allow trusted"]

    settings.delete_filter(0)
    assert settings.filter_names() == ["Allow trusted"]

    settings.delete_filter(0)
    assert settings.filter_count() == 0
    assert _shows_empty_state(app), f"no filters listed, no empty state; error={app.error_text()!r}"
