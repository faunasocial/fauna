"""tier_3 e2e: a like or a message queued as the retired identity lands once,
as the successor.

Owner: ``docs/goal/behavior/succession-aftermath.md`` § Re-key scope →
*Adjudicating what the aftermath carries across*, the device-local outbox row.
On a device that attests the predecessor, the successor's first store-ready
moves the predecessor's undrained intents into its own outbox verbatim, and the
nest's replay cache moves with them, so a carried replay lands exactly once.
Headless pins cover each half: ``fauna_account_plane::outbox_carry``'s tests,
``fauna_client_accounts``'s
``a_retired_identitys_removal_refuses_while_its_outbox_carry_is_owed`` and the
nest's ``rpc_idempotency`` move test. This journey is the user's view of all
of them together. A like given while offline survives the user taking their
account back, and it is counted once.

**The drain hold.** A queued like drains in the first pump pass that reaches
the nest, and the identity ceremony needs the nest up. Without a hold the like
would drain as the predecessor before the ceremony could run, and nothing would
be left to carry. ``helpers/outbox_drain.py`` holds the generic drain and the
app's MLS and identity drains alike. The hold
is ``fauna_account_plane::outbox``'s ``outbox_drain_held``, compiled into debug
and ``e2e-agent`` builds only, convention 15: the
``FAUNA_E2E_OUTBOX_DRAIN_HOLD_FILE`` file on tui, linux, macos and ios (whose
pumping process inherits the launch's environment), the in-process flag set through the e2e agent's
``outbox_drain_hold`` on web and android (whose pump runs in the process the
test drives). The hold leaves the carry itself
running, so the carry happens at the successor's store-ready exactly as in
production. Lifting the hold is the only thing that lets the carried intent
drain.

**What proves which identity sent it.** The count stays 0 across the ceremony,
read after a pump pass with the nest up and the hold on, so the predecessor
never sent it. The ceremony revokes every predecessor session, so after it the
only seat that can still send the like is the successor's. A count of exactly
1 after the release is therefore the carried intent landing once, as the
successor.

Not covered here: the unsent-changes surface (``settings.md``'s
``unsent-changes-section``) is proposed under rule A and not built, so there is
no list to watch empty.

**The MLS variant** (a message queued to a conversation the sweep moves) adds
the item-4 hold: the carry moves an MLS row only once the successor's engine
holds its group, which the ceremony's own sweep arranges on this device. A held
app drain ends its loop (it never keeps a retired identity's conversations
engine alive), so its release is the file's deletion plus a ring of the loop
(``ring_app_outbox_drain``).

tier_3: a real ``fauna-nest`` taken down and brought back on the same data
dir, the app under test and its sync agent. Every mutation is a UI gesture
(convention 8), and every wait is a deadline poll on painted state
(convention 14).
"""
from __future__ import annotations

import time

import pytest

from common.nest import start_nest_in_place
from helpers.conversations_restart import fauna_thread
from helpers.fleet import poked_pass
from helpers.nest_outage import hold_nest_down
from helpers.outbox_drain import (
    hold_outbox_drain,
    release_outbox_drain,
    ring_app_outbox_drain,
)
from helpers.waiting import wait_until
from i18n.strings import S
from tests.api import conv_api
from tests.test_identity_succession_carryover import _succeed

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.android,
    pytest.mark.macos,
    pytest.mark.ios,
]

#: A post composed online reaching the seat's feed state.
POST_S = 120.0

#: The offline tap lighting the toggle (a local outbox write).
QUEUE_S = 60.0

#: The seat reconnecting once the nest is back.
RECONNECT_S = 120.0

#: The released drain reaching the nest and the feed re-reading the count.
CONVERGE_S = 300.0


def _stamp() -> int:
    return int(time.time() * 1000) % 1_000_000


def _like_count(app, post_id: str, want: int) -> int | None:
    """Re-enter the feed and poll briefly for ``want``; the last count read."""
    app.feed.reenter()
    return app.feed.wait_for_interaction_count_by_id(post_id, "like", want, timeout=5.0)


def _like_count_now(app, post_id: str) -> int | None:
    """Re-enter the feed and read the count once (a read that must NOT move,
    taken behind a pass barrier, so a poll for the value would prove nothing).

    The row's PRESENCE is waited for first: after web's ceremony the page is a
    fresh document whose feed is still loading, and a single read there finds
    no row at all — absence, not a count (convention 14: wait for the state to
    exist, then read the value that must not move)."""
    app.feed.reenter()
    seen: dict = {}

    def present() -> bool:
        seen["row"] = app.feed.post_state_by_id(post_id)
        return seen["row"] is not None

    wait_until(
        present, POST_S,
        diagnose=lambda: (
            f"post {post_id} never reached the re-entered feed's state; "
            f"error-message {app.error_text()!r}. The count cannot be read"
        ),
    )
    return seen["row"].get("like_count")


@pytest.mark.feature("take-your-account-back")
def test_a_like_queued_before_the_recovery_lands_once_as_the_successor(
    succeedable_app, nest_instance
):
    app, _user = succeedable_app

    # ── An online post to like ──────────────────────────────────────────────
    text = f"carry-like {_stamp():06d}"
    app.feed.navigate()
    app.feed.create_post(text=text)
    row = app.feed.wait_for_post_state_by_text(text, timeout=POST_S)
    assert row is not None and row.get("post_id"), (
        f"the online post {text!r} never reached the feed state; "
        f"error-message {app.error_text()!r}. Nothing below can be trusted"
    )
    post_id = row["post_id"]
    assert row.get("like_count") == 0 and not row.get("viewer_liked"), (
        f"a fresh post starts un-liked at 0; got {row!r}"
    )

    hold_outbox_drain(app)
    try:
        # ── The like, given with no nest: queued as the predecessor ─────────
        hold_nest_down(app, nest_instance, act="the like below")
        try:
            app.feed.like_post(app.feed.card_index_by_id(post_id))
            seen: dict = {}

            def toggle_lit() -> bool:
                seen["row"] = app.feed.post_state_by_id(post_id)
                return bool(seen["row"] and seen["row"].get("viewer_liked"))

            wait_until(
                toggle_lit, QUEUE_S,
                diagnose=lambda: (
                    f"the like tapped with the nest down never lit its toggle: "
                    f"row {seen.get('row')!r}, error-message {app.error_text()!r}. "
                    "The tap failed instead of being queued in the outbox, so "
                    "there is nothing to carry"
                ),
            )
        finally:
            start_nest_in_place(nest_instance)

        # ── The nest is back and the drain is held: still queued ────────────
        # A pump pass begun with the seat connected, and observed finished, is
        # the moment the like would otherwise drain as the predecessor. The
        # barrier is the pass, not a clock (convention 14); the read after it
        # is one read. Reading 0 there shows the hold held, which is what makes
        # everything below a carry.
        status: dict = {}

        def connected() -> bool:
            if not app.is_visible("connection-status"):
                return False
            status["text"] = app.driver.get_text("connection-status")
            return status["text"] == S.common.connected

        wait_until(
            connected, RECONNECT_S,
            diagnose=lambda: (
                f"connection-status reads {status.get('text')!r} with the nest "
                "back. The seat never reconnected, so no pass below reaches it"
            ),
        )
        poked_pass(app.driver, what="the held drain's pass with the nest back")
        before = _like_count_now(app, post_id)
        assert before == 0, (
            f"with the nest back and the drain held the like count reads "
            f"{before!r}, not 0. The hold did not hold and the like drained as "
            "the predecessor, which leaves nothing to carry"
        )

        # ── The account taken back ──────────────────────────────────────────
        old_actor, new_actor = _succeed(app)
        poked_pass(app.driver, what="the successor's held pass after the carry")
        held = _like_count_now(app, post_id)
        assert held == 0, (
            f"the like count reads {held!r} with the drain still held after the "
            f"ceremony ({old_actor[:12]}… → {new_actor[:12]}…). It moved without "
            "the drain, so it was not this device's carried intent that sent it"
        )
    finally:
        release_outbox_drain(app)

    # ── The hold lifted: the carried like lands, once ───────────────────────
    counts: dict = {}

    def landed_once() -> bool:
        poked_pass(app.driver, what="the successor's drain of the carried like")
        counts["like"] = _like_count(app, post_id, 1)
        return counts["like"] == 1

    wait_until(
        landed_once, CONVERGE_S, interval=1.0,
        diagnose=lambda: (
            f"after the ceremony and the released hold the like count reads "
            f"{counts.get('like')!r}, not 1. The like queued as "
            f"{old_actor[:12]}… was not carried into {new_actor[:12]}…'s outbox, "
            f"or was carried and never drained. Row "
            f"{app.feed.post_state_by_id(post_id)!r}, error-message "
            f"{app.error_text()!r}"
        ),
    )

    # Exactly once: a further pass (a replay of the carried intent, if the
    # carry had duplicated it) must not move the count past 1.
    poked_pass(app.driver, what="a later pass after the carried like landed")
    again = _like_count_now(app, post_id)
    assert again == 1, (
        f"the like count reads {again!r} after a further pass. The carried like "
        "landed more than once, so the replay key did not collapse it"
    )
    row = app.feed.post_state_by_id(post_id)
    assert row and row.get("viewer_liked"), (
        f"the successor's feed shows the post un-liked after the carry: {row!r}"
    )


#: The returning nest, the successor's restore and its MLS drain.
MLS_CONVERGE_S = 300.0


def _rendered_texts(app) -> list[str]:
    n = app.driver.count("dm-message-text")
    return [app.driver.get_text("dm-message-text", index=i) for i in range(n)]


@pytest.mark.real_conversations
@pytest.mark.feature("take-your-account-back")
def test_a_message_queued_before_the_recovery_is_delivered_once_after_it(
    succeedable_app, nest_instance
):
    """The MLS half of the carry (``succession-aftermath.md``, the device-local
    outbox row, item 4): a message composed with no nest, as the predecessor,
    is carried once the successor's engine holds its conversation's group,
    and the peer's channel then holds it exactly once.

    The witness is the peer's read of the nest's channel log, counted in
    application envelopes (``conv_api.application_count``). The sweep adds the
    successor and has it tell the group, which is an application envelope too,
    so the baseline for "delivered" is read after the sweep reports every
    group re-pointed, with the drain still held."""
    app, user = succeedable_app
    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]
    app.conversations.enable_real_faunamls()

    # ── An online 1:1: queuing needs a bound group ──────────────────────────
    peer = conv_api.reachable_peer(port, admin_sk, user["actor_id_hex"])
    pre_existing = {
        t.channel_id_hex for t in app.conversations.list_threads() if t.channel_id_hex
    }
    app.conversations.real_resolve_send_new(peer["actor_id_hex"], "before the recovery")
    thread = fauna_thread(app, exclude=pre_existing)
    channel_hex = thread.channel_id_hex
    assert channel_hex, "the 1:1 thread should bind a channel after the first send"
    online = conv_api.application_count(port, peer, channel_hex)
    assert online >= 1, f"the online send never reached the channel log ({online})"

    line = f"carried across the recovery {_stamp():06d}"
    hold_outbox_drain(app)
    try:
        # ── The message, sent with no nest: queued as the predecessor ───────
        app.conversations.navigate()
        app.conversations.open_thread_by_channel(channel_hex)
        hold_nest_down(app, nest_instance, act="the send below")
        try:
            app.conversations.send_in_open_thread(line)
            seen: dict = {}

            def own_copy_shown() -> bool:
                seen["texts"] = _rendered_texts(app)
                return any(line in t for t in seen["texts"])

            wait_until(
                own_copy_shown, QUEUE_S,
                diagnose=lambda: (
                    f"the message sent with the nest down never showed in the "
                    f"thread: bubbles {seen.get('texts')!r}, error-message "
                    f"{app.error_text()!r}. The send failed instead of being "
                    "queued, so there is nothing to carry"
                ),
            )
        finally:
            start_nest_in_place(nest_instance)

        # ── The nest is back and the drain is held: still queued ────────────
        status: dict = {}

        def connected() -> bool:
            if not app.is_visible("connection-status"):
                return False
            status["text"] = app.driver.get_text("connection-status")
            return status["text"] == S.common.connected

        wait_until(
            connected, RECONNECT_S,
            diagnose=lambda: (
                f"connection-status reads {status.get('text')!r} with the nest "
                "back. The seat never reconnected, so no drain below reaches it"
            ),
        )
        # The reconnect edge ran the app drain; ringing it again and taking a
        # pump pass makes the read below one taken after a held drain that
        # had every chance to send (convention 14: a barrier, not a clock).
        ring_app_outbox_drain(app)
        poked_pass(app.driver, what="the held drain's pass with the nest back")
        before = conv_api.application_count(port, peer, channel_hex)
        assert before == online, (
            f"with the nest back and the drain held the channel holds {before} "
            f"application messages, not {online}. The hold did not hold and the "
            "message went out as the predecessor, which leaves nothing to carry"
        )

        # ── The account taken back; the sweep re-points the group ───────────
        old_actor, new_actor = _succeed(app)
        sweep: dict = {}

        def swept() -> bool:
            sweep["report"] = app.driver.get_state("data.succession_sweep")
            report = sweep["report"] or {}
            return (
                report.get("status") == "ran"
                and report.get("groups", 0) >= 1
                and report.get("old_leaf_removed_everywhere") is True
            )

        wait_until(
            swept, MLS_CONVERGE_S,
            diagnose=lambda: (
                f"the ceremony ({old_actor[:12]}… → {new_actor[:12]}…) never "
                f"reported its group sweep finished: {sweep.get('report')!r}. "
                "Without the successor in the group the carry waits for good"
            ),
        )
        ring_app_outbox_drain(app)
        poked_pass(app.driver, what="the successor's held drain after the sweep")
        baseline = conv_api.application_count(port, peer, channel_hex)
        # The sweep's in-group statement is the only application envelope the
        # ceremony may add; the held message must not be among them.
        assert online <= baseline <= online + 1, (
            f"the channel holds {baseline} application messages after the "
            f"ceremony with the drain held (before it: {online}). More than the "
            "sweep's statement went out, so the held message was sent"
        )
    finally:
        release_outbox_drain(app)

    # ── The hold lifted: the carried message is delivered, once ────────────
    counts: dict = {}

    def delivered() -> bool:
        ring_app_outbox_drain(app)
        counts["n"] = conv_api.application_count(port, peer, channel_hex)
        return counts["n"] >= baseline + 1

    wait_until(
        delivered, MLS_CONVERGE_S, interval=2.0,
        diagnose=lambda: (
            f"after the ceremony and the released hold the channel holds "
            f"{counts.get('n')!r} application messages, not {baseline + 1}. The "
            f"message queued as {old_actor[:12]}… was not carried into "
            f"{new_actor[:12]}…'s outbox, or was carried and never drained. "
            f"error-message {app.error_text()!r}"
        ),
    )
    ring_app_outbox_drain(app)
    poked_pass(app.driver, what="a later pass after the carried message landed")
    again = conv_api.application_count(port, peer, channel_hex)
    assert again == baseline + 1, (
        f"the channel holds {again} application messages after a further drain, "
        f"not {baseline + 1}. The carried message went out more than once, so "
        "its intent id did not collapse the replay"
    )
