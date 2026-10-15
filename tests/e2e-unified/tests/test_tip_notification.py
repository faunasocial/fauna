"""When someone tips your post, you get a notification — the witness for
``docs/features/paid-posts-and-tips.md`` outcome 18.

``docs/goal/behavior/monetization.md`` § Tips: a tip's consequence is
attribution, display and notification, never a grant. The ``tip`` row's type,
body and destination are owned by ``docs/goal/behavior/notifications.md``
§ The notification type and § Deep-link destinations.

**The journey.** Through its own UI the seat writes a post, links a Nostr key,
shares its posts, and names the Lightning provider whose receipts it believes. The provider then reports a tip on that post: a signed kind-9735 receipt,
embedding a stranger's kind-9734 request, posted to the nest's relay inbox — a
real credited receipt through the production trust gate, never the feed's
inject seam. The seat opens Notifications, finds the tip row naming the amount,
and activating it opens the tipped post.

**Why the provider is headless (convention 8).** Every mutation the *user*
makes — the link, the share toggle, the signer, the post, the tap — goes
through the app UI. The receipt is the outside world arriving: a wallet
provider the user does not drive, speaking the same NIP-01 ``EVENT`` any relay
client sends.

**Convention 14.** Every wait is on element state or a relay read-back, never
elapsed time.

tier_3: a real app driver, a real nest, a real receipt over the relay wire.
tui first, the lead app; the other six follow in the trickle-down lift.
"""

from __future__ import annotations

import os
import uuid

import pytest

from helpers import nostr_relay
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.waiting import wait_until
from helpers.zap_receipts import ZapSigner

pytestmark = [pytest.mark.tier_3]


def _tip_rows(app) -> list[tuple[int, str]]:
    rows = []
    for i in range(app.notifications.notification_count()):
        text = app.driver.get_text("notification-item", index=i) or ""
        if "tipped" in text:
            rows.append((i, text))
    return rows


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.feature("paid-posts-and-tips")
def test_a_tip_on_your_post_arrives_as_a_notification(logged_in_app, nest_instance):
    app = logged_in_app
    signer = ZapSigner()

    # 1. Write the post through the composer.
    body = f"a post worth a tip {uuid.uuid4().hex[:8]}"
    app.feed.navigate()
    app.feed.create_post(body)

    # 2. Link a Nostr key, share posts, and name the provider — all in the UI.
    #    Turning sharing on signs the account's posts onto the relay there and
    #    then (a post written while it is already on waits for the next
    #    sweep), so a seat a prior test left sharing turns it off first.
    app.nostr.navigate()
    assert app.nostr.ensure_linked(), (
        f"could not link a Nostr key. error: {app.nostr.page_error_text()!r}"
    )
    app.nostr.set_toggle("nostr-expose-content", False)
    assert app.nostr.wait_for_toggle_state("nostr-expose-content", "off"), (
        f"sharing did not turn off: {app.driver.diagnose('nostr-expose-content')}"
    )
    app.nostr.set_toggle("nostr-expose-content", True)
    assert app.nostr.wait_for_toggle_state("nostr-expose-content", "on"), (
        f"sharing did not turn on: {app.driver.diagnose('nostr-expose-content')}"
    )
    assert app.nostr.wait_for_zap_signer_section(), (
        f"Zap signers section should render: "
        f"{app.driver.diagnose('nostr-zap-signer-add-btn')}"
    )
    before = app.nostr.zap_signer_count()
    app.nostr.add_zap_signer(signer.pubkey, label="my wallet provider")
    assert app.nostr.wait_for_zap_signer_count(before + 1), (
        f"designating the provider should add a roster row. "
        f"error: {app.nostr.page_error_text()!r}"
    )

    # The shared post reaches the nest's relay — where a tipper's wallet finds it.
    relay = nostr_relay.nest_relay_url(nest_instance)
    note = wait_until(
        lambda: next(
            (e for e in nostr_relay.query(relay, {"kinds": [1], "limit": 200})
             if e["content"] == body),
            None,
        ),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"the shared post {body!r} never reached the nest's relay",
    )

    # 3. The provider reports a 21-sat tip from a stranger.
    accepted, message = nostr_relay.publish(
        relay,
        signer.receipt(
            payee_pubkey=note["pubkey"], target_event_id=note["id"],
            tipper_secret=os.urandom(32),
        ),
    )
    assert accepted, f"the designated provider's receipt must be believed: {message!r}"

    # 4. The seat sees it.
    app.notifications.navigate()
    rows = wait_until(
        lambda: [r for r in _tip_rows(app) if "21 sats" in r[1]] or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"no tip notification on the page: {app.driver.diagnose('notification-item')}"
        ),
    )
    index, text = rows[0]
    assert "tipped 21 sats on your post" in text, text

    # 5. It opens the tipped post.
    app.notifications.open_notification(index=index)
    app.driver.wait_for("feed-post-detail-dialog", timeout=RPC_ROUNDTRIP_S)
    assert body in app.feed.post_detail_body(), (
        "activating the tip notification must open the tipped post: "
        f"{app.driver.diagnose('feed-post-detail-dialog')}"
    )
