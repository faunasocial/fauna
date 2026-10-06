"""Tier_3 proof that the web conversations rail's **push arm** delivers — the
one arm of that rail no test could previously exercise.

``apps/fauna-web/src/lib/conversations.ts`` hand-mirrors the arms of native's
``ConversationsSession::start_receive_loop`` (the ``tokio::select!`` is
``cfg(not(wasm32))``): a backstop ticker **plus** a push arm on
``fauna.conversations.{channel.message,welcome.received}``, both funnelled
through one serialized pump (``transport.md`` § Push events → *Web consumes the
seam on its live surfaces as of 2026-07-12*).

**Why this test did not exist until now**, in ``transport.md``'s own words: "The
conversations rail is deliberately **not** the probe: its backstop drops to 2 s
under the e2e agent, which would mask a dead push arm; proving that rail
end-to-end needs a web poll-suppression knob (the twin of native's
``FAUNA_E2E_SUPPRESS_CONV_PUSH``) and a second real MLS engine." Both now exist:

* the knob is ``WebBridgeDriver.set_conv_poll_secs`` →
  ``window.__fauna_setConvPollSecs`` → ``setConvPollSecs`` (the web twin of
  native's ``FAUNA_CONV_POLL_SECS``), which **re-arms the sleep already in
  flight**, so once it returns the next ticker sweep is provably an hour away —
  no wall-clock assumption about the cadence it replaced (convention 14);
* the second real MLS engine is ``real_faunamls_linux_sender`` — a real-engine
  **linux** GUI whose engine mints a genuine Welcome + Application envelope. An
  API-tier actor cannot produce a decryptable DM, which is why the sender has to
  be a driven GUI (``api-layers.md`` § Inbox & Messaging, layer 5).

**What makes it able to fail.** With the ticker muted and the rail quiesced, a
delivery has exactly one remaining trigger: the push arm. Delete
``subscribeReceivePushes``' conv case and this test spends its whole budget and
fails. Its twin ``test_fauna_mls_web_receives_from_linux_sender`` proves the
opposite arm (the durable drain over the ticker), so between them each arm is
covered alone rather than jointly — the shape convention 14 keeps asking for.

The counters are asserted **frozen** across the delivery window as a
self-invalidation guard: ``conv_receive_cycles`` counts ticker, poke and
reconnect sweeps but *not* single-rail push wakes, so a growth there means some
other arm ran and the attribution to the push arm would be unearned. Failing on
that is the point — it converts a would-be false GREEN into a loud red.

web-only: the native side of this property is
``test_fauna_mls_two_client_inbox_drain.py`` (which suppresses the opposite arm,
via ``FAUNA_E2E_SUPPRESS_CONV_PUSH``).
"""

from __future__ import annotations

import time

import pytest

from helpers.app_surface import declared_absence
from helpers.waiting import conv_receive_cycles, wait_until
from tests.api import conv_api

pytestmark = [pytest.mark.web, pytest.mark.tier_3]

# The muted cadence. An hour is not a wait — nothing in this test ever reaches
# it; it is the statement "the ticker is out of the picture", the same value
# `second_real_faunamls_app` mutes its native ticker to (`FAUNA_CONV_POLL_SECS=3600`).
MUTED_POLL_SECS = 3600

# Generous ceilings, paid only on failure (convention 14): a green run resolves
# each deadline poll on its first or second read.
KEYPACKAGE_BUDGET_S = 20
QUIESCE_BUDGET_S = 30
PUSH_DELIVERY_BUDGET_S = 60


@pytest.mark.feature("conversations")
def test_conv_rail_push_wakes_web(
    real_faunamls_app, real_faunamls_linux_sender, nest_instance, test_user
):
    bob_app = real_faunamls_app
    if not bob_app.driver.is_web():
        declared_absence(
            bob_app.driver,
            capability="the web conv-rail push-arm proof",
            doc="testing.md § Cross-app e2e conventions, point 7 — the natives "
            "prove the same rail's opposite arm in "
            "test_fauna_mls_two_client_inbox_drain.py (--app linux), which "
            "suppresses the push arm instead of the ticker",
        )
    alice_app, alice = real_faunamls_linux_sender
    port = nest_instance["port"]
    bob_actor_id = test_user["actor_id_hex"]  # the web GUI is the recipient
    body = "hi web via push"

    # ── Precondition: bob's WEB engine has published a key package ────────────
    # The real-backend opt-in fires `ensureKeypackages` fire-and-forget, and it
    # rides the manager, NOT the receive ticker — which is why muting the ticker
    # below cannot starve it. bob keeps the private half, so he can DECRYPT.
    wait_until(
        lambda: conv_api.keypackage_count(port, alice, bob_actor_id) >= 1,
        KEYPACKAGE_BUDGET_S,
        diagnose=lambda: (
            "bob's web client never published a key package "
            "(the real-backend `ensureKeypackages` replenish); without one "
            "alice cannot bootstrap a group at him at all"
        ),
    )
    before = conv_api.keypackage_count(port, alice, bob_actor_id)

    # bob must ACCEPT alice or the nest refuses her Welcome outright: the DM
    # plane consults the recipient's inbox mode, and bob's stored default is
    # `allow_knock` ⇒ Knock ⇒ `fauna.conversations.forbidden`
    # (`direct-messages.md` § Reach policy). Arranged on bob's side because HE is
    # the recipient here.
    conv_api.accept_contact(port, test_user, alice["actor_id_hex"])

    # ── Mute bob's backstop ticker, and quiesce the rail ──────────────────────
    # After this call the next ticker sweep is an hour away *by construction*
    # (the SPA re-arms the pending sleep). What it does NOT settle is a sweep
    # already running: that pass began before alice sent, but a long
    # `drainInbox`/`pollConversations` round-trip could still be in flight when
    # she does, and would then deliver her message by poll. So wait for the
    # counters to level off — `started == completed` is "no cycle is running",
    # a state read, not a duration.
    bob_app.driver.set_conv_poll_secs(MUTED_POLL_SECS)
    try:
        wait_until(
            lambda: _quiesced(bob_app.driver),
            QUIESCE_BUDGET_S,
            diagnose=lambda: (
                "bob's receive rail never went quiet after the ticker was muted "
                f"(cycles: {conv_receive_cycles(bob_app.driver)}). A cycle still "
                "in flight when alice sends could deliver her message by POLL, "
                "which would make this test's attribution to the push arm unearned"
            ),
        )
        muted_at = conv_receive_cycles(bob_app.driver)
        assert muted_at is not None, (
            "the web app stopped publishing 'conv_receive_cycles' — this test's "
            "whole guard rests on it (fauna_e2e_agent::CONV_RECEIVE_CYCLES_KEY)"
        )

        # ── alice (real linux engine) resolves bob by actor id and sends ──────
        # The real cross-engine bootstrap: fetch bob's key package → create the
        # group → deliver the Welcome to his durable inbox → post the Application
        # envelope. The nest pushes BOTH kinds at bob
        # (`conversations_handlers.rs` — Welcome and ChannelMessage), and with his
        # ticker muted those pushes are the only thing that can wake his rail.
        alice_app.conversations.real_resolve_send_new(bob_actor_id, body)

        # A nest-side witness that the bootstrap really fired, so a missing thread
        # below is read as "bob never woke", never as "alice never sent".
        assert conv_api.keypackage_count(port, alice, bob_actor_id) < before, \
            "the 1:1 bootstrap should consume one of bob's key packages"

        # ── bob's WEB GUI wakes on the push, drains, joins, decrypts ──────────
        got = _await_thread(bob_app, body, PUSH_DELIVERY_BUDGET_S, muted_at)
        assert got.rail == "FaunaMls"
        assert body in got.snippet

        # ── The guard: no OTHER arm ran ──────────────────────────────────────
        # Ticker, poke and reconnect sweeps all bump these counters; a push wake
        # deliberately does not (it is single-rail). Unchanged counters therefore
        # mean the delivery above can only have come from the push arm.
        after = conv_receive_cycles(bob_app.driver)
        assert after == muted_at, (
            f"a full receive cycle ran during the delivery window ({muted_at} → "
            f"{after}), so this run does NOT prove the push arm — the ticker is "
            "muted and nothing pokes, which leaves the reconnect arm "
            "(`subscribeReconnectSweep`, a socket flap). Re-run; a repeat means "
            "the socket is flapping under load and the flap, not this test, is "
            "the finding"
        )
    finally:
        # Restore the shared session driver's cadence — every later test on this
        # driver receives on the fast e2e tick again.
        bob_app.driver.set_conv_poll_secs(None)


def _quiesced(driver) -> bool:
    """True when no full receive cycle is in flight (``started == completed``)."""
    cycles = conv_receive_cycles(driver)
    return cycles is not None and cycles[0] == cycles[1]


def _await_thread(bob_app, body: str, budget_s: float, muted_at):
    """Deadline-poll bob's snapshot for the FaunaMls thread carrying ``body``."""
    deadline = time.time() + budget_s
    while True:
        got = next(
            (
                t
                for t in bob_app.conversations.list_threads()
                if t.rail == "FaunaMls" and body in t.snippet
            ),
            None,
        )
        if got is not None:
            return got
        if time.time() >= deadline:
            raise AssertionError(
                f"alice's message never reached bob's web GUI within {budget_s}s "
                "with the backstop ticker muted, so the conversations PUSH ARM "
                "did not wake the rail — `subscribeReceivePushes`' "
                "`fauna.conversations.{channel.message,welcome.received}` cases "
                "in apps/fauna-web/src/lib/conversations.ts, or the nest-side "
                "producers in conversations_handlers.rs. Receive cycles at mute "
                f"time: {muted_at}; now: {conv_receive_cycles(bob_app.driver)} "
                "(unchanged confirms no ticker sweep masked or rescued it). "
                f"bob's threads: {bob_app.conversations.list_threads()}"
            )
        time.sleep(0.5)
