"""A native message that arrived while the app was closed is unread at the
next launch — the app-level witness of synced read state.

``docs/goal/behavior/conversation-read-state.md`` § Goal: "a message that
arrived while every app was closed is unread at the next launch, and nothing
about either depends on a device clock"; § How the carriers meet the in-memory
set (the *Native, position known* arm). The shared-Rust proofs (the fill rule,
the raise, two runtimes converging) live in ``unread_tracking_tests.rs`` and
``read_positions_convergence.rs``; this is the same promise through a real app,
a real nest and a real quit-and-reopen, on every app that hosts an account runtime.

The sequence, and why each step is there:

1. An API-tier sender (a one-shot ``mls-group-gen`` engine) seats the app's
   user in a 1:1 and posts message one. The app opens the thread: that read
   raises the channel's read marker in the app's own account store.
2. **Durability barrier** — the ``read_marker_state`` reader reports the marker
   from the store. Quitting before the raise is durable would make a red
   ambiguous (lost write vs. wrong fill), so the test waits on the store's
   answer, never on a clock (convention 14).
3. The app quits with its store pinned (``preserve_state_across_relaunch``,
   convention 10's opt-in); the sender posts message two while it is closed.
4. The app relaunches. Message two is stamped before this launch, so the
   launch floor — the position-unknown arm — would call it history and count
   **zero**; only the synced position (its ``seq`` above the marker) makes it
   unread. So ``unread_count == 1`` is the witness, and a store that never got
   the seam registered reads 0 (the red arm).
"""

from __future__ import annotations

import json

import pytest

from common import create_actor_and_register
from helpers.budgets import MLS_HANDSHAKE_S
from helpers import waiting
from helpers.waiting import await_account_runtime_assembled, wait_until
from tests.api import conv_api
from tests.test_conversations_message_banner import _require_banner_leg

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]

FIRST_BODY = "read-state witness: read before the restart"
SECOND_BODY = "read-state witness: sent while the app was closed"


def _read_marker(app, channel_hex: str) -> dict | None:
    """This app's account-store read marker for the channel, or ``None`` when
    the app has no reader (the dispatcher stashed nothing)."""
    raw = app.driver.call_command("read_marker_state", {"channel_id_hex": channel_hex})
    if raw is None:
        return None
    return json.loads(raw) if isinstance(raw, str) else raw


def _thread(app, channel_hex: str):
    return next(
        (
            t
            for t in app.conversations.list_threads()
            if t.rail == "FaunaMls" and t.channel_id_hex == channel_hex
        ),
        None,
    )


def _arrive_while_closed_then_relaunch(app, nest_instance, test_user) -> str:
    """Steps 1–4 of the module docstring; returns the channel's hex id once the
    relaunched app reads exactly one unread message in it."""
    if not app.driver.preserve_state_across_relaunch():
        pytest.skip(
            "driver cannot preserve the client's long-term store across a relaunch, "
            "so 'unread at the next launch' would be vacuous"
        )

    port = nest_instance["port"]
    me = test_user["actor_id_hex"]
    app.conversations.enable_real_faunamls()
    await_account_runtime_assembled(app.driver)

    # ── 1. A sender seats us in a 1:1; message one arrives and is read. ──
    sender = create_actor_and_register(
        port, admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    wait_until(
        lambda: conv_api.keypackage_count(port, test_user, me) > 0,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for the app to publish a key package",
    )
    my_kp = conv_api.keypackage_fetch(port, sender, me)
    assert my_kp is not None, "the app's user should have a fetchable key package"
    channel_hex, welcome, (first, second) = conv_api.mint_group_welcome_with_messages(
        bytes(sender["signing_key"]), my_kp, [FIRST_BODY, SECOND_BODY]
    )
    conv_api.accept_contact(port, test_user, sender["actor_id_hex"])
    inbox_id = conv_api.welcome_deliver(port, sender, me, channel_hex, welcome)
    # The acked Welcome is the join witness: a Welcome-only 1:1 is listed on
    # no rail until its first message, so the thread appears only below.
    conv_api.await_welcome_applied(port, test_user, inbox_id, MLS_HANDSHAKE_S)
    conv_api.channel_send(port, sender, channel_hex, first)
    wait_until(
        lambda: (_thread(app, channel_hex) is not None)
        and _thread(app, channel_hex).message_count >= 1,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for the app to decrypt message one",
    )

    marker = _read_marker(app, channel_hex)
    # Every app that hosts an account runtime answers the barrier (tui, linux,
    # web, windows, macOS, iOS, android), so an absent reader is a defect in the
    # build under test — not an unbuilt surface to skip past (convention 7).
    assert marker is not None, (
        "this app does not answer the `read_marker_state` reader; every "
        "runtime-hosting app registers it over the shared "
        "fauna_account_plane::account_driver::e2e_readers::read_marker_state"
    )

    app.conversations.navigate()
    app.conversations.open_thread_by_channel(channel_hex)
    wait_until(
        lambda: _thread(app, channel_hex).unread_count == 0,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "opening the thread should read message one",
    )

    # ── 2. Durability barrier: the read reached the account store. ──
    wait_until(
        lambda: (_read_marker(app, channel_hex) or {}).get("found") is True,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "the read never raised the channel's marker in the account "
        f"store: {_read_marker(app, channel_hex)!r}",
    )
    read_through = _read_marker(app, channel_hex)["through"]

    # ── 3. Quit; message two arrives while the app is closed. ──
    if app.driver.is_web():
        # Web's quit keeps the profile: `teardown()` would end the whole session
        # (bridge included), leaving `hard_reload()` nothing to reopen.
        app.driver.kill_uncleanly()
    else:
        # The user quits and the machine stays: `teardown()` discards it (and on
        # windows kills the bridge and the pinned store), leaving `hard_reload()`
        # nothing to reopen.
        app.driver.quit_app()
    conv_api.channel_send(port, sender, channel_hex, second)

    # ── 4. Relaunch against the pinned store. ──
    app.driver.hard_reload()
    app.conversations.enable_real_faunamls()
    await_account_runtime_assembled(app.driver)
    wait_until(
        lambda: (_thread(app, channel_hex) is not None)
        and _thread(app, channel_hex).unread_count == 1,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "after the relaunch the channel should hold exactly one unread "
        f"message (the one above the marker, through={read_through}); the thread "
        f"reads {getattr(_thread(app, channel_hex), 'unread_count', None)!r} — 0 means "
        "the launch floor decided it (the read-position seam never delivered)",
    )
    return channel_hex


@pytest.mark.feature("conversations")
def test_a_native_message_sent_while_the_app_was_closed_is_unread_at_launch(
    logged_in_app, nest_instance, test_user
):
    _arrive_while_closed_then_relaunch(logged_in_app, nest_instance, test_user)


@pytest.mark.feature("conversations")
def test_a_message_that_arrived_while_the_app_was_closed_raises_no_banner(
    logged_in_app, nest_instance, test_user
):
    """``docs/goal/behavior/message-banners.md`` § Where logic lives: a message
    that arrived while the app was closed is history to the banner tracker,
    whichever snapshot delivers it — the unread mark carries it, a banner does
    not. Same arc as the test above; then a banner diff tick that began after
    the relaunched app showed the unread message has finished, and none fired
    for that thread (the negative is anchored on that tick, never a clock)."""
    app = logged_in_app
    _require_banner_leg(app.driver)
    waiting.require_message_banners(app.driver)
    channel_hex = _arrive_while_closed_then_relaunch(app, nest_instance, test_user)
    thread_id = _thread(app, channel_hex).thread_id

    # The barrier: a tick that BEGAN after the unread message was on screen. A
    # quiet app runs no tick, so make one — opening the thread reads it, a
    # snapshot change — and wait for it to finish; every snapshot that carried
    # the arrival was diffed before it. (An open thread raises nothing new, but
    # a banner the arrival raised earlier is already in the log.)
    planted = waiting.message_banners(app.driver)["started"]
    app.conversations.navigate()
    app.conversations.open_thread_by_channel(channel_hex)
    waiting.await_banner_pass_after(
        app.driver, planted, what="opening the thread the relaunch delivered into"
    )
    fired = waiting.fired_banner_threads(app.driver)
    assert thread_id not in fired, (
        f"thread {thread_id} raised a banner for a message that arrived while the "
        f"app was closed (fired this run: {fired}); it should be carried by the "
        "unread mark alone"
    )
