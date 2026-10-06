"""Tier_3 cross-device MLS group-state sync GUI proof on ``--client web``.

The web analogue of ``test_fauna_mls_cross_device_sync.py`` (the linux leg): the
end-to-end validation that the **web** cross-device MLS state-sync leg (slice 5
leg 2 — ``libs/fauna-wasm/src/conversations.rs`` ``restoreMlsState`` /
``saveMlsState`` + the ``WsMlsReplicaTransport`` adapter) actually FIRES —
the one thing the shared-Rust journey (``libs/fauna-client-mls-sync`` tier_1)
and the linux GUI proof cannot show for the web wiring. Authority:
``docs/goal/behavior/devices.md`` § Cross-device MLS group-state sync; design tracked internally, §5
(restore-before-first-poll) + §1 (a sender cannot MLS-decrypt its own
application messages ⇒ own history rides the replica, never log replay).

**The web's "second device" is a page reload, not a second browser.** The
browser MLS engine is **in-memory** (``MlsEngine::new_in_memory`` in
``with_conversations`` — the browser has no SQLite; the at-rest backup-of-record
is the nest replica), so a ``location.reload()`` rebuilds a **fresh, empty**
engine under the SAME identity (the secret persists in the page's local storage
⇒ same ``ActorId`` + ``BackupKey``). That fresh engine MUST reconstruct its
conversations by restoring the cross-device MLS state replica on the next manager
build (``restoreMlsState``, before the first ``pollConversations``) — exactly the
role linux's fresh-``mls_state.db`` ``alice_second_device`` plays. The LIVE
concurrent case — two tabs open at once, each in its own BrowserContext —
is ``test_fauna_mls_web_concurrent_tabs`` below (slice 6 sub-part (a), via the
``alice_second_web_device`` twin-page fixture).

* **device A** — alice (``real_faunamls_app`` = ``logged_in_app`` on ``web``, the
  real ``FaunaMlsBackend`` over ``WsConversationsRpc``). She creates a 1:1 with an
  API-tier peer and posts her OWN message; her debounced autosave
  (``saveMlsState``, scheduled off the post-mutation ``refreshConversations``
  chokepoint) then seals + uploads the ``provider`` + ``history/<ch>`` replicas
  under her ``BackupKey``.
* **device B** — the SAME page after ``hard_reload()``: a fresh in-memory engine,
  same secret. On the post-reload manager build it restores the replica
  (``restoreMlsState``, before the first poll) and — the CRUX — reads alice's OWN
  posted message, which log replay alone can NEVER reconstruct (own-leaf messages
  are not MLS-decryptable by the sender, design §1).

The nest-wire replica plane is proven at the API tier
(``tests/api/test_mls_replica_sync.py``), the resync convergence in process
(``libs/fauna-client-mls-sync/gate_impl.rs``), and the *client-agnostic* crux
(own-message-restore) on linux (``test_fauna_mls_cross_device_sync.py``). THIS
test is the only one that drives the leg through the real WEB wiring over a real
nest — the success bar for slice 5 leg 2.

web-only: the leg under test is the ``fauna-wasm`` web wiring. Skips under any
non-web ``app`` param (linux has its own proof; the native UniFFI apps are
route-3 hand-offs).
"""

from __future__ import annotations

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.app_surface import declared_absence
from helpers.budgets import MLS_COMMIT_FOLD_S, MLS_HANDSHAKE_S
from helpers.waiting import await_folded_commit_after, mls_folded_commits
from tests.api import conv_api

# Reuse the client-agnostic poll helpers from the linux cross-device proof — the
# replica-upload gate, the thread-appearance poll, and the rendered-bubble read
# are all driver-agnostic (they go through the shared ``app.conversations`` action
# layer + a raw WS-RPC observer), so there is one shape, not a web copy (#2).
from tests.test_fauna_mls_cross_device_sync import (
    _wait_channel_grows,
    _wait_fauna_thread,
    _wait_message_texts,
    _wait_replica_uploaded,
    _wait_thread_snippet,
)

pytestmark = [pytest.mark.web, pytest.mark.tier_3]


@pytest.mark.feature("conversations")
def test_fauna_mls_web_cross_device_sync(real_faunamls_app, nest_instance, test_user):
    alice = real_faunamls_app  # device A (web)
    if not alice.driver.is_web():
        declared_absence(
            alice.driver,
            capability="the web cross-device MLS-sync GUI proof",
            doc="testing.md § Cross-app e2e conventions, point 7 (native "
            "apps prove the identical guarantee via their own twin, "
            "test_fauna_mls_cross_device_sync.py)",
        )

    port = nest_instance["port"]
    node_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    def _alice_client():
        # A raw User-class WS-RPC connection as alice — a read-only 'device' that
        # observes her replica plane (`fauna.mls.get`), same shape as the linux
        # proof + tests/api/test_mls_replica_sync.py::_client.
        return WsRpcAdminClient(
            node_url,
            actor_id=test_user["actor_id_bytes"],
            signing_key=bytes(test_user["signing_key"]),
        )

    # ── An API-tier peer publishing a real key package, so alice's bootstrap can
    # fetch one (fake KPs fail to parse; see test_fauna_mls_real_roundtrip). The
    # peer has no MLS engine — it never decrypts, only observes nest-side effects,
    # which is all this test needs of it. ──
    peer = conv_api.reachable_peer(port, admin_sk, test_user["actor_id_hex"])
    assert conv_api.keypackage_count(port, peer, peer["actor_id_hex"]) == 3

    # ── device A: create a 1:1 with the peer and post alice's OWN message. ──
    own_line = "device-A owns this line (web)"
    alice.conversations.real_resolve_send_new(peer["actor_id_hex"], own_line)
    # Pin THIS test's 1:1 by its own echo (a bare flavor match is ambiguous:
    # the session-scoped alice may carry other restored OneToOne threads).
    a_thread = _wait_thread_snippet(alice, own_line, timeout=MLS_HANDSHAKE_S)
    assert a_thread is not None, "the 1:1 should surface alice's own echo"
    channel_hex = a_thread.channel_id_hex
    assert channel_hex, "the 1:1 thread should bind a channel after send"

    # ── device A's debounced autosave (`saveMlsState`) uploads the sealed replica.
    # Gate on the ACTUAL upload (not a fixed sleep): the history/<ch> slice is what
    # carries alice's own (own-leaf) message plaintext — the thing log replay can't
    # give device B (design §1). ──
    _wait_replica_uploaded(_alice_client, "provider")
    _wait_replica_uploaded(_alice_client, f"history/{channel_hex}")

    # ── device B: reload the page → a FRESH, empty in-memory MLS engine under the
    # same identity (its `restoreMlsState` on the next manager build must
    # reconstruct alice's conversations from the replica — the web analogue of a
    # fresh `mls_state.db`). Re-arm the real backend (idempotent; the fixture's
    # enable is per-page) so the post-reload manager runs the real plane. ──
    alice.driver.hard_reload()
    alice.conversations.enable_real_faunamls()

    # ── CRUX: the post-reload restore materialised alice's 1:1, and — the thing log
    # replay could NEVER give it (a sender cannot MLS-decrypt its own application
    # messages, design §1) — the OWN message she posted. First confirm the restore
    # surfaced the thread, then open it and read the rendered own bubble as the
    # definitive proof. ──
    # Pin the wait to THIS test's channel (alice is the session-scoped
    # `test_user`; the replica may carry other tests' restored threads too).
    b_thread = _wait_fauna_thread(
        alice, flavor="OneToOne", timeout=50.0, channel_hex=channel_hex
    )
    dump = [
        (t.rail, t.flavor, t.channel_id_hex, t.snippet)
        for t in alice.conversations.list_threads()
    ]
    assert b_thread is not None, (
        "the reloaded (fresh-engine) page should restore alice's 1:1 (channel "
        f"{channel_hex}) from the replica; its threads were {dump}"
    )

    alice.conversations.open_thread_by_channel(channel_hex)
    texts = _wait_message_texts(alice)
    assert any(own_line in t for t in texts), (
        "the reloaded page should render alice's OWN message bubble, restored from "
        "the history replica (log replay can't reconstruct an own-leaf message); "
        f"got {texts!r}"
    )

    # ── device B WRITES back to the pre-existing conversation. It did not author
    # the restored epoch in THIS (fresh) session, so its first send posts a
    # device-owned-epoch takeover self-Update commit (design §3c) THEN the
    # Application envelope — both land on the channel (device B gained WRITE access
    # to the pre-existing conversation). Observe nest-side via the API peer's
    # channel_fetch (neither device's traffic is peer-decryptable, but the record
    # count is). ──
    before = len(conv_api.channel_fetch(port, peer, channel_hex, after=0))
    reply = "device-B writes back (web)"
    alice.conversations.real_send(b_thread.thread_id, reply)
    assert _wait_thread_snippet(alice, reply, timeout=MLS_HANDSHAKE_S) is not None, (
        "the reloaded device's own reply should echo locally in its thread"
    )
    after_b = _wait_channel_grows(port, peer, channel_hex, before)
    assert after_b > before, (
        "device B's takeover commit + Application envelope should land on the "
        f"channel (records {before} → {after_b})"
    )


@pytest.mark.feature("conversations")
def test_fauna_mls_web_concurrent_tabs(
    real_faunamls_app, nest_instance, test_user, request
):
    """Slice 6 sub-part (a): two LIVE web tabs (same identity, isolated
    Playwright BrowserContexts = isolated storage) behave as two concurrent
    devices and converge via the nest replica + CAS — the live-concurrent
    analogue of the reload proof above, and the web mirror of the linux
    two-instance journey (``test_fauna_mls_cross_device_sync``). This is the
    proof that the web conversations plane needs NO single-tab guard: the
    retired ``fauna_mls_active_tab`` lease guarded only the legacy standalone
    key-package engine, which slice 6 deleted (devices.md § Cross-device MLS
    group-state sync)."""
    alice = real_faunamls_app  # device A (web, tab 1)
    if not alice.driver.is_web():
        declared_absence(
            alice.driver,
            capability="the concurrent-tabs web conversations-plane proof",
            doc="testing.md § Cross-app e2e conventions, point 7 (a "
            "concurrent-browser-tab proof is web-specific by construction; "
            "linux's twin proof is test_fauna_mls_cross_device_sync)",
        )

    port = nest_instance["port"]
    node_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    def _alice_client():
        return WsRpcAdminClient(
            node_url,
            actor_id=test_user["actor_id_bytes"],
            signing_key=bytes(test_user["signing_key"]),
        )

    # ── An API-tier peer with a real key package (as in the reload proof). ──
    peer = conv_api.reachable_peer(port, admin_sk, test_user["actor_id_hex"])

    # ── tab A: create a 1:1 with the peer and post alice's OWN message; gate on
    # the replica upload so tab B's restore finds it. ──
    own_line = "tab-A owns this line (concurrent web)"
    alice.conversations.real_resolve_send_new(peer["actor_id_hex"], own_line)
    # Pin THIS test's 1:1 by its own echo (alice carries the reload proof's
    # earlier 1:1 in the same session — a bare flavor match is ambiguous).
    a_thread = _wait_thread_snippet(alice, own_line, timeout=MLS_HANDSHAKE_S)
    assert a_thread is not None, "the 1:1 should surface tab A's own echo"
    channel_hex = a_thread.channel_id_hex
    assert channel_hex, "the 1:1 thread should bind a channel after send"
    _wait_replica_uploaded(_alice_client, "provider")
    _wait_replica_uploaded(_alice_client, f"history/{channel_hex}")

    # ── tab B: a SECOND live page in its own BrowserContext, same secret —
    # launched on-demand now (after the replica exists) WHILE tab A stays live
    # and polling. Its fresh in-memory engine restores the replica on the
    # manager build. ──
    device_b, _twin_driver = request.getfixturevalue("alice_second_web_device")

    # Pin the wait to THIS test's channel: alice is the session-scoped
    # `test_user`, so the replica also carries the reload proof's earlier 1:1 —
    # a bare first-FaunaMls-thread match would return that one.
    b_thread = _wait_fauna_thread(
        device_b, flavor="OneToOne", timeout=50.0, channel_hex=channel_hex
    )
    dump = [
        (t.rail, t.flavor, t.channel_id_hex, t.snippet)
        for t in device_b.conversations.list_threads()
    ]
    assert b_thread is not None, (
        "the concurrent second tab should restore alice's 1:1 (channel "
        f"{channel_hex}) from the replica; its threads were {dump}"
    )
    device_b.conversations.open_thread_by_channel(channel_hex)
    texts = _wait_message_texts(device_b)
    assert any(own_line in t for t in texts), (
        "the second tab should render alice's OWN message bubble from the "
        f"history replica; got {texts!r}"
    )

    # ── tab B writes back (device-owned-epoch takeover commit + Application
    # envelope) while tab A is live. ──
    before = len(conv_api.channel_fetch(port, peer, channel_hex, after=0))
    # Tab A's fold-in baseline, read BEFORE tab B writes — see the barrier below
    # (and the native twin, which carries the ordering argument in full).
    a_folded = mls_folded_commits(alice.driver, channel_hex)
    reply = "tab-B writes back (concurrent web)"
    device_b.conversations.real_send(b_thread.thread_id, reply)
    assert _wait_thread_snippet(device_b, reply, timeout=MLS_HANDSHAKE_S) is not None, (
        "the second tab's own reply should echo locally in its thread"
    )
    after_b = _wait_channel_grows(port, peer, channel_hex, before)
    assert after_b > before, (
        "tab B's takeover commit + Application envelope should land on the "
        f"channel (records {before} → {after_b})"
    )

    # ── tab A converges LIVE: its receive poll folds B's own-leaf takeover (the
    # slice-4c OwnLeafCommit resync) without a reload, and its next send re-takes
    # the epoch through the gate — the two-live-devices crux. Anchor on the
    # fold-in itself rather than on poll cycles (mirrors the native proof, which
    # carries the reasoning and the do-not-re-attempt warning). ──
    await_folded_commit_after(
        alice.driver,
        channel_hex,
        a_folded,
        budget_s=MLS_COMMIT_FOLD_S,
        what="tab B's takeover commit",
    )
    follow = "tab-A after B took over (concurrent web)"
    alice.conversations.real_send(a_thread.thread_id, follow)
    after_a = _wait_channel_grows(port, peer, channel_hex, after_b)
    assert after_a > after_b, (
        "tab A should post a follow-up after tab B's takeover — its live "
        "receive loop must process B's own-leaf commit without wedging "
        f"(records {after_b} → {after_a})"
    )
