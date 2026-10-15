"""A deleted message's tombstone and a reaction pill are still there after an
app restart.

``docs/goal/ui/conversations.md`` § Reactions & message delete → *Durability
across a relaunch*; ``docs/goal/behavior/devices.md`` § Cross-device MLS
group-state sync (the ``history/<ch>`` contents bullet — the derived state is
carried twice).

The shared-Rust pin
(``a_tombstone_and_its_reaction_pills_survive_a_history_slice_restore``)
restores a hand-built ``ConversationsManager`` slice. This is the end-to-end
witness: delete a message and react to another through the app UI, quit and
reopen the app against its pinned store, and assert both are painted again. It
exercises the production slice writers (``persist_channel_history``'s flush and
``snapshot_replica``'s debounced autosave), the seal/upload through
``fauna.mls.put``, and the launch-time ``restore_and_wire`` — the layers between
``delete_message`` and a relaunched thread that the manager-level pin skips.

Modelled on ``test_conversations_history_survives_restart_cross_app.py`` — same
CR-1 ``preserve_state_across_relaunch()`` vacuity guard (``e2e-launch-isolation.md``,
convention 10: tui carries the signed-in store across a per-module cold
relaunch), same autosave settle gate, same two-level assertion — so a red
localizes: thread missing ⇒ the launch restore found no slice; thread present
but tombstone/pill missing ⇒ the derived state was dropped on the way into, or
out of, the slice.

Settle-gated and latency-independent (``e2e-latency-independent-assertions.md``,
convention 14): the debounced replica autosave must have flushed before the
restart, so a red is attributable to the restore path, never to racing the save;
every post-restart wait polls for the state, never a fixed delay. The reaction is
the user's OWN (the API-tier peer has no engine and cannot send), which the
manager records in the same ``reaction_log`` a peer's lands in. Mutations go
through the app UI (convention 8).
"""

from __future__ import annotations

import time

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

KEPT_LINE = "durable derived state — kept line"
DELETED_LINE = "durable derived state — deleted line"
THUMBS_UP = "👍"  # dm-reaction-option[0], the quick-set head


def _wait_count(driver, element_id, want, timeout=20.0):
    """Poll ``count(element_id) >= want``; return the final count."""
    deadline = time.time() + timeout
    n = 0
    while time.time() < deadline:
        try:
            n = driver.count(element_id)
        except Exception:
            n = 0
        if n >= want:
            return n
        time.sleep(0.3)
    return n


@pytest.mark.tui
@pytest.mark.feature("reactions-and-message-delete")
def test_tombstone_and_reaction_pill_survive_restart(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    conv = app.conversations
    d = app.driver

    # Vacuity guard (CR-1 precedent): without the store pin the relaunched
    # process is a fresh device, and "the tombstone is gone" could not be told
    # apart from the cross-device path.
    if not d.preserve_state_across_relaunch():
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

    conv.enable_real_faunamls()

    peer = conv_api.reachable_peer(port, admin_sk, test_user["actor_id_hex"])
    assert conv_api.keypackage_count(port, peer, peer["actor_id_hex"]) == 3

    # ── A 1:1 with two OWN messages: the first will carry a reaction, the
    # second will be deleted. ──
    pre_existing = {
        t.channel_id_hex for t in conv.list_threads() if t.channel_id_hex
    }
    conv.real_resolve_send_new(peer["actor_id_hex"], KEPT_LINE)
    thread = fauna_thread(app, exclude=pre_existing)
    channel_hex = thread.channel_id_hex
    assert channel_hex, "the 1:1 thread should bind a channel after the first send"
    conv.real_send(thread.thread_id, DELETED_LINE)

    conv.navigate()
    conv.open_thread_by_channel(channel_hex)
    assert wait_rendered_messages(app, 2) == 2, (
        "pre-restart the thread should render both own messages — the send "
        "failed, so the restart proves nothing"
    )

    # ── Mutate through the UI: react on the first bubble, delete the second. ──
    conv.react_to_message(message_index=0, reaction_index=0)
    conv.wait_for_reaction_pill(expected_count=1)
    conv.delete_message(message_index=1)
    conv.wait_for_message_deleted()

    # Pre-restart sanity: the state we are about to prove survives is really
    # painted now, so a post-restart zero cannot be "the mutation never took".
    assert d.count("dm-message-deleted") == 1
    assert d.count("dm-reaction-pill") == 1
    assert THUMBS_UP in conv.reaction_pill_name(0)

    # ── Gate on the debounced autosave having SETTLED (settled, not merely
    # present — see wait_replica_settled). ──
    wait_replica_settled(_own_client, f"history/{channel_hex}")
    wait_replica_settled(_own_client, "provider")

    # ── RESTART: force-quit + relaunch against the pinned store. In-memory
    # manager state dies here; the pinned store and the nest replica return. ──
    d.hard_reload()
    conv.enable_real_faunamls()

    restored = wait_fauna_thread(app, channel_hex)
    assert restored is not None, (
        f"after the restart no FaunaMls thread on channel {channel_hex} came back "
        "at all — the launch restore (restore_and_wire) found no history slice"
    )

    conv.navigate()
    conv.open_thread_by_channel(channel_hex)

    # Both bubbles are back (the kept body + the tombstone placeholder), and
    # the deleted body did NOT come back as text.
    tombstones = _wait_count(d, "dm-message-deleted", 1)
    assert tombstones == 1, (
        f"after the restart the thread painted {tombstones} tombstone(s), expected 1: "
        f"the deletion was dropped between delete_message and the history/{channel_hex} "
        "slice (snapshot_channel_slice → persist/autosave → restore_and_wire)"
    )
    assert wait_rendered_messages(app, 1) >= 1
    texts = [d.get_text("dm-message-text", index=i) for i in range(d.count("dm-message-text"))]
    assert any(KEPT_LINE in t for t in texts), f"kept line missing from {texts}"
    assert not any(DELETED_LINE in t for t in texts), (
        f"the deleted body came back as text after the restart: {texts}"
    )

    pills = _wait_count(d, "dm-reaction-pill", 1)
    assert pills == 1, (
        f"after the restart the thread painted {pills} reaction pill(s), expected 1: "
        f"the reaction was dropped between toggle_reaction and the history/{channel_hex} slice"
    )
    assert THUMBS_UP in conv.reaction_pill_name(0), (
        f"restored pill is {conv.reaction_pill_name(0)!r}, expected a {THUMBS_UP} pill"
    )
