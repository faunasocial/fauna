"""Tier_3 two-driven-client FaunaMls inbox-drain proof (every app with a second seat).

Layer 5 (native leg) of the durable inbox-apply consumer
(``docs/goal/architecture/api-layers.md`` § Inbox & Messaging — "Durable
inbox-apply consumer", layer 5). Proves a GUI member *receives + decrypts* a
Welcome via the **poll backstop alone**, with its push arm suppressed:

* **alice** (``real_faunamls_app``) is a real-engine GUI app. She
  resolves bob by 64-hex actor id and sends — the real MLS group bootstrap:
  fetch bob's key package → create the group → deliver the Welcome to bob's
  durable inbox → post the Application envelope.
* **bob** (``second_real_faunamls_app``) is a SECOND real-engine GUI app of
  the SAME client on the same nest, launched with
  ``FAUNA_E2E_SUPPRESS_CONV_PUSH=1`` so his
  conversations session is built with **no push arm** (``conv_push_source`` →
  ``None``). His ONLY receive path is the layer-3 drain backstop: the receive
  loop's ticker (``FAUNA_CONV_POLL_SECS=2``) drives ``fauna.inbox.fetch`` →
  decodes the ``InboxEnvelope{Welcome}`` → ``SessionInboxApply`` →
  ``ConversationsSession::ingest_welcome`` (the real engine binds the channel) →
  the *same* tick's ``poll_bound`` pulls the pre-join history → bob's engine
  **decrypts** the Application envelope.

This is the canonical-envelope → drain → decrypt round-trip over the real wire
that nothing else catches: ``conformance_inbox.rs`` covers only raw-byte
transport, and ``test_fauna_mls_real_roundtrip`` 's API-tier peers observe the
nest-side effects (key package consumed, Welcome delivered) but cannot decrypt.
Two real engines decrypting is proven in-process in
``libs/fauna-conversations/tests/fauna_mls_backend_tests.rs``; THIS test proves
it end-to-end through two real GUI apps over a real nest, via the *drain*
(not the push) path.

**Where this runs is the FIXTURE's call, not this module's.** The test carries no
app marker, so it collects everywhere and
``conftest.py::_launch_second_real_faunamls_app`` decides: the two direct-Rust
clients (linux + tui, which wire the in-process ``ConversationsSession`` at
login), apple (macos + ios), and windows — whose per-launch isolation each
supply bob's second seat — and a skip for anything else. web is the one
principled absence: the SPA has no push arm to suppress (poll-only by
construction, ``api-layers.md`` layer 4), so its layer-5 leg is tracked with the
web app's own backlog.

This docstring said "linux + tui" until 2026-09-22, long after macos, ios and
windows joined that fixture — and the
feature ledger
(``docs/features/ledger/``) records greens for **linux, macos and windows**,
which is the opposite of what the sentence implied. Read the ledger, not prose,
for which legs have actually been demonstrated: on macOS both halves of bob's
own real-rail precondition were proven here on 2026-09-22 (a red naming BOB when
his gate was closed, then a green), and no run is recorded for ios or tui.
"""

from __future__ import annotations

import time

import pytest

from helpers.budgets import MLS_HANDSHAKE_S, RECEIVE_CYCLE_S
from helpers.waiting import (
    await_receive_cycle_after,
    conv_receive_cycles,
    poke_receive_cycle,
    wait_until,
)
from tests.api import conv_api

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]


@pytest.mark.feature("conversations")
def test_fauna_mls_two_client_inbox_drain(
    real_faunamls_app, second_real_faunamls_app, nest_instance
):
    alice = real_faunamls_app
    bob_app, bob = second_real_faunamls_app
    port = nest_instance["port"]
    body = "hi via drain"

    # ── Precondition: bob's GUI engine has published its key packages on the nest ──
    # (login-time `ConversationsSession::start_receive_loop replenish`), so alice's
    # bootstrap can fetch one. bob keeps the private half — that is what lets him
    # DECRYPT below.
    #
    # Wait for the pool to SETTLE, not merely to become non-empty. The login
    # replenish tops up to `KEYPACKAGE_TARGET` (20) one package at a time, so a
    # `>= 1` gate captures `before` mid-climb; the pool then keeps rising while
    # alice's bootstrap consumes one, and the consumption assertion below reads
    # as its own opposite (`assert 19 < 18`) — a wall-clock race, not a product
    # failure (e2e-conventions.md § convention 14: assert latency-independent
    # state). Quiescence is the honest gate: the replenish is login-owned and
    # one-shot (`session.rs` — there is no periodic top-up), so once the count
    # stops moving it stays put and a consumption is unambiguous. Green runs pay
    # only the quiet window.
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

    # ── alice resolves bob by actor id and sends — the real cross-engine bootstrap.
    # bob's best-effort push is SUPPRESSED, so the Welcome reaches him ONLY via the
    # durable inbox queue his drain ticker polls.
    alice.conversations.real_resolve_send_new(bob["actor_id_hex"], body)

    # alice's bootstrap consumed one of bob's key packages (the Welcome was minted
    # against it) — a nest-side check that the cross-engine bootstrap really fired.
    assert conv_api.keypackage_count(port, bob, bob["actor_id_hex"]) < before, \
        "the 1:1 bootstrap should consume one of bob's key packages"

    # ── bob receives via the drain backstop ALONE (push suppressed) and DECRYPTS.
    # Proving InboxEnvelope{Welcome} (layer 1) → fauna.inbox.fetch → drain decode
    # (layer 2) → SessionInboxApply/ingest_welcome → real-engine decrypt
    # (layer 3) over the real wire, with no push arm in the loop.
    #
    # ⚠ **The anchor is a receive CYCLE, not a tick** (convention 14 mechanism 3).
    # This test used to poll a bare 40 s deadline that only
    # `FAUNA_CONV_POLL_SECS=2` kept inside its budget — the cadence was what made
    # it pass, which is a wall-clock dependence wearing a deadline-poll's clothes.
    # Now bob's loop is *poked* and the wait anchors on his cycle counters, so the
    # verdict is independent of both the tick and the machine's load.
    #
    # Poking does not weaken what this proves. The poke is an arm of the loop's
    # own `select!` and expands the identical `full_sweep!` the ticker expands, so
    # the drain → ingest → decrypt chain under test is byte-for-byte the ticked
    # one; what the poke removes is only the waiting. That the ticker itself
    # exists and is sized from `FAUNA_CONV_POLL_SECS` is pinned at tier_1
    # (`libs/fauna-conversations/src/session.rs` `cadence_tests`), which is
    # exactly the split convention 14 asks for: cadence logic at tier_1, one
    # wiring proof in the e2e.
    cycles = conv_receive_cycles(bob_app.driver)
    started = cycles[0] if cycles else None
    poke_receive_cycle(bob_app.driver)
    await_receive_cycle_after(
        bob_app.driver,
        started,
        budget_s=RECEIVE_CYCLE_S,
        what="alice's Welcome reaching bob's durable inbox",
    )

    # A completed cycle proves bob's loop LOOKED; it says nothing about what it
    # found (a sweep of an empty inbox completes identically). So the arrival is
    # its own assertion — and it gets a second cycle's worth of budget, because
    # the Welcome and the pre-join history can land on either side of one sweep's
    # internal ordering.
    def _decrypted_thread():
        return next(
            (
                t
                for t in bob_app.conversations.list_threads()
                if t.rail == "FaunaMls" and body in t.snippet
            ),
            None,
        )

    wait_until(
        lambda: _decrypted_thread() is not None,
        RECEIVE_CYCLE_S,
        diagnose=lambda: (
            "bob should receive + decrypt alice's message via the drain backstop "
            "(push arm suppressed); his threads were: "
            + str(
                [
                    (t.rail, t.flavor, t.snippet, t.message_count)
                    for t in bob_app.conversations.list_threads()
                ]
            )
        ),
    )
    got = _decrypted_thread()
    assert got.flavor == "OneToOne", \
        f"expected a 1:1 FaunaMls thread, got flavor={got.flavor!r}"
    assert got.message_count >= 1, \
        "the decrypted Application envelope should surface as a message"
