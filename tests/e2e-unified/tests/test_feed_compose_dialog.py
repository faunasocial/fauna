"""Feed rich compose dialog — ``feed-compose-dialog``, web/linux/windows/macos/ios.

``tests/e2e-unified/ui.yaml`` ``feed-compose-dialog`` (feed page ``elements``,
required on all 7 apps). ``compose-dialog-button`` (part of the shared
``feed-compose-bar`` component) opens it.

android excluded: no `feed-compose-dialog`/`compose-dialog-button` wiring
found in `apps/fauna-android/` — a real product gap for whichever session
next picks up android feed parity, not attempted here.

tier_3: real nest binary + real client driver, no mocks.
"""

import pytest

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.tui,
]


@pytest.mark.feature("feed-compose")
def test_compose_dialog_button_opens_feed_compose_dialog(logged_in_app):
    app = logged_in_app
    app.feed.open_compose_dialog()
    assert app.feed.compose_dialog_visible(), (
        "compose-dialog-button should open feed-compose-dialog: "
        f"{app.driver.diagnose('feed-compose-dialog')}"
    )
