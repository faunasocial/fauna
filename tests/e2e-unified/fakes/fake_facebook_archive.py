"""A generated Facebook JSON-export archive for tier_3 journeys
(`docs/goal/behavior/archive-import.md` § Parser contract rule 7: fixtures are
generated, never real). The member paths are the ones
`libs/fauna-archive/src/facebook/layout.rs` locates by name; the JSON shapes
are the ones `facebook/{posts,profile,social,threads}.rs` decode; every
non-ASCII string is escaped byte-wise the way the real export does
(`text.rs::repair_facebook_mojibake` undoes it).

Every name is fictional. The "photos" are a few bytes of text -- the parser
hashes bytes and maps MIME from the extension, it never decodes images.
"""

from __future__ import annotations

import io
import json
import zipfile
from dataclasses import dataclass, field
from pathlib import Path

FRIENDS_TEXT = "Dobrý den, přátelé"
PUBLIC_TEXT = "At the park"
ONLY_ME_TEXT = "Only me note"
FRIENDS_TS = 1_600_000_000
PUBLIC_TS = 1_610_000_000
ONLY_ME_TS = 1_620_000_000
PHOTO_PATH = "your_facebook_activity/posts/media/Timeline_photos/photo_1.jpg"
PHOTO_TAKEN_TS = PUBLIC_TS - 1_000
PROFILE_MEMBER = "personal_information/profile_information/profile_information.json"
POSTS_MEMBER = "your_facebook_activity/posts/your_posts__check_ins__photos_and_videos_1.json"

# A fixed member mtime so two builds are byte-identical.
_MTIME = (2026, 1, 1, 0, 0, 0)


def mojibake(s: str) -> str:
    """Facebook's byte-wise escape: each UTF-8 byte of a non-ASCII character
    becomes its own `\\u00XX`. Re-reading the UTF-8 bytes as Latin-1 yields one
    code point per byte, which `json.dumps(ensure_ascii=True)` then escapes
    exactly as the export does."""
    return s.encode("utf-8").decode("latin-1")


def _dumps(value) -> bytes:
    return json.dumps(value, ensure_ascii=True, indent=2).encode("ascii")


@dataclass(frozen=True)
class ArchivePostSpec:
    text: str
    timestamp: int
    privacy: str | None
    photo: bytes | None = None


@dataclass
class FakeFacebookArchive:
    owner: str = "Test Owner"
    tag: str = ""
    posts: list[ArchivePostSpec] = field(default_factory=list)

    def __post_init__(self) -> None:
        if not self.posts:
            self.posts = [
                ArchivePostSpec(FRIENDS_TEXT, FRIENDS_TS, "Friends"),
                ArchivePostSpec(PUBLIC_TEXT, PUBLIC_TS, "Public", photo=b"fixture-photo-1"),
                ArchivePostSpec(ONLY_ME_TEXT, ONLY_ME_TS, "Only me"),
            ]

    # ── members ─────────────────────────────────────────

    def _profile(self) -> dict:
        return {
            "profile_v2": {
                "name": {"full_name": mojibake(self.owner), "first_name": "Test", "last_name": "Owner"},
                "emails": {"emails": []},
                "registration_timestamp": 1_262_304_000,
                "profile_uri": "https://www.facebook.com/test.owner.fixture",
                "username": "test.owner.fixture",
                "intro_bio": {"text": mojibake("Fixture bio ❤")},
                "websites": [{"address": "https://example.invalid/owner"}],
            }
        }

    def _posts(self) -> list[dict]:
        out = []
        for spec in self.posts:
            record: dict = {
                "timestamp": spec.timestamp,
                "data": [{"post": mojibake(spec.text + self.tag)}],
                "title": mojibake(f"{self.owner} updated his status."),
            }
            if spec.privacy is not None:
                record["privacy"] = spec.privacy
            if spec.photo is not None:
                record["attachments"] = [
                    {"data": [{"media": {
                        "uri": PHOTO_PATH,
                        "creation_timestamp": PHOTO_TAKEN_TS,
                        "title": "Timeline photos",
                        "description": "Photo caption",
                    }}]},
                ]
            out.append(record)
        return out

    def _album(self) -> dict:
        return {
            "name": "Mobile uploads",
            "photos": [{"uri": "your_facebook_activity/posts/media/Mobile_uploads/photo_2.jpg",
                        "creation_timestamp": 1_580_000_000, "description": "Album photo"}],
            "cover_photo": {"uri": "your_facebook_activity/posts/media/Mobile_uploads/photo_2.jpg",
                            "creation_timestamp": 1_580_000_000},
            "last_modified_timestamp": 1_580_000_200,
            "description": "Album description",
        }

    def _comments(self) -> dict:
        return {"comments_v2": [
            {"timestamp": 1_601_000_000,
             "data": [{"comment": {"timestamp": 1_601_000_000, "comment": mojibake("Děkuji!"), "author": mojibake(self.owner)}}],
             "title": mojibake(f"{self.owner} commented on Friend One's photo.")},
            {"timestamp": 1_602_000_000,
             "data": [{"comment": {"timestamp": 1_602_000_000, "comment": "Nice one", "author": "Friend Two"}}],
             "title": mojibake(f"Friend Two commented on {self.owner}'s post."),
             "attachments": [{"data": [{"external_context": {"url": "https://example.invalid/post/1"}}]}]},
        ]}

    def _reactions(self) -> list[dict]:
        return [
            {"timestamp": 1_603_000_000, "data": [{"reaction": {"reaction": "LIKE", "actor": mojibake(self.owner)}}],
             "title": mojibake(f"{self.owner} likes Friend One's post.")},
        ]

    def _friends(self) -> dict:
        return {"friends_v2": [{"name": "Friend One", "timestamp": 1_500_000_000},
                               {"name": "Friend Two", "timestamp": 1_510_000_000}]}

    def _events(self) -> dict:
        return {"event_responses_v2": {
            "events_joined": [{"name": "Fixture Picnic", "start_timestamp": 1_630_000_000,
                               "end_timestamp": 1_630_010_000, "place": {"name": "Fixture Park"},
                               "description": "Bring food"}],
            "events_interested": [], "events_declined": [],
        }}

    def _thread(self) -> dict:
        return {
            "participants": [{"name": "Friend One"}, {"name": mojibake(self.owner)}],
            "messages": [
                {"sender_name": mojibake(self.owner), "timestamp_ms": 1_650_000_002_000,
                 "content": "See you there", "type": "Generic"},
                {"sender_name": "Friend One", "timestamp_ms": 1_650_000_000_000,
                 "content": mojibake("Ahoj! Přijdeš?"), "type": "Generic"},
            ],
            "title": "Friend One", "is_still_participant": True,
            "thread_path": "inbox/friendone_abc123", "thread_type": "Regular",
        }

    def members(self) -> dict[str, bytes]:
        members = {
            PROFILE_MEMBER: _dumps(self._profile()),
            POSTS_MEMBER: _dumps(self._posts()),
            "your_facebook_activity/posts/album/0.json": _dumps(self._album()),
            "your_facebook_activity/posts/media/Mobile_uploads/photo_2.jpg": b"fixture-photo-2",
            "your_facebook_activity/comments_and_reactions/comments.json": _dumps(self._comments()),
            "your_facebook_activity/comments_and_reactions/likes_and_reactions_1.json": _dumps(self._reactions()),
            "connections/friends/your_friends.json": _dumps(self._friends()),
            "your_facebook_activity/events/your_event_responses.json": _dumps(self._events()),
            "your_facebook_activity/messages/inbox/friendone_abc123/message_1.json": _dumps(self._thread()),
        }
        for spec in self.posts:
            if spec.photo is not None:
                members[PHOTO_PATH] = spec.photo
        return members

    # ── the zip ─────────────────────────────────────────

    def build(self) -> bytes:
        """The archive as bytes. JSON members are Deflated (the real exports
        are), everything else Stored; paths sorted, mtimes fixed, so the same
        spec always yields the same bytes."""
        buf = io.BytesIO()
        with zipfile.ZipFile(buf, "w") as z:
            for name, data in sorted(self.members().items()):
                info = zipfile.ZipInfo(name, date_time=_MTIME)
                info.compress_type = zipfile.ZIP_DEFLATED if name.endswith(".json") else zipfile.ZIP_STORED
                z.writestr(info, data)
        return buf.getvalue()

    def write(self, path: Path) -> Path:
        path = Path(path)
        path.write_bytes(self.build())
        return path
