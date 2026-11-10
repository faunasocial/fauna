"""A message sent in an end-to-end-encrypted conversation while the nest cannot
be reached is queued, survives closing the app, and reaches the nest once it is
back — exactly once.

Owners:

* ``docs/goal/behavior/devices.md`` § Offline compose — the plaintext-intent
  contract: with no nest, a send on a bound MLS group becomes a plaintext intent
  in the device's outbox (never pre-built ciphertext), drained on reconnect from
  the process hosting the conversations engine, sealed at the then-current
  epoch, keyed by its intent id so the nest records it once.
* ``docs/features/offline-aware-controls.md`` outcome 6 — the MLS form of a
  change made with no nest that is kept, survives a restart and reaches the
  nest; ``test_offline_queued_like.py`` witnesses the feed's form.

The discriminator for "queued, not failed": a send into a dead connection
fails, keeps the draft and paints the send-failure slot; a queued send shows the
own message in the thread and clears the compose bar with no error. The nest's
channel log is the witness that it arrived: its count of *application*
envelopes moves by exactly one after the nest is back. The raw log length is
not that count — the restarted seat's first send in the epoch rides behind a
self-``Update`` takeover Commit (``devices.md`` § Offline compose → *Drain*,
"take epoch ownership if needed"; every launch pays it), so the log grows by
two records for the one message.

tier_3: the real nest, taken down and brought back on the same data dir; the
real MLS rail; the offline send is a UI gesture (convention 8); every wait a
deadline poll on painted or nest state (convention 14). tui first
(``testing.md`` § Default app and nest mode), then linux, which hosts the same
shared drain loop natively, then android, whose outbox and drain live in
``fauna-ffi`` (the feed manager's connection word wires the process's outbox,
which the conversations session borrows), and macos and ios, which reach the
same ``fauna-ffi`` outbox through FaunaKit's ``ConnectionWordFeed``, and web,
which runs it in ``fauna-wasm`` over the tab's outbox (its relaunch is a reload
over the same origin's store); the other apps add their marker as their
trickle-down batch reaches them.
"""
from __future__ import annotations

import time

import pytest

from common.nest import start_nest_in_place, stop_nest
from helpers.conversations_restart import fauna_thread, wait_fauna_thread
from helpers.waiting import wait_until
from i18n.strings import S
from tests.api import conv_api

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.android,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.web,
    pytest.mark.real_conversations,
]

#: The connection indicator leaving ``Connected`` once the nest is held down.
DISCONNECT_S = 90.0

#: The offline send painting its own copy (a local outbox write).
QUEUE_S = 60.0

#: The returning nest, the restarted seat's restore and its MLS drain.
CONVERGE_S = 300.0


def _stamp() -> int:
    return int(time.time() * 1000) % 1_000_000


def _hold_nest_down(app, nest_instance) -> None:
    stop_nest(nest_instance, graceful=True)
    status = {}

    def disconnected() -> bool:
        if not app.is_visible("connection-status"):
            return False
        status["text"] = app.driver.get_text("connection-status")
        return status["text"] != S.common.connected

    wait_until(
        disconnected, DISCONNECT_S,
        diagnose=lambda: (
            f"connection-status still read {status.get('text')!r} with the nest "
            "down — the seat never noticed the outage, so the send below would "
            "not be offline at all"
        ),
    )


def _rendered_texts(app) -> list[str]:
    n = app.driver.count("dm-message-text")
    return [app.driver.get_text("dm-message-text", index=i) for i in range(n)]


@pytest.mark.feature("offline-aware-controls")
def test_a_conversation_message_sent_offline_is_queued_kept_across_restart_and_delivered_once(
    logged_in_app, nest_instance, test_user
):
    app = logged_in_app
    # The restart must be THIS device restarting: without the pin a relaunch
    # gets an empty store, and the queued intent would be gone for that reason.
    assert app.driver.preserve_state_across_relaunch(), (
        "the app could not pin its client store across the relaunch, so the restart "
        "half of this outcome would be vacuous"
    )
    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]
    app.conversations.enable_real_faunamls()

    # An online 1:1 first: queuing needs a bound group (a first send bootstraps
    # one — membership, which stays online-only).
    peer = conv_api.reachable_peer(port, admin_sk, test_user["actor_id_hex"])
    pre_existing = {t.channel_id_hex for t in app.conversations.list_threads() if t.channel_id_hex}
    app.conversations.real_resolve_send_new(peer["actor_id_hex"], "online first line")
    thread = fauna_thread(app, exclude=pre_existing)
    channel_hex = thread.channel_id_hex
    assert channel_hex, "the 1:1 thread should bind a channel after the first send"
    before = conv_api.application_count(port, test_user, channel_hex)
    assert before >= 1, f"the online send never reached the channel log ({before})"

    line = f"typed with no nest {_stamp():06d}"
    app.conversations.navigate()
    app.conversations.open_thread_by_channel(channel_hex)
    _hold_nest_down(app, nest_instance)
    try:
        app.conversations.send_in_open_thread(line)
        seen: dict = {}

        def own_copy_shown() -> bool:
            seen["texts"] = _rendered_texts(app)
            return any(line in t for t in seen["texts"])

        wait_until(
            own_copy_shown, QUEUE_S,
            diagnose=lambda: (
                f"the message sent with the nest down never showed in the thread: "
                f"bubbles {seen.get('texts')!r}, error-message {app.error_text()!r} "
                "— the send went into the dead connection and failed instead of "
                "being queued as a plaintext intent"
            ),
        )
        assert not app.error_text(), (
            f"a queued send is not a failure, yet error-message reads "
            f"{app.error_text()!r}"
        )

        # Close the app and open it again, the nest still down: the message
        # lives only in this device's outbox now.
        app.driver.hard_reload()
    finally:
        start_nest_in_place(nest_instance)

    app.conversations.enable_real_faunamls()
    assert wait_fauna_thread(app, channel_hex) is not None, (
        f"after the restart the thread on {channel_hex} never came back"
    )

    # The nest is back: the channel log's application messages grow by exactly
    # the queued one (the takeover Commit ahead of it is not a message).
    counts: dict = {}

    def delivered() -> bool:
        counts["n"] = conv_api.application_count(port, test_user, channel_hex)
        return counts["n"] >= before + 1

    wait_until(
        delivered, CONVERGE_S, interval=1.0,
        diagnose=lambda: (
            f"after the nest came back the channel log holds {counts.get('n')!r} "
            f"application messages, not {before + 1} — the message queued offline and kept "
            f"across the restart never drained. error-message {app.error_text()!r}"
        ),
    )
    assert counts["n"] == before + 1, (
        f"the queued message reached the nest {counts['n'] - before} times — the "
        "drain must append it once (its intent id is the idempotency key)"
    )
    app.conversations.navigate()
    app.conversations.open_thread_by_channel(channel_hex)
    texts = _rendered_texts(app)
    assert sum(line in t for t in texts) == 1, (
        f"the delivered message should render once in its thread, got {texts!r}"
    )
