"""A message of a kind this app does not know is missed on its own, and the
conversation keeps working — the app-level witness of the record-level skip.

``docs/goal/architecture/version-compatibility.md`` § MLS application-message
payloads (client↔client): ``ChannelMessageBody`` is additive-only and an
unknown variant fails only that record's decode, so an older member misses one
message's meaning, never the channel; and a blanket "a message could not be
shown" line is NOT the remedy — the message is simply missed. The shared-Rust
pin of the skip is
``fauna_mls_backend_tests.rs::an_unknown_top_level_body_skips_only_its_record_in_the_inbound_walk``;
this is the same promise through a real app, a real nest and a real second
member's MLS engine.

The sequence:

1. An API-tier sender (a one-shot ``mls-group-gen`` engine) seats the app's
   user in a 1:1. Its stream is three messages of one sender: a LATER build's
   message kind (``conv_api.FUTURE_BODY`` — a body variant this build has no
   arm for), then two ordinary text messages.
2. The sender posts the unknown kind and the first text message. The app's
   thread shows the text message and only it — the unknown one is missed, not
   rendered as a placeholder — and no ``error-message`` is raised.
3. The sender posts the second text message: it arrives too, so the unknown
   record stalled nothing.
"""

from __future__ import annotations

import secrets

import pytest

from common import create_actor_and_register
from helpers.budgets import MLS_HANDSHAKE_S
from helpers.waiting import await_account_runtime_assembled, wait_until
from tests.api import conv_api

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]


def _thread(app, channel_hex: str):
    return next(
        (
            t
            for t in app.conversations.list_threads()
            if t.rail == "FaunaMls" and t.channel_id_hex == channel_hex
        ),
        None,
    )


def _bubbles(app) -> list[str]:
    """The open thread's message bubbles' text, in order."""
    return [t or "" for t in app.driver.get_texts("dm-message-text")]


@pytest.mark.feature("upgrades-never-lose-data")
def test_a_message_of_an_unknown_kind_is_missed_alone_and_the_conversation_keeps_working(
    real_faunamls_app, nest_instance, test_user
):
    app = real_faunamls_app
    port = nest_instance["port"]
    me = test_user["actor_id_hex"]
    await_account_runtime_assembled(app.driver)
    # The session-scoped user keeps earlier runs' threads; every body carries
    # this run's token so nothing can match a leftover.
    run = secrets.token_hex(4)
    after = f"sent after a newer kind of message {run}"
    later = f"the conversation keeps working {run}"

    # ── 1. A sender seats us in a 1:1 whose stream opens with an unknown kind. ──
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
    channel_hex, welcome, (unknown, first, second) = (
        conv_api.mint_group_welcome_with_messages(
            bytes(sender["signing_key"]), my_kp, [conv_api.FUTURE_BODY, after, later]
        )
    )
    conv_api.accept_contact(port, test_user, sender["actor_id_hex"])
    conv_api.welcome_deliver(port, sender, me, channel_hex, welcome)
    wait_until(
        lambda: _thread(app, channel_hex) is not None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for the app to join the sender's Welcome",
    )

    # ── 2. The unknown kind, then an ordinary message: only the latter shows. ──
    conv_api.channel_send(port, sender, channel_hex, unknown)
    conv_api.channel_send(port, sender, channel_hex, first)
    # The walk is in channel order, so once the ordinary message (the second
    # record) has landed, the unknown one (the first) has been passed over.
    wait_until(
        lambda: after in (_thread(app, channel_hex).snippet or ""),
        MLS_HANDSHAKE_S,
        diagnose=lambda: "the ordinary message after the unknown kind never arrived "
        f"(thread: {vars(_thread(app, channel_hex)) if _thread(app, channel_hex) else None!r}) "
        "— an unknown body must fail only its own record, never stall the walk",
    )
    assert _thread(app, channel_hex).message_count == 1, (
        "exactly the ordinary message should be in the thread; the unknown kind is "
        f"missed, not shown: {vars(_thread(app, channel_hex))!r}"
    )
    app.conversations.navigate()
    app.conversations.open_thread_by_channel(channel_hex)
    wait_until(
        lambda: any(after in b for b in _bubbles(app)),
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"the open thread should show the ordinary message: {_bubbles(app)!r}",
    )
    assert len(_bubbles(app)) == 1, (
        "the thread should show the one ordinary message and nothing for the unknown "
        f"kind — no placeholder line (version-compatibility.md): {_bubbles(app)!r}"
    )
    assert app.error_text() == "", (
        f"a missed message of an unknown kind raised an error: {app.error_text()!r}"
    )

    # ── 3. The conversation keeps working. ──
    conv_api.channel_send(port, sender, channel_hex, second)
    wait_until(
        lambda: any(later in b for b in _bubbles(app)),
        MLS_HANDSHAKE_S,
        diagnose=lambda: "a message after the unknown kind's should keep arriving: "
        f"{_bubbles(app)!r}",
    )
    bubbles = _bubbles(app)
    assert len(bubbles) == 2 and after in bubbles[0] and later in bubbles[1], (
        f"the thread should hold the two ordinary messages, in order: {bubbles!r}"
    )
    assert app.error_text() == "", f"unexpected error: {app.error_text()!r}"
