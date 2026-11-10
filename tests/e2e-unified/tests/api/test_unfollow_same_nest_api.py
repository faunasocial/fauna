"""tier_3, a real nest binary: the unfollow on one nest.

``docs/goal/behavior/subscriptions.md`` § The free followers tier — follow and
unfollow is the owner: an unfollow is the subscriber's leave of the free
``followers`` tier on the one unsubscribe door, and a leave ends the tier it
names and every tier held above it (rule 1), so an unfollow ends a paid
subscription too, while a cancel of a paid tier keeps the follow. The author's
client commits each leave by the removal rotation (rule 3).

Both client halves run through the shared Rust every app calls, app-less:
the subscriber's follow and leave are ``fauna_ffi.harness_follow`` (the local
arm of ``SubscriptionsClient::subscribe_across_nests`` /
``unsubscribe_across_nests``), and the author's tier editor and pump are
``harness_author_create_tier`` / ``harness_author_reconcile``. A designated
(bought-post) tier surviving every leave is pinned in the nest's unit tests
(``subscription_handlers::tests::an_unfollow_ends_the_follow_and_every_paid_tier_but_not_the_purchase``).

**Convention 14:** every wait is a deadline poll over ``status.get`` and the
author's roster read.
"""

from __future__ import annotations

import time

import pytest

import fauna_ffi
from actions.api_actor import ApiActor
from common.auth import create_actor_and_register
from helpers.federated_follow import CROSSING_BUDGET_S, FOLLOWERS_TIER

pytestmark = pytest.mark.tier_3


def _actor(nest: dict) -> tuple[dict, ApiActor]:
    user = create_actor_and_register(
        nest["port"], base_url=nest["url"], admin_signing_key=nest["admin"]["signing_key"]
    )
    return user, ApiActor(nest["url"], user["token"], user["actor_id_hex"], bytes(user["signing_key"]))


def _pump_until(nest: dict, author: dict, what: str, read, done):
    """Run the author's pump, then poll ``read`` until ``done``."""
    deadline = time.monotonic() + CROSSING_BUDGET_S
    last = None
    while time.monotonic() < deadline:
        fauna_ffi.harness_author_reconcile(nest["url"], bytes(author["signing_key"]))
        inner = time.monotonic() + 5
        while time.monotonic() < min(inner, deadline):
            last = read()
            if done(last):
                return last
            time.sleep(0.25)
    raise AssertionError(f"timed out waiting for {what}; last read: {last!r}")


def _rosters(author_api: ApiActor, subscriber: dict, tiers: tuple[str, ...]) -> set[str]:
    """The tiers whose roster names ``subscriber``."""
    me = subscriber["actor_id_hex"]
    return {
        t
        for t in tiers
        if any(bytes(e["subscriber_id"]).hex() == me for e in author_api.subscription_subscribers_list(t))
    }


def test_an_unfollow_ends_the_follow_and_a_paid_tier_while_a_paid_cancel_keeps_the_follow(nest_instance):
    """A reader follows an author and unfollows: the follow ends. A second
    reader follows and takes the author's paid tier, then cancels the paid
    tier alone: the follow stays. The second reader then unfollows: the paid
    tier goes with the follow.

    Flow: ``harness_follow`` → ``fauna.subscriptions.subscribe`` → the author's
    pump approves (the followers tier's key healed into custody) → leave:
    ``harness_follow(leave=True, tier=…)`` → ``fauna.subscriptions.unsubscribe``
    with ``tier`` → ``unsubscribe_core`` queues one leave per tier at or above
    the floor, designated tiers excluded → the pump's ``commit_unsubscribe``
    rotates each tier past the leaver → ``subscribers.remove`` → the roster
    and ``status.get`` no longer name them.
    """
    nest = nest_instance
    author, author_api = _actor(nest)
    author_id = bytes(author["actor_id_bytes"])
    tiers = (FOLLOWERS_TIER, "gold")
    created = fauna_ffi.harness_author_create_tier(
        nest["url"], bytes(author["signing_key"]), "gold", 1, auto_approve=True
    )
    assert created.get("created") is True, created

    def follow(reader: dict, tier: str) -> None:
        fauna_ffi.harness_follow(nest["url"], bytes(reader["signing_key"]), author_id, tier, None)

    def leave(reader: dict, tier: str) -> None:
        reply = fauna_ffi.harness_follow(
            nest["url"], bytes(reader["signing_key"]), author_id, tier, None, leave=True
        )
        assert reply.get("outcome") == "queued", f"a leave is queued for the author's client: {reply!r}"

    def holds(reader: dict, want: set[str], what: str) -> None:
        _pump_until(
            nest,
            author,
            what,
            lambda: _rosters(author_api, reader, tiers),
            lambda held: held == want,
        )

    # ── A follower's unfollow ends the follow ─────────────────────────────
    first, first_api = _actor(nest)
    follow(first, FOLLOWERS_TIER)
    holds(first, {FOLLOWERS_TIER}, "the follow approved")
    assert first_api.subscription_status(author_id).get("tier") == FOLLOWERS_TIER
    leave(first, FOLLOWERS_TIER)
    holds(first, set(), "the unfollow committed")
    status = first_api.subscription_status(author_id)
    assert status.get("tier") is None and not status.get("follow_pending"), status

    # ── A paid cancel keeps the follow ────────────────────────────────────
    second, second_api = _actor(nest)
    follow(second, FOLLOWERS_TIER)
    follow(second, "gold")
    holds(second, {FOLLOWERS_TIER, "gold"}, "the follow and the paid tier approved")
    leave(second, "gold")
    holds(second, {FOLLOWERS_TIER}, "the paid cancel committed, the follow kept")
    assert second_api.subscription_status(author_id).get("tier") == FOLLOWERS_TIER

    # ── An unfollow of a paid subscriber ends the paid tier too ───────────
    follow(second, "gold")
    holds(second, {FOLLOWERS_TIER, "gold"}, "the paid tier approved again")
    leave(second, FOLLOWERS_TIER)
    holds(second, set(), "the unfollow committed for both tiers")
    status = second_api.subscription_status(author_id)
    assert status.get("tier") is None and not status.get("follow_pending"), status
