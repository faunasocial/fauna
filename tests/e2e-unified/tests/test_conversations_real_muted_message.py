"""Tier_3 real-MLS proof of the muted-keyword collapse render (Q3) on a REAL
received message.

The real-wire twin of ``test_muted_words.py``'s tier_2
``test_muted_message_collapses_behind_reveal``, which seeds the inbound DM via
``inject_inbound_for_test`` (mock). That proves the collapse/reveal render
mechanics; it does NOT prove the render fires on a message that arrived over
the real wire and was actually **decrypted** by the client (as opposed to a
locally-injected plaintext body handed straight to the render layer) — the
same "mock vs real decrypt" gap ``test_moderation_local_detection.py`` closes
for the moderation-queue local-detection render.

``docs/goal/behavior/moderation.md`` § Muted keywords: the collapse is
computed at render, over the user's own sealed ``muted_keywords`` list,
against the **decrypted** body — a hide/collapse verb, not a spam-queue flag
(does not feed ``LocalDetectionStore`` / the moderation queue). So the
mechanism under test is: a real MLS Welcome + Application envelope, delivered
and decrypted by bob's real client, whose plaintext body matches a term bob
muted via the real Settings UI, renders ``dm-message-muted`` +
``dm-message-muted-reveal-button`` instead of ``dm-message-text`` — and reveal
un-collapses it for the session.

bob is the only real GUI engine needed (mirrors
``test_web_local_spam_detection_surfaces_in_moderation_queue``'s shape, which
is portable across every ``real_faunamls_app`` client — unlike the two-real-GUI
moderation leg, this needs no second-client fixture): alice is an API-tier
throwaway sender (``create_actor_and_register`` + a one-shot ``mls-group-gen``
engine, discarded after minting — she never processes bob's reply, and there
is none). Runs on ``--client linux``, ``--client web`` and ``--client tui`` —
every app that has BOTH a real ``FaunaMlsBackend`` (``real_faunamls_app``)
AND a muted-words Settings page. tui joined 2026-07-29, when its Settings
sub-page landed and its skip (whose stated reason was exactly that gap) was
deleted.
"""

from __future__ import annotations

import pytest

from common import create_actor_and_register
from helpers.budgets import MLS_HANDSHAKE_S
from helpers.waiting import wait_until
from tests.api import conv_api

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]

MUTED_TERM = "lottery"
# Matches MUTED_TERM case-insensitively (the shared matcher is a
# case-insensitive substring match), mirroring test_muted_words.py.
MUTED_BODY = "Congratulations — you just won the LOTTERY jackpot!"


@pytest.mark.feature("muted-words")
def test_real_muted_message_collapses_behind_reveal(real_faunamls_app, nest_instance, test_user):
    bob_app = real_faunamls_app
    port = nest_instance["port"]
    bob_actor_id = test_user["actor_id_hex"]  # bob is the real GUI, the recipient

    # Mute a term via the real Settings UI (the mutation under test, cf. the
    # mock-inject test's identical first step). test_user/nest_instance are
    # session-scoped, so this durable per-actor mutation MUST be undone
    # regardless of outcome (the same convention _isolate_spam_model follows
    # for a seeded spam model) — a later test in the same run
    # (test_muted_words.py's crud test) asserts an EMPTY list, and the receive
    # loop under test can time out well before any assertion below runs.
    bob_app.muted_words.navigate()
    bob_app.muted_words.add(MUTED_TERM)

    try:
        # Inside the try: a term that lands after this gives up must still be
        # undone below, or the next test's empty list is not empty.
        assert bob_app.muted_words.wait_for_word(MUTED_TERM), (
            f"muted term did not persist; words={bob_app.muted_words.words()!r}"
        )
        # alice — an API-tier throwaway actor whose keypair also seeds a one-shot
        # sender engine (mls-group-gen). She is the authed caller for
        # keypackage.fetch / welcome.deliver / channel.send; her MLS state is
        # discarded after minting (she never processes a reply).
        alice = create_actor_and_register(port, admin_signing_key=nest_instance["admin"]["signing_key"])

        # bob's real client publishes key packages on login (real-backend opt-in),
        # so alice can add him to a group — bob keeps the private half, which is
        # what lets him DECRYPT (and thus render-classify) below.
        wait_until(
            lambda: conv_api.keypackage_count(port, alice, bob_actor_id) > 0,
            MLS_HANDSHAKE_S,
            diagnose=lambda: "waiting for bob's client to publish a key package",
        )
        bob_kp = conv_api.keypackage_fetch(port, alice, bob_actor_id)
        assert bob_kp is not None, "bob should have a fetchable key package"

        # Mint a real 1:1 group + Welcome for bob's key package, plus a sealed
        # application message carrying the muted body. bob is a member from group
        # creation, so the envelope decrypts for him once he has joined.
        channel_id_hex, welcome_bytes, muted_envelope = conv_api.mint_group_welcome_with_message(
            bytes(alice["signing_key"]), bob_kp, MUTED_BODY
        )

        # Deliver the Welcome to bob's durable inbox and let his receive loop join
        # the group before the muted message is posted to it.
        # Mode gate (direct-messages.md § Reach policy): accept alice as a
        # contact so her DM Welcome flows under the default mode — per-pair,
        # so the session-scoped recipient's own mode stays untouched.
        conv_api.accept_contact(port, test_user, alice["actor_id_hex"])
        conv_api.welcome_deliver(port, alice, bob_actor_id, channel_id_hex, welcome_bytes)
        wait_until(
            lambda: any(
                t.rail == "FaunaMls" and t.channel_id_hex == channel_id_hex
                for t in bob_app.conversations.list_threads()
            ),
            MLS_HANDSHAKE_S,
            diagnose=lambda: "waiting for bob's client to drain the inbox + join the Welcome's group",
        )

        # Post the muted message. bob's receive loop fetches + DECRYPTS it — the
        # ingest path that also runs the muted-keyword render check.
        conv_api.channel_send(port, alice, channel_id_hex, muted_envelope)
        wait_until(
            lambda: any(
                t.rail == "FaunaMls" and t.channel_id_hex == channel_id_hex and "LOTTERY" in (t.snippet or "")
                for t in bob_app.conversations.list_threads()
            ),
            MLS_HANDSHAKE_S,
            diagnose=lambda: "waiting for bob's client to decrypt alice's muted message",
        )

        # Open the thread by its channel identity — the unambiguous pick, since
        # threads accumulate in the session-scoped nest across tests and a
        # needle-based opener could collide with another test's thread.
        bob_app.conversations.open_thread_by_channel(channel_id_hex)

        d = bob_app.driver
        assert d.count("dm-message-muted") >= 1, (
            "a real decrypted message matching a muted term should collapse behind "
            f"dm-message-muted; error={bob_app.error_text()!r}"
        )
        assert d.is_visible("dm-message-muted-reveal-button"), "collapsed DM missing its reveal button"

        d.click("dm-message-muted-reveal-button")
        assert d.count("dm-message-text") >= 1, "reveal should show the muted body"
        assert d.count("dm-message-muted") == 0, "the placeholder should be gone after reveal"
    finally:
        bob_app.muted_words.navigate()
        if bob_app.muted_words.row_count() > 0:
            bob_app.muted_words.remove(0)
