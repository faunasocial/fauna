"""A conversation you join reaches your other device while it runs, and one you
were just added to survives an immediate quit.

``docs/features/conversations.md`` § Coverage contract, outcome 22. Authority:
``docs/goal/behavior/devices.md`` § Cross-device MLS group-state sync — *A
sibling-joined group is adopted mid-session by a targeted import* (the walk's
first step probes the ``provider`` tip and imports a group the engine lacks),
*Who may consume a Welcome — any device that holds its key*, and the
immediate-quit ruling (the join is durable before the app says it applied the
Welcome). The shared-Rust proofs are ``fauna_conversations::session``'s
``adopt_sibling_groups_first`` tests and ``orchestration``'s
``own_message_survives_a_hard_quit_immediately_after_send``; these are the
same promises through real apps over a real nest.

Two tests, one per clause:

* **While it runs.** Device B is launched and running BEFORE alice is added.
  An API-tier sender (a one-shot ``mls-group-gen`` engine) seats alice in a
  1:1 and posts one message. The witness is device B listing the conversation
  with no relaunch. Whichever device consumes the Welcome — the ruling lets any
  device holding the key — the other one must come to show it while running.
* **An immediate quit.** One device. The sender seats alice; the moment the app
  acknowledges the Welcome it is quit, with no settle gate (the point is the
  quit racing every debounce). The sender's message is posted while it is
  closed; the relaunched app decrypts it — which it can only do if the group it
  had just joined survived the quit. No second device runs, so nothing else can
  hand the group back through the replica.
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

LIVE_BODY = "join witness: the room your other device should show"
QUIT_BODY = "join witness: sent while the just-joined app was closed"


def _threads(app) -> list:
    return [(t.rail, t.channel_id_hex) for t in app.conversations.list_threads()]


def _seat(port, nest_instance, test_user, body):
    """A fresh sender and a 1:1 minted against one of alice's key packages
    (fetched now). Returns ``(sender, channel_hex, welcome, envelope)``."""
    me = test_user["actor_id_hex"]
    sender = create_actor_and_register(
        port, admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    wait_until(
        lambda: conv_api.keypackage_count(port, test_user, me) > 0,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for alice's device to publish a key package",
    )
    kp = conv_api.keypackage_fetch(port, sender, me)
    assert kp is not None, "alice should have a fetchable key package"
    channel_hex, welcome, (envelope,) = conv_api.mint_group_welcome_with_messages(
        bytes(sender["signing_key"]), kp, [body]
    )
    conv_api.accept_contact(port, test_user, sender["actor_id_hex"])
    return sender, channel_hex, welcome, envelope


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.feature("conversations")
def test_a_conversation_joined_on_one_device_appears_on_the_other_while_it_runs(
    logged_in_app, nest_instance, test_user, request
):
    app = logged_in_app
    port = nest_instance["port"]
    me = test_user["actor_id_hex"]
    app.conversations.enable_real_faunamls()
    await_account_runtime_assembled(app.driver)

    # Device B restores from alice's replica at launch; let device A's provider
    # (its key packages included) reach the nest first, so B starts as a full
    # device of the account rather than one still catching up.
    def _alice_client():
        return WsRpcAdminClient(
            nest_instance["url"],
            actor_id=test_user["actor_id_bytes"],
            signing_key=bytes(test_user["signing_key"]),
        )

    wait_until(
        lambda: conv_api.keypackage_count(port, test_user, me) > 0,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for device A to publish a key package",
    )
    wait_rail_blob_settled(_alice_client, "fauna.mls.get", "provider")
    sender, channel_hex, welcome, envelope = _seat(port, nest_instance, test_user, LIVE_BODY)

    # Device B is running before alice joins: the conversation does not exist
    # yet, so it cannot be something B restored at launch.
    device_b, _driver_b = request.getfixturevalue("alice_second_device")
    assert _thread(device_b, channel_hex) is None, (
        f"device B lists channel {channel_hex} before alice was added to it: {_threads(device_b)}"
    )

    inbox_id = conv_api.welcome_deliver(port, sender, me, channel_hex, welcome)
    conv_api.await_welcome_applied(port, test_user, inbox_id, MLS_HANDSHAKE_S)
    conv_api.channel_send(port, sender, channel_hex, envelope)
    wait_until(
        lambda: (_thread(app, channel_hex) is not None)
        and _thread(app, channel_hex).message_count >= 1,
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"device A should show the new conversation: {_threads(app)}",
    )
    wait_until(
        lambda: _thread(device_b, channel_hex) is not None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "device B was running when alice joined the conversation and "
        f"never came to list it (no relaunch); its threads: {_threads(device_b)} — the "
        "sibling-group adoption (a `provider` tip change → targeted import) did not "
        "reach a running device",
    )


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.feature("conversations")
def test_a_conversation_you_were_just_added_to_survives_an_immediate_quit(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    if not app.driver.preserve_state_across_relaunch():
        pytest.skip(
            "driver cannot preserve the client's long-term store across a relaunch, "
            "so the relaunched app would be a fresh device"
        )
    port = nest_instance["port"]
    me = test_user["actor_id_hex"]
    app.conversations.enable_real_faunamls()
    await_account_runtime_assembled(app.driver)

    sender, channel_hex, welcome, envelope = _seat(port, nest_instance, test_user, QUIT_BODY)
    inbox_id = conv_api.welcome_deliver(port, sender, me, channel_hex, welcome)
    conv_api.await_welcome_applied(port, test_user, inbox_id, MLS_HANDSHAKE_S)

    # Quit at once: no replica settle, no wait on any debounce. (Web's quit
    # keeps the profile: `teardown()` would end the whole session, leaving
    # `hard_reload()` nothing to reopen — as the read-state witness does.)
    if app.driver.is_web():
        app.driver.kill_uncleanly()
    else:
        app.driver.quit_app()
    conv_api.channel_send(port, sender, channel_hex, envelope)

    app.driver.hard_reload()
    app.conversations.enable_real_faunamls()
    await_account_runtime_assembled(app.driver)
    wait_until(
        lambda: (_thread(app, channel_hex) is not None)
        and _thread(app, channel_hex).message_count >= 1,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "the app was quit the moment it acknowledged joining the "
        f"conversation; relaunched, it never decrypted the message posted meanwhile "
        f"on {channel_hex} — the join did not survive the quit. Its threads: {_threads(app)}",
    )
