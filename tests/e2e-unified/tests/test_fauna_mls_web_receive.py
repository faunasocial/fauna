"""Tier_3 web FaunaMls **receive** proof — the durable inbox-apply loop (layer 4).

The mirror of ``test_fauna_mls_real_roundtrip``: there the GUI app *sends* and
engine-less peers observe the nest-side effects. Here the GUI app *receives* —
the proof of the web's new durable inbox-apply receive path (``drainInbox`` →
``ingest_welcome``, driven by the JS ``startReceivePoll`` loop). The web app
(bob) is the only real GUI engine; the *sender* (alice) is a throwaway engine
driven via the ``mls-group-gen`` helper (the same pattern ``mls-keypackage-gen``
uses to give engine-less peers a real key package).

Flow:
  1. bob = the web GUI, opted into the real ``FaunaMlsBackend`` (``real_faunamls_app``),
     which publishes real key packages so a sender can add him to a group.
  2. alice = an API-tier actor *and* a throwaway MLS engine. We fetch one of bob's
     published key packages, then ``mls-group-gen`` creates a real 1:1 MLS group +
     Welcome for it.
  3. We deliver that Welcome to bob's **durable inbox**
     (``fauna.conversations.welcome.deliver`` → ``push_inbox``, the canonical
     layer-1 ``InboxEnvelope``) — exactly as a real same-nest sender would, except
     bob is offline-equivalent (no push subscription on web at all).
  4. bob's web receive loop ticks ``drainInbox``, which fetches the Welcome,
     dispatches it to ``apply_welcome`` → joins + binds the MLS group, and a new
     FaunaMls thread appears in bob's conversations view — bound to the same
     channel id the sender created.

This proves **layer 4** (the web drains its durable inbox + joins a real Welcome it
never got a push for). Layer 5 — a two-driven-GUI-client harness (sender + receiver
both real GUIs) — is a separate, larger follow-on. Web-only: the FaunaMls receive
loop (``startReceivePoll`` → ``drainInbox``) is the web's; native apps drive the
shared ``ConversationsSession`` receive loop (layer 3, a separate track).
"""

from __future__ import annotations

import pytest

from common import create_actor_and_register
from helpers.app_surface import declared_absence
from helpers.budgets import MLS_HANDSHAKE_S
from helpers.waiting import wait_until
from tests.api import conv_api

pytestmark = [pytest.mark.web, pytest.mark.tier_3]


@pytest.mark.feature("conversations")
def test_fauna_mls_web_receive(real_faunamls_app, nest_instance, test_user):
    app = real_faunamls_app
    if not app.driver.is_web():
        declared_absence(
            app.driver,
            capability="the web FaunaMls receive-loop proof (drainInbox / "
            "startReceivePoll)",
            doc="testing.md § Cross-app e2e conventions, point 7 (web-only "
            "by construction — native apps drive the shared "
            "ConversationsSession receive loop instead, a separate layer-3 "
            "track per this file's own module docstring)",
        )

    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]
    bob_actor_id = test_user["actor_id_hex"]  # the web GUI is the recipient

    # alice — an API-tier actor (the authed caller for keypackage.fetch /
    # welcome.deliver) whose 32-byte secret also seeds the throwaway sender engine.
    alice = create_actor_and_register(port, admin_signing_key=admin_sk)

    # 1) Wait until bob's web app has published its key packages. The real
    #    backend opt-in (`enable_real_faunamls`) kicks off `ensureKeypackages`
    #    fire-and-forget, so the publish lands a moment after the fixture returns.
    wait_until(
        lambda: conv_api.keypackage_count(port, alice, bob_actor_id) > 0,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for bob's web client to publish a key package",
    )

    # 2) Fetch one of bob's key packages and mint a real 1:1 group + Welcome for it
    #    (alice = throwaway sender engine). The Welcome is sealed to bob's published
    #    key package, so only bob's GUI engine can join it.
    bob_kp = conv_api.keypackage_fetch(port, alice, bob_actor_id)
    assert bob_kp is not None, "bob should have a fetchable key package"
    channel_id_hex, welcome_bytes, _group_id_hex = conv_api.mint_group_welcome(
        bytes(alice["signing_key"]), bob_kp
    )

    # 3) Deliver the Welcome to bob's durable inbox (canonical layer-1 envelope).
    #    bob has NO push subscription on web, so the durable poll is his only path.
    # Mode gate (direct-messages.md § Reach policy): accept alice as a
    # contact so her DM Welcome flows under the default mode — per-pair,
    # so the session-scoped recipient's own mode stays untouched.
    conv_api.accept_contact(port, test_user, alice["actor_id_hex"])
    conv_api.welcome_deliver(port, alice, bob_actor_id, channel_id_hex, welcome_bytes)

    # 4) bob's web receive loop (`startReceivePoll` → `drainInbox`) must, within a
    #    few poll ticks, fetch + dispatch the Welcome, join the group, and surface a
    #    FaunaMls thread bound to the sender's channel id.
    def _received_thread():
        threads = app.conversations.list_threads()
        for t in threads:
            if t.rail == "FaunaMls" and t.channel_id_hex == channel_id_hex:
                return t
        return None

    received = wait_until(
        _received_thread,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for bob's web GUI to drain the inbox + join the Welcome's group",
    )
    assert received.channel_id_hex == channel_id_hex, (
        "the joined thread must bind the sender's channel id "
        f"(got {received.channel_id_hex!r}, want {channel_id_hex!r})"
    )
