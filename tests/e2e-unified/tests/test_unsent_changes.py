"""Changes that have not reached the nest are shown where they were made and
listed in one place on the account page; one the nest refused can be sent again
or discarded.

Owners:

* ``docs/goal/architecture/account-offline-mutation.md`` § The offline-mutation
  contract → *The unsent-changes surface*: point 1, the standing list on
  the account page (``unsent-changes-section``, one ``unsent-change-item`` per
  undrained intent with its description and state, retry and a confirmed
  discard on a not-sent row only, the section present and empty afterwards);
  point 2, the mark on the affordance (``feed-like-button``'s ``state`` reads
  ``queued`` while a like waits; the queued own copy of a message carries
  ``dm-message-queued``).
* ``docs/features/offline-aware-controls.md`` outcome 8.

The refusal is made real, never faked: the like is queued with the nest down,
the drain is held (``helpers.outbox_drain``) while the nest comes back and the
author deletes the post through the feed's own delete, and the released drain
then meets the nest's refusal and parks the like. Retrying re-arms it, and the
cause still standing, it parks again; discarding it behind the confirm empties
the list.

tier_3: the real nest, taken down and brought back on the same data dir; every
mutation a UI gesture (convention 8); every wait a deadline poll on painted
state (convention 14). tui first (``testing.md`` § Default app and nest mode);
the other six followed in their trickle-down batches, windows last, so all seven
carry the marker.
"""
from __future__ import annotations

import time

import pytest

from common.nest import start_nest_in_place
from helpers.conversations_restart import fauna_thread
from helpers.fleet import poked_pass
from helpers.nest_outage import hold_nest_down
from helpers.outbox_drain import hold_outbox_drain, release_outbox_drain
from helpers.waiting import wait_until
from i18n.strings import S
from tests.api import conv_api

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.android,
    pytest.mark.windows,
]

#: A post composed online reaching the seat's feed state.
POST_S = 120.0

#: A local outbox write reaching the surface that marks it (the unsent tick
#: re-reads every two seconds).
QUEUE_S = 60.0

#: The returning nest, the released drain, and the refusal parking the like.
CONVERGE_S = 300.0


def _stamp() -> int:
    return int(time.time() * 1000) % 1_000_000


def _rows(app) -> list[tuple[str, str]]:
    """Each ``unsent-change-item``'s (description, status), in list order."""
    n = app.driver.count("unsent-change-item")
    return [
        (
            app.driver.get_text("unsent-change-description", scope=f"unsent-change-item[{i}]"),
            app.driver.get_text("unsent-change-status", scope=f"unsent-change-item[{i}]"),
        )
        for i in range(n)
    ]


def _open_account(app) -> None:
    app.settings.navigate()
    app.settings.navigate_to_account()
    app.driver.wait_for("unsent-changes-section")


def _like_state(app, post_id: str) -> str:
    index = app.feed.card_index_by_id(post_id)
    return (
        app.driver.get_attr("feed-like-button", "state", scope=f"post-card[{index}]") or ""
    ).strip()


@pytest.mark.feature("offline-aware-controls")
def test_unsent_changes_are_listed_and_a_refused_one_is_retried_or_discarded(
    app, request, nest_instance
):
    from conftest import _make_user
    from helpers import enrollment

    user = _make_user(nest_instance)
    enrollment.sign_in(app, request, nest_instance, user)
    enrollment.await_enrollment(app, nest_instance["url"], user)

    text = f"unsent-like {_stamp():06d}"
    app.feed.navigate()
    app.feed.create_post(text=text)
    row = app.feed.wait_for_post_state_by_text(text, timeout=POST_S)
    assert row is not None and row.get("post_id"), (
        f"the online post {text!r} never reached the feed state; "
        f"error-message {app.error_text()!r} — nothing below can be trusted"
    )
    post_id = row["post_id"]

    # The section stands before anything is queued, and says nothing waits.
    _open_account(app)
    wait_until(
        lambda: app.driver.get_text("unsent-changes-section") == S.settings.unsent_changes.none,
        QUEUE_S,
        diagnose=lambda: (
            f"with nothing queued the section reads "
            f"{app.driver.get_text('unsent-changes-section')!r}, not the empty line"
        ),
    )

    # The drain is held from here until the post is gone, so the queued like
    # meets the nest only after the nest has a reason to refuse it.
    hold_outbox_drain(app)
    request.addfinalizer(lambda: release_outbox_drain(app))
    app.feed.navigate()
    hold_nest_down(app, nest_instance, act="the like below")
    try:
        app.feed.like_post(app.feed.card_index_by_id(post_id))
        seen: dict = {}

        def heart_queued() -> bool:
            seen["state"] = _like_state(app, post_id)
            return seen["state"] == "queued"

        wait_until(
            heart_queued, QUEUE_S,
            diagnose=lambda: (
                f"the like tapped with the nest down reads state "
                f"{seen.get('state')!r}, not 'queued' — the heart does not say "
                f"that its like has not reached the nest. error-message "
                f"{app.error_text()!r}"
            ),
        )

        _open_account(app)

        def listed_waiting() -> bool:
            seen["rows"] = _rows(app)
            return seen["rows"] == [
                (S.settings.unsent_changes.kind_like, S.settings.unsent_changes.state_waiting)
            ]

        wait_until(
            listed_waiting, QUEUE_S,
            diagnose=lambda: (
                f"the account page lists {seen.get('rows')!r}, not the one queued "
                "like waiting to send"
            ),
        )
        assert app.driver.is_absent("unsent-change-retry-button"), (
            "retry is offered on a not-sent row only, yet a waiting like shows it"
        )
    finally:
        start_nest_in_place(nest_instance)

    # The nest is back, the drain still held: the author deletes the post, so
    # the like now names a post the nest no longer has.
    app.feed.navigate()
    app.feed.reenter()
    app.feed.delete_post(app.feed.card_index_by_id(post_id, timeout=CONVERGE_S))

    release_outbox_drain(app)
    _open_account(app)
    unsent_prefix = f"{S.settings.unsent_changes.state_unsent} — "

    def listed_unsent() -> bool:
        poked_pass(app.driver, what="the released outbox drain")
        seen["rows"] = _rows(app)
        return (
            len(seen["rows"]) == 1
            and seen["rows"][0][0] == S.settings.unsent_changes.kind_like
            and seen["rows"][0][1].startswith(unsent_prefix)
        )

    wait_until(
        listed_unsent, CONVERGE_S, interval=1.0,
        diagnose=lambda: (
            f"after the drain met the deleted post the list reads "
            f"{seen.get('rows')!r}, not the like as not sent with the nest's "
            f"reason — error-message {app.error_text()!r}"
        ),
    )
    reason = seen["rows"][0][1][len(unsent_prefix):]
    assert reason, "a not-sent row says why, yet the reason is empty"

    # Retry re-arms the like; the post still gone, the next pass parks it again.
    app.driver.click("unsent-change-retry-button", scope="unsent-change-item[0]")
    wait_until(
        listed_unsent, CONVERGE_S, interval=1.0,
        diagnose=lambda: (
            f"after the retry the list reads {seen.get('rows')!r} — a retried "
            "like the nest still refuses must park again as not sent, never "
            "vanish or stay waiting"
        ),
    )

    # Discard asks first, then removes the one row; the section stays.
    app.driver.click("unsent-change-discard-button", scope="unsent-change-item[0]")
    app.driver.wait_for("unsent-change-discard-confirm-button")
    assert app.driver.count("unsent-change-item") == 1, (
        "arming the discard must not remove anything before the confirm"
    )
    app.driver.click("unsent-change-discard-confirm-button", scope="unsent-change-item[0]")
    wait_until(
        lambda: app.driver.count("unsent-change-item") == 0
        and app.driver.get_text("unsent-changes-section") == S.settings.unsent_changes.none,
        QUEUE_S,
        diagnose=lambda: (
            f"after the confirmed discard the list holds {_rows(app)!r} and the "
            f"section reads {app.driver.get_text('unsent-changes-section')!r} — "
            "the row should be gone and the section present and empty"
        ),
    )


@pytest.mark.real_conversations
@pytest.mark.feature("offline-aware-controls")
def test_a_message_sent_offline_is_marked_queued_and_listed(
    logged_in_app, request, nest_instance, test_user
):
    app = logged_in_app
    port = nest_instance["port"]
    admin_sk = nest_instance["admin"]["signing_key"]
    app.conversations.enable_real_faunamls()

    peer = conv_api.reachable_peer(port, admin_sk, test_user["actor_id_hex"])
    pre_existing = {t.channel_id_hex for t in app.conversations.list_threads() if t.channel_id_hex}
    app.conversations.real_resolve_send_new(peer["actor_id_hex"], "online first line")
    channel_hex = fauna_thread(app, exclude=pre_existing).channel_id_hex
    assert channel_hex, "the 1:1 thread should bind a channel after the first send"

    line = f"queued line {_stamp():06d}"
    app.conversations.navigate()
    app.conversations.open_thread_by_channel(channel_hex)
    hold_nest_down(app, nest_instance, act="the send below")
    try:
        app.conversations.send_in_open_thread(line)
        seen: dict = {}

        def marked_queued() -> bool:
            seen["n"] = app.driver.count("dm-message-queued")
            return seen["n"] >= 1

        wait_until(
            marked_queued, QUEUE_S,
            diagnose=lambda: (
                f"the message sent with the nest down shows {seen.get('n')!r} "
                "dm-message-queued lines — its own copy does not say it has not "
                f"been sent. error-message {app.error_text()!r}"
            ),
        )
        assert app.driver.get_text("dm-message-queued") == S.conversations.unified.dm_message_queued
        assert app.driver.count("dm-message-unsent") == 0, (
            "a message still waiting is not a not-sent one"
        )

        _open_account(app)
        expected = S.settings.unsent_changes.kind_message(excerpt=line)

        def listed() -> bool:
            seen["rows"] = _rows(app)
            return (expected, S.settings.unsent_changes.state_waiting) in seen["rows"]

        wait_until(
            listed, QUEUE_S,
            diagnose=lambda: (
                f"the account page lists {seen.get('rows')!r}, not the queued "
                f"message {expected!r} waiting to send"
            ),
        )
    finally:
        start_nest_in_place(nest_instance)
