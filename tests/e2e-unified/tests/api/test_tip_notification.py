"""A credited tip notifies the post's author — the nest half.

`docs/goal/behavior/monetization.md` § Tips: a tip's consequence is
attribution, display and notification, never a grant. The `tip` type, its
localized body and its destination are owned by
`docs/goal/behavior/notifications.md` § The notification type.

The drive is the production one end to end: a user links a Nostr key, writes
a post, and shares their posts (which signs them onto the nest's own relay),
and designates a Lightning provider's signing key; that provider then
reports a tip — a signed kind-9735 receipt embedding the tipper's kind-9734
request — to the nest's relay inbox, as any wallet's provider would. The
author's `fauna.notifications.list` is the observable.

The app half — the row on the Notifications page — is
`tests/e2e-unified/tests/test_tip_notification.py`, the catalog witness.

tier_3: a real `fauna-nest` binary over its production doors.
"""

from __future__ import annotations

import os

import pytest

from helpers import nostr_payee, nostr_relay
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.waiting import wait_until
from helpers.zap_receipts import ZapSigner

pytestmark = pytest.mark.tier_3


def _tips(nest, user) -> list[dict]:
    with nostr_payee.ws(nest, user) as ws:
        rows = ws.call("fauna.notifications.list", {"limit": 100})["notifications"]
    return [r for r in rows if r.get("notif_type") == "tip"]


def test_a_credited_tip_notifies_the_posts_author_once(nest_instance):
    nest = nest_instance
    user, post_id, note = nostr_payee.shared_post(nest)
    signer = ZapSigner()
    with nostr_payee.ws(nest, user) as ws:
        ws.call(
            "fauna.nostr.zap_signers.add",
            {"signer_pubkey": signer.pubkey, "label": "my wallet provider"},
        )

    receipt = signer.receipt(
        payee_pubkey=note["pubkey"], target_event_id=note["id"],
        tipper_secret=os.urandom(32),
    )
    relay = nostr_relay.nest_relay_url(nest)
    accepted, message = nostr_relay.publish(relay, receipt)
    assert accepted, f"the designated provider's receipt must be believed: {message!r}"

    rows = wait_until(
        lambda: _tips(nest, user) or None,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: "the credited tip rang no `tip` notification",
    )
    assert len(rows) == 1, rows
    row = rows[0]
    assert row["content_id"] == post_id, (
        f"the row must name the tipped post (its deep link): {row!r}"
    )
    assert row["body"]["key"] == "notifications.row_tip_sats", row
    assert row["body"]["args"]["value"] == "21", row
    assert "21 sats" in row["summary"], row

    # The provider reports the same receipt again: no second notification.
    accepted, message = nostr_relay.publish(relay, receipt)
    assert accepted, message
    assert len(_tips(nest, user)) == 1, "a redelivered receipt is not a new tip"


def test_a_receipt_from_a_signer_nobody_named_notifies_nobody(nest_instance):
    nest = nest_instance
    user, _post_id, note = nostr_payee.shared_post(nest)
    stranger = ZapSigner()  # never designated

    receipt = stranger.receipt(
        payee_pubkey=note["pubkey"], target_event_id=note["id"],
        tipper_secret=os.urandom(32),
    )
    accepted, message = nostr_relay.publish(nostr_relay.nest_relay_url(nest), receipt)
    assert not accepted and message.startswith("restricted:"), message
    assert _tips(nest, user) == [], "an unbelieved receipt is not a tip"
