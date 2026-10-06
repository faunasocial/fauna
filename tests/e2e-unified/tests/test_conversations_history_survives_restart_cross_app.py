"""Conversations history is still there after an app restart — the cross-app
witness.

`docs/features/conversations.md` outcome 5; ``docs/goal/ui/conversations.md``
§ Persistence.

The wide-parity disposition pass found outcome 5
witnessed only on windows: both restart tests are windows-marked because only
the windows driver's ``recover()`` preserved the launch's data dir — on every
other driver a relaunch minted a fresh store, so "history survived" could only
be the cross-device replica path outcome 6's witnesses already prove. The CR-1
``preserve_state_across_relaunch()`` idiom removes that limit: the driver pins
its launch's store, ``hard_reload()`` relaunches against it, and the same actor
reconnects — a genuine quit-and-reopen, expressed once for every column (web's
pin is a no-op by construction: localStorage survives a reload; a driver that
cannot pin skips, the ``test_confirmed_identity_survives_relaunch.py`` vacuity
guard verbatim).

Each column's run asserts the same user promise through its own restart door:
quit the app right after sending, reopen it, and the thread still shows the
messages. The windows sibling ``test_conversations_history_survives_app_restart.py``
remains the mls.db data-dir localization proof; this file carries the outcome
to the other columns with the same two-level assertion, so a red localizes:
store-empty + render-empty ⇒ the launch restore never rebuilt the history;
store-full + render-empty ⇒ the app's conversations page never re-read the
restored snapshot.

Settle-gated like that sibling: the debounced replica autosave (``history/<ch>``
carries the own-message plaintext — a sender cannot MLS-decrypt its own
application messages) must have flushed before the restart, so a red is
attributable to the restore path, never to racing the save. The no-settle-gate
durability contract (durable-before-done, Rule 3) is
``test_conversations_own_message_survives_an_immediate_restart.py``'s subject,
deliberately not re-proven here.
"""

from __future__ import annotations

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.conversations_restart import (
    fauna_thread,
    wait_fauna_thread,
    wait_rendered_messages,
    wait_replica_settled,
)
from tests.api import conv_api

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
]

FIRST_LINE = "cross-app restart line one"
SECOND_LINE = "cross-app restart line two"


@pytest.mark.feature("conversations")
def test_conversations_history_survives_restart_cross_app(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app

    # Vacuity guard (CR-1 precedent): without the store pin the relaunched
    # process gets a fresh store and loses its identity, so "the history did
    # not survive" would be indistinguishable from "a different device logged
    # in fresh" — the cross-device path, not a restart.
    if not app.driver.preserve_state_across_relaunch():
        pytest.skip(
            "driver cannot preserve the client's long-term store across a "
            "relaunch, so the restart-survival assertion would be vacuous"
        )

    port = nest_instance["port"]
    node_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    def _own_client():
        # A raw User-class WS-RPC connection as this actor — a read-only
        # 'device' observing its own replica plane (`fauna.mls.get`).
        return WsRpcAdminClient(
            node_url,
            actor_id=test_user["actor_id_bytes"],
            signing_key=bytes(test_user["signing_key"]),
        )

    app.conversations.enable_real_faunamls()

    # ── An API-tier peer publishing real key packages, so the MLS bootstrap can
    # fetch one. It has no engine and never decrypts — this test only needs the
    # send to bind a channel and append our OWN messages. ──
    peer = conv_api.reachable_peer(port, admin_sk, test_user["actor_id_hex"])
    assert conv_api.keypackage_count(port, peer, peer["actor_id_hex"]) == 3

    # ── Create a 1:1 and post two OWN messages over the real MLS rail. The
    # pre-send channel snapshot excludes threads restored from EARLIER tests in
    # this session (same session-scoped actor + nest). ──
    pre_existing = {
        t.channel_id_hex
        for t in app.conversations.list_threads()
        if t.channel_id_hex
    }
    app.conversations.real_resolve_send_new(peer["actor_id_hex"], FIRST_LINE)
    thread = fauna_thread(app, exclude=pre_existing)
    channel_hex = thread.channel_id_hex
    assert channel_hex, "the 1:1 thread should bind a channel after the first send"

    app.conversations.real_send(thread.thread_id, SECOND_LINE)

    # ── Pre-restart sanity: both sends really landed and really rendered, so a
    # post-restart zero cannot be "the send never happened". ──
    app.conversations.navigate()
    app.conversations.open_thread_by_channel(channel_hex)
    before = wait_rendered_messages(app, 2)
    assert before == 2, (
        f"pre-restart the thread should render both own messages, got {before} — "
        "the send or the recipient resolve failed, so the restart proves nothing"
    )
    assert fauna_thread(app, channel_hex).message_count >= 2

    # ── Gate on the debounced autosave having SETTLED (see module docstring
    # and wait_replica_settled — settled, not merely present). ──
    wait_replica_settled(_own_client, f"history/{channel_hex}")
    wait_replica_settled(_own_client, "provider")

    # ── RESTART: force-quit + relaunch against the pinned store + replay the
    # login — each driver's own quit-and-reopen door. In-memory thread state
    # dies here; the pinned store and the nest replica come back. ──
    app.driver.hard_reload()
    app.conversations.enable_real_faunamls()

    restored = wait_fauna_thread(app, channel_hex)
    assert restored is not None, (
        f"after the restart no FaunaMls thread on channel {channel_hex} came back "
        "at all — the launch restore (restore_and_wire) found no history slice"
    )

    # (1) store level — did the restore rebuild the messages?
    assert restored.message_count >= 2, (
        f"the restored thread lists {restored.message_count} message(s), expected "
        f">= 2: the history/{channel_hex} slice restored WITHOUT its messages, "
        "so the shared ThreadStore is empty (not a render bug)"
    )

    # (2) render level — did this app's detail pane show them?
    app.conversations.navigate()
    app.conversations.open_thread_by_channel(channel_hex)
    after = wait_rendered_messages(app, 2)
    assert after == 2, (
        f"after the restart the thread rendered {after} message bubble(s), expected 2 "
        f"(the store has {restored.message_count}) — the restored snapshot never "
        "reached this app's conversations page"
    )

    texts = [app.driver.get_text("dm-message-text", index=i) for i in range(after)]
    assert any(FIRST_LINE in t for t in texts), f"own first line missing from {texts}"
    assert any(SECOND_LINE in t for t in texts), f"own second line missing from {texts}"
