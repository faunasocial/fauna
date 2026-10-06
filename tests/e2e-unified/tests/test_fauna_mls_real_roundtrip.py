"""Tier_3 real-wire FaunaMls round-trip.

This is the real-wire counterpart to the snapshot-helper conversations tests
(``test_thread_membership`` / ``test_thread_rename``, which drive FaunaMls
through the e2e **mock** backend). Here the linux app opts into its **real**
``FaunaMlsBackend`` and fires real
``fauna.conversations.*`` wire ops the real nest carries:

* **alice** is the linux GUI app, running the real backend against the real
  nest (``logged_in_app`` + ``test_user``). Her async manager wire-drivers are
  driven via the ``conversations_real_*`` bridge commands. Thread **creation**
  goes through the recipient picker's real resolution
  (``conversations_real_resolve_send_new``): alice types the peer's actor id, and
  ``resolve_recipient`` promotes it to a ``Fauna`` chip via the real
  ``fauna.conversations.keypackage.count`` probe — no ``actor_id`` injection.
  Membership ops (add/remove) here inject the peer ``actor_id`` instead, which
  keeps this module's focus on the wire effects rather than the picker.

  ⚠ **Convention 8 (drive mutations through the UI) — the membership ops below
  are command-driven, and the carve-out that permits that is
  ``test_thread_membership_real.py::test_in_place_mls_add_through_the_ui``**: it
  proves a real user reaches the same in-place add through
  ``thread-add-participant-button`` → ``recipient-picker-input`` → the resolve →
  ``add-participant-confirm``. Keep that test alive; without it these adds are an
  API-only mutation path with no UI witness.

  (The handle→actor lookup is **no longer deferred** — ``resolve_address`` Form 2
  resolves a bare localpart via ``fauna.actor.by_handle`` and canonicalises it to
  ``localpart@domain``; the UI-driven test above types a bare handle. This
  docstring claimed otherwise until 2026-08-03.)
* **bob / carol / dave** are API-tier actors on the same nest
  (``create_actor_and_register`` + ``tests.api.conv_api``). They have **no MLS
  engine**, so they observe nest-side effects — key package consumed, Welcome
  delivered to their inbox, envelopes (Application + Commit) on the channel —
  but cannot decrypt. The decrypt round-trip is proven separately with two real
  engines in ``libs/fauna-conversations/tests/fauna_mls_backend_tests.rs``;
  together they cover the end goal's "real nest carrying channel + welcomes,
  second actor observing the effects".

Because the real backend **parses** each peer's key package during group
bootstrap (``MlsEngine::key_package_from_bytes``), the API-tier peers can't use
the fake byte strings ``test_mls_channels.py`` uses — a fake KP is consumed but
fails to parse, yielding no group. So each peer publishes a **real** key package
minted by the ``mls-keypackage-gen`` helper (a throwaway in-memory engine bound
to the peer's identity; the private half is discarded, which is fine because the
API-tier peer never processes the Welcome).

Runs on ``--client linux``, ``--client web``, ``--client tui``, and
``--client windows`` — the first three drive the real ``FaunaMlsBackend`` over
their own conversations RPC seam (``NestConversationsRpc`` for the two
direct-Rust clients / ``WsConversationsRpc`` wasm for web); windows drives the
same manager membership/send surface directly over UniFFI async-export
(``ConversationsCommands.{RealAdd,RealRemove,RealRename}`` calling
``ConversationsManager.{OpenAddParticipant→ConfirmAddParticipant,
RemoveParticipant,RenameThread}``). The remaining native UniFFI apps
(macos/ios/android) still wait on Track E for this surface.

⚠ **Pin every thread by identity, never by flavor or list position.** alice is
the session-scoped ``test_user``, so she keeps the threads an EARLIER run of this
very test left behind: under ``--app sweep`` (or any multi-app run) the second
app's param restores the first app's threads through the cross-device MLS
replica, carrying the same flavors and, with literal bodies, the same snippets.
A bare "the MlsGroup thread" pick then landed on the previous run's group once
the new group's send re-sorted the list: the in-place add of this run's dave
still passed its assertions there, and the remove of this run's carol failed
with ``is not a member of this group`` and posted no Commit (``assert 6 > 6``).
Every app after the first failed that way, while every standalone run passed.
"""

from __future__ import annotations

import secrets

import pytest

from common import create_actor_and_register
from helpers.budgets import RPC_ROUNDTRIP_S
from tests.api import conv_api
from tests.api.conv_api import inbox as _inbox
from tests.api.conv_api import mint_key_packages as _mint_keypackages

# Snippet-pinned thread wait (client-agnostic, shared with the cross-device
# proofs): pin a just-sent thread by its own echo. The needle must be unique to
# this run, see the module docstring's last section.
from tests.test_fauna_mls_cross_device_sync import _wait_thread_snippet

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]


def _thread_by_id(app, thread_id: str):
    """The thread ``thread_id`` in alice's snapshot, asserted present."""
    threads = app.conversations.list_threads()
    matches = [t for t in threads if t.thread_id == thread_id]
    assert matches, (
        f"thread {thread_id} is not in alice's snapshot "
        f"(have {[(t.thread_id, t.rail, t.flavor) for t in threads]})"
    )
    return matches[0]


@pytest.mark.feature("conversations")
def test_fauna_mls_real_roundtrip(real_faunamls_app, nest_instance, test_user):
    app = real_faunamls_app
    port = nest_instance["port"]
    base = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]
    # Every message body carries this run's token, so an echo pin cannot match a
    # thread an earlier run of this test left on the shared alice.
    run = secrets.token_hex(4)
    hi_bob = f"hi bob {run}"
    hi_group = f"hi group {run}"

    # API-tier peers, each publishing real key packages (bob is added to two
    # groups — the 1:1 then the forked group — so he needs ≥2).
    bob = create_actor_and_register(port, admin_signing_key=admin_sk)
    carol = create_actor_and_register(port, admin_signing_key=admin_sk)
    dave = create_actor_and_register(port, admin_signing_key=admin_sk)
    # bob is resolved (not injected) in step 1, so he needs no handle here; carol
    # and dave are injected for the membership adds below. Their handles carry NO
    # `@domain`: carol/dave are same-nest actors (registered on `nest_instance`),
    # and a bare handle classifies same-nest (`peer_domain_for` → None) on every
    # app regardless of how it derives its own `self_domain` in the harness
    # (web: the nest's whoami `localhost`; linux: the nest host `127.0.0.1`).
    # A `@self-nest.test` form would instead route cross-nest — which linux's
    # `NestConversationsRpc` resolves back to the same nest via loopback authority,
    # but web's `WsConversationsRpc` does not yet carry (the deferred cross-nest
    # web Track). Same-nest is the membership test's actual intent; cross-nest has
    # its own dedicated proof (`conformance_cross_nest_conversations_client.rs`).
    carol_handle = "carol"
    dave_handle = "dave"
    for peer in (bob, carol, dave):
        conv_api.keypackage_upload(port, peer, _mint_keypackages(bytes(peer["signing_key"]), 3))
        assert conv_api.keypackage_count(port, peer, peer["actor_id_hex"]) == 3
        # Each peer accepts alice, WITHOUT which every Welcome below is refused.
        # The family-safety reach pillar made the
        # Welcome plane consult the recipient's inbox mode, whose default is
        # `allow_knock` → `dm_initiation_mode_verdict(None, AllowKnock)` = Knock →
        # `forbidden` (direct-messages.md § Reach policy). This test predated that
        # ruling and had been RED on every app since — the key package is still
        # consumed by the fetch, so it presented as the far more confusing
        # "no Welcome delivered" rather than a refusal.
        conv_api.accept_contact(port, peer, test_user["actor_id_hex"])

    # ── 1) alice → 1:1 with bob, resolve + send ──────────────────────────────
    # alice types bob's actor id into the recipient picker; `resolve_recipient`
    # probes `keypackage.count(bob) > 0` and promotes it to a `Fauna` chip (no
    # actor_id injection). Then the real bootstrap: fetch bob's key package →
    # create MLS group → deliver Welcome → post the Application envelope.
    app.conversations.real_resolve_send_new(bob["actor_id_hex"], hi_bob)

    assert conv_api.keypackage_count(port, bob, bob["actor_id_hex"]) == 2, \
        "bob's key package should be consumed by the 1:1 bootstrap"
    assert len(_inbox(base, bob)) >= 1, "a Welcome should be delivered to bob"

    # Pin the just-created 1:1 by its own echo.
    one = _wait_thread_snippet(app, hi_bob, timeout=RPC_ROUNDTRIP_S)
    assert one is not None, "the 1:1 with bob should surface its own echo"
    c1 = one.channel_id_hex
    assert c1, "the 1:1 thread should be bound to a channel after send"
    assert len(conv_api.channel_fetch(port, bob, c1, after=0)) >= 1, \
        "the Application envelope should be on the channel"

    # ── 2) add carol → forks a new group; send → bootstrap it ────────────────
    # On a FaunaMls 1:1, add forks a fresh participant-keyed group (snapshot
    # only). The first send on it bootstraps the 3-party group [alice, bob,
    # carol] on a new channel, welcoming bob + carol. The fork has no echo to pin
    # by until that send, so it is pinned as the one MlsGroup thread the add
    # itself created.
    known = {t.thread_id for t in app.conversations.list_threads()}
    app.conversations.real_add(one.thread_id, carol["actor_id_hex"], carol_handle)
    forked = [
        t for t in app.conversations.list_threads()
        if t.rail == "FaunaMls" and t.flavor == "MlsGroup" and t.thread_id not in known
    ]
    assert len(forked) == 1, (
        "adding carol to the 1:1 should fork exactly one new MlsGroup thread, got "
        f"{[(t.thread_id, t.label) for t in forked]}"
    )
    app.conversations.real_send(forked[0].thread_id, hi_group)

    assert conv_api.keypackage_count(port, bob, bob["actor_id_hex"]) == 1, \
        "bob's key package should be consumed again by the forked-group bootstrap"
    assert conv_api.keypackage_count(port, carol, carol["actor_id_hex"]) == 2, \
        "carol's key package should be consumed by the forked-group bootstrap"
    assert len(_inbox(base, carol)) >= 1, "a Welcome should be delivered to carol"

    # The bound group, pinned by its own echo exactly as the 1:1 above.
    group = _wait_thread_snippet(app, hi_group, timeout=RPC_ROUNDTRIP_S)
    assert group is not None and group.flavor == "MlsGroup", (
        "the forked group should surface its own echo, got "
        f"{group and (group.thread_id, group.flavor)}"
    )
    c2 = group.channel_id_hex
    assert c2 and c2 != c1, "the forked group should bind a distinct channel"
    base_envelopes = len(conv_api.channel_fetch(port, bob, c2, after=0))
    assert base_envelopes >= 1, "the group's Application envelope should be on the channel"

    # ── 3) in-place add dave → MLS Commit + Welcome on the bound group ────────
    # Command-driven mutation; the convention-8 carve-out is satisfied by the
    # UI-driven twin of this exact step,
    # `test_thread_membership_real.py::test_in_place_mls_add_through_the_ui`.
    app.conversations.real_add(group.thread_id, dave["actor_id_hex"], dave_handle)

    assert conv_api.keypackage_count(port, dave, dave["actor_id_hex"]) == 2, \
        "dave's key package should be consumed by the in-place add"
    assert len(_inbox(base, dave)) >= 1, "a Welcome should be delivered to dave"
    after_add = len(conv_api.channel_fetch(port, bob, c2, after=0))
    assert after_add > base_envelopes, "the add should post a Commit envelope on the channel"

    # ── 4) remove carol → MLS Commit (no Welcome) ────────────────────────────
    app.conversations.real_remove(group.thread_id, carol["actor_id_hex"], carol_handle)
    after_remove = len(conv_api.channel_fetch(port, bob, c2, after=0))
    assert after_remove > after_add, "the remove should post a Commit envelope on the channel"

    # ── 5) rename → encrypted NameChanged Application envelope ────────────────
    app.conversations.real_rename(group.thread_id, "Lunch Crew")
    after_rename = len(conv_api.channel_fetch(port, bob, c2, after=0))
    assert after_rename > after_remove, "the rename should post a NameChanged envelope on the channel"
    assert _thread_by_id(app, group.thread_id).label == "Lunch Crew", \
        "the rename should relabel alice's thread"

# NOTE — handle→actor resolution (`fauna.actor.by_handle`) is NOT exercised in
# THIS test (the same-nest peers carol/dave are injected by actor id; bob is
# resolved by 64-hex actor id, not handle). The recipient picker's *handle* form
# is proven at tier_1 in `libs/fauna-conversations/tests/fauna_mls_backend_tests.rs`
# (`resolve_address` form-2) + `manager_integration_tests.rs`; the nest wire
# contract by `bins/fauna-nest/tests/conformance_discovery.rs`.
#
# A real-wire GUI handle-resolution proof was once thought infeasible here (no
# fixture could provision a *handled* actor). That is RESOLVED (2026-06-01): the
# nest fixture now starts a foreign peer with a configured `handle_domain` +
# open registration, and `common.auth.register_handled_actor` seeds a handled
# actor over the wire (`fauna.account.register` → `create_user_with_handle`). The
# *cross-nest* handle path is now proven GUI-side in the sibling test
# `test_fauna_mls_cross_nest_roundtrip.py` (alice resolves `bob@<foreign>` and
# sends). The catch-all mock-Email fall-through is no longer a false-confidence
# trap there: the assertion keys on bob's *foreign* key package being consumed
# (which only a real cross-nest FaunaMls bootstrap does), so an Email fall-through
# fails the test rather than masking it.
