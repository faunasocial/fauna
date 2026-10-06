"""Tier_3 real-MLS proof that the conversation-list search filter — a LOCAL,
in-memory substring filter over already-loaded threads, not a nest re-query
(``docs/goal/ui/conversations.md:460-469``; no ``fauna.conversations.search``
kind) — operates correctly against a REAL decrypted message body.

``test_conversations_search.py``'s tier_2-shaped twin proves the filter
mechanics (``ConversationsManager::set_search_query`` -> ``filter_summaries``
over each thread's label + snippet) against ``inject_and_resolve_thread``'s
mock-injected content. It does NOT prove the filter runs correctly on a
``snippet`` populated by a real MLS Welcome + Application envelope, actually
DECRYPTED by the client (as opposed to a locally-injected plaintext body
handed straight to the render layer) — the same "mock vs real decrypt" gap
``test_conversations_real_muted_message.py`` closes for the muted-keyword
collapse render.

Do not oversell this as "search over real history": the search itself stays a
local filter over the already-loaded thread list. This test proves that local
filter is sound when it runs over real decrypted content, closing the
mocked-coverage gap honestly.

bob is the only real GUI engine needed (mirrors
``test_conversations_real_muted_message.py``'s shape): alice is an API-tier
throwaway sender (``create_actor_and_register`` + a one-shot ``mls-group-gen``
engine, discarded after minting — she never processes a reply). Unlike the
muted-message test, no app-specific skip is needed here: ``real_faunamls_app``
gates this to linux/web/tui/windows/macOS/iOS (only android remains Track E), and
all six of those also have ``conversation-search-box`` wired today (confirmed via
``ui-actual-{windows,macos,ios}.yaml``).

**windows/macOS/iOS are marker-gated (2026-07-15), not yet live-verified — see
``real_faunamls_app``'s docstring.** bob never calls any of the still-missing
native membership commands (``real_add``/``real_remove``/``real_rename``) or even
``real_send``/``real_resolve_send_new`` — he is purely a receiver, so this test
needed no code change to extend past linux/web/tui once the fixture generalized.
The ``real_conversations`` marker below is what makes windows/macOS/iOS launch
their REAL backend instead of the mock (``_apply_real_conversations_env``); run
this module in its own pytest invocation on those clients (mixing it with
mock-inject DM tests in the same invocation would flip those to real too).
Verification on windows/macOS/iOS is entrusted to follow-on sessions on those
platforms.
"""

from __future__ import annotations

import secrets

import pytest

from common import create_actor_and_register
from helpers.budgets import MLS_HANDSHAKE_S, UI_SETTLE_S
from helpers.waiting import wait_until
from tests.api import conv_api

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]

# Namespaced by a per-run TAG, mirroring test_conversations_search.py's identical
# rationale: the thread list is SESSION-scoped and accumulates every other test's
# threads, so a bare needle risks a stray match and an exact count would not be
# sound (testing.md § Cross-app e2e conventions: scoped queries, never global
# counts).
TAG = "rlsrch" + secrets.token_hex(3)
MATCHING_BODY = f"the {TAG} quarterly numbers are attached"


@pytest.mark.feature("find-a-conversation")
def test_search_filters_on_real_decrypted_snippet(real_faunamls_app, nest_instance, test_user):
    bob_app = real_faunamls_app
    port = nest_instance["port"]
    bob_actor_id = test_user["actor_id_hex"]  # bob is the real GUI, the recipient

    # alice — an API-tier throwaway actor whose keypair also seeds a one-shot
    # sender engine (mls-group-gen). She is the authed caller for
    # keypackage.fetch / welcome.deliver / channel.send; her MLS state is
    # discarded after minting (she never processes a reply).
    alice = create_actor_and_register(port, admin_signing_key=nest_instance["admin"]["signing_key"])

    # bob's real client publishes key packages on login (real-backend opt-in),
    # so alice can add him to a group — bob keeps the private half, which is
    # what lets him DECRYPT (and thus render + filter over) below.
    wait_until(
        lambda: conv_api.keypackage_count(port, alice, bob_actor_id) > 0,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for bob's client to publish a key package",
    )
    bob_kp = conv_api.keypackage_fetch(port, alice, bob_actor_id)
    assert bob_kp is not None, "bob should have a fetchable key package"

    # Mint a real 1:1 group + Welcome for bob's key package, plus a sealed
    # application message carrying MATCHING_BODY. bob is a member from group
    # creation, so the envelope decrypts for him once he has joined.
    channel_id_hex, welcome_bytes, envelope = conv_api.mint_group_welcome_with_message(
        bytes(alice["signing_key"]), bob_kp, MATCHING_BODY
    )

    # Deliver the Welcome to bob's durable inbox and let his receive loop join
    # the group before the tagged message is posted to it.
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

    # Post the tagged message. bob's receive loop fetches + DECRYPTS it, and
    # the manager's snapshot exposes the decrypted body as the thread's
    # snippet — the value the search filter runs over.
    conv_api.channel_send(port, alice, channel_id_hex, envelope)
    wait_until(
        lambda: any(
            t.channel_id_hex == channel_id_hex and TAG in (t.snippet or "")
            for t in bob_app.conversations.list_threads()
        ),
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for bob's client to decrypt alice's message",
    )

    conv = bob_app.conversations
    driver = bob_app.driver
    conv.navigate()

    wait_until(
        lambda: any(t.channel_id_hex == channel_id_hex for t in conv.list_threads()),
        UI_SETTLE_S,
        diagnose=lambda: "the real thread never rendered before filtering",
    )
    unfiltered = driver.count("conversation-item")

    # TAG appears only in the real, DECRYPTED snippet -> exactly one match.
    conv.search_conversations(TAG)
    wait_until(
        lambda: driver.count("conversation-item") == 1,
        UI_SETTLE_S,
        diagnose=lambda: "search over the real decrypted snippet never filtered to the one matching thread",
    )

    # A query that cannot match anything seeded this run -> zero matches.
    conv.search_conversations(TAG + "-nomatch")
    wait_until(
        lambda: driver.count("conversation-item") == 0,
        UI_SETTLE_S,
        diagnose=lambda: "a non-matching query never filtered the real thread out",
    )

    # Clearing the box restores the full (unfiltered) list.
    conv.search_conversations("")
    wait_until(
        lambda: driver.count("conversation-item") == unfiltered,
        UI_SETTLE_S,
        diagnose=lambda: "clearing the query never restored the full thread list",
    )
