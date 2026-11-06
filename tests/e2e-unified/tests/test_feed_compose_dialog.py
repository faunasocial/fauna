"""Feed rich compose dialog — ``feed-compose-dialog``, all 7 apps.

``tests/e2e-unified/ui.yaml`` ``feed-compose-dialog`` (feed page ``elements``,
required on all 7 apps). ``compose-dialog-button`` (part of the shared
``feed-compose-bar`` component) opens it.

android: the feed's TopAppBar ``compose-dialog-button`` opens the full compose
screen, whose Scaffold carries ``feed-compose-dialog``. Not yet run on a device
(android e2e venue).

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
    pytest.mark.android,
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
