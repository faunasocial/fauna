"""Accepting someone's contact request lets you write to them straight away.

``docs/goal/ui/contacts.md`` § Persistence → *The accept makes the edge
mutual*: a contact request is the requester's standing consent to hear from
the person they asked, so accepting it writes the requester's edge as well as
the acceptor's — and the acceptor can open the conversation at once. Before
the ruling the acceptor was a stranger to the requester, the requester's
default ``allow_knock`` inbox refused the acceptor's Welcome, and the refusal
told the acceptor to send a contact request to the person who had just sent
one (seen live 2026-10-23).

The journey, through the app: a real registered peer with key packages sends
the app's user a real message, which the user's ``allow_knock`` inbox holds as
a knock; the user accepts it on the Contacts page; the user then starts a
conversation with the peer through the recipient picker, and the thread binds
its channel with no error on the page. The peer's side is the API tier — it
has no app and never decrypts; the knock it sends is the arrangement, the
accept and the send are the user's own gestures (convention 8).
"""

from __future__ import annotations

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common import build_email_inbox_payload, create_actor_and_register
from helpers.contacts_knocks import knock_row, open_people
from helpers.conversations_restart import fauna_thread
from helpers.waiting import wait_until
from tests.api import conv_api

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
]

FIRST_LINE = "thanks for reaching out"


def _client(node_url: str, actor: dict) -> WsRpcAdminClient:
    return WsRpcAdminClient(
        node_url,
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


@pytest.mark.feature("contacts")
@pytest.mark.feature("notifications")
def test_accepting_a_contact_request_lets_you_start_the_conversation(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    port = nest_instance["port"]
    node_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]
    user_hex = test_user["actor_id_hex"]

    app.conversations.enable_real_faunamls()

    # The requester: registered here, with key packages for the user's
    # bootstrap to fetch — and deliberately NOT accepting the user first
    # (`conv_api.reachable_peer`'s third fact is exactly what this test must
    # not arrange: the accept has to provide it).
    peer = create_actor_and_register(port, admin_signing_key=admin_sk)
    peer_hex = peer["actor_id_hex"]
    conv_api.keypackage_upload(
        port, peer, conv_api.mint_key_packages(bytes(peer["signing_key"]), 3)
    )

    # The session-scoped user's inbox must hold a stranger's message as a
    # knock; an earlier test may have opened it, so pin the default and put
    # back what was there.
    with _client(node_url, test_user) as c:
        prior_mode = c.call("fauna.inbox.mode.get", {})["mode"]
        if prior_mode != "allow_knock":
            c.call("fauna.inbox.mode.set", {"mode": "allow_knock"})
    try:
        payload, _post_id = build_email_inbox_payload(
            peer["signing_key"],
            user_hex,
            "Hello",
            "May we be in touch?",
            node_url=node_url,
        )
        with _client(node_url, peer) as c:
            reply = c.call(
                "fauna.inbox.send",
                {
                    "recipient_actor_id": user_hex,
                    "recipient_nest_url": None,
                    "payload_bytes": payload,
                },
            )
        assert reply.get("inbox_id") is None, (
            f"the requester's message must be held as a knock, got {reply!r}"
        )

        # The user accepts the request on the Contacts page.
        open_people(app)
        app.contacts.accept_knock(knock_row(app, peer_hex, peer["handle"]))

        # The requester's own edge now names the user (the API-side half, so a
        # red localizes: no edge here ⇒ the nest; an edge but a failed send ⇒
        # the app).
        with _client(node_url, peer) as c:
            rows = {r["peer_id"]: r["status"] for r in c.call("fauna.contacts.list", {})["contacts"]}
        assert rows.get(user_hex) == "accepted", (
            f"accepting the request must write the requester's edge too "
            f"(contacts.md § Persistence); the requester's contacts read {rows!r}"
        )

        # The user starts the conversation through the recipient picker.
        pre_existing = {
            t.channel_id_hex for t in app.conversations.list_threads() if t.channel_id_hex
        }
        app.conversations.real_resolve_send_new(peer_hex, FIRST_LINE)
        error = app.error_text()
        assert not error, (
            f"starting a conversation with someone whose request you just "
            f"accepted must not be refused; the page reads {error!r}"
        )
        thread = fauna_thread(app, exclude=pre_existing)
        assert thread.channel_id_hex, (
            "the 1:1 with the requester should bind a channel after the first send"
        )
        welcomes = conv_api.inbox(node_url, peer)
        assert welcomes, (
            "the user's Welcome must reach the requester's inbox — the nest "
            "refused it or it never left the app"
        )

        # The request's notification row names the requester the way the
        # Contacts page does — by the handle the contact roster holds — and
        # not by the nest's 8-hex prefix (`value-formatting.md` § Peer display
        # label; `notifications.md` § Localized body → *the sender arg*).
        app.notifications.navigate()
        knock_rows: list[str] = []

        def _knock_row_named() -> bool:
            nonlocal knock_rows
            knock_rows = [
                app.driver.get_text("notification-item", index=i)
                for i in range(app.notifications.notification_count())
            ]
            return any(peer["handle"] in text for text in knock_rows)

        wait_until(
            _knock_row_named,
            30,
            diagnose=lambda: (
                f"the knock row should name the accepted contact by handle "
                f"{peer['handle']!r}; the Notifications page reads {knock_rows!r}; "
                f"error: {app.error_text()!r}"
            ),
        )
        # The fixture's handle is derived from the actor id, so the prefix is
        # looked for only outside it.
        assert not any(
            peer_hex[:8] in text.replace(peer["handle"], "") for text in knock_rows
        ), (
            f"no row may name the contact by the 8-hex prefix {peer_hex[:8]!r} "
            f"once the roster knows their handle; rows read {knock_rows!r}"
        )
    finally:
        if prior_mode != "allow_knock":
            with _client(node_url, test_user) as c:
                c.call("fauna.inbox.mode.set", {"mode": prior_mode})
