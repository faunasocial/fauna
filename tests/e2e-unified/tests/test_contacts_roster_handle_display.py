"""A contact the roster knows by handle is named by it — never by a short id.

``value-formatting.md`` § Implementation status today → *Peer display label —
the roster handle* (2026-10-29; the roster re-read at each Profile open since
2026-11-04): a surface that holds no handle for a person the contact roster
knows by one names them by it, in shared Rust — ``ContactsCache::peer_label``
for the OTHER Profile header, and the conversations snapshot's
``participant_displays`` for a handle-less member chip. The knock-row arm is
witnessed in ``test_contacts_accept_opens_conversation.py``; this module
witnesses the Profile header and the member chip (``contacts`` outcome 13).

The journey: a real registered peer with key packages, who accepts the user
(``conv_api.reachable_peer``), and whom the user accepts back through the API
(arrangement — the edge is nest-side account state; the gestures under test
are the opens). The user opens the peer's Profile from their contact row: the
header reads the peer's handle. The user then starts a 1:1 with the peer
through the recipient picker by RAW actor id — so the thread holds no handle of
its own for them — and the open thread's member chip for the peer reads the
roster's address, not a short id. The peer has no app and never decrypts.

Assertions are on rendered state under deadline polls (convention 14). The
fixture's handle is derived from the actor id, so the hex prefix is looked for
only outside the handle.
"""

from __future__ import annotations

import pytest

from actions.api_actor import ApiActor
from helpers.conversations_restart import fauna_thread
from helpers.other_profile import open_other_profile
from helpers.waiting import wait_until
from tests.api import conv_api

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
]

FIRST_LINE = "hello from the roster handle witness"


def _named_by_handle(text: str, handle: str, peer_hex: str) -> bool:
    """``text`` carries ``handle`` and no 8-hex prefix of the peer's id outside
    it (the short id is the id's first 12 hex characters and a ``…``)."""
    return handle in text and peer_hex[:8] not in text.replace(handle, "")


@pytest.mark.feature("contacts")
def test_a_roster_contact_is_named_by_handle_on_their_profile_header_and_member_chip(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]
    user_hex = test_user["actor_id_hex"]

    app.conversations.enable_real_faunamls()

    peer = conv_api.reachable_peer(port, admin_sk, user_hex)
    peer_hex, handle = peer["actor_id_hex"], peer["handle"]
    viewer = ApiActor(
        nest_instance["url"], test_user["token"], user_hex, bytes(test_user["signing_key"])
    )
    viewer.accept_knock(peer_hex)

    # The OTHER Profile header: no published profile, no handle of its own —
    # the roster's handle names the peer.
    open_other_profile(app, peer_hex)
    header = ""

    def _header_named() -> bool:
        nonlocal header
        header = app.profile.handle_text() or ""
        return _named_by_handle(header, handle, peer_hex)

    wait_until(
        _header_named,
        30,
        diagnose=lambda: (
            f"the OTHER Profile header should name the roster contact by handle "
            f"{handle!r}, never a short id of {peer_hex[:12]!r}; it reads {header!r}; "
            f"error: {app.error_text()!r}"
        ),
    )

    # A 1:1 opened by raw actor id holds no handle for the peer: the member
    # chip takes the roster's address.
    pre_existing = {
        t.channel_id_hex for t in app.conversations.list_threads() if t.channel_id_hex
    }
    app.conversations.real_resolve_send_new(peer_hex, FIRST_LINE)
    error = app.error_text()
    assert not error, f"the 1:1 with an accepted contact must start; the page reads {error!r}"
    thread = fauna_thread(app, exclude=pre_existing)
    assert thread.channel_id_hex, "the 1:1 should bind a channel after the first send"
    app.conversations.open_thread_by_channel(thread.channel_id_hex)

    chips: list[str] = []

    def _chip_named() -> bool:
        nonlocal chips
        chips = [
            app.driver.get_text("thread-member-chip", index=i) or ""
            for i in range(app.conversations.member_chip_count())
        ]
        return any(_named_by_handle(c, handle, peer_hex) for c in chips)

    wait_until(
        _chip_named,
        30,
        diagnose=lambda: (
            f"a member chip should name the roster contact by handle {handle!r}; "
            f"chips read {chips!r}; error: {app.error_text()!r}"
        ),
    )
    short_id = peer_hex[:12] + "…"
    assert not any(short_id in c for c in chips), (
        f"no member chip may name the contact by the short id {short_id!r} once the "
        f"roster knows their handle; chips read {chips!r}"
    )
