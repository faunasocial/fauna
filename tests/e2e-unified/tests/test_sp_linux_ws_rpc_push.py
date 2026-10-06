"""End-to-end probe: a single nest-side push fans out through the
WS-RPC broker, the Linux push pump, and `handle_ws_event` into the
state-serialized UI.

Validates the linux WS-RPC adoption design (tracked internally), § Step 4:
the long-lived `NestClient::subscribe_pushes` broadcast → typed
`WsEvent::Push(_)` → per-variant arm → `fetch_*` UI refresh path is
plumbed end-to-end.

The nest's `POST /api/v1/test/push/notify` (gated on the `test-hooks`
Cargo feature, registered by `bins/fauna-nest/src/push_test_hooks.rs`)
inserts a notification row and fires one `PushEvent::Notification`.
On the Linux side, `app.rs::handle_ws_event` routes that into
`fauna_client.fetch_notifications()`, which dispatches
`DataMessage::NotificationsLoaded` and bumps
`AppState::notifications_unread_count` — surfaced as
`state.data.notifications.unread_count` for this assertion.
"""
from __future__ import annotations

import time
import uuid

import requests

import pytest

# `linux` marker: this probe asserts through **linux's state bridge**
# (`state.data.notifications.unread_count`), which no other app publishes — web's
# notifications are component-local runes, so under `--client web` the item was kept
# by the parametrization and then failed on a missing state field. It is a linux
# internals probe by construction (the pump → `handle_ws_event` → `fetch_*` path),
# not a cross-app behaviour test. The client-agnostic, UI-level assertion of the
# same push — "a mounted page live-refreshes" — is `test_push_live_refresh.py`, which
# carries no client marker and runs everywhere.
pytestmark = [pytest.mark.tier_2, pytest.mark.linux]


@pytest.mark.feature("notifications")
def test_linux_ws_rpc_push_pump(logged_in_app, nest_instance, test_user):
    """Nest push → broker → pump → fetch_notifications → state.unread_count."""
    driver = logged_in_app.driver

    # WS-RPC connect happens off the AuthSuccess arm in `logged_in_app`;
    # give the supervisor a moment to land the handshake before the push
    # is fired so the broker is wired to a live socket.
    deadline = time.monotonic() + 5.0
    while time.monotonic() < deadline:
        state = driver.get_state() or {}
        if state.get("session", {}).get("authenticated") and state.get("data") is not None:
            break
        time.sleep(0.1)

    actor_id_hex = test_user["actor_id_hex"]
    # Fresh `content_id`: the nest dedups notifications on
    # `(actor_id, notif_type, sender_id, content_id)` and `nest_instance`/`test_user` are
    # session-scoped, so a fixed key means only the FIRST test-hook push at this actor
    # inserts and any later one 500s ("notification not inserted"). Without this, this
    # test passes alone but fails whenever another push test ran before it in the same
    # session — which is exactly what happened once `test_push_live_refresh` was added.
    resp = requests.post(
        f"{nest_instance['url']}/api/v1/test/push/notify",
        json={
            "actor_id": actor_id_hex,
            "summary": "linux ws-rpc push probe",
            "notif_type": "test",
            "content_id": uuid.uuid4().hex,
        },
        timeout=10,
    )
    assert resp.status_code == 200, (
        f"test-hooks push endpoint returned {resp.status_code}: {resp.text}"
    )
    body = resp.json()
    assert body.get("ok") is True, body

    # Push fan-out → Linux pump → handle_ws_event → fetch_notifications →
    # NotificationsLoaded → unread_count. Allow up to 5s end-to-end.
    deadline = time.monotonic() + 5.0
    seen_unread = 0
    while time.monotonic() < deadline:
        state = driver.get_state() or {}
        notifs = state.get("data", {}).get("notifications") or {}
        seen_unread = notifs.get("unread_count", 0)
        if seen_unread >= 1:
            return
        time.sleep(0.2)

    raise AssertionError(
        f"WS-RPC push pump never bumped notifications.unread_count "
        f"(stuck at {seen_unread}) within 5s of the test-hook notify call"
    )
