"""Notifications page — ``notification-type-icon``, cross-app.

``tests/e2e-unified/ui.yaml`` ``notification-row`` component
(``notification-item`` / ``notification-type-icon``, both ``indexed``).

Seeded via the nest's ``POST /api/v1/test/push/notify`` test-hook
(``bins/fauna-nest/src/push_test_hooks.rs``, gated on the ``test-hooks``
Cargo feature) — the same seam ``test_sp_linux_ws_rpc_push.py`` uses. This
calls the real production ``insert_notification`` path and fires a real
``PushEvent::Notification``; the shortcut is only in how the notification
is TRIGGERED (a direct HTTP call instead of a live like/reply/mention), not
in how it's stored or rendered — fixture setup arranging a precondition
(e2e rule 8b), not a stand-in for a UI action. The client-driven trigger
path (a second actor liking/replying to generate a notification) is real
follow-on work, not attempted here — this test's job is only "an existing
notification renders its type-icon", which the test-hook proves without
needing a second actor.

tier_2: a real client driver renders the Notifications page, but the
notification row itself is seeded via the nest's own test-hook rather than a
live user-to-user interaction.
"""

from __future__ import annotations

import uuid

import pytest
import requests

pytestmark = [pytest.mark.tier_2]


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.feature("notifications")
def test_notification_row_shows_type_icon(logged_in_app, nest_instance, test_user):
    """A real notification row renders ``notification-type-icon`` with
    non-empty text."""
    actor_id_hex = test_user["actor_id_hex"]
    resp = requests.post(
        f"{nest_instance['url']}/api/v1/test/push/notify",
        json={
            "actor_id": actor_id_hex,
            "summary": "type-icon coverage probe",
            "notif_type": "test",
            "content_id": uuid.uuid4().hex,
        },
        timeout=10,
    )
    assert resp.status_code == 200, (
        f"test-hooks push endpoint returned {resp.status_code}: {resp.text}"
    )
    assert resp.json().get("ok") is True, resp.json()

    app = logged_in_app
    app.notifications.navigate()
    app.driver.wait_for("notification-item", timeout=10)

    assert app.driver.count("notification-type-icon") >= 1, (
        "at least one notification row should render notification-type-icon: "
        f"{app.driver.diagnose('notification-type-icon')}"
    )
    icon_text = app.driver.get_text("notification-type-icon", index=0)
    assert icon_text, (
        "notification-type-icon should resolve non-empty text for a real "
        f"notification; got {icon_text!r}"
    )
