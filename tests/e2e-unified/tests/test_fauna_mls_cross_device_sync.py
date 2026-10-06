"""Tier_3 cross-device MLS group-state sync GUI proof on ``--client linux`` / ``macos`` / ``windows`` / ``tui``.

The end-to-end validation that a client's **cross-device MLS state-sync leg**
actually FIRES on login — linux: slice 5 leg 1
(``apps/fauna-linux/src/conversations/conv_backend.rs`` ``wire_mls_state_sync``
+ ``attach_replica_autosave``); macOS: the shared FFI-factory leg (the
``MlsSyncLauncher`` seam, ``libs/fauna-ffi/src/mls_sync_launch.rs`` → the
shared tokio launcher ``fauna_client_mls_sync::launcher`` →
``fauna_client_mls_sync::orchestration``, the same wiring windows/android/iOS
consume); tui: the same shared tokio launcher consumed directly (no FFI
factory — ``apps/fauna-tui/src/conversations/conv_backend.rs``) — the one
thing the Rust journey (``libs/fauna-client-mls-sync`` tier_1) cannot prove.
Authority: ``docs/goal/behavior/devices.md`` § Cross-device
MLS group-state sync; design tracked internally, §5
(restore-before-first-poll) + §1 (a sender cannot MLS-decrypt its own
application messages ⇒ own history rides the replica, never log replay).

⚠ Run this module in its OWN pytest invocation on macOS / windows: the
``real_conversations`` marker flips ``FAUNA_E2E_REAL_CONVERSATIONS`` session-wide
for every native (apple / windows) config built that session
(``_apply_real_conversations_env``), which would flip mock-inject DM tests
collected alongside it to the real backend. linux ignores the flag (its real
backend is a runtime toggle).

* **device A** — alice (``real_faunamls_app`` = ``logged_in_app``, the real
  ``FaunaMlsBackend`` over ``NestConversationsRpc``). She creates a 1:1 with an
  API-tier peer and posts her OWN message; her debounced autosave
  (``REPLICA_DEBOUNCE`` 1.5 s) then seals + uploads the ``provider`` +
  ``history/<ch>`` replicas under her ``BackupKey``.
* **device B** — ``alice_second_device``: a SECOND GUI app (same client as
  device A) with alice's *same* secret key (⇒ same ``ActorId`` + ``BackupKey``)
  but a fresh, EMPTY MLS state db (linux: per-launch ``mkdtemp`` HOME; macOS: a
  pinned fresh ``home`` → ``CFFIXED_USER_HOME`` isolating Application Support;
  windows: a pinned fresh ``data_dir`` → ``FAUNA_E2E_DATA_DIR``. Windows launches
  are isolated by default now, but device B still PINS its dir — the two-device
  claim rests on a root this fixture owns and knows is empty, not on a driver
  default).
  On login it restores the replica (restore-before-first-poll) and — the CRUX —
  reads alice's OWN posted message, which log replay alone can NEVER reconstruct
  (own-leaf messages are not MLS-decryptable by the sender, design §1). Then it
  writes back (a device-owned-epoch takeover self-``Update`` commit + the
  Application envelope, both landing on the channel — device B gained WRITE
  access to the pre-existing conversation), and device A's receive loop processes
  B's own-leaf takeover (the slice-4c ``OwnLeafCommit`` resync) and stays live
  enough to post a follow-up of its own.

The nest-wire replica plane is proven at the API tier
(``tests/api/test_mls_replica_sync.py``), and the resync convergence in process
(``libs/fauna-client-mls-sync/gate_impl.rs``
``twin_device_takeover_resyncs_the_other_device``). THIS test is the only one
that drives the whole leg through two real GUI apps over a real nest — the
success bar for slice 5 leg 1 (linux) and the native FFI-factory leg
(macOS + windows).

The web second-device leg is ``test_fauna_mls_web_cross_device_sync.py``;
android runs this same shape from its own machine. ``alice_second_device`` skips
under any other ``app`` param.
"""

from __future__ import annotations

import time
from pathlib import Path

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.app_surface import skip_unbuilt
from helpers.budgets import MLS_COMMIT_FOLD_S, MLS_HANDSHAKE_S
from helpers.waiting import (
    await_folded_commit_after,
    mls_folded_commits,
    wait_rail_blob_settled,
    wait_until,
)
from tests.api import conv_api

FIXTURE_IMAGE = Path(__file__).parent.parent / "fixtures" / "test-image.png"

# real_conversations: macOS opts into the real ConversationsSession at LAUNCH
# (FAUNA_E2E_REAL_CONVERSATIONS via _apply_real_conversations_env); linux ignores
# the flag (runtime toggle via enable_real_faunamls). See module docstring for
# the isolated-invocation requirement this marker imposes on apple runs.
pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]


def _replica_blob(client_factory, path):
    """The sealed blob at replica ``path`` right now (``None`` when absent)."""
    with client_factory() as c:
        return c.call("fauna.mls.get", {"path": path}).get("blob")


def _wait_replica_uploaded(client_factory, path, timeout=25.0, *, superseding=None):
    """Device A's debounced autosave uploaded replica ``path`` — present AND
    settled, never a fixed sleep. The shared gate carries the reasoning
    (presence alone is not enough; ``superseding`` insists on a newer blob):
    ``helpers.waiting.wait_rail_blob_settled``."""
    wait_rail_blob_settled(
        client_factory, "fauna.mls.get", path, superseding=superseding, timeout=timeout
    )


def _wait_fauna_thread(app, flavor=None, timeout=45.0, channel_hex=None):
    """Poll ``app``'s snapshot until a FaunaMls thread appears (optionally with a
    specific ``flavor`` and/or bound to ``channel_hex``). Returns the thread
    summary, or None on timeout. Used to detect that device B's login restore
    materialised the conversation — the snippet's exact content is NOT relied on
    (the definitive own-message proof is reading the rendered bubble in the opened
    thread). Pass ``channel_hex`` when the session-scoped identity may have
    accumulated several FaunaMls threads (earlier tests' restored 1:1s ride the
    same replica), so the wait pins THIS test's channel instead of first-match."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        for t in app.conversations.list_threads():
            if (
                t.rail == "FaunaMls"
                and (flavor is None or t.flavor == flavor)
                and (channel_hex is None or t.channel_id_hex == channel_hex)
            ):
                return t
        time.sleep(1.0)
    return None


def _wait_thread_snippet(app, needle, rail="FaunaMls", timeout=25.0):
    """Poll until a thread on ``rail`` carries ``needle`` in its snippet. Returns
    the thread summary or None — used for the local own-echo of a just-sent
    message (where the snippet IS the freshest own line)."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        for t in app.conversations.list_threads():
            if t.rail == rail and needle in (t.snippet or ""):
                return t
        time.sleep(1.0)
    return None


def _wait_message_texts(app, timeout=12.0):
    """Poll the open thread until ≥1 ``dm-message-text`` bubble renders, then
    return every rendered body. Restore + detail render is asynchronous, so a bare
    read can race an empty pane."""
    deadline = time.time() + timeout
    n = 0
    while time.time() < deadline:
        n = app.driver.count("dm-message-text")
        if n >= 1:
            break
        time.sleep(0.5)
    return [app.driver.get_text("dm-message-text", index=i) for i in range(n)]


def _wait_channel_grows(port, peer, channel_hex, baseline, timeout=45.0):
    """Poll the peer's ``channel_fetch`` until the on-channel record count exceeds
    ``baseline``. Returns the observed count (== ``baseline`` on timeout)."""
    deadline = time.time() + timeout
    count = baseline
    while time.time() < deadline:
        count = len(conv_api.channel_fetch(port, peer, channel_hex, after=0))
        if count > baseline:
            return count
        time.sleep(1.0)
    return count


# The six non-web apps: web has its own twin (test_fauna_mls_web_cross_device_sync.py),
# so this witness is MARKED for the apps it can drive rather than gated by
# declared_absence on web — an absence names a behaviour a platform lacks, a mark names
# the apps a test can drive (feature-catalog.md § Cell semantics). ios and android stay
# listed so their skip_unbuilt below keeps recording the build gap.
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.windows
@pytest.mark.tui
@pytest.mark.ios
@pytest.mark.android
@pytest.mark.feature("conversations")
def test_fauna_mls_cross_device_sync(
    real_faunamls_app, nest_instance, test_user, request
):
    alice = real_faunamls_app  # device A
    if not (
        alice.driver.is_linux()
        or alice.driver.is_macos()
        or alice.driver.is_windows()
        or alice.driver.is_tui()
    ):
        skip_unbuilt(
            alice.driver,
            surface="the cross-device MLS-sync GUI proof",
            detail="runs on linux (slice 5 leg 1), macOS + windows (the "
            "shared native FFI-factory leg), and tui (the shared tokio "
            "launcher consumed directly); android is pending on its machine",
            tracked="devices.md",
        )

    port = nest_instance["port"]
    node_url = nest_instance["url"]
    admin_sk = nest_instance["admin"]["signing_key"]

    if alice.driver.is_macos():
        # apple has no client-side "real backend live" signal (launch-time gate,
        # no enable command — see real_faunamls_app). The login replenish
        # publishes alice's key packages once the real session's receive loop is
        # up (AFTER the MLS replica restore, session-owned) — poll that nest-side
        # effect as the readiness gate before driving a real send.
        deadline = time.time() + 30.0
        while time.time() < deadline:
            if conv_api.keypackage_count(
                port, test_user, test_user["actor_id_hex"]
            ) > 0:
                break
            time.sleep(0.5)
        else:
            raise AssertionError(
                "device A (macOS) never published login key packages — the real "
                "ConversationsSession did not come up (is the real_conversations "
                "marker / FAUNA_E2E_REAL_CONVERSATIONS gate active?)"
            )

    def _alice_client():
        # A raw User-class WS-RPC connection as alice — a read-only 'device' that
        # observes her replica plane (`fauna.mls.get`), same shape as
        # tests/api/test_mls_replica_sync.py::_client.
        return WsRpcAdminClient(
            node_url,
            actor_id=test_user["actor_id_bytes"],
            signing_key=bytes(test_user["signing_key"]),
        )

    # ── An API-tier peer publishing a real key package, so alice's bootstrap can
    # fetch one (fake KPs fail to parse; see test_fauna_mls_real_roundtrip). The
    # peer has no MLS engine — it never decrypts, only observes nest-side effects,
    # which is all this test needs of it (the second real engine is device B). ──
    peer = conv_api.reachable_peer(port, admin_sk, test_user["actor_id_hex"])
    assert conv_api.keypackage_count(port, peer, peer["actor_id_hex"]) == 3

    # ── device A: create a 1:1 with the peer and post alice's OWN message. ──
    # Pin THIS test's 1:1 by its own echo, exactly as the web twin does. A bare
    # flavor match is ambiguous: alice is the session-scoped `test_user`, so
    # earlier modules' 1:1s ride her replica and restore into every later
    # launch — first-match then picks someone else's channel and the device-B
    # comparison below fails on two unrelated hexes. (Invisible until 2026-08-03,
    # because the earlier modules' sends were themselves being refused and so
    # created no threads to collide with — see the reach-policy fix in this
    # commit.)
    own_line = "device-A owns this line"
    alice.conversations.real_resolve_send_new(peer["actor_id_hex"], own_line)
    a_thread = _wait_thread_snippet(alice, own_line, timeout=MLS_HANDSHAKE_S)
    assert a_thread is not None, "the 1:1 should surface alice's own echo"
    channel_hex = a_thread.channel_id_hex
    assert channel_hex, "the 1:1 thread should bind a channel after send"

    # ── device A's debounced autosave uploads the sealed replica. Gate on the
    # ACTUAL upload (not a fixed sleep): the history/<ch> slice is what carries
    # alice's own (own-leaf) message plaintext — the thing log replay can't give
    # device B (design §1). ──
    _wait_replica_uploaded(_alice_client, "provider")
    _wait_replica_uploaded(_alice_client, f"history/{channel_hex}")

    # ── device B: alice's SECOND device (same secret, empty mls_state.db). Launch
    # it ON-DEMAND now — after the replica exists — so its login restore finds it.
    device_b, _driver_b = request.getfixturevalue("alice_second_device")

    # ── CRUX: device B's login restore materialised alice's 1:1, and — the thing
    # log replay could NEVER give it (a sender cannot MLS-decrypt its own
    # application messages, design §1) — the OWN message she posted. First confirm
    # the restore surfaced the thread (its snippet content is not relied on), then
    # open it and read the rendered own bubble as the definitive proof. ──
    b_thread = _wait_fauna_thread(
        device_b, flavor="OneToOne", timeout=50.0, channel_hex=channel_hex
    )
    dump = [
        (t.rail, t.flavor, t.snippet)
        for t in device_b.conversations.list_threads()
    ]
    assert b_thread is not None, (
        "device B should restore alice's 1:1 from the replica on login; its "
        f"threads were {dump}"
    )
    assert b_thread.channel_id_hex == channel_hex, (
        "device B should restore the SAME channel device A created"
    )

    device_b.conversations.open_thread_by_rail("FaunaMls")
    texts = _wait_message_texts(device_b)
    assert any(own_line in t for t in texts), (
        "device B should render alice's OWN message bubble, restored from the "
        f"history replica (log replay can't reconstruct an own-leaf message); "
        f"got {texts!r}"
    )

    # ── device B WRITES back to the pre-existing conversation. Because B did not
    # author the restored epoch (device A did), its first send posts a
    # device-owned-epoch takeover self-Update commit (design §3c) THEN the
    # Application envelope — both land on the channel. Observe nest-side via the
    # API peer's channel_fetch (neither device's traffic is peer-decryptable, but
    # the record count is). ──
    before = len(conv_api.channel_fetch(port, peer, channel_hex, after=0))
    # Device A's fold-in baseline, read BEFORE B writes — see the barrier below.
    # Read here and not later: once B's commit is on the channel A may fold it in
    # at any moment, and a baseline taken after that would already include it,
    # leaving the wait hunting a second commit nobody authors.
    a_folded = mls_folded_commits(alice.driver, channel_hex)
    reply = "device-B writes back"
    device_b.conversations.real_send(b_thread.thread_id, reply)
    assert _wait_thread_snippet(device_b, reply, timeout=MLS_HANDSHAKE_S) is not None, (
        "device B's own reply should echo locally in its thread"
    )
    after_b = _wait_channel_grows(port, peer, channel_hex, before)
    assert after_b > before, (
        "device B's takeover commit + Application envelope should land on the "
        f"channel (had {before}, now {after_b}) — B gained write access"
    )

    # ── device A converges: its receive loop (FAUNA_CONV_POLL_SECS=2) processes
    # B's own-leaf takeover — the slice-4c OwnLeafCommit resync — and does NOT
    # wedge. Prove it end-to-end by having device A post a follow-up that lands on
    # the channel: A's application-send tail runs `ensure_takeover`, which can only
    # complete if A resynced onto B's epoch through the gate rebase. (The strict
    # epoch-takeover accounting is tier_1-proven in gate_impl.rs
    # `twin_device_takeover_resyncs_the_other_device`; here we prove the real GUI
    # + poll loop + real nest stay live across the twin-device epoch handoff.)
    #
    # ── The barrier: A must have FOLDED IN B's commit before A sends, or the
    # send is a stale blind-append and the growth assertion below passes without
    # exercising the takeover path at all. This waited out a few of A's 2 s poll
    # cycles until 2026-08-14; the natural causal anchor — wait for A to render
    # B's reply — is not available here and must not be re-attempted: A and B are
    # one actor on one leaf, so A can never MLS-decrypt B's message (it was built
    # and measured RED at the full 90 s budget). A does process B's *commit*, and
    # that fold-in is what this anchors on. ──
    await_folded_commit_after(
        alice.driver,
        channel_hex,
        a_folded,
        budget_s=MLS_COMMIT_FOLD_S,
        what="device B's takeover commit",
    )
    follow = "device-A after B took over"
    alice.conversations.real_send(a_thread.thread_id, follow)
    after_a = _wait_channel_grows(port, peer, channel_hex, after_b)
    assert after_a > after_b, (
        "device A should post a follow-up after device B's takeover — its receive "
        "loop must process B's own-leaf commit without wedging "
        f"(had {after_b}, now {after_a})"
    )


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.macos
@pytest.mark.windows
@pytest.mark.feature("conversation-attachments")
def test_an_attachment_restores_onto_a_second_device(
    real_faunamls_app, nest_instance, test_user, request
):
    """A picture device A sent shows on alice's second device, restored from her
    history: ``docs/goal/ui/conversations.md`` § Attachments → *Retention* — "The
    coordinates ride the ``history/<ch>`` slice …, so a device restored from the
    replica — whose poll never re-walks the records that named its restored
    attachments — fetches each on its first render instead of rendering it declared."

    Device B starts with an EMPTY attachment store and cannot recover the location
    from the log (a sender never walks its own record back), so a painted picture on B
    is the slice's coordinates reaching B's first render, the refill fetching the
    sealed blob, and B opening it. The replica wait is anchored on the slice CHANGING
    after the attachment send — the slice already existed from the opener, and
    launching B on that earlier blob would restore a thread with no picture in it.
    The same-device half of the outcome is
    ``test_conversations_attachment_retention.py::test_an_attachment_this_device_dropped_is_fetched_again``.
    """
    alice = real_faunamls_app
    port = nest_instance["port"]
    peer = conv_api.reachable_peer(port, nest_instance["admin"]["signing_key"], test_user["actor_id_hex"])

    def _alice_client():
        return WsRpcAdminClient(
            nest_instance["url"],
            actor_id=test_user["actor_id_bytes"],
            signing_key=bytes(test_user["signing_key"]),
        )

    opener = f"device-A attaches {int(time.time() * 1000)}"
    alice.conversations.real_resolve_send_new(peer["actor_id_hex"], opener)
    a_thread = _wait_thread_snippet(alice, opener, timeout=MLS_HANDSHAKE_S)
    assert a_thread is not None and a_thread.channel_id_hex, "the 1:1 should bind a channel"
    history = f"history/{a_thread.channel_id_hex}"
    _wait_replica_uploaded(_alice_client, history)
    before = _replica_blob(_alice_client, history)

    driver = alice.driver
    alice.conversations.open_thread_by_id(a_thread.thread_id)
    driver.clear_and_type("dm-text-field", "a picture for my other device")
    driver.set_input_files("attachment-button", str(FIXTURE_IMAGE))
    assert not alice.has_error(), f"staging the attachment failed: {alice.error_text()}"
    driver.click("dm-send-button")
    wait_until(
        lambda: alice.conversations.attachment_image_states() == ["painted"],
        MLS_HANDSHAKE_S,
        diagnose=lambda: f"device A's echo never painted: {alice.conversations.attachment_image_states()}",
    )
    _wait_replica_uploaded(_alice_client, history, superseding=before)

    device_b, _driver_b = request.getfixturevalue("alice_second_device")
    b_thread = _wait_fauna_thread(device_b, timeout=50.0, channel_hex=a_thread.channel_id_hex)
    assert b_thread is not None, (
        "device B should restore the 1:1 from the replica; its threads were "
        f"{[(t.rail, t.snippet) for t in device_b.conversations.list_threads()]}"
    )
    device_b.conversations.open_thread_by_id(b_thread.thread_id)
    wait_until(
        lambda: device_b.conversations.attachment_image_states() == ["painted"],
        MLS_HANDSHAKE_S,
        diagnose=lambda: "device B never painted the picture device A sent: "
        f"{device_b.conversations.attachment_image_states()} error={device_b.error_text()!r}",
    )
