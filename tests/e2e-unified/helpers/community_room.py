"""A community room and its room posts, founded over the wire with no app.

Fixture setup for the nest-side witnesses of room-restricted posts
(``docs/goal/behavior/restricted-posts.md`` § Encryption at rest): the room is
founded the way a member's engine founds one — the founder's binding birth
salt, signed birth policy and reception key to
``fauna.conversations.room.create``; the floor read back from
``room.list_roster``; a generation minted over it, the home nest included, to
``room.publish_generation`` — and a room post is sealed under that generation
and created through ``fauna.posts.create`` after its sealed body uploads, as a
client's compose does. Every signed or sealed byte comes from the shared Rust
the apps run (``fauna_ffi.build_room_birth``, ``build_room_generation_mint``,
``build_room_post``, ``sign_room_labelers``); this module only carries it.

Because the home nest is on the floor the first mint wraps to, the nest that
stores a room post here is the room's home nest holding the tip's wrap — the
one nest that opens a room post (ruling 7).
"""

from __future__ import annotations

import time
from dataclasses import dataclass

import fauna_ffi
from conftest import _make_user
from helpers.blob_upload import SEALED_MIME, upload_blob
from tests.api import conv_api, ws_api


@dataclass
class Room:
    nest: dict
    owner: dict
    room_hex: str
    gen_key: bytes
    generation: bytes
    floor: list[dict]

    @property
    def room_id(self) -> bytes:
        return bytes.fromhex(self.room_hex)


def secret_of(actor: dict) -> bytes:
    return bytes(actor["signing_key"])


def call(nest: dict, actor: dict, kind: str, payload: dict) -> dict:
    """One WS-RPC call as ``actor`` on ``nest``."""
    return conv_api._client_for(conv_api._base_url(nest["port"]), actor).call(kind, payload)


def wrap_targets(floor: list[dict], *, without_nest: bool = False) -> list[tuple[bytes, bytes, bytes]]:
    """The roster rows a minter wraps to — those carrying a slot and a key —
    as ``(entry_id, principal, reception_pubkey)``. ``without_nest`` leaves the
    home nest out: the mint that takes back the nest's read."""
    return [
        (bytes.fromhex(m["entry_id"]), bytes.fromhex(m["principal"]), bytes(m["reception_pubkey"]))
        for m in floor
        if m.get("entry_id")
        and m.get("reception_pubkey")
        and not (without_nest and m.get("kind") == "nest")
    ]


def found_room(nest: dict, *, labelers: list[bytes] | None = None, owner: dict | None = None) -> Room:
    """Found a community room on ``nest`` and mint its first generation over
    the whole floor; when ``labelers`` is given, the owner names them as the
    room's labeler set (version 1)."""
    owner = owner or _make_user(nest)
    salt, policy, reception = fauna_ffi.build_room_birth(secret_of(owner), "the square")
    created = call(
        nest,
        owner,
        "fauna.conversations.room.create",
        {"salt": salt.hex(), "policy": policy, "reception_pubkey": reception},
    )
    room_hex = created["room_id"]
    floor = conv_api.room_list_roster(nest["port"], owner, room_hex)["members"]
    assert any(m.get("kind") == "nest" for m in floor), (
        f"a community room is born with its home nest on the floor: {floor!r}"
    )
    targets = wrap_targets(floor)
    mint, gen_key, generation = fauna_ffi.build_room_generation_mint(
        secret_of(owner), targets, [], int(time.time() * 1000)
    )
    published = call(
        nest, owner, "fauna.conversations.room.publish_generation", {"room_id": room_hex, "mint": mint}
    )
    assert published.get("covered") == len(targets), published
    if labelers:
        signed = fauna_ffi.sign_room_labelers(secret_of(owner), bytes.fromhex(room_hex), 1, labelers)
        call(nest, owner, "fauna.conversations.room.set_labelers", {"room_id": room_hex, "labelers": signed})
    return Room(nest, owner, room_hex, gen_key, generation, floor)


def rotate_nest_out(room: Room) -> None:
    """The owner mints a generation over the floor minus the home nest,
    parented on the current one — taking back the nest's read of the room."""
    mint, gen_key, generation = fauna_ffi.build_room_generation_mint(
        secret_of(room.owner),
        wrap_targets(room.floor, without_nest=True),
        [room.generation],
        int(time.time() * 1000),
    )
    call(
        room.nest,
        room.owner,
        "fauna.conversations.room.publish_generation",
        {"room_id": room.room_hex, "mint": mint},
    )
    room.gen_key, room.generation = gen_key, generation


def seat_member(room: Room, member: dict) -> None:
    """Seat ``member`` on the room's floor the way a joiner is seated: the
    member accepts the owner as a contact, the owner signs and sends the
    invitation (``room.invite``, under the birth policy's version 1), and the
    member accepts it naming a fresh wrap target (``room.accept_invite``). No
    backfill is published, so the member holds a wrap for no generation
    minted before it sat down — a joiner without the room's earlier history."""
    conv_api.accept_contact(room.nest["port"], member, room.owner["actor_id_hex"])
    invite = fauna_ffi.sign_room_member_invite(
        secret_of(room.owner), room.room_id, bytes.fromhex(member["actor_id_hex"]), 1
    )
    call(room.nest, room.owner, "fauna.conversations.room.invite", {"invite": invite, "invitee_node": ""})
    call(
        room.nest,
        member,
        "fauna.conversations.room.accept_invite",
        {"room_id": room.room_hex, "reception_pubkey": fauna_ffi.mint_room_reception_pubkey()},
    )
    room.floor = conv_api.room_list_roster(room.nest["port"], room.owner, room.room_hex)["members"]


def rotate(room: Room) -> None:
    """The owner mints a generation over the room's whole current floor,
    parented on the current one — the mint that covers a newly seated member."""
    targets = wrap_targets(room.floor)
    mint, gen_key, generation = fauna_ffi.build_room_generation_mint(
        secret_of(room.owner), targets, [room.generation], int(time.time() * 1000)
    )
    published = call(
        room.nest,
        room.owner,
        "fauna.conversations.room.publish_generation",
        {"room_id": room.room_hex, "mint": mint},
    )
    assert published.get("covered") == len(targets), published
    room.gen_key, room.generation = gen_key, generation


def room_post(room: Room, teaser: str, words: str) -> tuple[str, bytes, str]:
    """Create a room post by the room's owner the way a client does: the
    sealed body uploads first, then the signed post. Returns the post id, the
    sealed body's bytes and its hash."""
    post, blob = fauna_ffi.build_room_post(
        secret_of(room.owner), teaser, words, room.room_id, room.generation, room.gen_key
    )
    blob_hash = upload_blob(
        room.nest["port"],
        room.owner["token"],
        blob,
        audience_class="GroupRestrictedPost",
        mime=SEALED_MIME,
    )
    return ws_api.create_post(room.nest["port"], room.owner, post), blob, blob_hash


def room_search_posts(room: Room, actor: dict, term: str) -> list[dict]:
    """``fauna.conversations.room.search`` as ``actor``, asking for post hits
    (``include_posts``); returns the post hits only."""
    reply = call(
        room.nest,
        actor,
        "fauna.conversations.room.search",
        {"room_id": room.room_hex, "query": term, "include_posts": True},
    )
    return [h for h in reply.get("hits", []) if h.get("kind") == "post"]


def room_labels(nest: dict, actor: dict, post_id: str) -> list[dict]:
    """``fauna.posts.room_labels`` as ``actor`` for one post — the entries the
    nest serves for it (empty when it serves none)."""
    reply = call(nest, actor, "fauna.posts.room_labels", {"post_ids": [post_id]})
    return [p for p in reply.get("posts", []) if p.get("post_id") == post_id]
