"""Unread is decided by when your nest received a message, never by the
sender's clock: a backdated message still shows unread, and nothing reads unread
by mistake after a restart.

``docs/features/conversations.md`` § Coverage contract, outcome 25. Authority:
``docs/goal/behavior/conversation-read-state.md`` § The read-marker record (the
value is ``through``, a channel ``seq`` the nest assigns inside the per-channel
lock) and § How the carriers meet the in-memory set — "There is no clock anywhere
in the unread rule … a sender-backdated stamp can no longer hide it". The
shared-Rust pins are in ``fauna-conversations``' ``unread_tracking_tests.rs``;
this is the same promise through a real app, a real nest and a real restart.

The sender is a one-shot ``mls-group-gen`` engine whose two messages carry a
sender timestamp one day in the past (``conv_api.BACKDATED``) — older than the
app's launch, older than the read the test makes. A clock-based rule would call
each of them history. The sequence:

1. Message one (backdated) arrives while the app runs: it is **unread** — its
   nest ``seq`` lies above the channel's position, whatever its stamp says.
2. The app opens the thread; the read raises the marker (durability barrier:
   ``read_marker_state`` reports it from the store).
3. The app quits with its store pinned; message two (backdated too) is posted
   while it is closed.
4. Relaunched, the thread reads **exactly one** unread: message two, above the
   marker; message one, read before the restart, does not come back unread.
"""

from __future__ import annotations

import pytest

from common import create_actor_and_register
from helpers.budgets import MLS_HANDSHAKE_S
from helpers.waiting import await_account_runtime_assembled, wait_until
from tests.api import conv_api
from tests.test_conversations_read_state_across_restart import _read_marker, _thread

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]

FIRST_BODY = "nest-position witness: backdated, arrives while the app runs"
SECOND_BODY = "nest-position witness: backdated, arrives while the app is closed"


def _describe(app, channel_hex: str) -> str:
    t = _thread(app, channel_hex)
    if t is None:
        return "the thread is not listed"
    return (
        f"messages={t.message_count} unread={t.unread_count}; "
        f"marker={_read_marker(app, channel_hex)!r}"
    )


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.feature("conversations")
def test_unread_follows_the_nests_order_never_the_senders_clock(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    if not app.driver.preserve_state_across_relaunch():
        pytest.skip(
            "driver cannot preserve the client's long-term store across a relaunch, "
            "so 'after a restart' would be a fresh device"
        )
    port = nest_instance["port"]
    me = test_user["actor_id_hex"]
    app.conversations.enable_real_faunamls()
    await_account_runtime_assembled(app.driver)

    sender = create_actor_and_register(
        port, admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    wait_until(
        lambda: conv_api.keypackage_count(port, test_user, me) > 0,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for the app to publish a key package",
    )
    kp = conv_api.keypackage_fetch(port, sender, me)
    assert kp is not None, "the app's user should have a fetchable key package"
    channel_hex, welcome, (first, second) = conv_api.mint_group_welcome_with_messages(
        bytes(sender["signing_key"]),
        kp,
        [conv_api.BACKDATED + FIRST_BODY, conv_api.BACKDATED + SECOND_BODY],
    )
    conv_api.accept_contact(port, test_user, sender["actor_id_hex"])
    inbox_id = conv_api.welcome_deliver(port, sender, me, channel_hex, welcome)
    conv_api.await_welcome_applied(port, test_user, inbox_id, MLS_HANDSHAKE_S)

    # ── 1. A backdated message arriving live is unread. ──
    conv_api.channel_send(port, sender, channel_hex, first)
    wait_until(
        lambda: (_thread(app, channel_hex) is not None)
        and _thread(app, channel_hex).message_count >= 1,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for the app to decrypt message one",
    )
    assert _read_marker(app, channel_hex) is not None, (
        "this app does not answer the `read_marker_state` reader; every "
        "runtime-hosting app registers it over the shared "
        "fauna_account_plane::account_driver::e2e_readers::read_marker_state"
    )
    wait_until(
        lambda: _thread(app, channel_hex).unread_count == 1,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "a message stamped a day ago by its sender, received just now, "
        f"should read unread: {_describe(app, channel_hex)} — 0 means the sender's "
        "clock decided it",
    )

    # ── 2. Read it; the marker is durable. ──
    app.conversations.navigate()
    app.conversations.open_thread_by_channel(channel_hex)
    wait_until(
        lambda: _thread(app, channel_hex).unread_count == 0
        and (_read_marker(app, channel_hex) or {}).get("found") is True,
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"opening the thread should read message one and raise the marker: {_describe(app, channel_hex)}",
    )
    read_through = _read_marker(app, channel_hex)["through"]

    # ── 3. Quit; another backdated message arrives while closed. ──
    if app.driver.is_web():
        app.driver.kill_uncleanly()
    else:
        app.driver.quit_app()
    conv_api.channel_send(port, sender, channel_hex, second)

    # ── 4. Relaunched: exactly the new one is unread. ──
    app.driver.hard_reload()
    app.conversations.enable_real_faunamls()
    await_account_runtime_assembled(app.driver)
    wait_until(
        lambda: (_thread(app, channel_hex) is not None)
        and _thread(app, channel_hex).message_count >= 2
        and _thread(app, channel_hex).unread_count == 1,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "after the restart exactly message two (above the marker, "
        f"through={read_through}) should read unread: {_describe(app, channel_hex)} — "
        "0 means the backdated stamp hid it, 2 that the read message came back unread",
    )
