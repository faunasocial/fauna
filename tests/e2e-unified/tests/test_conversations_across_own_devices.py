"""What one of your devices does to a conversation, seen from another of them.

Two outcomes of the ``conversations`` family that need a second device signed
in as the same identity (``alice_second_device``, web's twin page
``alice_second_web_device``: same secret key, distinct device id, empty client
state that restores the MLS replica on login):

* **A message you sent from another of your devices raises no notification**
  (``docs/goal/behavior/message-banners.md`` § Where logic lives: the banner
  key is the unread count, which "never counts the user's own messages, so … a
  message the user sent from another device banners nowhere").
* **A conversation you read on one of your devices shows as read on your
  others** (``docs/goal/behavior/conversation-read-state.md`` § Goal; the read
  raises the channel's read marker in the account store, the peer leg carries
  it, and the other device's seam empties the thread's unread set).

The shared-Rust proofs are ``fauna-conversations``' ``notification.rs`` /
``unread_tracking_tests.rs`` and ``fauna-client-account-runtime``'s
``read_positions_convergence.rs``; these are the same promises through two real
apps over a real nest.

Both arcs start the same way: an API-tier sender (a one-shot ``mls-group-gen``
engine) seats alice in a 1:1 and posts one message, which device A decrypts;
device A's replica reaches the nest; device B launches and restores the channel.
"""

from __future__ import annotations

import uuid

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from common import create_actor_and_register
from helpers import waiting
from helpers.budgets import MLS_COMMIT_FOLD_S, MLS_HANDSHAKE_S, RECEIVE_CYCLE_S
from helpers.waiting import (
    await_account_runtime_assembled,
    wait_rail_blob_settled,
    wait_until,
)
from tests.api import conv_api
from tests.test_conversations_message_banner import _require_banner_leg
from tests.test_conversations_read_state_across_restart import _read_marker, _thread

pytestmark = [pytest.mark.tier_3, pytest.mark.real_conversations]

OPENER = "own-devices witness: from the sender"


def _banner_log(driver) -> str:
    """The app's own banner/toast log lines (convention 6 — the failure carries
    its cause), from a driver that exposes its app log; "" otherwise."""
    read = getattr(driver, "app_log_text", None)
    if read is None:
        return "(this driver exposes no app log)"
    lines = [
        line for line in (read() or "").splitlines()
        if "banner" in line.lower() or "toast" in line.lower()
    ]
    return "\n".join(lines[-20:]) or "(no banner/toast lines)"


def _seed_banner_tracker(app):
    """Make sure device A's banner tracker has seeded before the sender's 1:1
    arrives. The tracker seeds silently on its first non-empty snapshot (the
    login load is not news, `message-banners.md`), so on an app that holds no
    thread yet the 1:1 itself would be the seeding batch and could never banner
    — the control would then fail for the order the tests ran in, not for the
    app. An injected thread of its own seeds it, as the banner journey does."""
    if waiting.require_message_banners(app.driver).get("seeded_with"):
        return
    planted = waiting.message_banners(app.driver)["started"]
    app.conversations.inject_and_resolve_thread(
        rail="FaunaMls",
        sender=f"own-devices-seed-{uuid.uuid4().hex[:8]}@self-nest.test",
        subject=None,
        body="the thread that seeds the banner tracker",
    )
    waiting.await_banner_pass_after(app.driver, planted, what="the seeding thread")
    assert waiting.require_message_banners(app.driver).get("seeded_with"), (
        f"the tracker did not seed on a non-empty snapshot: {waiting.message_banners(app.driver)}"
    )


def _seat_both_devices(app, nest_instance, test_user, request, *, before_delivery=None):
    """Seat alice's device A in a sender's 1:1, deliver one message, let A's
    replica reach the nest, then launch device B and wait for it to restore the
    channel. ``before_delivery(app)`` runs once A's real session is up, before
    the 1:1 reaches it. Returns ``(channel_hex, sender, device_b)``."""
    port = nest_instance["port"]
    me = test_user["actor_id_hex"]
    app.conversations.enable_real_faunamls()
    await_account_runtime_assembled(app.driver)
    if before_delivery is not None:
        before_delivery(app)

    sender = create_actor_and_register(
        port, admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    wait_until(
        lambda: conv_api.keypackage_count(port, test_user, me) > 0,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for device A to publish a key package",
    )
    my_kp = conv_api.keypackage_fetch(port, sender, me)
    assert my_kp is not None, "alice should have a fetchable key package"
    channel_hex, welcome, (opener,) = conv_api.mint_group_welcome_with_messages(
        bytes(sender["signing_key"]), my_kp, [OPENER]
    )
    conv_api.accept_contact(port, test_user, sender["actor_id_hex"])
    inbox_id = conv_api.welcome_deliver(port, sender, me, channel_hex, welcome)
    conv_api.await_welcome_applied(port, test_user, inbox_id, MLS_HANDSHAKE_S)
    conv_api.channel_send(port, sender, channel_hex, opener)
    wait_until(
        lambda: (_thread(app, channel_hex) is not None)
        and _thread(app, channel_hex).message_count >= 1,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "waiting for device A to decrypt the sender's message",
    )

    def _alice_client():
        return WsRpcAdminClient(
            nest_instance["url"],
            actor_id=test_user["actor_id_bytes"],
            signing_key=bytes(test_user["signing_key"]),
        )

    # Device B's login restore needs the replica device A uploads (debounced):
    # gate on the upload itself, never a sleep.
    wait_rail_blob_settled(_alice_client, "fauna.mls.get", "provider")
    wait_rail_blob_settled(_alice_client, "fauna.mls.get", f"history/{channel_hex}")

    # Web's second device is a twin page in its own BrowserContext; every other
    # app launches a second process.
    second_device = (
        "alice_second_web_device" if app.driver.is_web() else "alice_second_device"
    )
    device_b, _driver_b = request.getfixturevalue(second_device)
    wait_until(
        lambda: _thread(device_b, channel_hex) is not None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "device B should restore the 1:1 from the replica on login; "
        f"its threads were {[(t.rail, t.channel_id_hex) for t in device_b.conversations.list_threads()]}",
    )
    return channel_hex, sender, device_b


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.feature("conversations")
def test_a_conversation_read_on_one_device_shows_read_on_the_other(
    logged_in_app, nest_instance, test_user, request
):
    """Device B shows the sender's message unread; device A opens the thread;
    device B's unread count falls to zero with nothing done on B — the read
    travelled as the channel's read marker, not as a gesture on B."""
    app = logged_in_app
    channel_hex, _sender, device_b = _seat_both_devices(
        app, nest_instance, test_user, request
    )

    # The precondition that keeps the witness from being vacuous: B counts the
    # message as unread before A reads it.
    wait_until(
        lambda: (_thread(device_b, channel_hex).unread_count or 0) >= 1,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "device B should show the sender's message unread before "
        f"device A reads it; it reads {_thread(device_b, channel_hex).unread_count!r} "
        f"(marker on B: {_read_marker(device_b, channel_hex)!r})",
    )

    app.conversations.navigate()
    app.conversations.open_thread_by_channel(channel_hex)
    wait_until(
        lambda: _thread(app, channel_hex).unread_count == 0,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "opening the thread on device A should read the message",
    )
    wait_until(
        lambda: (_read_marker(app, channel_hex) or {}).get("found") is True,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "the read never raised the channel's marker in device A's "
        f"account store: {_read_marker(app, channel_hex)!r}",
    )

    wait_until(
        lambda: _thread(device_b, channel_hex).unread_count == 0,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "device A read the conversation, but device B still counts "
        f"{_thread(device_b, channel_hex).unread_count!r} unread (marker on A: "
        f"{_read_marker(app, channel_hex)!r}; on B: {_read_marker(device_b, channel_hex)!r}) "
        "— the read marker never reached B, or B's read-position seam never delivered it",
    )


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.feature("conversations")
def test_a_message_sent_from_another_of_your_devices_raises_no_banner(
    logged_in_app, nest_instance, test_user, request
):
    """Device A banners the sender's message (the positive control: this thread
    and this app do fire); device B then sends into the same conversation; a
    receive cycle of A's that began after B's message was on the channel has
    finished, then a banner tick that began after that — and the thread's
    banner count on A has not moved."""
    app = logged_in_app
    _require_banner_leg(app.driver)
    waiting.require_message_banners(app.driver)
    port = nest_instance["port"]

    channel_hex, sender, device_b = _seat_both_devices(
        app, nest_instance, test_user, request, before_delivery=_seed_banner_tracker
    )
    thread_id = _thread(app, channel_hex).thread_id
    wait_until(
        lambda: thread_id in waiting.fired_banner_threads(app.driver),
        MLS_HANDSHAKE_S,
        diagnose=lambda: "device A should banner the sender's message (the control "
        f"that this thread can banner at all); fired: {waiting.fired_banner_threads(app.driver)}; "
        f"A's unread count for it: {_thread(app, channel_hex).unread_count!r} (the "
        "banner keys on its rise); A's read marker: "
        f"{_read_marker(app, channel_hex)!r}; the app's own banner/toast log lines: "
        f"{_banner_log(app.driver)}",
    )
    banners_before = waiting.fired_banner_threads(app.driver).count(thread_id)
    messages_before = _thread(app, channel_hex).message_count

    # Device B writes into the conversation: its takeover commit, then the
    # message. Both are on the channel once the record count grows by two.
    on_channel = len(conv_api.channel_fetch(port, sender, channel_hex, after=0))
    folded = waiting.mls_folded_commits(app.driver, channel_hex)
    b_thread_id = _thread(device_b, channel_hex).thread_id
    device_b.conversations.real_send(b_thread_id, "own-devices witness: from device B")
    wait_until(
        lambda: len(conv_api.channel_fetch(port, sender, channel_hex, after=0))
        >= on_channel + 2,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "device B's commit and message should land on the channel "
        f"(had {on_channel}, now "
        f"{len(conv_api.channel_fetch(port, sender, channel_hex, after=0))})",
    )

    # Device A has walked past B's message: it folded B's commit, and a whole
    # receive cycle began after the message was on the channel.
    waiting.await_folded_commit_after(
        app.driver, channel_hex, folded, budget_s=MLS_COMMIT_FOLD_S,
        what="device B's takeover commit",
    )
    cycles = waiting.conv_receive_cycles(app.driver)
    assert cycles is not None, "device A does not publish its receive-loop cycles"
    waiting.poke_receive_cycle(app.driver)
    waiting.await_receive_cycle_after(
        app.driver, cycles[0], budget_s=RECEIVE_CYCLE_S, what="device B's message"
    )

    # The banner barrier: a tick that began after that cycle (a quiet app runs
    # none, so make one by opening the thread) has finished; every snapshot the
    # cycle produced was diffed before it.
    planted = waiting.message_banners(app.driver)["started"]
    app.conversations.navigate()
    app.conversations.open_thread_by_channel(channel_hex)
    waiting.await_banner_pass_after(
        app.driver, planted, what="opening the thread device B wrote into"
    )
    fired = waiting.fired_banner_threads(app.driver)
    assert fired.count(thread_id) == banners_before, (
        f"thread {thread_id} raised a banner for a message alice sent from her other "
        f"device (banners for it: {banners_before} before B's send, "
        f"{fired.count(thread_id)} after; A's message count {messages_before} → "
        f"{_thread(app, channel_hex).message_count})"
    )


DRAFTS_GET = "fauna.drafts.get"


@pytest.mark.tui
@pytest.mark.linux
@pytest.mark.web
@pytest.mark.macos
@pytest.mark.ios
@pytest.mark.windows
@pytest.mark.android
@pytest.mark.feature("drafts-survive")
def test_a_draft_from_another_device_never_overwrites_what_you_are_typing(
    logged_in_app, nest_instance, test_user, request
):
    """``docs/goal/behavior/conversation-drafts.md`` § Persistence — "A restore
    FILLS: it never clears a slot, and never overwrites a draft the user is
    composing into"; "a slot the user has not authored into is still adopted,
    which is what carries a draft written on another device onto this one".

    Device A quits. Device B writes two drafts into alice's drafts plane — words
    in the 1:1 and a half-written new message — and quits (the quit flushes
    them). Device A relaunches with its launch restore parked at the nest
    (``fauna.drafts.get`` held), types its own new message into the composer,
    and only then is the restore let through. The restore lands — device A's
    1:1 composer adopts B's words (the barrier: an untouched slot is filled) —
    and the new-message composer still holds what A typed."""
    from helpers.rpc_hold import arm_rpc_hold, release_rpc_hold, wait_for_held_rpc

    app = logged_in_app
    if not app.driver.preserve_state_across_relaunch():
        pytest.skip(
            "driver cannot preserve the client's long-term store across a relaunch, "
            "so device A's relaunch would be a fresh device"
        )
    port = nest_instance["port"]
    channel_hex, _sender, device_b = _seat_both_devices(
        app, nest_instance, test_user, request
    )
    # Each device names the 1:1 by its own local thread id, which need not
    # agree: a per-thread draft rests under the conversation's channel, so it
    # crosses between them whatever each device numbered it.
    b_thread_id = _thread(device_b, channel_hex).thread_id
    in_thread = "device B's words in the 1:1"
    b_new = "device B's half-written new message"
    a_new = "device A's own new message, typed before the restore"

    def drafts_blob():
        with WsRpcAdminClient(
            nest_instance["url"],
            actor_id=test_user["actor_id_bytes"],
            signing_key=bytes(test_user["signing_key"]),
        ) as dev:
            return dev.call(DRAFTS_GET, {"path": "conversations"}).get("blob")

    # Device A leaves first: nothing it holds may be saved over B's drafts.
    app.driver.quit_app()

    # Device B writes both drafts; the plane changing proves B's launch load
    # completed (a save before it is a no-op), and its quit flushes the last edit.
    baseline = drafts_blob()
    bconv = device_b.conversations
    bconv.open_thread_by_id(b_thread_id)
    device_b.driver.clear_and_type("dm-text-field", in_thread)
    bconv.navigate()
    device_b.driver.click("new-conversation-button")
    device_b.driver.wait_for("dm-text-field", timeout=10.0)
    device_b.driver.clear_and_type("dm-text-field", b_new)
    wait_until(
        lambda: drafts_blob() not in (None, baseline),
        MLS_HANDSHAKE_S,
        diagnose=lambda: "device B's drafts never reached alice's drafts plane",
    )
    device_b.driver.quit_app()

    # Device A relaunches with its restore parked at the nest.
    conv = app.conversations
    arm_rpc_hold(port, DRAFTS_GET)
    try:
        app.driver.hard_reload()
        conv.enable_real_faunamls()
        # Opening the page comes first: web builds its conversations manager,
        # and so issues the restore, only when the page opens; the native apps
        # issue it at launch.
        conv.navigate()
        wait_for_held_rpc(port, DRAFTS_GET)
        app.driver.click("new-conversation-button")
        app.driver.wait_for("dm-text-field", timeout=10.0)
        app.driver.clear_and_type("dm-text-field", a_new)
        wait_until(
            lambda: conv.compose_body_text() == a_new,
            MLS_HANDSHAKE_S,
            diagnose=lambda: f"device A's composer reads {conv.compose_body_text()!r}",
        )
    finally:
        release_rpc_hold(port, DRAFTS_GET)

    # The barrier: the restore landed — the 1:1's untouched composer adopts B's words.
    wait_until(
        lambda: _thread(app, channel_hex) is not None,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "device A's relaunch should list the 1:1 again",
    )

    def thread_composer():
        # Re-read after the relaunch: device A may number the 1:1 differently
        # this run than the last.
        conv.open_thread_by_id(_thread(app, channel_hex).thread_id)
        return conv.compose_body_text()

    wait_until(
        lambda: thread_composer() == in_thread,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "device A's restore should fill the 1:1's untouched composer "
        f"with device B's words; it reads {conv.compose_body_text()!r}",
    )

    conv.navigate()
    app.driver.click("new-conversation-button")
    app.driver.wait_for("dm-text-field", timeout=10.0)
    # A deadline poll, not one read: `dm-text-field` is the same id on the 1:1
    # page just left, so the first read can still see that composer (observed on
    # web: the 1:1's words, then A's message a moment later). The restore has
    # already landed (the barrier above), so an overwrite would hold for good and
    # still fail here.
    wait_until(
        lambda: conv.compose_body_text() == a_new,
        MLS_HANDSHAKE_S,
        diagnose=lambda: "device B's saved new message overwrote what device A was "
        f"typing: the composer reads {conv.compose_body_text()!r}, expected {a_new!r}",
    )

    # Discard the new message this journey leaves on alice — the shared session
    # `test_user`. Its drafts plane outlives the test, and the next composer opened
    # on her restores it under its own typing: a tui run of the drafts-survive
    # selection measured `test_conversations_draft_persistence.py` reading
    # "device B's half-written new message" in front of its own body. Cancel is
    # the user's discard (`conversation-drafts.md` § Persistence); asserted, never
    # best-effort, so a no-op shows up here rather than in the next victim.
    before_discard = drafts_blob()
    app.driver.click("new-conversation-cancel")
    wait_until(
        lambda: drafts_blob() not in (None, before_discard),
        MLS_HANDSHAKE_S,
        diagnose=lambda: "cleanup: discarding device A's new message never reached "
        "alice's drafts plane, so it leaks into the next test's composer",
    )
