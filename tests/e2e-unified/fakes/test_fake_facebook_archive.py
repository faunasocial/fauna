"""Self-test for `fake_facebook_archive`: the zip it builds carries the member
paths `libs/fauna-archive/src/facebook/layout.rs` locates, the JSON shapes
`facebook/{posts,profile,social,threads}.rs` decode, and Facebook's byte-wise
mojibake exactly as the export writes it. A fake a journey leans on is worth
its own witness (the `test_fake_imap_source.py` reasoning).

tier_1: pure in-process, no nest, no driver, no app.
"""

from __future__ import annotations

import io
import json
import zipfile

import pytest

from fake_facebook_archive import (
    FRIENDS_TEXT,
    FRIENDS_TS,
    PUBLIC_TEXT,
    PUBLIC_TS,
    FakeFacebookArchive,
    mojibake,
)

pytestmark = pytest.mark.tier_1

POSTS_MEMBER = "your_facebook_activity/posts/your_posts__check_ins__photos_and_videos_1.json"
PROFILE_MEMBER = "personal_information/profile_information/profile_information.json"


def _zip(archive: FakeFacebookArchive) -> zipfile.ZipFile:
    return zipfile.ZipFile(io.BytesIO(archive.build()))


def test_the_archive_carries_the_members_the_parser_locates():
    names = set(_zip(FakeFacebookArchive()).namelist())
    for member in [
        PROFILE_MEMBER,
        POSTS_MEMBER,
        "your_facebook_activity/posts/album/0.json",
        "your_facebook_activity/posts/media/Timeline_photos/photo_1.jpg",
        "your_facebook_activity/comments_and_reactions/comments.json",
        "your_facebook_activity/comments_and_reactions/likes_and_reactions_1.json",
        "connections/friends/your_friends.json",
        "your_facebook_activity/events/your_event_responses.json",
        "your_facebook_activity/messages/inbox/friendone_abc123/message_1.json",
    ]:
        assert member in names, f"{member} missing; have {sorted(names)}"


def test_posts_carry_timestamps_privacy_and_the_public_photo():
    with _zip(FakeFacebookArchive()) as z:
        posts = json.loads(z.read(POSTS_MEMBER))
    assert [p["timestamp"] for p in posts][0] == FRIENDS_TS
    assert posts[0]["privacy"] == "Friends"
    assert posts[1]["privacy"] == "Public"
    assert posts[1]["data"][0]["post"] == PUBLIC_TEXT
    media = posts[1]["attachments"][0]["data"][0]["media"]
    assert media["uri"] == "your_facebook_activity/posts/media/Timeline_photos/photo_1.jpg"
    assert media["creation_timestamp"] == PUBLIC_TS - 1_000
    assert posts[2]["privacy"] == "Only me"


def test_non_ascii_text_is_written_byte_wise_like_facebook_does():
    raw = _zip(FakeFacebookArchive()).read(POSTS_MEMBER)
    # "ř" is C5 99 in UTF-8; Facebook escapes each byte as its own \u00XX --
    # the exact escape the hand-written Rust fixture
    # (libs/fauna-archive/tests/fixtures/facebook-json/your_facebook_activity/
    # posts/your_posts__check_ins__photos_and_videos_1.json) uses for the same
    # character, so this is the same byte pattern the parser's own test data
    # already commits to.
    assert rb"\u00c5\u0099" in raw
    assert FRIENDS_TEXT.encode("utf-8") not in raw
    assert mojibake("ř") == "Å\x99"


def test_the_tag_suffixes_every_post_and_the_build_is_deterministic():
    a = FakeFacebookArchive(tag=" #pass-a")
    with _zip(a) as z:
        posts = json.loads(z.read(POSTS_MEMBER))
    assert all(p["data"][0]["post"].endswith(" #pass-a") for p in posts)
    assert FakeFacebookArchive().build() == FakeFacebookArchive().build()


def test_write_lands_the_zip_at_the_path(tmp_path):
    path = FakeFacebookArchive().write(tmp_path / "facebook.zip")
    assert path.exists() and zipfile.is_zipfile(path)
