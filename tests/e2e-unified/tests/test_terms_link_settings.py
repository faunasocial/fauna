"""The terms link in the Settings About row.

`docs/goal/behavior/terms-and-content-guidelines.md` § The decisions, item 4 (b)
and § Element IDs: `settings-terms-link` sits beside `settings-app-version` in
the Settings About row on every app, always present once that row renders. It
opens `fauna_core::TERMS_URL` in the browser — or, on a terminal that cannot
open one, shows the URL and copies it (the `admin-mail-health-delist-link`
precedent). Where each app paints the About row: `product-version.md`
§ Implementation status today, the About-row bullet (`SettingsActions.
navigate_to_about`).

tui led; all seven apps paint it. The by-continuing notice on the onboarding pages is
`test_terms_notice.py`.
"""

import pytest

from helpers.app_surface import app_name
from i18n.strings import S

pytestmark = [pytest.mark.tier_3, pytest.mark.feature("terms-and-content-guidelines")]

#: `fauna_core::TERMS_URL`.
TERMS_URL = "https://fauna.social/terms"

#: The apps whose link text carries the URL (a terminal shows and copies it);
#: the others open it in the browser and label the link.
_SHOWS_URL = ("tui",)


def test_settings_links_the_terms_and_content_guidelines(logged_in_app):
    app = logged_in_app
    app.settings.navigate_to_about()
    # The About block is the last on the General page: below the fold on windows.
    assert app.driver.is_visible_scrolled("settings-app-version"), app.driver.diagnose(
        "settings-app-version"
    )
    assert app.is_enabled("settings-terms-link"), (
        "the About row should carry the terms link beside the version: "
        f"{app.driver.diagnose('settings-terms-link')} error={app.error_text()!r}"
    )
    text = app.get_text("settings-terms-link")
    assert S.settings.terms_link in text, (
        f"the terms link reads {text!r}, expected the label {S.settings.terms_link!r}"
    )
    if app_name(app.driver) in _SHOWS_URL:
        assert TERMS_URL in text, (
            f"the terms link {text!r} does not show {TERMS_URL} — a terminal shows the "
            f"address it copies"
        )
