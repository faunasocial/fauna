"""Tier_3 proof that a fauna-native MLS conversation's message history survives a
real app-process restart on ``--client windows``.

The user-reported bug this pins (2026-06-19): after restarting the windows app, clicking a conversation opens the
thread but renders **ZERO message bubbles**, while the conversation **list is
populated**.

Authority: ``docs/goal/behavior/devices.md`` § Cross-device MLS group-state sync
(read its ``## Implementation status today`` first — the restore, Rule-2
save-ordering, launch-retry and debounced autosave legs are Live) +
``docs/goal/ui/conversations.md`` § Persistence. The load-bearing claim
(``devices.md`` § Cross-device MLS group-state sync): the ``history/<channel_hex>``
replica slice carries **own message plaintext** + the ingest watermark, *because a
sender cannot MLS-decrypt its own application messages* — so log replay alone can
never rebuild own history. A restart that shows a thread but none of its messages
means that slice was restored empty, or never carried the messages at all.

Why **windows-only**, and why that is not a priority-#1 divergence: windows'
driver keeps its data dir on the instance across ``recover()``, so the native
``mls.db`` engine state survives the relaunch with no opt-in — the exact
same-device restart of the user's report, localized to the windows engine.
The other columns' restart-survival promise is witnessed by the cross-app
sibling ``test_conversations_history_survives_restart_cross_app.py``, which
pins each driver's store via ``preserve_state_across_relaunch()`` (the CR-1
idiom) and asserts the same two-level outcome everywhere; before that idiom a
relaunch minted a fresh store on every non-windows driver, which is why this
file was the outcome's only witness.

The assertion is deliberately made at **two levels**, so a red run localizes the
fault instead of merely reporting it:

1. ``thread.message_count`` — the shared-Rust ``ThreadStore`` (what
   ``restore_channel_slice`` rebuilt from the replica).
2. ``driver.count("dm-message-text")`` — what the windows detail pane rendered.

Store-empty + render-empty  ⇒ the ``history/<ch>`` slice restored without messages.
Store-full  + render-empty  ⇒ the windows ConversationsPage/VM never re-read the
restored snapshot. The user's report ("list populated, zero bubbles") is
consistent with either until this test runs.
"""

from __future__ import annotations

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.app_surface import skip_unbuilt
from helpers.conversations_restart import (
    fauna_thread,
    wait_fauna_thread,
    wait_rendered_messages,
    wait_replica_settled,
)
from tests.api import conv_api

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.windows,
    pytest.mark.real_conversations,
]

FIRST_LINE = "restart-proof line one"
SECOND_LINE = "restart-proof line two"


# The poll/settle helpers (fauna_thread, wait_replica_settled, wait_fauna_thread,
# wait_rendered_messages) live in helpers/conversations_restart.py, shared with
# the immediate-restart sibling and the cross-app restart witness.


@pytest.mark.feature("conversations")
def test_conversations_history_survives_app_restart(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    if not app.driver.is_windows():
        skip_unbuilt(
            app.driver,
            surface="a same-device-restart mls.db data-dir persistence proof",
            detail="only the windows recover() preserves the native mls.db "
            "data dir with no opt-in (a stable per-driver-instance data dir "
            "kept across relaunch); the other columns' restart-survival is "
            "witnessed by test_conversations_history_survives_restart_"
            "cross_app.py via the preserve_state_across_relaunch() pin"
        )

    port = nest_instance["port"]
    node_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    def _own_client():
        # A raw User-class WS-RPC connection as this actor — a read-only 'device'
        # observing its own replica plane (`fauna.mls.get`).
        return WsRpcAdminClient(
            node_url,
            actor_id=test_user["actor_id_bytes"],
            signing_key=bytes(test_user["signing_key"]),
        )

    # The real wire-backed FaunaMls manager is registered at login under
    # FAUNA_E2E_REAL_CONVERSATIONS (set session-wide by the real_conversations
    # marker); this polls `data.conv_real_backend_active` for that readiness.
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

    # ── Pre-restart sanity: both sends really landed and really rendered. This is
    # what makes a post-restart zero unambiguous — it cannot be "the send never
    # happened" or "the recipient never resolved". ──
    app.conversations.navigate()
    app.conversations.open_thread_by_channel(channel_hex)
    before = wait_rendered_messages(app, 2)
    assert before == 2, (
        f"pre-restart the thread should render both own messages, got {before} — "
        "the send or the recipient resolve failed, so the restart proves nothing"
    )
    assert fauna_thread(app, channel_hex).message_count >= 2

    # ── Gate on the debounced autosave having SETTLED. history/<ch> is what
    # carries our own message plaintext (a sender cannot MLS-decrypt its own
    # application messages), so if it has not flushed, a restart could never show
    # these bubbles and the test would be red for a harness reason rather than
    # for the bug. Settled — not merely present: see wait_replica_settled. ──
    wait_replica_settled(_own_client, f"history/{channel_hex}")
    wait_replica_settled(_own_client, "provider")

    # ── RESTART the app process (force-quit + relaunch + replay the login), the
    # windows-native equivalent of the user quitting and reopening FaunaApp.
    # the launch's data dir (and its mls.db) survives; the in-RAM ThreadStore
    # does not. ──
    app.driver.hard_reload()
    app.conversations.enable_real_faunamls()

    # ── The bug: the list is populated but the thread renders no bubbles. ──
    restored = wait_fauna_thread(app, channel_hex)
    assert restored is not None, (
        f"after the restart no FaunaMls thread on channel {channel_hex} came back "
        "at all — the launch restore (restore_and_wire) found no history slice; "
        "that is a DIFFERENT failure from the reported one (list populated)"
    )

    # (1) store level — did restore_channel_slice rebuild the messages?
    assert restored.message_count >= 2, (
        f"the restored thread lists {restored.message_count} message(s), expected "
        f">= 2: the history/{channel_hex} replica slice restored WITHOUT its "
        "messages, so the shared ThreadStore is empty (not a windows render bug)"
    )

    # (2) render level — did the windows detail pane show them?
    app.conversations.navigate()
    app.conversations.open_thread_by_channel(channel_hex)
    after = wait_rendered_messages(app, 2)
    assert after == 2, (
        f"after the restart the thread rendered {after} message bubble(s), expected 2 "
        f"(the store has {restored.message_count}) — the restored snapshot never "
        "reached the windows ConversationsPage"
    )

    texts = [app.driver.get_text("dm-message-text", index=i) for i in range(after)]
    assert any(FIRST_LINE in t for t in texts), f"own first line missing from {texts}"
    assert any(SECOND_LINE in t for t in texts), f"own second line missing from {texts}"
