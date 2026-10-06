"""Tier_3 proof that the NATIVE conversations rail's **push arm** delivers
alone — the leg ``test_conv_rail_push_wakes_web``'s own docstring names as
untested.

``libs/fauna-conversations``' ``ConversationsSession::start_receive_loop`` is a
``tokio::select!`` over a backstop ticker **and** a push arm on
``fauna.conversations.{channel.message,welcome.received}``
(``transport.md`` § Push events). Two existing tests each isolate one side of
this rail, but not the push side natively:

* ``test_fauna_mls_two_client_inbox_drain`` suppresses the push arm
  (``FAUNA_E2E_SUPPRESS_CONV_PUSH=1``) and proves the drain backstop alone,
  via an explicit ``conv_receive_now`` poke.
* ``test_conv_rail_push_wakes_web`` proves the push arm alone, but only on
  web — its own docstring says so explicitly.

So natively, ``test_fauna_mls_two_client_inbox_drain`` suppresses push (drain
proof) and its own bob fixture mutes the ticker AND suppresses push — nothing
before this test exercised "a push notification alone wakes the native rail"
on its own. This test closes that gap with
``second_real_faunamls_app_push_only``: bob's ticker is muted the same way,
but his push arm stays live and nothing pokes his receive loop — delivery has
exactly one remaining trigger.

**The counters are asserted frozen** across the delivery window, the same
self-invalidation guard ``test_conv_rail_push_wakes_web`` uses:
``conv_receive_cycles`` counts ticker, poke and reconnect sweeps but *not*
single-rail push wakes, so growth there means some other arm ran and the
attribution to the push arm would be unearned.

Runs on the direct-Rust clients (linux + tui) — web is out of scope here; its
own leg of this same property is ``test_conv_rail_push_wakes_web``.
"""

from __future__ import annotations

import time

import pytest

from helpers.budgets import MLS_HANDSHAKE_S, RECEIVE_CYCLE_S
from helpers.waiting import conv_receive_cycles
from tests.api import conv_api

pytestmark = pytest.mark.tier_3


# App scope, declared where COLLECTION can see it — the module docstring has
# always said "the direct-Rust clients (linux + tui)", but nothing enforced it,
# so this test also collected on macos/ios/windows/android and reached
# `real_faunamls_app` WITHOUT the `real_conversations` marker. On apple that
# silently yielded a MOCK-backed app (measured 2026-09-22: the app's own log
# said `real-conversations gate CLOSED`), and on windows the fixture's readiness
# poll had been burning its 60 s budget and erroring for the same reason. That
# measurement is this change's own red-first proof;
# the marker is the cure, and `helpers/real_rail_control.py` is what made the
# defect legible instead of silent.
@pytest.mark.linux
@pytest.mark.tui
@pytest.mark.feature("conversations")
def test_conv_rail_push_wakes_native(
    real_faunamls_app, second_real_faunamls_app_push_only, nest_instance
):
    alice = real_faunamls_app
    bob_app, bob = second_real_faunamls_app_push_only
    port = nest_instance["port"]
    body = "hi native via push"

    # ── Precondition: bob's engine has published its key packages ────────────
    # Same quiescence gate as the drain test's — the login-time replenish tops
    # up one package at a time, so wait for the count to SETTLE rather than
    # merely become non-empty (e2e-conventions.md § convention 14).
    _QUIET_S = 3.0
    deadline = time.time() + MLS_HANDSHAKE_S
    last, stable_since, before = None, None, None
    while time.time() < deadline:
        count = conv_api.keypackage_count(port, bob, bob["actor_id_hex"])
        now = time.time()
        if count != last:
            last, stable_since = count, now
        elif count >= 1 and now - stable_since >= _QUIET_S:
            before = count
            break
        time.sleep(0.5)
    assert before is not None, (
        "bob's engine never settled a key-package pool within "
        f"{MLS_HANDSHAKE_S}s (last count {last!r}) — the login-time "
        "ensure_keypackages never ran or never finished"
    )

    # bob's ticker is muted from LAUNCH (FAUNA_CONV_POLL_SECS=3600, set before
    # his receive loop ever starts), so — unlike the web variant, which mutes
    # a running ticker mid-test — there is no in-flight cycle to quiesce here.
    muted_at = conv_receive_cycles(bob_app.driver)
    assert muted_at is not None, (
        "this client stopped publishing 'conv_receive_cycles' — this test's "
        "whole guard rests on it (fauna_e2e_agent::CONV_RECEIVE_CYCLES_KEY)"
    )

    # ── alice resolves bob by actor id and sends — the real cross-engine
    # bootstrap. bob's push arm is LIVE and his ticker is muted, so the nest's
    # push (Welcome + Application, both fired by conversations_handlers.rs) is
    # the only thing that can wake his rail.
    alice.conversations.real_resolve_send_new(bob["actor_id_hex"], body)

    # A nest-side witness that the bootstrap really fired, so a missing thread
    # below reads as "bob never woke", never as "alice never sent".
    assert conv_api.keypackage_count(port, bob, bob["actor_id_hex"]) < before, (
        "the 1:1 bootstrap should consume one of bob's key packages"
    )

    # ── bob wakes on the push, drains, joins, decrypts — with NO poke ────────
    got = _await_thread(bob_app, body, RECEIVE_CYCLE_S, muted_at)
    assert got.rail == "FaunaMls"
    assert body in got.snippet

    # ── The guard: no OTHER arm ran ───────────────────────────────────────
    # Ticker and poke sweeps both bump these counters; a push wake
    # deliberately does not (it is single-rail). Unchanged counters therefore
    # mean the delivery above can only have come from the push arm.
    after = conv_receive_cycles(bob_app.driver)
    assert after == muted_at, (
        f"a full receive cycle ran during the delivery window ({muted_at} → "
        f"{after}), so this run does NOT prove the push arm — the ticker is "
        "muted and nothing pokes, which leaves the reconnect arm (a socket "
        "flap). Re-run; a repeat means the socket is flapping under load and "
        "the flap, not this test, is the finding"
    )


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
                f"alice's message never reached bob's GUI within {budget_s}s "
                "with the backstop ticker muted and nothing poking, so the "
                "conversations PUSH ARM did not wake the rail — the "
                "`ConversationsSession::start_receive_loop` push case in "
                "libs/fauna-conversations, or the nest-side producers in "
                "conversations_handlers.rs. Receive cycles at mute time: "
                f"{muted_at}; now: {conv_receive_cycles(bob_app.driver)} "
                "(unchanged confirms no ticker/poke sweep masked or rescued "
                f"it). bob's threads: {bob_app.conversations.list_threads()}"
            )
        time.sleep(0.5)
