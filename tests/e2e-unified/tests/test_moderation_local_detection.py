"""Tier_3 two-driven-client FaunaMls **local-detection** proof on ``--client linux``.

The encrypted-mode social-content moderation signal, end-to-end through two real
GUI apps over a real nest: **alice** sends a spammy MLS conversation message;
**bob**'s client decrypts it post-delivery and its post-decrypt classify hook
(``ConversationsManager::ingest_inbound_to_thread`` →
``fauna_client_moderation::LocalDetectionStore``) retains a spam local detection;
bob's moderation queue then surfaces it as a **client-side** row (a
``content-label-badge`` + a **blank** action column + a ``train-correction-button``)
— the union half the nest cannot produce in encrypted mode
(``docs/goal/behavior/moderation.md`` § Layout & flow; ``content-scoring.md`` § the
two plaintext positions → the user's client, post-decrypt).

Why two real GUI engines (not the API-tier peers of ``test_fauna_mls_real_roundtrip``):
only a member holding the private key **decrypts**, and only a decrypted body is
classified. The in-process two-real-engine decrypt→classify is proven at tier_1/3-rust
in ``libs/fauna-conversations/tests/fauna_mls_backend_tests.rs``
(``inbound_driver_retains_post_decrypt_spam_local_detection``); THIS test proves it
end-to-end over the real wire *and* that the moderation GUI renders the local half of
the queue. A fresh nest configures no obligation rules (``test_moderation.py``'s
empty-queue contract), so the server ``fauna.moderation.actions`` queue is empty and
**any** queue row here is necessarily bob's own post-decrypt local detection.

Two legs, one contract:

* **linux** (``test_local_spam_detection_surfaces_in_moderation_queue``) — two real GUI
  apps: alice's linux GUI sends, bob's linux GUI receives. Uses the
  client-matched second-real-client fixture (``second_real_faunamls_app``).
* **web** (``test_web_local_spam_detection_surfaces_in_moderation_queue``) — bob's web
  SPA is the only real GUI engine; the *sender* is a throwaway engine driven via the
  ``mls-group-gen`` helper (the pattern ``test_fauna_mls_web_receive.py`` established,
  extended to also seal an **application message** — a join alone proves nothing here,
  since only a *decrypted* body is classified). This is the proof of web's slice-5
  ``LocalDetectionStore`` install on ``WasmConversationsManager``: before it, the shared
  receive-loop writer silently no-opped and web's queue rendered an always-empty local
  half in encrypted mode.

The native FFI clients' readers are the cross-machine slice-4 legs.
"""

from __future__ import annotations

import pytest

from common import create_actor_and_register
from helpers.budgets import MLS_HANDSHAKE_S, RECEIVE_CYCLE_S, UI_SETTLE_S
from helpers.waiting import (
    await_receive_cycle_after,
    conv_receive_cycles,
    poke_receive_cycle,
    wait_until,
)
from tests.api import conv_api

# tier_3 only (matching the sibling two-driven-client tests). Each leg guards its own
# app: the two-real-engine leg skips any `app` param that is neither linux nor tui
# (those are the apps that both build a standalone moderation queue and are supported
# by the `second_real_faunamls_app` fixture — tui joined 2026-07-29), and the web leg
# skips any non-web one.
pytestmark = pytest.mark.tier_3

# A body the shared ``fauna_core::text_heuristic::classify_text`` flags as spam well
# above the 0.3 (per-mille 300) gate — several spam phrases ("buy now", "click here",
# "free money", "act now", "limited time").
SPAM_BODY = "BUY NOW!!! CLICK HERE for FREE MONEY!!! Limited time — act now!!!"


# App scope, declared where COLLECTION can see it. The in-body
# `require_second_real_engine_fixture_supported()` below stays as the runtime
# backstop and remains the single place the supported set is described — but a
# guard in the body runs AFTER every fixture, and `real_faunamls_app` is now a
# real precondition that raises on a launch-gate app whose invocation never set
# `FAUNA_E2E_REAL_CONVERSATIONS` (helpers/real_rail_control.py). Marking the app
# scope deselects this at collection instead, which is the shape
# `pytest_collection_modifyitems` already prefers ("without it they are kept and
# then `pytest.skip` in-body under the wrong client, inflating the skip count").
# ⚠ Widening the second-real-engine fixture means widening BOTH — the guard's
# docstring says so.
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.feature("moderation-queue")
def test_local_spam_detection_surfaces_in_moderation_queue(
    real_faunamls_app, second_real_faunamls_app, nest_instance
):
    alice = real_faunamls_app
    alice.moderation.require_second_real_engine_fixture_supported(
        flow="the moderation-queue UI"
    )
    bob_app, bob = second_real_faunamls_app
    port = nest_instance["port"]

    # ── Precondition: bob's GUI engine has published a key package (login-time
    # `ensure_keypackages`) so alice's bootstrap can fetch one; bob keeps the private
    # half, which is what lets him DECRYPT (and thus classify) below.
    wait_until(
        lambda: conv_api.keypackage_count(port, bob, bob["actor_id_hex"]) >= 1,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "bob's linux/tui engine never published a key package",
    )

    # ── alice resolves bob by actor id and sends a spammy message — the real
    # cross-engine MLS group bootstrap (fetch key package → create group → Welcome →
    # Application envelope).
    alice.conversations.real_resolve_send_new(bob["actor_id_hex"], SPAM_BODY)

    # ── bob receives + DECRYPTS. ⚠ **The anchor is a receive CYCLE, not a tick**
    # (e2e-conventions.md convention 14 mechanism 3) — bob is a
    # `second_real_faunamls_app` (push suppressed by construction), so a bare
    # deadline poll here would race his backstop ticker, which is deliberately
    # MUTED for every consumer of this fixture (the D9 fixture-level mute); the
    # poke is his ONLY delivery trigger. Same shape as
    # `test_fauna_mls_two_client_inbox_drain.py`.
    cycles = conv_receive_cycles(bob_app.driver)
    started = cycles[0] if cycles else None
    poke_receive_cycle(bob_app.driver)
    await_receive_cycle_after(
        bob_app.driver, started, budget_s=RECEIVE_CYCLE_S,
        what="alice's spammy message reaching bob's durable inbox",
    )

    def _decrypted_thread():
        return next(
            (
                t
                for t in bob_app.conversations.list_threads()
                if t.rail == "FaunaMls"
                and ("FREE MONEY" in t.snippet or "BUY NOW" in t.snippet)
            ),
            None,
        )

    wait_until(
        _decrypted_thread,
        RECEIVE_CYCLE_S,
        diagnose=lambda: (
            "bob should receive + decrypt alice's spammy message over the real wire; "
            f"threads: {[(t.rail, t.snippet) for t in bob_app.conversations.list_threads()]}"
        ),
    )

    # ── bob's moderation queue surfaces the post-decrypt local detection. Navigating
    # re-fetches + merges the (empty) server queue with bob's local detections; the
    # classify ran during decrypt (proven above), so the row is present. Re-navigate
    # to repaint in case the decrypt landed a beat after the first fetch — client-local
    # UI settle, not a nest round-trip.
    wait_until(
        lambda: (bob_app.moderation.navigate(), bob_app.moderation.correction_count() >= 1)[1],
        UI_SETTLE_S,
        diagnose=lambda: "bob's moderation queue never repainted the local-detection row",
    )

    assert bob_app.moderation.correction_count() >= 1, (
        "bob's post-decrypt spam local detection should surface as a moderation-queue "
        "row with a train-correction-button (the server queue is empty on a fresh nest, "
        "so this row is necessarily the client-side local detection)"
    )
    # The row renders a content-label badge; on a spam-only local signal it is the
    # spam category via the shared `content_label_style` map (not a per-app string).
    assert bob_app.driver.count("content-label-badge") >= 1, (
        "the local-detection row renders a content-label-badge for its category"
    )
    badge = bob_app.driver.get_text("content-label-badge")
    assert badge, "the content-label-badge resolves a (non-empty) category label"


# Web-only, declared where COLLECTION can see it — same reason as the
# two-real-engine leg above: `require_web_throwaway_sender_local_detection_leg()`
# sits in the body and cannot be reached behind `real_faunamls_app`, which now
# raises on the launch-gate apps rather than yielding a mock-backed app
# (helpers/real_rail_control.py). The guard stays the runtime backstop and the
# single description of the supported set.
@pytest.mark.web
@pytest.mark.feature("moderation-queue")
def test_web_local_spam_detection_surfaces_in_moderation_queue(
    real_faunamls_app, nest_instance, test_user
):
    """The web leg: bob's SPA decrypts a spammy MLS message and its moderation queue
    surfaces the post-decrypt local detection.

    The proof of the slice-5 store install: the shared classify hook already ran on the
    web receive path, but `observe_local_detection` early-returns while no
    `LocalDetectionStore` is installed — so before this, the queue's local half was
    *always* empty in encrypted mode, which is the only mode where it matters.
    """
    bob_app = real_faunamls_app
    bob_app.moderation.require_web_throwaway_sender_local_detection_leg()

    port = nest_instance["port"]
    bob_actor_id = test_user["actor_id_hex"]  # the web GUI is the recipient

    # alice — an API-tier actor (the authed caller for keypackage.fetch / welcome.deliver
    # / channel.send) whose 32-byte secret also seeds the throwaway sender engine.
    alice = create_actor_and_register(port, admin_signing_key=nest_instance["admin"]["signing_key"])

    # 1) bob's web app publishes key packages on the real-backend opt-in, so a sender
    #    can add him to a group — and he keeps the private half, which is what lets him
    #    DECRYPT (and therefore classify) below.
    wait_until(
        lambda: conv_api.keypackage_count(port, alice, bob_actor_id) > 0,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for bob's web client to publish a key package",
    )
    bob_kp = conv_api.keypackage_fetch(port, alice, bob_actor_id)
    assert bob_kp is not None, "bob should have a fetchable key package"

    # 2) Mint a real 1:1 group + Welcome for bob's key package, plus a sealed application
    #    message carrying the spam body. bob is a member from group creation, so the
    #    envelope decrypts for him once he has joined.
    channel_id_hex, welcome_bytes, spam_envelope = conv_api.mint_group_welcome_with_message(
        bytes(alice["signing_key"]), bob_kp, SPAM_BODY
    )

    # 3) Deliver the Welcome to bob's durable inbox and let his receive loop join the
    #    group (proven independently by `test_fauna_mls_web_receive`), so the channel is
    #    bound before the spam message is posted to it.
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
        # Generous: the web receive loop is poll-only (no push arm), so this waits on a
        # poll tick, not a slow code path — and a dev machine running parallel builds
        # stretches that cadence well past a tight 30s.
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for bob's web GUI to drain the inbox + join the Welcome's group",
    )

    # 4) Post the spam message to the channel. bob's poll fetches + DECRYPTS it — the
    #    `poll_inbound_conv` → `ingest_inbound_to_thread` path, which is exactly where
    #    the post-decrypt classify hook fires.
    conv_api.channel_send(port, alice, channel_id_hex, spam_envelope)
    wait_until(
        lambda: any(
            t.rail == "FaunaMls" and ("FREE MONEY" in t.snippet or "BUY NOW" in t.snippet)
            for t in bob_app.conversations.list_threads()
        ),
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for bob's web GUI to decrypt alice's spammy message",
    )

    # 5) bob's moderation queue (web embeds it in Settings) surfaces the local detection.
    #    A fresh nest configures no obligation rules, so the server `fauna.moderation.actions`
    #    queue is empty — ANY row here is necessarily bob's own post-decrypt local
    #    detection, merged in by the shared `merge_queue` via the `moderationQueue` wasm face.
    def _queue_row():
        bob_app.moderation.navigate()
        return bob_app.moderation.correction_count() >= 1

    wait_until(
        _queue_row,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for the post-decrypt local detection to surface as a moderation-queue row",
    )

    assert bob_app.driver.count("content-label-badge") >= 1, (
        "the local-detection row renders a content-label-badge for its category"
    )
    assert bob_app.driver.get_text("content-label-badge"), (
        "the content-label-badge resolves a (non-empty) category label"
    )

    # 6) The tier-1 spam-model client-write switch (mail-spam.md § Encrypted-mode
    #    interaction): correcting a LOCAL row reads its retained decrypted body
    #    (the new `messageBody` wasm face over `ConversationsManager::message_body`),
    #    removes the flag, and — since a local detection has no server obligation to
    #    train against — feeds the ham correction to the client-side tier-1 model
    #    (`trainSpamModelClient`) with that SAME text, mirroring linux
    #    `FaunaClient::correct_moderation_row`. Prove the click round-trips clean end
    #    to end: the row leaves the queue and no error surfaces (a wasm marshalling
    #    bug in the new local-row path would either throw — surfacing here — or leave
    #    the row stuck).
    bob_app.moderation.train_correction(0)
    wait_until(
        lambda: bob_app.moderation.correction_count() == 0,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for the corrected local-detection row to leave the queue",
    )
    assert not bob_app.has_error(), (
        f"train-correction-button on a local row should not error: {bob_app.error_text()!r}"
    )
