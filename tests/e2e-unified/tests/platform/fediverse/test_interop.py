"""Real-fediverse interop — the produce/discovery/follow core (F1–F4), consume (F5–F9), and reply/quote (F10–F11).

A pinned official third-party server federates with a locally-built fauna-nest
across a real TLS boundary (fixture `peer`), driven black-box through its client
REST API. This is what our own in-test fediverse server cannot prove
(`activitypub.md` § Implementation status today, gap 4): that a *real* server
accepts our WebFinger, actor JSON-LD, addressing, and HTTP signatures.

**One assertion set, two peers.** The same flows run against whichever peer the
run selected — Mastodon (mainstream, lenient) or GoToSocial (a different
implementation in a different language, strict about signed fetches). Nothing
below names a peer: everything implementation-specific lives behind the
`FediversePeer` contract in `helpers/fediverse_peer.py`. A second, re-pointed
copy of these assertions would drift, and a drifted assertion that passes proves
nothing.

F1–F4 assert peer-side (the peer accepts + surfaces our output):

  F1  the peer resolves `@user@nest.test`          → webfinger + actor JSON-LD parse
  F2  the peer follows us → our auto-Accept        → relationship following=true
  F3  Fauna post → the follower's home timeline    → signed Create accepted + rendered
  F4  Fauna delete → the status disappears         → signed Delete accepted

F5–F9 assert nest-internal state (the peer's input reaches us intact and lands
where it should) — these flows leave no peer-visible result:

  F5  the peer replies to our note                 → inbound Create, addressed arm
  F6  the peer favourites it                       → synthetic upvote, ownership arm
  F7  the peer boosts it                           → synthetic repost
  F8  the peer un-favourites / un-boosts           → both retracted, fully
  F9  we follow an account on the peer; it posts   → outbound Follow + follow arm

F10–F11 assert peer-side again — our reply to, and quote of, a peer status
(`activitypub.md` § Reply and quote):

  F10 we reply to a peer status                    → in the status's context descendants
  F11 we quote a peer status                       → our note arrives with its `RE:` link

Opt-in like tier_4: `just e2e-fediverse-test` (Mastodon) or
`just e2e-gotosocial-test`, both of which set FAUNA_E2E_FEDIVERSE=1. A general
`--tier 3` run skips the suite — the `peer` fixture never boots a third-party
server without the opt-in env.
"""

import time

import pytest

from common import create_actor_and_register
from helpers import ap_nest
from helpers.ap_nest import AP_BRIDGE_ID, enable_ap
from helpers.budgets import CROSS_NEST_S
from helpers.waiting import wait_until
from tests.api import ws_api
from tests.api.bare import (
    post_reference,
    sign_and_encode_post,
    sign_and_encode_tombstone,
)

pytestmark = [pytest.mark.tier_3, pytest.mark.fediverse_interop]


def _now_us() -> int:
    return int(time.time() * 1_000_000)


@pytest.fixture(scope="module")
def peer_user(peer):
    """A peer-local account + OAuth token that will follow our nest user."""
    _username, token = peer.new_user("alice")
    return {"username": "alice", "token": token}


@pytest.fixture(scope="module")
def following_pair(peer, peer_user):
    """A nest AP actor that `peer_user` has resolved and (accepted-)follows.

    Establishes F1 (the peer resolves the actor) + F2 (auto-accepted follow) as
    the shared precondition every later flow builds on — F3/F4 push to this
    follower, and F5–F8 need our notes to be *mapped* (the Create-push's
    `ap_post_map` row is what makes an inbound reply/reaction resolve to a local
    post at all). Module-scoped so one follow round-trip serves both classes.
    """
    token = peer_user["token"]

    nest_user = create_actor_and_register(
        peer.nest["port"], admin_signing_key=peer.nest["admin"]["signing_key"]
    )
    username, actor_url = enable_ap(peer.nest, nest_user)
    acct = f"{username}@{peer.nest['domain']}"

    # F1 — the peer resolves us (webfinger → actor JSON-LD → local account).
    account = wait_until(
        lambda: peer.resolve_account(acct, token),
        CROSS_NEST_S,
        diagnose=lambda: (
            f"{peer.name} could not resolve @{acct} — webfinger or actor JSON-LD was "
            f"rejected (a real-server interop break, not a timeout)"
        ),
    )
    account_id = account["id"]

    # F2 — follow us; the nest auto-accepts (default-on), which the peer
    # records as following=true only once it receives our signed Accept.
    peer.follow(account_id, token)
    wait_until(
        lambda: peer.relationship(account_id, token).get("following") or None,
        CROSS_NEST_S,
        diagnose=lambda: (
            f"{peer.name} never saw following=true for @{acct} — our Accept was not "
            f"delivered or was rejected"
        ),
    )

    return {
        "nest_user": nest_user,
        "username": username,
        "actor_url": actor_url,
        "acct": acct,
        "account_id": account_id,
        "token": token,
    }


def _followed_peer_status(peer, following_pair, username):
    """A fresh peer-local account we follow, and one status of theirs our nest
    ingested: `(status, ap_post_map row, token)`.

    The follow graph's outbound direction end to end — F9's flow, shared by
    F10/F11, which need a peer status that is also a *mapped* local post.
    """
    _username, token = peer.new_user(username)
    # A locked account parks our Follow as a pending request and reports
    # followed_by=false forever — the SAME observable as our Follow being
    # rejected on the wire — so the precondition is asserted, not assumed.
    assert peer.account_locked(token) is False, (
        f"{username} requires manual follower approval, so this flow would test "
        "the peer's approval queue rather than our outbound Follow"
    )
    uri = peer.actor_uri(username)
    ws_api.bridge_add_follow(
        peer.nest["port"], following_pair["nest_user"], AP_BRIDGE_ID, uri
    )
    follows = ws_api.bridge_list_follows(
        peer.nest["port"], following_pair["nest_user"], AP_BRIDGE_ID
    )
    assert any(f["id"] == uri for f in follows), (
        f"the follow of {uri} was not recorded: {follows}"
    )

    us = wait_until(
        lambda: peer.resolve_account(following_pair["acct"], token),
        CROSS_NEST_S,
        diagnose=lambda: (
            f"{username} could not resolve @{following_pair['acct']} — discovery "
            f"broke for a second peer-local account"
        ),
    )
    wait_until(
        lambda: peer.relationship(us["id"], token).get("followed_by") or None,
        CROSS_NEST_S,
        diagnose=lambda: (
            f"{peer.name} never recorded {username} as followed_by our actor — the "
            "signed outbound Follow was not delivered or was rejected"
        ),
    )

    marker = f"{username}-post-{_now_us()}"
    status = peer.post_status(f"hello from {username}: {marker}", token)
    row = wait_until(
        lambda: ap_nest.ap_post_map_row(peer.nest, status["uri"]),
        CROSS_NEST_S,
        diagnose=lambda: (
            f"{username}'s note {status['uri']!r} was never ingested — the Create "
            f"was not delivered, or the relationship gate's follow arm did not admit it"
        ),
    )
    return status, row, token


def _our_referencing_post(peer, following_pair, kind, target_post_id, text):
    """`fauna.posts.create` of our signed post referencing `target_post_id`;
    returns the pushed note's URL (push/pull-symmetric)."""
    post_id = ws_api.create_post(
        peer.nest["port"],
        following_pair["nest_user"],
        sign_and_encode_post(
            following_pair["nest_user"]["signing_key"],
            _now_us(),
            text,
            references=[post_reference(kind, target_post_id)],
        ),
    )
    return f"{following_pair['actor_url']}/notes/{post_id}"


class TestFediverseInteropProduce:
    """F1–F4: a real peer discovers us, follows us, and receives our posts."""

    def test_f1_peer_resolves_actor(self, following_pair):
        """F1: `@user@nest.test` resolves — webfinger + actor JSON-LD accepted."""
        # The fixture already resolved + asserted; pin the acct shape here so a
        # discovery break fails this named test, not an opaque fixture error.
        assert following_pair["acct"].endswith("@nest.test")
        assert following_pair["account_id"]

    @pytest.mark.feature("fediverse")
    def test_f2_follow_is_auto_accepted(self, peer, following_pair):
        """F2: the follow is auto-accepted — relationship reports following=true."""
        rel = peer.relationship(
            following_pair["account_id"], following_pair["token"]
        )
        assert rel.get("following") is True, f"relationship not following: {rel}"

    @pytest.mark.feature("fediverse")
    def test_f3_post_reaches_follower_timeline(self, peer, following_pair):
        """F3: a Fauna post arrives in the follower's home timeline, rendered.

        Proves the produce direction against a real peer: the signed Create,
        its Digest/Date/Host, addressing (Public + followers), and HTML content
        are all accepted — the half our in-test fediverse server accepts by
        construction and so cannot prove.
        """
        marker = f"hello-peer-{_now_us()}"
        body = f"interop check: {marker}"

        ws_api.create_post(
            peer.nest["port"], following_pair["nest_user"],
            sign_and_encode_post(following_pair["nest_user"]["signing_key"], _now_us(), body),
        )

        status = wait_until(
            lambda: next(
                (s for s in peer.home_timeline(following_pair["token"])
                 if marker in (s.get("content") or "")),
                None,
            ),
            CROSS_NEST_S,
            diagnose=lambda: (
                f"our post never reached the follower's home timeline — the signed "
                f"Create was not delivered or {peer.name} rejected it (marker {marker!r})"
            ),
        )
        # A remote account's `acct` in the Mastodon client API (which both peers
        # speak) is fully-qualified `user@domain`.
        assert status["account"]["acct"] == following_pair["acct"], (
            f"timeline status attributed to {status['account']['acct']!r}, "
            f"expected {following_pair['acct']!r}"
        )

    @pytest.mark.feature("fediverse")
    def test_f4_delete_removes_status_from_timeline(self, peer, following_pair):
        """F4: deleting the Fauna post chases the copy with a signed Delete.

        The status must first arrive (a fresh post, so this test is independent),
        then vanish from the timeline once the Delete is accepted — exercising the
        Delete leg + the peer's plain-string-object tolerance (gap 3).
        """
        marker = f"delete-me-{_now_us()}"
        body = f"interop delete check: {marker}"

        post_id = ws_api.create_post(
            peer.nest["port"], following_pair["nest_user"],
            sign_and_encode_post(following_pair["nest_user"]["signing_key"], _now_us(), body),
        )

        def _find():
            return next(
                (s for s in peer.home_timeline(following_pair["token"])
                 if marker in (s.get("content") or "")),
                None,
            )

        wait_until(
            _find,
            CROSS_NEST_S,
            diagnose=lambda: f"post never arrived, cannot test delete (marker {marker!r})",
        )

        reply = ws_api.delete_post(
            peer.nest["port"], following_pair["nest_user"],
            sign_and_encode_tombstone(following_pair["nest_user"]["signing_key"], post_id, _now_us()),
        )
        assert reply["deleted"] is True, f"nest did not delete the post: {reply}"

        wait_until(
            lambda: _find() is None,
            CROSS_NEST_S,
            diagnose=lambda: (
                f"the status is still in the timeline after Delete — {peer.name} did not "
                f"accept our signed Delete (marker {marker!r})"
            ),
        )


class TestFediverseInteropConsume:
    """F5–F9: a real peer replies to, reacts to, and is followed by us.

    The mirror of F1–F4: these flows have no peer-visible result to read
    back (the reply and the synthetic upvote/repost a reaction mints are *our*
    rows), so every assertion is nest-internal — `ap_post_map` + the `content`
    projection, read at rest via the `helpers.ap_nest` readers.
    """

    @pytest.fixture(scope="class")
    def mapped_note(self, peer, following_pair):
        """A Fauna post that reached the peer, with both sides' handles on it.

        F5–F8 all need the same precondition: one of our notes that the peer
        holds a copy of (so it can reply/favourite/boost it) *and* that the
        Create-push mapped locally (so the inbound activity resolves to a local
        post). Class-scoped: one post serves the reply, the favourite and the
        boost — they are independent activities on the same object.
        """
        marker = f"reactable-{_now_us()}"

        post_id = ws_api.create_post(
            peer.nest["port"], following_pair["nest_user"],
            sign_and_encode_post(
                following_pair["nest_user"]["signing_key"], _now_us(),
                f"interop consume check: {marker}",
            ),
        )

        status = wait_until(
            lambda: next(
                (s for s in peer.home_timeline(following_pair["token"])
                 if marker in (s.get("content") or "")),
                None,
            ),
            CROSS_NEST_S,
            diagnose=lambda: (
                f"our post never reached {peer.name}, so nothing can react to it "
                f"(marker {marker!r})"
            ),
        )

        # The object URL a remote reaction names is the one the push published
        # under — read it back from the map row rather than reconstructing it.
        note_url = ap_nest.local_note_url(peer.nest, post_id)
        assert note_url, (
            f"the Create-push wrote no ap_post_map row for {post_id} — an "
            f"inbound reaction could never resolve it to a local post"
        )
        assert status["uri"] == note_url, (
            f"{peer.name} filed our note under {status['uri']!r} but we published "
            f"it as {note_url!r} — a push/pull id asymmetry breaks every "
            f"inbound interaction on it"
        )

        return {
            "post_id": post_id,
            "note_url": note_url,
            "status_id": status["id"],
            "marker": marker,
        }

    @pytest.mark.feature("fediverse")
    def test_f5_peer_reply_is_ingested(
        self, peer, following_pair, mapped_note
    ):
        """F5: a reply from the peer to our note lands as a stored post.

        Exercises the inbound `Create` **addressed** arm of the relationship
        gate: we do not follow this peer-local account, so the reply is ingested
        only because it is addressed to an enabled local account — resolved
        from the activity's `to`/`cc` when it arrives at the shared inbox.
        """
        marker = f"peer-reply-{_now_us()}"

        reply = peer.post_status(
            f"@{following_pair['acct']} {marker}",
            following_pair["token"],
            in_reply_to_id=mapped_note["status_id"],
        )

        row = wait_until(
            lambda: ap_nest.ap_post_map_row(peer.nest, reply["uri"]),
            CROSS_NEST_S,
            diagnose=lambda: (
                f"the reply {reply['uri']!r} was never ingested — the inbound Create "
                f"was rejected, or the relationship gate dropped it (we do not follow "
                f"this actor, so it rides the addressed arm)"
            ),
        )
        assert not row["tombstoned"], "the ingested reply was mapped already-tombstoned"
        assert ap_nest.content_row_exists(peer.nest, row["fauna_post_id"]), (
            f"the reply mapped to post {row['fauna_post_id']} but wrote no content "
            f"projection row — it is mapped but unreadable"
        )
        assert (row["remote_actor_uri"] or "").startswith(f"https://{peer.domain}/"), (
            f"the ingested reply recorded owner {row['remote_actor_uri']!r}, not a "
            f"{peer.domain} actor — the outbound interact path would misroute"
        )

    @pytest.mark.feature("fediverse")
    def test_f6_peer_favourite_mints_a_synthetic_upvote(
        self, peer, following_pair, mapped_note
    ):
        """F6: a favourite on our note mints exactly one synthetic upvote.

        Exercises the **ownership arm** of the reaction gate — the reacting
        actor is not followed by anyone here, so the mint happens only because
        the reacted-to object is an enabled local account's own post.
        """
        note_url = mapped_note["note_url"]

        peer.favourite(mapped_note["status_id"], following_pair["token"])

        rows = wait_until(
            lambda: ap_nest.reaction_rows_for_object(peer.nest, "like", note_url) or None,
            CROSS_NEST_S,
            diagnose=lambda: (
                f"no synthetic upvote was minted for a favourite on {note_url!r} — the "
                f"Like was rejected, or the reaction gate's ownership arm dropped it"
            ),
        )
        assert len(rows) == 1, f"a single favourite minted {len(rows)} rows: {rows}"
        row = rows[0]
        assert not row["tombstoned"], "the fresh upvote was minted already-tombstoned"
        assert ap_nest.content_row_exists(peer.nest, row["fauna_post_id"]), (
            "the synthetic upvote has a map row but no content projection"
        )
        # The key is `{verb}:{actor}:{object}` — the stable shape that makes a
        # replayed Like idempotent and gives Undo something to resolve.
        assert row["ap_url"] == ap_nest.reaction_map_key(
            "like", row["remote_actor_uri"], note_url
        ), f"upvote filed under an off-shape key: {row['ap_url']!r}"

    @pytest.mark.feature("fediverse")
    def test_f7_peer_boost_mints_a_synthetic_repost(
        self, peer, following_pair, mapped_note
    ):
        """F7: a boost of our note mints exactly one synthetic repost."""
        note_url = mapped_note["note_url"]

        peer.reblog(mapped_note["status_id"], following_pair["token"])

        rows = wait_until(
            lambda: ap_nest.reaction_rows_for_object(peer.nest, "announce", note_url)
            or None,
            CROSS_NEST_S,
            diagnose=lambda: (
                f"no synthetic repost was minted for a boost of {note_url!r} — the "
                f"Announce was rejected or dropped by the reaction gate"
            ),
        )
        assert len(rows) == 1, f"a single boost minted {len(rows)} rows: {rows}"
        row = rows[0]
        assert not row["tombstoned"], "the fresh repost was minted already-tombstoned"
        assert ap_nest.content_row_exists(peer.nest, row["fauna_post_id"]), (
            "the synthetic repost has a map row but no content projection"
        )
        assert row["ap_url"] == ap_nest.reaction_map_key(
            "announce", row["remote_actor_uri"], note_url
        ), f"repost filed under an off-shape key: {row['ap_url']!r}"

    @pytest.mark.feature("fediverse")
    def test_f8_undo_retracts_both_synthetic_reactions(
        self, peer, following_pair, mapped_note
    ):
        """F8: unfavourite/unboost retract the rows F6/F7 minted — fully.

        The retraction tombstones the map row **and** withdraws the synthetic
        post's `content` projection; tombstoning only the map row would orphan
        one content row per un-react/re-react cycle (a tombstoned row is
        deliberately not a duplicate, so re-reacting mints again).

        Ordered after F6/F7 by name — they mint what this retracts.
        """
        note_url = mapped_note["note_url"]
        token = following_pair["token"]

        minted = {}
        for verb in ("like", "announce"):
            rows = ap_nest.reaction_rows_for_object(peer.nest, verb, note_url)
            assert len(rows) == 1, (
                f"expected exactly one live {verb} row from F6/F7 before undoing, "
                f"got {rows}"
            )
            minted[verb] = rows[0]

        peer.unfavourite(mapped_note["status_id"], token)
        peer.unreblog(mapped_note["status_id"], token)

        def _tombstoned(key):
            row = ap_nest.ap_post_map_row(peer.nest, key)
            return row if row and row["tombstoned"] else None

        for verb in ("like", "announce"):
            key = minted[verb]["ap_url"]
            post_id = minted[verb]["fauna_post_id"]
            row = wait_until(
                lambda k=key: _tombstoned(k),
                CROSS_NEST_S,
                diagnose=lambda: (
                    f"the Undo{{{verb}}} never tombstoned {key!r} — un-reacting left "
                    f"the synthetic reaction live"
                ),
            )
            assert not ap_nest.content_row_exists(peer.nest, post_id), (
                f"the Undo{{{verb}}} tombstoned the map row but left the synthetic "
                f"post {post_id} in the content projection — the orphan-per-cycle "
                f"growth the withdrawal closes"
            )

    @pytest.mark.feature("fediverse")
    def test_f9_followed_peer_account_posts_are_ingested(
        self, peer, following_pair
    ):
        """F9: we follow an account on the peer; its posts arrive.

        The outbound direction of the follow graph, and the **follow arm** of
        the inbound relationship gate — this note is addressed to Public and
        our followers collection, never to us by name, so nothing but the
        outbound follow row can admit it.
        """
        _status, row, _token = _followed_peer_status(peer, following_pair, "carol")
        assert ap_nest.content_row_exists(peer.nest, row["fauna_post_id"]), (
            f"carol's note mapped to {row['fauna_post_id']} but wrote no content "
            f"projection row"
        )

    @pytest.mark.feature("fediverse")
    def test_f10_our_reply_joins_the_peer_status_thread(self, peer, following_pair):
        """F10: our reply to a peer status is in that status's thread on the peer.

        The whole reply path against a real server: the peer's status is
        ingested (F9's flow) → `fauna.posts.create` of our signed reply
        referencing its local id → the push derives `inReplyTo` + a Mention and
        delivers to the author's inbox → the peer threads it, so
        `GET /statuses/{id}/context` lists our note among the descendants.
        """
        status, row, token = _followed_peer_status(peer, following_pair, "dave")
        note_url = _our_referencing_post(
            peer, following_pair, "Reply", row["fauna_post_id"],
            f"a reply from the nest {_now_us()}",
        )

        def _threaded():
            descendants = peer.status_context(status["id"], token)["descendants"]
            return next((d for d in descendants if d.get("uri") == note_url), None)

        reply = wait_until(
            _threaded,
            CROSS_NEST_S,
            diagnose=lambda: (
                f"{peer.name} never threaded our reply {note_url!r} under "
                f"{status['uri']!r} — the Create was not delivered to the author, "
                f"or its inReplyTo was not honoured"
            ),
        )
        assert reply["in_reply_to_id"] == status["id"], reply

    @pytest.mark.feature("fediverse")
    def test_f11_our_quote_arrives_with_its_re_link(self, peer, following_pair):
        """F11: our quote of a peer status arrives carrying the `RE:` link.

        The pinned Mastodon predates quote support, so the fallback line every
        server renders is the observable (the JSON-LD spellings are pinned on
        the push in `activitypub::push::tests` and the tier_3 in-test follower).
        The quoted author receives the note — a quote reaches its author in
        every fediverse UI — so the peer resolves it on their side.
        """
        status, row, token = _followed_peer_status(peer, following_pair, "erin")
        note_url = _our_referencing_post(
            peer, following_pair, "Quote", row["fauna_post_id"],
            f"a quote from the nest {_now_us()}",
        )

        quote = wait_until(
            lambda: peer.resolve_status(note_url, token),
            CROSS_NEST_S,
            diagnose=lambda: f"{peer.name} could not resolve our quote {note_url!r}",
        )
        assert status["uri"] in quote["content"], (
            f"our quote arrived without its RE: link to {status['uri']!r}: "
            f"{quote['content']!r}"
        )
        assert "RE:" in quote["content"], quote["content"]
