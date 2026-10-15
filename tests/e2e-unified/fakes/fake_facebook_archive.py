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
HTML_PROFILE_MEMBER = "personal_information/profile_information/profile_information.html"
# The reason `facebook::posts::parse_post` gives a record with no timestamp —
# what the skip log names when `bad_record` is on.
BAD_RECORD_REASON = "post has no timestamp"
# Members every real export carries that Fauna never imports (§ What each
# category becomes: ad interests, login history, saved items, liked pages),
# plus one from a layout newer than the parser knows (§ Parser contract rule
# 2: an unknown file is ignored, never a refusal). Their text is a marker the
# journeys can look for and must never find.
NEVER_IMPORTED_TEXT = "fixture-never-imported"
NEVER_IMPORTED_MEMBERS = (
    "ads_information/other_categories_used_to_reach_you.json",
    "security_and_login_information/login_and_logout.json",
    "your_facebook_activity/saved_items_and_collections/your_saved_items.json",
    "your_facebook_activity/pages/pages_you_ve_liked.json",
    "your_facebook_activity/a_section_from_a_newer_export/whatever_it_holds.json",
)

PADDING_MEMBER = "your_facebook_activity/a_section_from_a_newer_export/padding.bin"

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


# `reply_events`' extra events: answer → (title, start). Each lands on a
# different day so the calendar lists them apart.
REPLY_EVENTS = {
    "declined": ("Fixture Concert", 1_631_000_000),
    "interested": ("Fixture Market", 1_632_000_000),
    "hosted": ("Fixture Party", 1_633_000_000),
}


def _reply_event(answer: str) -> dict:
    title, start = REPLY_EVENTS[answer]
    return {"name": title, "start_timestamp": start, "end_timestamp": start + 3_600}


@dataclass(frozen=True)
class ArchivePostSpec:
    text: str
    timestamp: int
    privacy: str | None
    photo: bytes | None = None
    # How many times the record names its photo — past the parser's
    # `MAX_RECORD_MEDIA_REFS` (512) the tail is dropped and reported.
    photo_refs: int = 1


@dataclass
class FakeFacebookArchive:
    owner: str = "Test Owner"
    tag: str = ""
    posts: list[ArchivePostSpec] = field(default_factory=list)
    # One post record the parser cannot read (no timestamp) — skipped and
    # listed, never failing the import (§ Parser contract rule 1).
    bad_record: bool = False
    # Bytes for one more member no parser reads (stored, so the zip grows by
    # exactly this much) — what makes the archive's own size observable.
    padding: bytes = b""
    # One more event per other own answer — declined, interested, hosted —
    # beside the joined picnic (`REPLY_EVENTS`), so the owner's own reply is
    # observable per answer. Off by default: every event is one more record
    # the other journeys would count.
    reply_events: bool = False

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
                ] * spec.photo_refs
            out.append(record)
        if self.bad_record:
            out.append({"data": [{"post": "A record with no timestamp"}],
                        "title": mojibake(f"{self.owner} updated his status.")})
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
        extra = self.reply_events
        return {"event_responses_v2": {
            "events_joined": [{"name": "Fixture Picnic", "start_timestamp": 1_630_000_000,
                               "end_timestamp": 1_630_010_000, "place": {"name": "Fixture Park"},
                               "description": "Bring food"}],
            "events_interested": [_reply_event("interested")] if extra else [],
            "events_declined": [_reply_event("declined")] if extra else [],
        }}

    def _hosted_events(self) -> dict:
        return {"your_events_v2": [_reply_event("hosted")]}

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
        for name in NEVER_IMPORTED_MEMBERS:
            members[name] = _dumps({"entries": [{"text": NEVER_IMPORTED_TEXT, "timestamp": PUBLIC_TS}]})
        if self.padding:
            members[PADDING_MEMBER] = self.padding
        if self.reply_events:
            members["your_facebook_activity/events/your_events.json"] = _dumps(self._hosted_events())
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


def _zip(members: dict[str, bytes]) -> bytes:
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as z:
        for name, data in sorted(members.items()):
            z.writestr(zipfile.ZipInfo(name, date_time=_MTIME), data)
    return buf.getvalue()


def write_html_export(path: Path) -> Path:
    """A Facebook export requested in HTML rather than JSON — the Archive step
    refuses it, naming the format to request (§ Parser contract rule 5)."""
    path = Path(path)
    path.write_bytes(_zip({HTML_PROFILE_MEMBER: b"<html><body>Test Owner</body></html>"}))
    return path


def write_unrelated_zip(path: Path) -> Path:
    """A zip no supported service exported — refused at the Archive step."""
    path = Path(path)
    path.write_bytes(_zip({"notes/todo.json": b"[]"}))
    return path
