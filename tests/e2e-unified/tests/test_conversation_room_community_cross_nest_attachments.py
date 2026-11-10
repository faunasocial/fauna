"""Tier_3 — a two-nest community room's attachments rest on the room's home.

Outcome **33** of ``docs/features/group-conversations.md``
(``conversation-rooms.md`` § The home nest → *Attachment bytes*, and the
native apps' trust of a self-signed home, BUILT 2026-10-19): in a room
spanning two nests, attachments rest on the room's home nest — both members
open them, they survive clean-up on both nests, a stranger cannot upload, and
a native app opens them even when the home nest's certificate is self-signed.

**Topology** — ``test_conversation_room_community_cross_nest.py``'s two real
TLS nests (``cross_nest_room_seats``, imported): the founder on ``home``
(``cross_nest_foreign``, whose floor certificate is self-signed), the member
on ``away``. Every step a user takes is a gesture in their own app
(convention 8); the stranger's attempt and the clean-up are the nests' own
doors, driven from the test because no user takes them.

**The witnesses, in order:**

1. the member's picture — its bytes POSTed to the HOME under a write token
   the member's own nest relays — paints on the founder's seat, and the
   founder's on the member's, the member's app reading the bytes off the
   self-signed home;
2. a zero-grace clean-up (``fauna.admin.gc``) runs on BOTH nests, and the
   home's reference walk still pins both pictures (``conv_attachment_refs``
   up by two over a baseline read before the sends — a pinned blob is never
   deleted, ``test_conversations_attachments_outbound.py``'s blob-GC body says
   why the pin is the survival);
3. after the clean-up a third picture still opens on the other seat — the
   room's byte plane works on after both sweeps;
4. a stranger on ``away`` asking its nest for a write token into the room is
   refused at the home's door — a non-member cannot upload.

**Latency-independent** (convention 14): every wait is a deadline poll on the
painted state or the nest's answer, never a delay.
"""

from __future__ import annotations

import hashlib
import secrets
import struct
import zlib
from pathlib import Path

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.room_seats import (  # noqa: F401 — room_app is a pytest fixture adopted by import
    MESSAGE_BUDGET_S,
    found_cross_nest_community_room,
    room_app,
    seat_element,
    seat_log,
    seat_story,
    wait_for,
)
from tests.api import conv_api
from tests.test_conversation_room_community_cross_nest import (  # noqa: F401 — a pytest fixture adopted by import
    cross_nest_room_seats,
)

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.real_conversations,
    # tui leads; the six others join through the batched trickle-down.
    pytest.mark.tui,
]



def _admin_gc(nest: dict, *, dry_run: bool) -> dict:
    """``fauna.admin.gc`` with no grace period on ``nest``, as its admin."""
    sk = nest["admin"]["signing_key"]
    client = WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk))
    with client:
        return client.call("fauna.admin.gc", {"grace_period_secs": 0, "dry_run": dry_run})


def _painted(seat, needle: str) -> list[str]:
    """The open thread's image states once a bubble carrying ``needle`` has
    painted — empty until then."""
    d = seat.app.driver
    texts = [d.get_text("dm-message-text", index=i) or "" for i in range(d.count("dm-message-text"))]
    if not any(needle in t for t in texts):
        return []
    return seat.app.conversations.attachment_image_states()


def _distinct_picture(directory: Path, tag: str) -> Path:
    """A small solid-colour PNG whose colour is read off ``tag`` — a picture
    of its own, in its PIXELS. Needed because a community room's attachment
    seal is CONVERGENT (``fauna_mls::room_message::seal_room_attachment``: one
    file under one generation is one blob), so the same picture sent twice is
    one content address and the home's reference walk could count only one
    pin; and the send strips metadata chunks before sealing
    (``fauna_media::strip_metadata``), so a distinct ``tEXt`` chunk alone
    would not do."""
    rgb = hashlib.sha256(tag.encode()).digest()[:3]
    side = 32
    raw = b"".join(b"\x00" + rgb * side for _ in range(side))

    def chunk(kind: bytes, body: bytes) -> bytes:
        crc = zlib.crc32(kind + body) & 0xFFFFFFFF
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", crc)

    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", side, side, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw))
        + chunk(b"IEND", b"")
    )
    out = directory / f"picture-{tag}.png"
    out.write_bytes(png)
    return out


def _send_picture(sender, receiver, caption: str, directory: Path) -> None:
    """``sender`` sends a picture of its own (``_distinct_picture``) with
    ``caption`` in its open thread, and it paints — the picture itself, not its
    placeholder — on ``receiver``'s."""
    picture = _distinct_picture(directory, secrets.token_hex(6))
    sender.app.conversations.send_attachment_in_open_thread(str(picture), caption)
    assert not sender.app.has_error(), (
        f"{sender.app.error_text()}\n"
        f"{seat_log(sender, 'blob', 'write_token', 'write-token', 'attachment', 'upload', 'tls', 'pin')}"
    )
    wait_for(
        f"the picture {caption!r} painting on the other seat",
        lambda: _painted(receiver, caption),
        lambda states: states.count("painted") >= 1,
        MESSAGE_BUDGET_S,
        diagnose=lambda: (
            f"{seat_element(receiver, 'dm-attachment-image')}\n{seat_story(receiver)}\n"
            f"{seat_log(receiver, 'blob', 'attachment', 'tls', 'pin')}\n"
            f"{seat_log(sender, 'blob', 'write_token', 'attachment')}"
        ),
    )


@pytest.mark.feature("group-conversations")
@pytest.mark.requires_surface("community-room-foreign-attachment-pin")
@pytest.mark.web
@pytest.mark.linux
@pytest.mark.android
@pytest.mark.macos
@pytest.mark.ios
def test_a_two_nest_rooms_attachments_rest_on_its_home_and_survive_clean_up(
    cross_nest_room_seats, cross_nest_foreign, cross_nest_foreign_ephemeral, tmp_path
):
    """Pictures cross both ways off the self-signed home, stay pinned through
    a zero-grace clean-up on both nests, and a stranger cannot upload."""
    founder, member = cross_nest_room_seats
    home, away = cross_nest_foreign, cross_nest_foreign_ephemeral
    word = secrets.token_hex(4)

    room_id, founder_thread, member_thread = found_cross_nest_community_room(
        founder, member, home, away, f"hello {word}"
    )
    founder.app.conversations.open_thread_by_id(founder_thread)
    member.app.conversations.open_thread_by_id(member_thread)

    baseline = _admin_gc(home, dry_run=True)["conv_attachment_refs"]

    # ── (1) a picture each way, the member's bytes put on the home ───────
    _send_picture(member, founder, f"from the other nest {word}", tmp_path)
    _send_picture(founder, member, f"from the home {word}", tmp_path)

    # ── (2) clean-up on both nests; the home still pins both ─────────────
    swept_away = _admin_gc(away, dry_run=False)
    swept_home = _admin_gc(home, dry_run=False)
    assert swept_home["conv_attachment_refs"] >= baseline + 2, (
        "the home's reference walk pins both pictures through the clean-up: "
        f"baseline {baseline}, home sweep {swept_home!r}, away sweep {swept_away!r}"
    )

    # ── (3) the byte plane works on after both sweeps ────────────────────
    _send_picture(member, founder, f"after the clean-up {word}", tmp_path)

    # ── (4) a stranger cannot upload into the room ───────────────────────
    from common.auth import create_actor_and_register

    stranger = create_actor_and_register(
        away["port"], admin_signing_key=away["admin"]["signing_key"]
    )
    ask = {"channel_id": room_id, "nest_url": home["url"]}
    # The control: the same ask, from the member, is answered with a token —
    # so the stranger's refusal below is the gate, not a malformed request.
    granted = conv_api._client_for(conv_api._base_url(away["port"], "https"), member.actor).call(
        "fauna.conversations.blob.write_token.get", ask
    )
    assert granted.get("token"), f"the member's own ask is answered with a token: {granted!r}"
    client = conv_api._client_for(conv_api._base_url(away["port"], "https"), stranger)
    with pytest.raises(Exception) as refused:
        client.call(
            "fauna.conversations.blob.write_token.get",
            {"channel_id": room_id, "nest_url": home["url"]},
        )
    # Refused, never a token: the home's structural foreign-member gate turns
    # the stranger away before minting. The refusal's wording is the nest's
    # and is not asserted (a refusal never says which check fired).
    print(f"the stranger's write-token ask was refused: {refused.value!r}")
    for seat in (founder, member):
        assert not seat.app.has_error(), seat.app.error_text()
