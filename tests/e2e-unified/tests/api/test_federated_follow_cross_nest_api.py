"""tier_3, real nest binaries: what the nests do with a follow ACROSS them.

``docs/goal/behavior/federated-follow.md`` is the owner. A reader on one nest
follows an author homed on another; the author's home pushes the author's
posts to the reader's nest, which lists each to its own follower alone. The
journey that drives the follow through an app is
``tests/test_federated_follow_cross_nest.py``; this module observes the
``[nest]`` rules around it from outside, with no app in it:

* § Decision 5 → *Unsubscribe and removal* — an author's removal of a
  follower, and a follower's own unfollow, each reach the follower's nest and
  stop delivery (``subscriptions-and-tiers`` outcome 25).
* § Decision 5 → *Succession* — a followed author's succession keeps the follow
  and its posts; a follower's succession moves them on the author's roster
  (outcome 26).
* § Decision 3 → *custody is per home, listing per reader* — one post delivered
  by two homes lists once to each home's followers, from their own home; the
  real home's tombstone removes it for both, and a replay does not restore it
  (outcome 27).

The client halves — the subscriber's signed ``FollowIntent`` and the author's
pump that approves a follow by minting its ``KeyBlob`` — run through the
shared Rust every app calls (``fauna_ffi.harness_follow`` /
``harness_author_reconcile`` / ``harness_author_remove_subscriber``), so the
nests are reached the way an app reaches them. These are the outside-observed
twins of ``conformance_cross_nest_conversations_client.rs``'s in-process pins
(``a_followed_authors_posts_reach_the_followers_nest_and_only_the_follower``,
``a_followed_authors_succession_keeps_the_follow_and_its_posts``,
``a_followers_succession_re_points_the_authors_roster``,
``a_post_delivered_by_two_homes_lists_to_each_homes_followers_from_their_own_home``).

**Topology:** the handled, floor-TLS nests the app journey uses —
``cross_nest_foreign`` is the author's home, ``cross_nest_foreign_ephemeral``
the followers' nest; the two-home test adds ``report_origin_nest`` as the
second home.

**Convention 14:** every wait is a deadline poll over projected state (a
feed read, a ``status.get``, a roster read). An absence is asserted only after
a barrier: a post that must NOT reach one reader is checked once a later post,
or the same post, has reached another reader on the same nest.
"""

from __future__ import annotations

import secrets
import time

import pytest
from nacl.signing import SigningKey

import fauna_ffi
from common.auth import register_user
from conftest import provision_foreign_handled_actor
from helpers.federated_follow import CROSSING_BUDGET_S, FOLLOWERS_TIER, call
from helpers.federated_follow import hex_of as _hex
from helpers.federated_follow import listed_to as _listed
from helpers.federated_follow import new_reader as _follower
from helpers.federated_follow import peer_of as _peer
from helpers.federated_follow import post_as as _post
from helpers.federated_follow import pump_until as _pump_until
from helpers.federated_follow import seed_of as _seed
from helpers.federated_follow import status_of as _status
from helpers.federated_follow import wait_listed as _wait_listed
from helpers.room_seats import wait_for
from helpers.succession import register_recovery_kit, succeed_identity
from tests.api.bare import sign_and_encode_post, sign_and_encode_tombstone

pytestmark = pytest.mark.tier_3


# ── helpers ────────────────────────────────────────────────────────────────


def _actor_from_seed(seed: bytes) -> dict:
    sk = SigningKey(seed)
    return {"signing_key": sk, "actor_id_bytes": bytes(sk.verify_key)}


def _follow(away: dict, follower: dict, home: dict, author: dict) -> None:
    """``follower`` (on ``away``) follows ``author`` at ``home``, and the author's
    pump approves it: returns once ``away`` holds the follow as approved."""
    fauna_ffi.harness_follow(
        away["url"], _seed(follower), bytes(author["actor_id_bytes"]), FOLLOWERS_TIER, _peer(home)
    )
    _pump_until(
        home,
        author,
        "the follow approved at the home and the approval crossing back",
        lambda: _status(away, follower, _hex(author)),
        lambda s: s.get("tier") == FOLLOWERS_TIER and not s.get("follow_pending"),
    )


def _roster(home: dict, author: dict) -> set[str]:
    reply = call(home, author, "fauna.subscriptions.subscribers.list", {"tier_name": FOLLOWERS_TIER})
    return {bytes(e["subscriber_id"]).hex() for e in reply.get("subscribers", [])}


def _succeed(nest: dict, actor: dict) -> dict:
    """Register a recovery kit for ``actor`` on its own nest and take the
    account to a fresh key; returns the successor's actor dict."""
    old_seed = _seed(actor)
    successor_seed = secrets.token_bytes(32)
    kit = register_recovery_kit(nest["url"], actor_id_hex=_hex(actor), identity_seed_hex=old_seed.hex())
    successor_hex = succeed_identity(
        nest["url"],
        old_actor_id_hex=_hex(actor),
        recovery_secret_hex=kit,
        successor_seed_hex=successor_seed.hex(),
        old_seed_hex=old_seed.hex(),
    )
    successor = _actor_from_seed(successor_seed)
    assert _hex(successor) == successor_hex
    return successor


# ── outcome 25: an author's removal, and a follower's unfollow, reach the
#    follower's nest and stop delivery


@pytest.mark.feature("subscriptions-and-tiers")
def test_an_authors_removal_of_a_follower_on_another_nest_reaches_the_followers_nest(
    cross_nest_foreign, cross_nest_foreign_ephemeral
):
    """Three readers on one nest follow an author on another. The author
    removes one: that reader's own nest marks the follow ended, and the
    author's next post reaches the reader still following and not the one
    removed. Then another reader unfollows: the same, by the reader's own act.

    Flow: ``harness_follow`` → away carries the signed intent to home → the
    author's pump approves → ``follow.state`` approved crosses back → a post
    delivered through home's outbox lists to both. Removal:
    ``remove_subscriber`` → ``fauna.subscriptions.subscribers.remove`` →
    ``follow_state removed`` → away's ``remote_follows`` row ends
    (``status.get`` holds no tier) → the next post lists to the remaining
    follower only. Unfollow (``subscriptions.md`` § The free followers tier —
    follow and unfollow): ``harness_follow(leave=True, tier=followers)`` →
    away marks its rows and relays the signed intent → the home's
    ``unsubscribe_core`` takes ``followers`` as the floor and queues the leave
    → the author's pump commits it by the removal rotation → ``follow_state
    removed`` → away's ``status.get`` holds no tier → the next post lists to
    the remaining follower only.
    """
    home, away = cross_nest_foreign, cross_nest_foreign_ephemeral
    author = provision_foreign_handled_actor(home)
    author_hex = _hex(author)
    removed, witness, leaver = away["actor"], _follower(away), _follower(away)

    _follow(away, removed, home, author)
    _follow(away, witness, home, author)
    _follow(away, leaver, home, author)

    first = _post(home, author, f"to both of you {secrets.token_hex(4)}")
    _wait_listed(away, removed, author_hex, first, "the first post reaching the reader to be removed")
    _wait_listed(away, witness, author_hex, first, "the first post reaching the other reader")

    fauna_ffi.harness_author_remove_subscriber(home["url"], _seed(author), FOLLOWERS_TIER, _hex(removed))
    roster = _roster(home, author)
    assert _hex(removed) not in roster and _hex(witness) in roster, roster
    ended = wait_for(
        "the author's removal reaching the follower's own nest",
        lambda: _status(away, removed, author_hex),
        lambda s: s.get("tier") is None and not s.get("follow_pending"),
        CROSSING_BUDGET_S,
    )
    assert ended.get("tier") is None, ended

    second = _post(home, author, f"after the removal {secrets.token_hex(4)}")
    _wait_listed(away, witness, author_hex, second, "the next post reaching the reader still following")
    assert second not in _listed(away, removed, author_hex), (
        "a post the author made after removing a follower still lists to them"
    )

    # ── The follower's own unfollow ───────────────────────────────────────
    left = fauna_ffi.harness_follow(
        away["url"],
        _seed(leaver),
        bytes(author["actor_id_bytes"]),
        FOLLOWERS_TIER,
        _peer(home),
        leave=True,
    )
    assert "err" not in left, f"the unfollow is answered, never refused: {left!r}"
    _pump_until(
        home,
        author,
        "the unfollow committed at the home and its end crossing back",
        lambda: (_status(away, leaver, author_hex), _roster(home, author)),
        lambda sr: sr[0].get("tier") is None
        and not sr[0].get("follow_pending")
        and _hex(leaver) not in sr[1],
    )
    assert _hex(witness) in _roster(home, author), "the unfollow ends only the leaver's follow"

    third = _post(home, author, f"after the unfollow {secrets.token_hex(4)}")
    _wait_listed(away, witness, author_hex, third, "the next post reaching the reader still following")
    assert third not in _listed(away, leaver, author_hex), (
        "a post the author made after a follower unfollowed still lists to them"
    )



@pytest.mark.feature("subscriptions-and-tiers")
def test_an_unfollow_of_a_follow_still_pending_at_the_home_withdraws_it(
    cross_nest_foreign, cross_nest_foreign_ephemeral
):
    """A follow asked across nests and not yet approved reads as following
    (``status.get``'s ``follow_pending``); the unfollow in that window takes it
    back (``subscriptions.md`` § The free followers tier — follow and unfollow,
    rule 4).

    Flow: ``harness_follow`` → away records a ``pending`` row and relays the
    signed intent → the home queues the subscribe request (no pump runs) →
    ``harness_follow(leave=True, tier=followers)`` → away relays the leave →
    the home's ``unsubscribe_core`` withdraws the pending request (``Removed``,
    never ``not_subscribed``) → away marks its pending row ``removed`` →
    away's ``status.get`` holds no tier and no ``follow_pending``, and the
    author's ``requests.list`` holds nothing from the reader.
    """
    home, away = cross_nest_foreign, cross_nest_foreign_ephemeral
    author = provision_foreign_handled_actor(home)
    author_hex = _hex(author)
    reader = _follower(away)

    asked = fauna_ffi.harness_follow(
        away["url"], _seed(reader), bytes(author["actor_id_bytes"]), FOLLOWERS_TIER, _peer(home)
    )
    assert "err" not in asked, f"the follow is answered: {asked!r}"
    pending = _status(away, reader, author_hex)
    assert pending.get("follow_pending") and pending.get("tier") is None, pending

    left = fauna_ffi.harness_follow(
        away["url"],
        _seed(reader),
        bytes(author["actor_id_bytes"]),
        FOLLOWERS_TIER,
        _peer(home),
        leave=True,
    )
    assert "err" not in left, f"a pending follow is withdrawn, never refused: {left!r}"
    after = _status(away, reader, author_hex)
    assert after.get("tier") is None and not after.get("follow_pending"), after
    requests = call(home, author, "fauna.subscriptions.requests.list", {}).get("requests", [])
    assert not [
        r for r in requests if bytes(r["subscriber_id"]).hex() == _hex(reader)
    ], f"the withdrawn follow leaves no request at the home: {requests!r}"

# ── outcome 26: a succession keeps the follow, on either side ──────────────


@pytest.mark.feature("subscriptions-and-tiers")
def test_a_followed_authors_succession_keeps_the_follow_and_a_followers_moves_them_on_the_roster(
    cross_nest_foreign, cross_nest_foreign_ephemeral
):
    """The author takes their account to a new key: the reader's nest still
    holds the follow (now of the successor), the post delivered before still
    lists to the reader, and the successor's next post reaches them. Then the
    reader takes their own account to a new key: the author's roster names
    the reader's successor, and the author's next post reaches the
    successor's feed.

    Flow (author): ``fauna.recovery.succession.submit`` on home → home's
    succession push reaches the follower's nest (``succession_push_targets``,
    the roster's homes) → ``record_peer_succession`` re-points
    ``remote_follows.author_id`` → ``status.get`` of X′ approved. Flow
    (follower): the submit on away re-points ``remote_follows.subscriber_id``
    and pushes to the follow's home → home's ``record_peer_succession`` moves
    the homed ``subscribers`` row → the roster lists S′.
    """
    home, away = cross_nest_foreign, cross_nest_foreign_ephemeral
    author = provision_foreign_handled_actor(home)
    reader = away["actor"]
    _follow(away, reader, home, author)

    before = _post(home, author, f"before the succession {secrets.token_hex(4)}")
    _wait_listed(away, reader, _hex(author), before, "the first post reaching the reader")

    # ── The author succeeds ───────────────────────────────────────────────
    author2 = _succeed(home, author)
    held = wait_for(
        "the reader's nest re-pointing the follow at the author's successor",
        lambda: _status(away, reader, _hex(author2)),
        lambda s: s.get("tier") == FOLLOWERS_TIER,
        CROSSING_BUDGET_S,
    )
    assert held.get("tier") == FOLLOWERS_TIER, held
    assert before in _listed(away, reader, _hex(author)) or before in _listed(
        away, reader, _hex(author2)
    ), "the post delivered before the succession still lists to the reader"
    after = _post(home, author2, f"from the successor {secrets.token_hex(4)}")
    _wait_listed(away, reader, _hex(author2), after, "the successor's post reaching the reader")

    # ── The reader succeeds ───────────────────────────────────────────────
    reader2 = _succeed(away, reader)
    wait_for(
        "the author's roster moving the reader to their successor",
        lambda: _roster(home, author2),
        lambda roster: _hex(reader2) in roster and _hex(reader) not in roster,
        CROSSING_BUDGET_S,
    )
    later = _post(home, author2, f"to the reader's successor {secrets.token_hex(4)}")
    _wait_listed(away, reader2, _hex(author2), later, "the author's next post reaching the reader's successor")


# ── outcome 27: two homes, one row, custody per home ───────────────────────


@pytest.mark.feature("subscriptions-and-tiers")
def test_a_post_delivered_by_two_homes_lists_once_per_home_and_the_real_homes_tombstone_removes_it(
    cross_nest_foreign, cross_nest_foreign_ephemeral, report_origin_nest
):
    """An author's identity is homed on two nests, and a reader on a third
    nest follows them at each: one reader at the first home, another at the
    second. The author's one signed post, published at both homes, reaches
    both readers once, each listed from the home their follow names. The
    author deletes it at their real home: it leaves both readers' feeds, and a
    later delivery of it from the second home does not bring it back.

    Flow: the author registered on both homes (one key) → reader A follows at
    home H, reader B at home S → the same signed post created on H and on S →
    each home's outbox delivers to the readers' nest → ``deliver_handler``
    stores the row once and adds the second home's custody
    (``follow_delivery_homes``) → each reader's ``home_nest_url`` is their own
    follow's home → the author's tombstone on H → ``follow_delete`` → the
    custody door admits it and the row goes whole → a third reader's follow
    at S backfills the post → ``post_was_deleted`` keeps it out.
    """
    home, away, second = cross_nest_foreign, cross_nest_foreign_ephemeral, report_origin_nest
    author = provision_foreign_handled_actor(home)
    author_hex = _hex(author)
    register_user(
        second["port"],
        author_hex,
        base_url=second["url"],
        admin_signing_key=second["admin"]["signing_key"],
        handle=f"twin{secrets.token_hex(4)}",
        label="twin",
    )
    at_home, at_second = away["actor"], _follower(away)
    _follow(away, at_home, home, author)
    _follow(away, at_second, second, author)

    created_at = int(time.time() * 1_000_000)
    body = sign_and_encode_post(author["signing_key"], created_at, f"one post, two homes {secrets.token_hex(4)}")
    post_id = call(home, author, "fauna.posts.create", {"body": body})["post_id"]
    assert call(second, author, "fauna.posts.create", {"body": body})["post_id"] == post_id, (
        "the same signed bytes are the same content-addressed post on both homes"
    )

    row_a = _wait_listed(away, at_home, author_hex, post_id, "the post reaching the reader who follows at the real home")
    row_b = _wait_listed(away, at_second, author_hex, post_id, "the post reaching the reader who follows at the second home")
    for reader, nest, row in ((at_home, home, row_a), (at_second, second, row_b)):
        listed = [
            p for p in call(away, reader, "fauna.feed.local.posts", {"limit": 200}).get("posts", [])
            if p["post_id"] == post_id
        ]
        assert len(listed) == 1, f"the post lists once to each reader: {listed!r}"
        shown = (row.get("home_nest_url") or "").rstrip("/")
        assert shown and shown == _peer(nest).rstrip("/"), (
            f"each reader is shown the home their own follow names: {shown!r} "
            f"vs {_peer(nest)!r}"
        )

    # ── The real home's tombstone removes it for both ─────────────────────
    tomb = sign_and_encode_tombstone(author["signing_key"], post_id, int(time.time() * 1_000_000))
    assert call(home, author, "fauna.posts.delete", {"body": tomb}).get("deleted") is True
    for reader, what in ((at_home, "the real home's reader"), (at_second, "the second home's reader")):
        wait_for(
            f"the tombstone removing the post for {what}",
            lambda r=reader: _listed(away, r, author_hex).get(post_id),
            lambda row: row is None,
            CROSSING_BUDGET_S,
        )

    # ── A later delivery from the second home does not restore it ─────────
    # A new follow at the second home backfills the author's recent posts from
    # there — the deleted one among them, since that home never deleted it.
    # A post made at the second home just before the follow is the barrier:
    # once the backfill has listed it, the replay has arrived.
    older = _post(second, author, f"before the late follow {secrets.token_hex(4)}")
    late = _follower(away)
    _follow(away, late, second, author)
    _wait_listed(away, late, author_hex, older, "the second home's backfill reaching a new reader")
    for reader in (at_home, at_second, late):
        assert post_id not in _listed(away, reader, author_hex), (
            "a replayed delivery restored a post its author deleted"
        )
