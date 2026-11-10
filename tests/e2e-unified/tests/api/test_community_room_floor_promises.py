"""What a community room's home nest promises about the room's messages, seen
from outside with no app driver (``docs/features/group-conversations.md``,
the ``[nest]`` outcomes 45, 46 and 47).

Owner doc: ``docs/goal/behavior/community-rooms.md`` § The three classes and
its § Implementation status today — the home nest opens the tip's wrap and
derives the room's search index and its labelers' verdicts from what the
room's members wrote; the verdicts are served beside the envelope on
``channel.fetch`` only to a live floor member (*Labels land*); the search door
serves a live floor member and refuses anyone else, naming only positions the
caller holds a wrap for (*The search index becomes readable*); a mint that
leaves the nest out withdraws its read, and an ordinary rotation keeps it
withdrawn (*The nest's read is the members' standing choice*); and the
derived-view builder verifies each message's author signature and binds the
author to the live floor, so neither a message sealed in another member's
name nor a non-member's reaches the corpus (*Attribution lands*); the send
door itself is floor-verified (*The send path is floor-verified*).

The tier_1 pins are ``bins/fauna-nest/tests/conformance_conversation_rooms.rs``
(the floor-gated label read, the search door's member gate and wrap bound,
``a_send_labelled_with_another_members_name_builds_no_view``,
``a_non_members_honestly_signed_send_builds_no_view``); these are their
outside-observed twins over a real nest binary. The room is founded and
messages are sent over the wire (``helpers/community_room.py``) with every
signed or sealed byte built by the shared Rust the apps run.

Every absence is anchored on a presence read first from the same door
(convention 14): the derived views are built inside ``channel.send`` before it
answers, so an empty answer afterwards is the act's doing.
"""

from __future__ import annotations

import time
import uuid

import pytest
from clients.ws_rpc_admin_client import RpcCallError

import fauna_ffi
from conftest import _make_user
from helpers.community_room import (
    found_room,
    nest_row,
    remove_member,
    room_send,
    rotate,
    rotate_nest_out,
    seat_member,
)
from helpers.labeler_publish import FIXTURES, publish_wasm_labeler
from tests.api import conv_api

pytestmark = pytest.mark.tier_3


def _word(prefix: str) -> str:
    """A word unique to this test, so a search can only hit its messages."""
    return f"{prefix}{uuid.uuid4().hex[:10]}"


def _search_seqs(room, actor: dict, term: str) -> list[int]:
    return sorted(h["seq"] for h in conv_api.room_search(room.nest["port"], actor, room.room_hex, term))


def _entry(room, actor: dict, seq: int) -> dict:
    """The ``channel.fetch`` entry at ``seq`` as ``actor`` reads it."""
    entries = conv_api.channel_fetch(room.nest["port"], actor, room.room_hex, after=seq - 1)
    found = [e for e in entries if e.get("seq") == seq]
    assert found, f"the envelope at seq {seq} is served to {actor['actor_id_hex'][:12]}: {entries!r}"
    return found[0]


@pytest.fixture
def labeler(run_seal_helper, nest_instance) -> bytes:
    """A published wasm labeler that labels any text holding "cat"."""
    return publish_wasm_labeler(
        run_seal_helper, nest_instance, FIXTURES / "room_commercial_labeler.wat"
    )


# ── outcome 45 ───────────────────────────────────────────────────────────────


@pytest.mark.feature("group-conversations")
def test_only_live_members_read_a_community_rooms_labels(nest_instance, labeler):
    """Outcome 45, first half: a member's message the room's labeler labels is
    served with its verdicts to a live member, and with the same envelope and
    no verdicts to someone off the floor and to a member the owner removed."""
    nest = nest_instance
    room = found_room(nest, labelers=[labeler])
    member, leaving, stranger = _make_user(nest), _make_user(nest), _make_user(nest)
    seat_member(room, member)
    seat_member(room, leaving)
    rotate(room)
    seq = room_send(room, member, f"a cat for the square {_word('lbl')}")

    seen = _entry(room, member, seq)
    assert seen.get("labels"), f"a live member reads the labeler's verdict: {seen!r}"

    off_floor = _entry(room, stranger, seq)
    assert off_floor.get("envelope") == seen.get("envelope"), "the sealed envelope is served"
    assert not off_floor.get("labels") and not off_floor.get("scores"), (
        f"someone off the floor gets no verdicts: {off_floor!r}"
    )

    remove_member(room, leaving)
    removed = _entry(room, leaving, seq)
    assert removed.get("envelope") == seen.get("envelope"), "the sealed envelope is still served"
    assert not removed.get("labels") and not removed.get("scores"), (
        f"a removed member gets no verdicts: {removed!r}"
    )
    assert _entry(room, member, seq).get("labels"), "the remaining member still reads them"


@pytest.mark.feature("group-conversations")
def test_a_withdrawn_read_stays_withdrawn_across_a_later_rekey(nest_instance, labeler):
    """Outcome 45, second half: the owner mints the nest out — the members
    withdraw its read — and the nest's own roster row says so; a later
    ordinary re-key (a removal's severance) keeps it out, so a message sealed
    under the newest generation is neither labelled nor searchable, while
    before the withdrawal the same kind of message was both."""
    nest = nest_instance
    room = found_room(nest, labelers=[labeler])
    member, leaving = _make_user(nest), _make_user(nest)
    seat_member(room, member)
    seat_member(room, leaving)
    rotate(room)
    before = _word("before")
    seq_before = room_send(room, member, f"a cat before the withdrawal {before}")
    assert _entry(room, member, seq_before).get("labels"), "the nest labels while it reads"
    assert _search_seqs(room, member, before) == [seq_before], "and indexes while it reads"

    rotate_nest_out(room)
    assert nest_row(room).get("tip_wrapped") is False, (
        "the nest's own roster row records the withdrawn read every member sees"
    )

    remove_member(room, leaving)
    assert nest_row(room).get("tip_wrapped") is False, "the re-key kept the read withdrawn"
    after = _word("after")
    seq_after = room_send(room, member, f"a cat after the re-key {after}")
    assert _entry(room, member, seq_after).get("envelope"), "the member's message is stored"
    assert not _entry(room, member, seq_after).get("labels"), (
        "a message under the re-keyed generation is never labelled"
    )
    assert _search_seqs(room, member, after) == [], "nor searchable"
    assert _search_seqs(room, member, before) == [], "and the withdrawal purged what it built"


# ── outcome 46 ───────────────────────────────────────────────────────────────


@pytest.mark.feature("group-conversations")
def test_a_community_rooms_search_serves_only_its_members(nest_instance):
    """Outcome 46, first half: the room's search finds a member's message for
    a member — naming its position — and refuses someone off the floor and a
    member the owner removed, rather than answering them empty."""
    nest = nest_instance
    room = found_room(nest)
    member, leaving, stranger = _make_user(nest), _make_user(nest), _make_user(nest)
    seat_member(room, member)
    seat_member(room, leaving)
    rotate(room)
    word = _word("srch")
    seq = room_send(room, member, f"meet at the fountain {word}")

    assert _search_seqs(room, member, word) == [seq]
    assert _search_seqs(room, leaving, word) == [seq], "every live member searches"

    with pytest.raises(RpcCallError):
        _search_seqs(room, stranger, word)

    remove_member(room, leaving)
    with pytest.raises(RpcCallError):
        _search_seqs(room, leaving, word)
    assert _search_seqs(room, member, word) == [seq], "the remaining member still finds it"


@pytest.mark.feature("group-conversations")
def test_a_joiner_without_the_rooms_history_never_finds_an_earlier_message(nest_instance):
    """Outcome 46, second half: a member seated with no wrap for the room's
    earlier generations (no history shared) is never served, by the room's
    search, a message sealed before they joined — while the owner finds it,
    and the joiner finds the message sealed after the mint that covered them,
    by the same word."""
    nest = nest_instance
    room = found_room(nest)
    word = _word("hist")
    seq_before = room_send(room, room.owner, f"before anyone joined {word}")

    joiner = _make_user(nest)
    seat_member(room, joiner)
    rotate(room)
    seq_after = room_send(room, room.owner, f"after the joiner sat down {word}")

    assert _search_seqs(room, room.owner, word) == sorted([seq_before, seq_after])
    assert _search_seqs(room, joiner, word) == [seq_after], (
        "the joiner holds a wrap for the message sealed after it joined, and for nothing before"
    )


# ── outcome 47 ───────────────────────────────────────────────────────────────


@pytest.mark.feature("group-conversations")
def test_nobody_outside_a_community_room_can_post_into_it(nest_instance):
    """Outcome 47, first half: someone off the floor sending into the room is
    refused at the send door, and a removed member is refused the same way —
    while a member's send is stored."""
    nest = nest_instance
    room = found_room(nest)
    member, leaving, stranger = _make_user(nest), _make_user(nest), _make_user(nest)
    seat_member(room, member)
    seat_member(room, leaving)
    rotate(room)
    assert room_send(room, member, f"a member speaks {_word('ok')}") > 0

    with pytest.raises(RpcCallError):
        room_send(room, stranger, f"a stranger speaks {_word('no')}")

    remove_member(room, leaving)
    with pytest.raises(RpcCallError):
        room_send(room, leaving, f"a removed member speaks {_word('no')}")


@pytest.mark.feature("group-conversations")
def test_a_message_in_another_members_name_or_a_non_members_never_becomes_a_hit(nest_instance):
    """Outcome 47, second half: a member seals a message labelled with the
    owner's name, and the owner carries bytes a non-member honestly signed;
    the nest stores both — it is not the judge of a member's bytes — and
    neither ever becomes a search hit, while an honest message under the same
    generation does. (The bubble half is the shared receive walk's, which
    verifies the same signature; this witnesses the nest's.)"""
    nest = nest_instance
    room = found_room(nest)
    member, outsider = _make_user(nest), _make_user(nest)
    seat_member(room, member)
    rotate(room)

    honest = _word("honest")
    seq_honest = room_send(room, member, f"signed by its author {honest}")
    forged = _word("forged")
    room_send(room, member, f"signed by a member, labelled the owner {forged}", claimed_author=room.owner)
    foreign = _word("foreign")
    envelope = fauna_ffi.build_room_message_envelope(
        bytes(outsider["signing_key"]),
        room.room_id,
        room.generation,
        room.gen_key,
        f"honestly signed by a non-member {foreign}",
        int(time.time() * 1000),
    )
    conv_api.channel_send(nest["port"], room.owner, room.room_hex, envelope)

    assert _search_seqs(room, room.owner, honest) == [seq_honest], "an honest message is a hit"
    assert _search_seqs(room, room.owner, forged) == [], (
        "a message sealed in another member's name is never a hit"
    )
    assert _search_seqs(room, room.owner, foreign) == [], "nor is a non-member's, however signed"
