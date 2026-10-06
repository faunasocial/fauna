"""A nest push live-refreshes a **mounted** page — no navigation, no manual reload.

The client-agnostic, UI-level counterpart to ``test_sp_linux_ws_rpc_push.py``
(which asserts the same fan-out through linux's *state bridge*). Here the
assertion is the one a user would make: the notification shows up on the page
they are already looking at.

**Why this proves the push and not a poll.** The notifications surface has *no*
poll on any client — linux refreshes it only on ``PushEvent::Notification`` /
``ResyncRequired`` / ``Reconnected`` (``app.rs``), and web's notifications page
fetched only on mount until the push arm landed. So the page is a clean probe: a
row that appears on a *mounted* page, with the driver never navigating, can only
have arrived over the push seam. (The conversations rail is *not* usable this way
— its 30s backstop drops to 2s under the e2e agent, which would mask a dead push
arm entirely. Proving the conv rail needs a poll-suppression knob;
tracked internally.)

The push is fired by the nest's ``POST /api/v1/test/push/notify`` (the
``test-hooks`` Cargo feature, ``bins/fauna-nest/src/push_test_hooks.rs``), which
inserts a real notification row and emits one real
``PushEvent::Notification`` — the same emit the production handlers make
(``routes.rs``/``interact_routes.rs``). Nothing about the client path is mocked:
real WS-RPC socket, real typed push decode, real fetch, real render.

Guards ``transport.md`` § Push events — *"every app consumes pushes on the one
authenticated socket — web included"* — on the client that was last to get there.
"""
from __future__ import annotations

import time
import uuid

import pytest
import requests

from helpers.connection import connection_observable, wait_until_online

pytestmark = pytest.mark.tier_2


@pytest.mark.feature("notifications")
def test_push_live_refreshes_mounted_notifications_page(
    logged_in_app, nest_instance, test_user,
):
    """Nest push → the one authenticated socket → mounted page re-fetches."""
    app = logged_in_app

    # Sit on the notifications page and take the baseline *while mounted*. The
    # session-scoped actor may already carry rows from earlier tests in this
    # module ordering, so assert on the delta, never on an absolute count.
    app.notifications.navigate()
    baseline = app.notifications.notification_count()

    # Fire only once the app's transport is online: `notify_push` fans out to live
    # connections alone, so a push fired into a handshake still in flight is
    # dropped by design and reads as a dead push arm exactly when the box is
    # loaded. The connection barrier waits on the app's own published verdict
    # under a named ceiling, never a settle-sleep (convention 14).
    wait_until_online(app.driver)

    # A FRESH `content_id` per call. The nest deduplicates notifications on
    # `(actor_id, notif_type, sender_id, content_id)` and drops a duplicate — and
    # `nest_instance` / `test_user` are **session-scoped**, so this one test firing once
    # per `--client` would otherwise collide with itself: the first client inserts, the
    # second gets a no-op and the hook 500s. (That is exactly how this surfaced —
    # green per-app, red on `--client linux,web`.) A unique content_id makes each
    # notification genuinely distinct, so the test is order- and client-count-independent.
    content_id = uuid.uuid4().hex

    resp = requests.post(
        f"{nest_instance['url']}/api/v1/test/push/notify",
        json={
            "actor_id": test_user["actor_id_hex"],
            "summary": "push live-refresh probe",
            "notif_type": "test",
            "content_id": content_id,
        },
        timeout=10,
    )
    assert resp.status_code == 200, (
        f"test-hooks push endpoint returned {resp.status_code}: {resp.text}"
    )
    assert resp.json().get("ok") is True, resp.text

    # The page must grow a row on its own. No navigate(), no reload — that is the
    # whole assertion.
    #
    # 30s, not the 5s a same-machine push round-trip actually needs: dev machines run
    # many sessions' suites at once, and a tight deadline turns load into a red test.
    # It costs nothing on the happy path (the loop returns the moment the row lands)
    # and nothing in fidelity either — the failure this guards, a client with no push
    # arm wired to the page, never converges at *any* timeout.
    deadline = time.monotonic() + 30.0
    seen = baseline
    while time.monotonic() < deadline:
        seen = app.notifications.notification_count()
        if seen > baseline:
            return
        time.sleep(0.25)

    err = app.error_text() if app.has_error() else "(no error-message shown)"
    raise AssertionError(
        f"a nest push never reached the mounted notifications page: "
        f"notification-item count stuck at {seen} (baseline {baseline}) 30s after "
        f"the test-hook fired a real PushEvent::Notification. "
        f"The page never re-fetched, so the client's push arm is not wired to it. "
        f"Page error banner: {err}. Transport at failure (online before the push): "
        f"{connection_observable(app.driver)}"
    )
