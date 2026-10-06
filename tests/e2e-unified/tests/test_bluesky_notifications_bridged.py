"""A bridged Bluesky notification reaches the unified notification list.

`docs/goal/behavior/bridges.md` § Bluesky bridge → *Notifications*: Bluesky
likes, replies, reposts, quotes, follows and mentions are bridged into the
unified notification stream, and the apps read them through the unified
``fauna.notifications.*`` surface "alongside native fauna notifications". The
apps need nothing Bluesky-specific — which is exactly the claim this test
exists to hold up: a row whose ``source`` is ``bluesky`` renders in the same
list, through the same elements, as a native one.

**What this witnesses, and what it does not.** The nest side of the bridge —
the ingest, its per-notification dedup token, and the D7 hosted-backing gate on
which accounts are polled at all — is witnessed by the worker's own Rust tests
(``bins/fauna-nest/src/bluesky/notif_sync.rs``,
``bins/fauna-nest/src/bluesky/notif_worker.rs``), which run the real code. What
no test can drive end-to-end today is the one network step: the consume-side
poller talks to a real ATProto PDS under a real OAuth session, and the harness
has no fake for that (``helpers/atproto_fakes.py`` fakes the PLC directory and
DNS for the *hosted* PDS bridge, not the consume-side AppView). So the row here
is seeded through the nest's own ``POST /api/v1/test/push/notify`` test-hook
with ``source="bluesky"`` — the same seam ``test_notifications_type_icon.py``
uses, and the same reasoning: the shortcut is in how the notification is
TRIGGERED, never in how it is stored or rendered. That is fixture setup
arranging a precondition (e2e rule 8b), not an API call standing in for a user
action — the user's part in a bridged notification is, by design, nothing at
all.

The wire assertion runs first so a failure diagnoses itself (convention 6): if
``fauna.notifications.list`` already lacks the row, the defect is nest-side and
no amount of element-hunting will say so.

tier_2: a real client driver renders the page, but the bridged row is seeded
via the nest's test-hook rather than a live Bluesky interaction.
"""

from __future__ import annotations

import uuid

import pytest
import requests

from clients.ws_rpc_admin_client import WsRpcAdminClient

pytestmark = [pytest.mark.tier_2]


@pytest.mark.web
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.android
@pytest.mark.windows
# apple (2026-09-21): both shells register `notification-item` with
# `text: { notif.body }` (`Fauna-iOS/.../NotificationsView.swift:26`,
# `Fauna-macOS/.../MacNotificationsView.swift:28`) and `SocialInboxFFIMapping`
# maps `body: ffi.summary` — so the render leg's summary match reads the same
# string every other app's driver returns. Nothing here is Bluesky-specific,
# which is the claim under test; the seeding seam is the same
# `POST /api/v1/test/push/notify` the apple legs of
# `test_notifications_type_icon.py` already drive.
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.feature("atproto")
def test_a_bridged_bluesky_notification_reaches_the_unified_list(
    logged_in_app, nest_instance, test_user
):
    """A ``source="bluesky"`` row is carried by the unified wire and rendered
    by the unified list."""
    actor_id_hex = test_user["actor_id_hex"]
    summary = "alice.bsky.social liked your post"

    resp = requests.post(
        f"{nest_instance['url']}/api/v1/test/push/notify",
        json={
            "actor_id": actor_id_hex,
            "summary": summary,
            "notif_type": "like",
            "source": "bluesky",
            # The real bridge's dedup token is the notification's own AT-URI
            # (`notif_sync::ingest_notifications`); the hook takes hex, and all
            # that matters here is that the session-scoped actor's rows stay
            # distinct across runs.
            "content_id": uuid.uuid4().hex,
        },
        timeout=10,
    )
    assert resp.status_code == 200, (
        f"test-hooks push endpoint returned {resp.status_code}: {resp.text}"
    )
    assert resp.json().get("ok") is True, resp.json()
    notification_id = resp.json()["notification_id"]

    # Wire leg: the unified list carries the row AND its origin protocol.
    # `NotifItem.source` is the field `notifications.md` declares as
    # `fauna`/`bluesky`/`nostr`/`activitypub`; nothing else distinguishes a
    # bridged notification from a native one.
    with WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes.fromhex(actor_id_hex),
        signing_key=bytes(test_user["signing_key"]),
    ) as client:
        reply = client.call("fauna.notifications.list", {"limit": 100})
    rows = {row["id"]: row for row in reply["notifications"]}
    assert notification_id in rows, (
        f"fauna.notifications.list did not return the bridged row "
        f"{notification_id}; got ids {sorted(rows)}"
    )
    bridged = rows[notification_id]
    assert bridged["source"] == "bluesky", (
        "a bridged notification must carry its origin protocol on the wire "
        f"(notifications.md, NotifItem.source); got {bridged['source']!r}"
    )
    assert bridged["summary"] == summary, (
        f"the bridged summary must survive the wire; got {bridged['summary']!r}"
    )

    # Render leg: the same unified list paints it, with no Bluesky-specific
    # element. The list does not paint `source` on any app and the goal doc
    # promises none — so the assertion is that the bridged row IS a row.
    app = logged_in_app
    app.notifications.navigate()
    app.driver.wait_for("notification-item", timeout=10)

    texts = [
        app.driver.get_text("notification-item", index=i)
        for i in range(app.driver.count("notification-item"))
    ]
    assert any(summary in (text or "") for text in texts), (
        "the bridged Bluesky notification should render in the unified list "
        f"alongside native ones; looked for {summary!r} in {texts!r}: "
        f"{app.driver.diagnose('notification-item')}"
    )
    assert not app.has_error(), (
        f"notifications page surfaced an error after load: {app.error_text()!r}"
    )
