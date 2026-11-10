"""A reaction made on one of your devices and a deletion made on another at the
same time both survive: neither device's save loses what the other did.

``docs/features/conversations.md`` § Coverage contract, outcome 24. Authority:
``docs/goal/behavior/devices.md`` § Cross-device MLS group-state sync — the
``history/<ch>`` slice's merge (§ Implementation status today, *the derived
state is carried twice* and *a device no longer loses its OWN reactions to a
concurrent save*): ``merge_history_slices`` unions the delete tombstone and the
stamped reaction log instead of taking the higher-watermark side whole, because
an own ``Delete`` or ``Reaction`` is MLS-opaque to its author and the acting
device need not be the furthest-polled one. The shared-Rust pins are
``merge_unions_a_tombstone_the_higher_watermark_side_has_not_seen`` and
``merge_unions_two_fully_stamped_reaction_logs``; this is the same promise
through two real apps over a real nest.

**Why the two edits are concurrent without racing a clock.** Device B restores
alice's replica at launch and never re-reads the ``history/<ch>`` slice in
session, so its save base is the slice as it stood at its launch. Device A's
save lands first (its reaction); device B's save (its deletion) then goes up
against a base that predates A's — exactly the save of a device that had not
seen the other's edit, which the nest's compare-and-swap turns into a merge.
Each upload is gated on the slice changing and settling, never on a sleep.

**The witness** is device A relaunched against its pinned store: its thread is
seeded from the merged replica, so it paints the reaction it made AND the
tombstone device B made — which A could never learn otherwise (it cannot
MLS-decrypt B's delete: one actor, one leaf). A side-pick by watermark loses one
of the two.
"""

from __future__ import annotations

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common import create_actor_and_register
from helpers.budgets import MLS_HANDSHAKE_S
from helpers.waiting import (
    await_account_runtime_assembled,
    wait_rail_blob_settled,
    wait_until,
)
from tests.api import conv_api
from tests.test_conversations_read_state_across_restart import _thread

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]

OPENER = "two-device merge witness: the sender's line, device A reacts"
OWN_LINE = "two-device merge witness: alice's line, device B deletes"
THUMBS_UP = "👍"  # dm-reaction-option[0], the quick-set head
REPLICA_GET = "fauna.mls.get"


def _texts(app) -> list[str]:
    d = app.driver
    return [d.get_text("dm-message-text", index=i) for i in range(d.count("dm-message-text"))]


def _index_of(app, needle: str) -> int | None:
    for i, text in enumerate(_texts(app)):
        if needle in text:
            return i
    return None


def _open(app, channel_hex: str) -> None:
    app.conversations.navigate()
    app.conversations.open_thread_by_channel(channel_hex)


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.feature("conversations")
def test_a_reaction_and_a_deletion_made_on_two_devices_at_once_both_survive(
    logged_in_app, nest_instance, test_user, request
):
    app = logged_in_app
    if not app.driver.preserve_state_across_relaunch():
        pytest.skip(
            "driver cannot preserve the client's long-term store across a relaunch, "
            "so device A's relaunch would be a fresh device"
        )
    port = nest_instance["port"]
    me = test_user["actor_id_hex"]
    app.conversations.enable_real_faunamls()
    await_account_runtime_assembled(app.driver)

    def _alice_client():
        return WsRpcAdminClient(
            nest_instance["url"],
            actor_id=test_user["actor_id_bytes"],
            signing_key=bytes(test_user["signing_key"]),
        )

    # ── A 1:1 holding the sender's line and alice's own, on device A. ──
    sender = create_actor_and_register(
        port, admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    wait_until(
        lambda: conv_api.keypackage_count(port, test_user, me) > 0,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for device A to publish a key package",
    )
    kp = conv_api.keypackage_fetch(port, sender, me)
    assert kp is not None, "alice should have a fetchable key package"
    channel_hex, welcome, (opener,) = conv_api.mint_group_welcome_with_messages(
        bytes(sender["signing_key"]), kp, [OPENER]
    )
    conv_api.accept_contact(port, test_user, sender["actor_id_hex"])
    inbox_id = conv_api.welcome_deliver(port, sender, me, channel_hex, welcome)
    conv_api.await_welcome_applied(port, test_user, inbox_id, MLS_HANDSHAKE_S)
    conv_api.channel_send(port, sender, channel_hex, opener)
    wait_until(
        lambda: (_thread(app, channel_hex) is not None)
        and _thread(app, channel_hex).message_count >= 1,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for device A to decrypt the sender's message",
    )
    history = f"history/{channel_hex}"
    before_own = wait_rail_blob_settled(_alice_client, REPLICA_GET, history)
    app.conversations.real_send(_thread(app, channel_hex).thread_id, OWN_LINE)
    wait_until(
        lambda: _thread(app, channel_hex).message_count >= 2,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "device A's own line should echo in the thread",
    )
    # The slice must carry alice's own line before device B restores it.
    wait_rail_blob_settled(_alice_client, REPLICA_GET, "provider")
    launch_base = wait_rail_blob_settled(
        _alice_client, REPLICA_GET, history, superseding=before_own
    )

    # ── Device B restores the 1:1 with both lines. ──
    second_device = (
        "alice_second_web_device" if app.driver.is_web() else "alice_second_device"
    )
    device_b, _driver_b = request.getfixturevalue(second_device)
    wait_until(
        lambda: (_thread(device_b, channel_hex) is not None)
        and _thread(device_b, channel_hex).message_count >= 2,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "device B should restore the 1:1 with both lines; it lists "
        f"{[(t.channel_id_hex, t.message_count) for t in device_b.conversations.list_threads()]}",
    )

    # ── Device A reacts; its save lands. ──
    _open(app, channel_hex)
    wait_until(
        lambda: _index_of(app, OPENER) is not None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"device A's thread should render the sender's line: {_texts(app)}",
    )
    app.conversations.react_to_message(message_index=_index_of(app, OPENER), reaction_index=0)
    app.conversations.wait_for_reaction_pill(expected_count=1)
    after_a = wait_rail_blob_settled(
        _alice_client, REPLICA_GET, history, superseding=launch_base
    )

    # ── Device B, whose base predates A's save, deletes alice's line. ──
    _open(device_b, channel_hex)
    wait_until(
        lambda: _index_of(device_b, OWN_LINE) is not None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"device B's thread should render alice's own line: {_texts(device_b)}",
    )
    device_b.conversations.delete_message(message_index=_index_of(device_b, OWN_LINE))
    device_b.conversations.wait_for_message_deleted()
    wait_rail_blob_settled(_alice_client, REPLICA_GET, history, superseding=after_a)

    # ── The witness: device A, relaunched, paints both. ──
    app.driver.hard_reload()
    app.conversations.enable_real_faunamls()
    await_account_runtime_assembled(app.driver)
    wait_until(
        lambda: _thread(app, channel_hex) is not None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "device A's relaunch should list the 1:1 again",
    )
    _open(app, channel_hex)
    d = app.driver
    wait_until(
        lambda: d.count("dm-message-deleted") >= 1 and d.count("dm-reaction-pill") >= 1,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "after both devices saved, device A's relaunched thread paints "
        f"{d.count('dm-message-deleted')} tombstone(s) and {d.count('dm-reaction-pill')} "
        f"reaction pill(s), expected one of each (texts: {_texts(app)}) — no tombstone: "
        "device B's deletion was lost to A's save; no pill: A's reaction was lost to B's",
    )
    assert THUMBS_UP in app.conversations.reaction_pill_name(0), (
        f"the surviving pill is {app.conversations.reaction_pill_name(0)!r}, expected {THUMBS_UP}"
    )
    assert not any(OWN_LINE in t for t in _texts(app)), (
        f"the line device B deleted came back as text on device A: {_texts(app)}"
    )
