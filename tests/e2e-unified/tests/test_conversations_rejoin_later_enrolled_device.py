"""A device enrolled after you left a conversation starts it unread when you are
brought back in.

``docs/features/conversations.md`` § Coverage contract, outcome 26 — this
module is its later-enrolled arm. The other arm (a device that kept the
conversation resumes at the old read position) is witnessed by
``test_conversations_rejoin_read_position.py``, which both pages cite. Authority:
``docs/goal/behavior/conversation-read-state.md`` § The read-marker record, the
retention bullet: when the account leaves a channel, each device that concludes
the departure retires the rows it wrote for the marker; "a device enrolled after
the leave holds no entry, and for it a re-joined channel starts unread". The
mechanism is ``docs/goal/architecture/delegable-scope-reclamation.md``
§ Delegable-scope reclamation, parts (6) and (7).

The sequence, and why each step is there:

1. A one-shot ``mls-group-gen --rejoin`` sender seats alice's device A and posts
   message one, which device A reads (the marker rises — durability barrier on
   ``read_marker_state``). The sender then removes alice; device A folds the
   Remove commit (it concludes the departure) and quits — so no device that
   kept the entry is running when alice comes back, and none can write the old
   position back.
2. Device B — alice's second device, enrolled now, after the leave — launches
   with an empty store. Precondition: it holds no read marker for the channel.
   It restores alice's MLS replica, which carries the key package the re-add
   was minted against (the ruling *any device that holds its key* may consume a
   Welcome, `devices.md` § Cross-device MLS group-state sync).
3. The sender re-adds alice and posts message two in the re-joined epoch;
   device B consumes the Welcome and decrypts it.
4. The witness: device B shows every message it holds in the conversation as
   unread — none is read, although device A had read message one before the
   leave. Opening the thread then reads them and raises a marker on B.
"""

from __future__ import annotations

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common import create_actor_and_register
from helpers import waiting
from helpers.budgets import MLS_COMMIT_FOLD_S, MLS_HANDSHAKE_S
from helpers.waiting import (
    await_account_runtime_assembled,
    wait_rail_blob_settled,
    wait_until,
)
from tests.api import conv_api
from tests.test_conversations_read_state_across_restart import _read_marker, _thread

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]

READ_BODY = "later-enrolled witness: read on device A before the removal"
AFTER_BODY = "later-enrolled witness: sent after the re-join"
REPLICA_GET = "fauna.mls.get"


def _live_state_rows(client_factory) -> int:
    """How many live rows the nest lists on the account's delegable scope
    (``state``) — among them the rows a concluded departure retires. Sealed:
    the nest knows no item's channel, so the count is the observable. One
    connection per read, closed with its ``with`` block."""
    with client_factory() as owner:
        reply = owner.call(
            "fauna.sync.changes.list",
            {"item_class": "state-entry", "scope": "state", "frontier": {}},
        )
    return len(reply["changes"])


def _describe(app, channel_hex: str) -> str:
    t = _thread(app, channel_hex)
    if t is None:
        return "the thread is not listed"
    return (
        f"messages={t.message_count} unread={t.unread_count} "
        f"participants={t.participant_count}; marker={_read_marker(app, channel_hex)!r}"
    )


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.feature("conversations")
def test_a_device_enrolled_after_you_left_starts_the_rejoined_conversation_unread(
    logged_in_app, nest_instance, test_user, request
):
    app = logged_in_app
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

    # ── 1. Joined on device A; message one read; removed; A leaves. ──
    sender = create_actor_and_register(
        port, admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    wait_until(
        lambda: conv_api.keypackage_count(port, test_user, me) >= 2,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for device A to publish two key packages",
    )
    first_kp = conv_api.keypackage_fetch(port, sender, me)
    second_kp = conv_api.keypackage_fetch(port, sender, me)
    assert first_kp and second_kp and first_kp != second_kp, (
        "alice should have two distinct fetchable key packages"
    )
    stream = conv_api.mint_group_rejoin_stream(
        bytes(sender["signing_key"]), first_kp, second_kp, [READ_BODY], [AFTER_BODY]
    )
    channel_hex = stream.channel_id_hex
    (read_msg,) = stream.before
    (after_msg,) = stream.after

    conv_api.accept_contact(port, test_user, sender["actor_id_hex"])
    inbox_id = conv_api.welcome_deliver(port, sender, me, channel_hex, stream.welcome)
    conv_api.await_welcome_applied(port, test_user, inbox_id, MLS_HANDSHAKE_S)
    conv_api.channel_send(port, sender, channel_hex, read_msg)
    wait_until(
        lambda: (_thread(app, channel_hex) is not None)
        and _thread(app, channel_hex).message_count >= 1,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for device A to decrypt message one",
    )
    assert _read_marker(app, channel_hex) is not None, (
        "this app does not answer the `read_marker_state` reader; every "
        "runtime-hosting app registers it over the shared "
        "fauna_account_plane::account_driver::e2e_readers::read_marker_state"
    )
    app.conversations.navigate()
    app.conversations.open_thread_by_channel(channel_hex)
    wait_until(
        lambda: _thread(app, channel_hex).unread_count == 0
        and (_read_marker(app, channel_hex) or {}).get("found") is True,
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"device A should read message one and raise the marker: {_describe(app, channel_hex)}",
    )
    app.conversations.navigate()
    # The machine concludes a departure only for a channel it subscribed to — a
    # full pass's derivation names it in the subscription marker. A join and a
    # leave between two passes leave nothing to conclude, so poke a pass while
    # device A is in the channel (beside a desktop's sync agent, the app's seed
    # pass writes the membership record and the agent's pass derives from it).
    # The barrier is the machine's own departure state, read off the store.
    waiting.poke_account_pump(app.driver)
    wait_until(
        lambda: (_read_marker(app, channel_hex) or {}).get("subscribed") is True,
        MLS_COMMIT_FOLD_S,
        diagnose=lambda: "the machine should subscribe to the channel at the poked "
        f"pass: {_describe(app, channel_hex)}",
    )

    folded = waiting.mls_folded_commits(app.driver, channel_hex)
    conv_api.channel_send(port, sender, channel_hex, stream.remove_commit)
    waiting.await_folded_commit_after(
        app.driver, channel_hex, folded, budget_s=MLS_COMMIT_FOLD_S,
        what="the sender's Remove commit",
    )
    # Device B must restore a provider that holds the second key package's
    # private half, and the post-removal state: let A's replica settle.
    wait_rail_blob_settled(_alice_client, REPLICA_GET, "provider")
    # The machine concludes the departure at its next account-pump pass, and a
    # folded commit nudges none (the backstop is minutes away), so poke the
    # app's pump (convention 14: a poke, not a sleep). An app that holds the
    # pump role concludes it in that pass; beside a desktop's sync agent the
    # poke runs the app's seed pass, which writes the membership record and
    # asks the agent for its pass, which concludes it. Either way the barrier
    # is the one every app shares: the machine lists the channel departed, and
    # its rows of the delegable scope leave the nest — its read marker retired.
    before = _live_state_rows(_alice_client)
    waiting.poke_account_pump(app.driver)
    wait_until(
        lambda: (_read_marker(app, channel_hex) or {}).get("departed") is True,
        MLS_COMMIT_FOLD_S,
        diagnose=lambda: "device A should conclude the departure at the poked "
        f"pass: {_describe(app, channel_hex)}",
    )
    wait_until(
        lambda: _live_state_rows(_alice_client) < before,
        MLS_COMMIT_FOLD_S,
        diagnose=lambda: "device A should conclude the departure and retire its "
        f"rows of the channel: the nest lists {_live_state_rows(_alice_client)} live rows "
        f"of the delegable scope, {before} before the poke",
    )
    app.driver.quit_app()

    # ── 2. Device B, enrolled after the leave, holds no marker. ──
    device_b, driver_b = request.getfixturevalue("alice_second_device")
    await_account_runtime_assembled(driver_b)
    marker_b = _read_marker(device_b, channel_hex)
    assert marker_b is not None and marker_b.get("found") is not True, (
        "device B was enrolled after alice left, so it should hold no read marker for "
        f"the channel; it reads {marker_b!r} — the departure did not retire device A's "
        "row, so this run cannot show a later-enrolled device starting unread"
    )

    # ── 3. Re-added; message two in the new epoch. ──
    conv_api.channel_send(port, sender, channel_hex, stream.add_commit)
    rejoin_inbox = conv_api.welcome_deliver(
        port, sender, me, channel_hex, stream.second_welcome
    )
    conv_api.await_welcome_applied(port, test_user, rejoin_inbox, MLS_HANDSHAKE_S)
    conv_api.channel_send(port, sender, channel_hex, after_msg)

    # ── 4. The witness: everything B holds there is unread. ──
    wait_until(
        lambda: (_thread(device_b, channel_hex) is not None)
        and (_thread(device_b, channel_hex).message_count or 0) >= 1
        and _thread(device_b, channel_hex).unread_count
        == _thread(device_b, channel_hex).message_count,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "device B, enrolled after the leave, should show every message "
        f"of the re-joined conversation unread: {_describe(device_b, channel_hex)} — "
        "fewer unread than messages means a read position reached it from before the leave",
    )
    device_b.conversations.navigate()
    device_b.conversations.open_thread_by_channel(channel_hex)
    wait_until(
        lambda: _thread(device_b, channel_hex).unread_count == 0
        and (_read_marker(device_b, channel_hex) or {}).get("found") is True,
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"opening the thread on device B should read it and raise a marker: {_describe(device_b, channel_hex)}",
    )
