"""A device whose nest cannot be reached when it starts still gets your
conversations once the nest is back — with no relaunch.

``docs/features/conversations.md`` § Coverage contract, outcome 23. Authority:
``docs/goal/behavior/devices.md`` § Cross-device MLS group-state sync, the
*launch resilience* entry of § Implementation status today: a transient
launch-time ``sync.load()`` failure re-attempts the whole restore with
exponential backoff (``fauna_client_mls_sync::orchestration::
restore_and_wire_with_retry``, 1 s doubling to a 30 s cap, indefinitely) — only
a transport fault retries; a nest-answered rejection fails fast. The shared-Rust
proof is the retry loop's tier_1 tests in ``orchestration.rs``; this is the same
promise through a real app over a real nest.

The sequence:

1. An API-tier sender seats alice's device A in a 1:1 and posts one message;
   device A's replica reaches the nest. Device A then quits (not on web, whose
   second device is a twin page of A's browser) so every replica read the nest
   sees from here on is device B's.
2. The nest is made unreachable FOR THE REPLICA READ: every ``fauna.mls.get``
   runs and then has its connection closed instead of a reply
   (``helpers.rpc_hold.drop_rpc_reply`` — the caller meets a transport fault,
   which is what an unreachable nest is to it; a refusal would be an answer).
3. Device B — alice's second device, an empty store that can only learn the
   conversation from the replica — launches. Two dropped reads are the proof it
   met the outage at launch and tried again; while they are dropped it lists
   no conversation on the channel (the precondition that keeps the witness from
   being vacuous).
4. The outage ends (the drop is released). The witness: device B lists the
   conversation, the same process, no relaunch.
"""

from __future__ import annotations

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common import create_actor_and_register
from helpers.budgets import MLS_HANDSHAKE_S
from helpers.rpc_hold import drop_rpc_reply, release_rpc_hold, wait_for_dropped_rpc
from helpers.waiting import (
    await_account_runtime_assembled,
    wait_rail_blob_settled,
    wait_until,
)
from tests.api import conv_api
from tests.test_conversations_read_state_across_restart import _thread

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]

OPENER = "launch-resilience witness: from the sender"
REPLICA_GET = "fauna.mls.get"

#: The restore's backoff caps at 30 s; two attempts and the release fit well
#: inside this, which only a genuine failure spends.
RETRY_BUDGET_S = 90.0


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.feature("conversations")
def test_a_device_whose_nest_was_unreachable_at_launch_gets_the_conversations_once_it_is_back(
    logged_in_app, nest_instance, test_user, request
):
    app = logged_in_app
    port = nest_instance["port"]
    me = test_user["actor_id_hex"]
    app.conversations.enable_real_faunamls()
    await_account_runtime_assembled(app.driver)

    # ── 1. Device A holds a 1:1; its replica reaches the nest. ──
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

    def _alice_client():
        return WsRpcAdminClient(
            nest_instance["url"],
            actor_id=test_user["actor_id_bytes"],
            signing_key=bytes(test_user["signing_key"]),
        )

    wait_rail_blob_settled(_alice_client, REPLICA_GET, "provider")
    wait_rail_blob_settled(_alice_client, REPLICA_GET, f"history/{channel_hex}")
    if not app.driver.is_web():
        app.driver.quit_app()

    # ── 2–3. The replica read is unreachable; device B launches into it. ──
    second_device = (
        "alice_second_web_device" if app.driver.is_web() else "alice_second_device"
    )
    drop_rpc_reply(port, REPLICA_GET)
    try:
        device_b, _driver_b = request.getfixturevalue(second_device)
        # Web's device A is still running and probes the replica too, so its
        # reads count here as well; on every other app both drops are B's.
        wait_for_dropped_rpc(port, REPLICA_GET, count=2, timeout=RETRY_BUDGET_S)
        assert _thread(device_b, channel_hex) is None, (
            "device B lists the conversation while every replica read is cut off — "
            "it learned it some other way, so this run proves nothing about the retry: "
            f"{[(t.rail, t.channel_id_hex) for t in device_b.conversations.list_threads()]}"
        )
    finally:
        release_rpc_hold(port, REPLICA_GET)

    # ── 4. The nest is back: the same process restores the conversation. ──
    wait_until(
        lambda: _thread(device_b, channel_hex) is not None,
        RETRY_BUDGET_S,
        diagnose=lambda: "the nest is reachable again, but device B — which started "
        "while it was not — never restored the conversation without a relaunch; its "
        f"threads: {[(t.rail, t.channel_id_hex) for t in device_b.conversations.list_threads()]} "
        "— the launch restore gave up (a single-device fallback) instead of retrying",
    )
