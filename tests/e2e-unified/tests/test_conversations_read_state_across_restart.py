"""A native message that arrived while the app was closed is unread at the
next launch — the app-level witness of synced read state.

``docs/goal/behavior/conversation-read-state.md`` § Goal: "a message that
arrived while every app was closed is unread at the next launch, and nothing
about either depends on a device clock"; § How the carriers meet the in-memory
set (the *Native, position known* arm). The shared-Rust proofs (the fill rule,
the raise, two runtimes converging) live in ``unread_tracking_tests.rs`` and
``read_positions_convergence.rs``; this is the same promise through a real app,
a real nest and a real quit-and-reopen. tui leads (the lead-app rule).

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
from helpers.app_surface import declared_absence, skip_unbuilt
from helpers.budgets import MLS_HANDSHAKE_S
from helpers.waiting import await_account_runtime_assembled, wait_until
from tests.api import conv_api

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


@pytest.mark.feature("conversations")
def test_a_native_message_sent_while_the_app_was_closed_is_unread_at_launch(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    if app.driver.is_web():
        declared_absence(
            app.driver,
            capability="an account runtime, so no synced read marker on the native rail",
            doc="docs/goal/behavior/conversation-read-state.md § web",
        )
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
        lambda: conv_api.keypackage_count(port, sender, me) > 0,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for the app to publish a key package",
    )
    my_kp = conv_api.keypackage_fetch(port, sender, me)
    assert my_kp is not None, "the app's user should have a fetchable key package"
    channel_hex, welcome, (first, second) = conv_api.mint_group_welcome_with_messages(
        bytes(sender["signing_key"]), my_kp, [FIRST_BODY, SECOND_BODY]
    )
    conv_api.accept_contact(port, test_user, sender["actor_id_hex"])
    conv_api.welcome_deliver(port, sender, me, channel_hex, welcome)
    wait_until(
        lambda: _thread(app, channel_hex) is not None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for the app to join the sender's Welcome",
    )
    conv_api.channel_send(port, sender, channel_hex, first)
    wait_until(
        lambda: _thread(app, channel_hex).message_count >= 1,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for the app to decrypt message one",
    )

    marker = _read_marker(app, channel_hex)
    if marker is None:
        skip_unbuilt(
            app.driver,
            surface="read_marker_state reader",
            detail="the e2e durability barrier over the shared "
            "fauna_client_account_runtime::read_marker_state_json",
            tracked="",
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
    app.driver.teardown()
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
