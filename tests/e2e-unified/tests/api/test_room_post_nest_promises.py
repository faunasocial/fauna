"""What the nest promises about a post addressed to a community room, observed
from outside with no app driver (``docs/features/room-restricted-posts.md``,
the ``[nest]`` outcomes 15–20 and 25).

Owner doc: ``docs/goal/behavior/restricted-posts.md`` § Encryption at rest —
a room post is its author's ordinary post whose body only the room's floor
opens (ruling 1); it never has a plaintext full-body wire form on any nest
(*A restricted post never has a plaintext-full-body wire form*); and a
community room's home nest, which holds the room's generation wrap, opens a
room post it stores to index it into the room's own search and run the room's
labelers over it, purging both with the post and with the room's other views
(ruling 7, *Built* detail (v)). Post search is ``docs/goal/ui/search.md``
§ The page's wire surface: ``fauna.search.query`` answers from the floor-derived
corpus, which holds a restricted post's public preview and nothing sealed.

**The room is founded over the wire** (``helpers/community_room.py``), the way
a member's engine founds one, with every signed or sealed byte built by the
shared Rust the apps run. Labels come from a published wasm labeler the owner
names with ``room.set_labelers`` (``helpers/labeler_publish.py``); it labels
any text holding "cat", so the sealed words carry one and the teaser does not.

Every absence here is anchored on a presence read first from the same door
(convention 14): the room's search finds the post and its labels read back
before the act that must take them away, so an empty answer afterwards is the
act's doing, never a pass that had not run yet — the reception pass runs inside
``fauna.posts.create``, before it answers.
"""

from __future__ import annotations

import time
import uuid

import pytest
from clients.ws_rpc_admin_client import RpcCallError

from conftest import _make_user
from helpers.blob_upload import download_blob
from helpers.community_room import (
    found_room,
    room_labels,
    room_post,
    room_search_posts,
    rotate,
    rotate_nest_out,
    seat_member,
)
from helpers.labeler_publish import FIXTURES, publish_wasm_labeler
from tests.api import ws_api
from tests.api.bare import sign_and_encode_tombstone

pytestmark = pytest.mark.tier_3


def _carries(value, needle: str) -> bool:
    """Whether ``needle`` appears anywhere in a decoded reply — any string, or
    any byte string read as text."""
    if isinstance(value, str):
        return needle in value
    if isinstance(value, (bytes, bytearray)):
        return needle.encode() in bytes(value)
    if isinstance(value, dict):
        return any(_carries(k, needle) or _carries(v, needle) for k, v in value.items())
    if isinstance(value, (list, tuple)):
        return any(_carries(v, needle) for v in value)
    return False


def _words() -> tuple[str, str, str]:
    """A teaser marker, a sealed marker, and the teaser as post text — unique
    per test so a search can only ever hit this test's post."""
    tease = f"teaser{uuid.uuid4().hex[:10]}"
    sealed = f"sealed{uuid.uuid4().hex[:10]}"
    return tease, sealed, f"A post for the square: {tease}"


def _post_ids(hits: list[dict]) -> list[str]:
    return [h.get("post_id") for h in hits]


@pytest.fixture
def labeler(run_seal_helper, nest_instance) -> bytes:
    """A published wasm labeler that labels any text holding "cat"."""
    return publish_wasm_labeler(
        run_seal_helper, nest_instance, FIXTURES / "room_commercial_labeler.wat"
    )


# ── outcome 15 ───────────────────────────────────────────────────────────────


@pytest.mark.feature("room-restricted-posts")
def test_no_door_serves_a_room_posts_words_in_the_clear(nest_instance):
    """Outcome 15: what the nest hands out of a room post is the teaser and the
    sealed body. Someone off the room's floor reads it through the by-id door
    and the local feed and fetches its sealed body; the teaser is there, the
    sealed words are nowhere."""
    nest = nest_instance
    room = found_room(nest)
    tease, sealed, teaser = _words()
    post_id, blob, blob_hash = room_post(room, teaser, f"{teaser}\n\nFor members only: {sealed}")
    stranger = _make_user(nest)

    by_id = ws_api.get_post(nest["port"], stranger, post_id)
    assert tease.encode() in by_id, "the post as served carries its teaser"
    assert sealed.encode() not in by_id, "the post as served must not carry the sealed words"

    feed = ws_api.local_feed_posts(nest["port"], stranger, limit=200)
    mine = [p for p in feed if p.get("post_id") == post_id]
    assert mine, f"a room post is an ordinary post: the local feed lists it ({len(feed)} posts)"
    assert not _carries(mine, sealed), f"the feed item must not carry the sealed words: {mine!r}"

    served = download_blob(nest["port"], blob_hash)
    assert served == blob, "the body the nest serves is exactly the sealed bytes uploaded"
    assert sealed.encode() not in served, "the sealed body must not carry the words in the clear"


# ── outcome 16 ───────────────────────────────────────────────────────────────


@pytest.mark.feature("room-restricted-posts")
def test_post_search_finds_a_room_post_by_its_teaser_only(nest_instance):
    """Outcome 16: ``fauna.search.query`` finds a room post by a word of its
    teaser and never by a word sealed for the room — not even for the room's
    own owner, though the home nest itself opened the post to index the
    room's search."""
    nest = nest_instance
    room = found_room(nest)
    tease, sealed, teaser = _words()
    post_id, _, _ = room_post(room, teaser, f"{teaser}\n\nFor members only: {sealed}")
    searcher = _make_user(nest)

    by_teaser = ws_api.search(nest["port"], searcher, tease)
    assert any(_carries(hit, post_id) for hit in by_teaser), (
        f"the teaser's word finds the post: {by_teaser!r}"
    )
    assert ws_api.search(nest["port"], searcher, sealed) == [], (
        "a word sealed for the room must find nothing"
    )
    assert ws_api.search(nest["port"], room.owner, sealed) == [], (
        "not even for the room's owner: post search holds no sealed word"
    )


# ── outcome 17 ───────────────────────────────────────────────────────────────


@pytest.mark.feature("room-restricted-posts")
def test_a_community_rooms_search_finds_a_room_post_by_its_words(nest_instance):
    """Outcome 17: the room's own search, asked for posts, finds a room post by
    a word sealed for the room — naming the post, never quoting it — for a
    member, and refuses someone off the floor."""
    nest = nest_instance
    room = found_room(nest)
    _, sealed, teaser = _words()
    post_id, _, _ = room_post(room, teaser, f"{teaser}\n\nFor members only: {sealed}")

    hits = room_search_posts(room, room.owner, sealed)
    assert _post_ids(hits) == [post_id], hits
    assert not _carries(hits, sealed), f"a hit names where, never what: {hits!r}"

    outsider = _make_user(nest)
    with pytest.raises(RpcCallError):
        room_search_posts(room, outsider, sealed)


# ── outcomes 18–20 ───────────────────────────────────────────────────────────


def _labelled_post(nest: dict, labeler: bytes):
    """A room naming ``labeler`` and one room post whose sealed words it
    labels — found by the room's search and labelled, asserted, before the
    caller acts, so every absence afterwards is the act's doing."""
    room = found_room(nest, labelers=[labeler])
    _, sealed, teaser = _words()
    post_id, _, _ = room_post(room, teaser, f"{teaser}\n\nA cat for the members: {sealed}")
    assert _post_ids(room_search_posts(room, room.owner, sealed)) == [post_id]
    labelled = room_labels(nest, room.owner, post_id)
    assert labelled and labelled[0].get("labels"), (
        f"the room's labeler labelled the post at reception: {labelled!r}"
    )
    return room, post_id, sealed


@pytest.mark.feature("room-restricted-posts")
def test_deleting_a_room_post_takes_it_out_of_the_rooms_search_and_drops_its_labels(
    nest_instance, labeler
):
    """Outcome 18: the author's delete takes the post's search hit and its
    labels with it."""
    nest = nest_instance
    room, post_id, sealed = _labelled_post(nest, labeler)

    tombstone = sign_and_encode_tombstone(
        room.owner["signing_key"], post_id, int(time.time() * 1_000_000)
    )
    ws_api.delete_post(nest["port"], room.owner, tombstone)

    assert room_search_posts(room, room.owner, sealed) == []
    assert room_labels(nest, room.owner, post_id) == []


@pytest.mark.feature("room-restricted-posts")
def test_taking_back_the_nests_read_ends_the_rooms_search_and_labels_for_its_posts(
    nest_instance, labeler
):
    """Outcome 19: the owner mints a generation that leaves the home nest out
    — taking back its read of the room — and the room's search stops finding
    the post and its labels are gone."""
    nest = nest_instance
    room, post_id, sealed = _labelled_post(nest, labeler)

    rotate_nest_out(room)

    assert room_search_posts(room, room.owner, sealed) == []
    assert room_labels(nest, room.owner, post_id) == []


@pytest.mark.feature("room-restricted-posts")
def test_a_moderation_flag_withholds_a_room_posts_search_hit_and_labels(nest_instance, labeler):
    """Outcome 20: while a legal takedown stands on a room post the room's
    search does not find it and its labels are not served; the overturn
    serves both again, so the takedown withheld rather than destroyed them."""
    nest = nest_instance
    room, post_id, sealed = _labelled_post(nest, labeler)
    admin_sk = nest["admin"]["signing_key"]
    admin = {"actor_id_bytes": bytes(admin_sk.verify_key), "signing_key": admin_sk}

    taken = ws_api.moderation_legal_takedown(
        nest["port"], admin, post_id, legal_reference="EU-DSA-2024/12345"
    )
    assert taken.get("status") == "taken_down", taken
    assert room_search_posts(room, room.owner, sealed) == []
    assert room_labels(nest, room.owner, post_id) == []

    restored = ws_api.moderation_legal_takedown(
        nest["port"], admin, post_id, legal_reference="", restore=True
    )
    assert restored.get("status") == "restored", restored
    assert _post_ids(room_search_posts(room, room.owner, sealed)) == [post_id]
    assert room_labels(nest, room.owner, post_id), "the overturn serves the labels again"


# ── outcome 25 ───────────────────────────────────────────────────────────────


@pytest.mark.feature("room-restricted-posts")
def test_a_joiner_without_the_rooms_history_never_finds_an_earlier_room_post(nest_instance):
    """Outcome 25: a member seated with no wrap for the room's earlier
    generations is never shown, by the room's search, a room post written
    before they joined — while the owner finds it, and the joiner finds the
    room post written after the mint that covered them, by the same word
    (`community-rooms.md` § Implementation status today: the search serves a
    position only for a generation the caller holds a wrap for)."""
    nest = nest_instance
    room = found_room(nest)
    _, sealed, teaser = _words()
    before_id, _, _ = room_post(room, teaser, f"{teaser}\n\nBefore anyone joined: {sealed}")

    joiner = _make_user(nest)
    seat_member(room, joiner)
    rotate(room)
    after_id, _, _ = room_post(room, teaser, f"{teaser}\n\nAfter the joiner sat down: {sealed}")

    assert sorted(_post_ids(room_search_posts(room, room.owner, sealed))) == sorted([before_id, after_id])
    assert _post_ids(room_search_posts(room, joiner, sealed)) == [after_id], (
        "the joiner holds a wrap for the post written after it joined, and for nothing before"
    )
