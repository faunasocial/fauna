"""The terms link in the Settings About row.

`docs/goal/behavior/terms-and-content-guidelines.md` § The decisions, item 4 (b)
and § Element IDs: `settings-terms-link` sits beside `settings-app-version` in
the Settings About row on every app, always present once that row renders. It
opens `fauna_core::TERMS_URL` in the browser — or, on a terminal that cannot
open one, shows the URL and copies it (the `admin-mail-health-delist-link`
precedent). Where each app paints the About row: `product-version.md`
§ Implementation status today, the About-row bullet (`SettingsActions.
navigate_to_about`).

tui leads; the six other apps follow in the batched trickle-down and skip as
unbuilt here. The by-continuing notice on the onboarding pages is
`test_terms_notice.py`.
"""

import pytest

from helpers.app_surface import app_name, skip_unbuilt
from i18n.strings import S

pytestmark = pytest.mark.tier_3

#: `fauna_core::TERMS_URL`.
TERMS_URL = "https://fauna.social/terms"

#: The apps that paint the link today (tui first, then the trickle-down).
_PAINTED = ("tui", "web", "linux", "android", "macos", "ios")

#: The apps whose link text carries the URL (a terminal shows and copies it);
#: the others open it in the browser and label the link.
_SHOWS_URL = ("tui",)


def test_settings_links_the_terms_and_content_guidelines(logged_in_app):
    app = logged_in_app
    if app_name(app.driver) not in _PAINTED:
        skip_unbuilt(
            app.driver,
            surface="settings-terms-link",
            detail="the Settings About row's terms link is painted on tui first",
            tracked="docs/goal/behavior/terms-and-content-guidelines.md § Implementation status today",
        )
    app.settings.navigate_to_about()
    assert app.is_visible("settings-app-version"), app.driver.diagnose("settings-app-version")
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
