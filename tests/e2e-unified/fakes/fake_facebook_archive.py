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
# The link the `Friend Two` comment's `external_context` names — the commented
# post's shared link (archive-import.md § Linking and merging, ruling 5's rung
# (iii)). A post sharing it (`LINK_POST`, opt-in) dated before the comment is
# where that comment folds; without one the comment stays unplaced.
COMMENTED_LINK = "https://example.invalid/post/1"
FRIEND_TWO_COMMENT = "Nice one"
FRIEND_TWO_COMMENT_TS = 1_602_000_000
LINK_TEXT = "Worth a read"
LINK_TS = 1_601_500_000
# Phase two (archive-import.md § Linking and merging, ruling 12): the friend's
# OWN archive holds their comment and their like on the owner's link post, both
# naming `COMMENTED_LINK`, so ruling 5's rung (iii) places them. The comment is
# the same record the owner's archive keeps as its private copy (same instant,
# same words — one external id), which is what a claim hides.
FRIEND_LIKE_TS = 1_603_000_000
# The friend's `LOVE` on the owner's link post (`love`, opt-in), which the
# merge maps to `React { emoji: "❤️" }` (archive-import.md ruling 7) — after
# the like, so the two reactions never tie.
FRIEND_LOVE_TS = 1_603_500_000
# The second-import variant's one new comment on the owner's post.
SECOND_COMMENT = "Still a good read"
SECOND_COMMENT_TS = 1_604_000_000
# The third-import variant's one more comment — what an import after an unlink
# must NOT bring back (outcome 19).
THIRD_COMMENT = "Read it again today"
THIRD_COMMENT_TS = 1_605_000_000
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
    # A link the post shared (`attachments[].data[].external_context.url`).
    link: str | None = None


DEFAULT_POSTS = (
    ArchivePostSpec(FRIENDS_TEXT, FRIENDS_TS, "Friends"),
    ArchivePostSpec(PUBLIC_TEXT, PUBLIC_TS, "Public", photo=b"fixture-photo-1"),
    ArchivePostSpec(ONLY_ME_TEXT, ONLY_ME_TS, "Only me"),
)

# The owner's public link post sharing `COMMENTED_LINK`, dated before the
# `Friend Two` comment — opt-in (`posts=[*DEFAULT_POSTS, LINK_POST]`), so the
# journeys that count posts are unchanged.
LINK_POST = ArchivePostSpec(LINK_TEXT, LINK_TS, "Public", link=COMMENTED_LINK)


@dataclass
class FakeFacebookArchive:
    owner: str = "Test Owner"
    tag: str = ""
    # The profile's `username` — a Facebook export carries its owner's. Derived
    # from `owner` when left empty, so two archives naming each other carry two.
    username: str = ""
    # The friend whose comment on the owner's post the owner's archive keeps
    # (its private copy), and the friends list's second name.
    friend_two: str = "Friend Two"
    # Phase two, opt-in: the friend whose link post the owner commented on and
    # liked, in the owner's OWN archive (`_own_comments_on`, `_reactions`).
    # `None` writes neither — every other journey's counts are unchanged.
    activity_on: str | None = None
    # The second-import variant: one more comment by the owner on
    # `activity_on`'s post, after the first.
    second_comment: bool = False
    # The third-import variant: one more again, after the second.
    third_comment: bool = False
    # The owner's `LOVE` on `activity_on`'s link post, beside the like.
    love: bool = False
    # The link every comment and reaction on a link post names — ruling 5's
    # rung (iii) shared link. A journey whose account may import more than once
    # in a session passes its own, so two of its link posts never share one
    # (the rung then refuses as `ambiguous`).
    link: str = COMMENTED_LINK
    # Whether the archive keeps `friend_two`'s comment on the owner's post (the
    # owner's private copy). A friend's OWN archive (`activity_on` set) passes
    # False: that record and the friend's own comment share instant and words,
    # so the one archive would carry one external id twice.
    friend_comment: bool = True
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
            self.posts = list(DEFAULT_POSTS)

    # ── members ─────────────────────────────────────────

    def _username(self) -> str:
        if self.username:
            return self.username
        return ".".join(self.owner.lower().split()) + ".fixture"

    def _profile(self) -> dict:
        return {
            "profile_v2": {
                "name": {"full_name": mojibake(self.owner), "first_name": "Test", "last_name": "Owner"},
                "emails": {"emails": []},
                "registration_timestamp": 1_262_304_000,
                "profile_uri": f"https://www.facebook.com/{self._username()}",
                "username": self._username(),
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
            if spec.link is not None:
                record.setdefault("attachments", []).append(
                    {"data": [{"external_context": {"url": spec.link}}]})
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
        friend = [
            {"timestamp": FRIEND_TWO_COMMENT_TS,
             "data": [{"comment": {"timestamp": FRIEND_TWO_COMMENT_TS, "comment": FRIEND_TWO_COMMENT,
                                   "author": mojibake(self.friend_two)}}],
             "title": mojibake(f"{self.friend_two} commented on {self.owner}'s post."),
             "attachments": [{"data": [{"external_context": {"url": self.link}}]}]},
        ] if self.friend_comment else []
        return {"comments_v2": [
            {"timestamp": 1_601_000_000,
             "data": [{"comment": {"timestamp": 1_601_000_000, "comment": mojibake("Děkuji!"), "author": mojibake(self.owner)}}],
             "title": mojibake(f"{self.owner} commented on Friend One's photo.")},
            *friend,
            *self._own_comments_on(),
        ]}

    def _own_comment(self, text: str, ts: int) -> dict:
        return {"timestamp": ts,
                "data": [{"comment": {"timestamp": ts, "comment": text, "author": mojibake(self.owner)}}],
                "title": mojibake(f"{self.owner} commented on {self.activity_on}'s post."),
                "attachments": [{"data": [{"external_context": {"url": self.link}}]}]}

    def _own_comments_on(self) -> list[dict]:
        """The owner's own comments on `activity_on`'s link post — what a
        linked owner claims back as their own replies."""
        if self.activity_on is None:
            return []
        out = [self._own_comment(FRIEND_TWO_COMMENT, FRIEND_TWO_COMMENT_TS)]
        if self.second_comment:
            out.append(self._own_comment(SECOND_COMMENT, SECOND_COMMENT_TS))
        if self.third_comment:
            out.append(self._own_comment(THIRD_COMMENT, THIRD_COMMENT_TS))
        return out

    def _reactions(self) -> list[dict]:
        out = [
            {"timestamp": 1_603_000_000, "data": [{"reaction": {"reaction": "LIKE", "actor": mojibake(self.owner)}}],
             "title": mojibake(f"{self.owner} likes Friend One's post.")},
        ]
        if self.activity_on is not None:
            # The owner's like on `activity_on`'s link post, naming the link it
            # shared (`parse_reaction` reads the attachment's url), dated after
            # the post so rung (iii) may fire.
            out.append(
                {"timestamp": FRIEND_LIKE_TS,
                 "data": [{"reaction": {"reaction": "LIKE", "actor": mojibake(self.owner)}}],
                 "title": mojibake(f"{self.owner} likes {self.activity_on}'s post."),
                 "attachments": [{"data": [{"external_context": {"url": self.link}}]}]})
            if self.love:
                out.append(
                    {"timestamp": FRIEND_LOVE_TS,
                     "data": [{"reaction": {"reaction": "LOVE", "actor": mojibake(self.owner)}}],
                     "title": mojibake(f"{self.owner} reacted to {self.activity_on}'s post."),
                     "attachments": [{"data": [{"external_context": {"url": self.link}}]}]})
        return out

    def _friends(self) -> dict:
        names = ["Friend One", self.friend_two]
        if self.activity_on is not None and self.activity_on not in names:
            names.append(self.activity_on)
        return {"friends_v2": [{"name": mojibake(n), "timestamp": 1_500_000_000 + 10_000_000 * i}
                               for i, n in enumerate(names)]}

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
