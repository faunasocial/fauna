"""A generated Instagram JSON-export archive for tier_3 journeys
(`docs/goal/behavior/archive-import.md` § Parser contract rule 7: fixtures are
generated, never real). The member paths are the ones
`libs/fauna-archive/src/instagram/layout.rs` locates by name; the JSON shapes
are the ones `instagram/{content,social}.rs` decode, and the direct-message
thread is the Messenger shape both Meta exports share. Meta escapes every
non-ASCII string byte-wise here exactly as in the Facebook export, so this
reuses that generator's `mojibake`.

Every name is fictional. The "photos" are a few bytes of text -- the parser
hashes bytes and maps MIME from the extension, it never decodes images.
"""

from __future__ import annotations

import io
import json
import zipfile
from dataclasses import dataclass, field
from pathlib import Path

from fake_facebook_archive import _MTIME, mojibake

CAFE_TEXT = "Café morning"
LAKE_TEXT = "At the lake"
ARCHIVED_TEXT = "Archived note"
CAFE_TS = 1_600_000_000
LAKE_TS = 1_610_000_000
ARCHIVED_TS = 1_620_000_000
OWNER_USERNAME = "test.owner.fixture"
PROFILE_MEMBER = "personal_information/personal_information/personal_information.json"
# Where the profile photo rides when `profile_photo` is set (a PNG under the
# export's own media layout); `instagram::content` reads it off the profile's
# "Profile Photo" media entry.
PROFILE_PHOTO_MEMBER = "media/profile/202001/profile_1.png"
OWNER_BIO = "Fixture bio ❤"
OWNER_WEBSITE = "https://example.invalid/owner"
POSTS_MEMBER = "your_instagram_activity/content/posts_1.json"
ARCHIVED_MEMBER = "your_instagram_activity/content/archived_posts.json"
# The reason `instagram::content::parse_post` gives a record with no
# timestamp -- what the skip log names when `bad_record` is on.
BAD_RECORD_REASON = "post has no creation_timestamp"
# Members a real export carries that Fauna never imports (stories, ads);
# their text is a marker the journeys must never find.
NEVER_IMPORTED_TEXT = "fixture-never-imported"
NEVER_IMPORTED_MEMBERS = (
    "your_instagram_activity/content/stories.json",
    "ads_information/instagram_ads_and_businesses/advertisers_using_your_activity_or_information.json",
)


def _dumps(value) -> bytes:
    return json.dumps(value, ensure_ascii=True, indent=2).encode("ascii")


@dataclass(frozen=True)
class InstagramPostSpec:
    text: str
    timestamp: int
    photos: tuple[bytes, ...] = (b"fixture-ig-photo",)


@dataclass
class FakeInstagramArchive:
    owner: str = "Test Owner"
    tag: str = ""
    posts: list[InstagramPostSpec] = field(default_factory=list)
    # One post the owner archived on Instagram (hidden from everyone): it
    # imports visible to the owner alone.
    archived: InstagramPostSpec | None = None
    bad_record: bool = False
    # The profile photo's bytes (a real image, so an app can paint it), or
    # None for a profile with no photo.
    profile_photo: bytes | None = None

    def __post_init__(self) -> None:
        if not self.posts:
            self.posts = [
                InstagramPostSpec(CAFE_TEXT, CAFE_TS, (b"fixture-ig-photo-1",)),
                InstagramPostSpec(LAKE_TEXT, LAKE_TS, (b"fixture-ig-photo-2", b"fixture-ig-photo-3")),
            ]
        if self.archived is None:
            self.archived = InstagramPostSpec(ARCHIVED_TEXT, ARCHIVED_TS, (b"fixture-ig-photo-4",))

    # ── members ─────────────────────────────────────────

    def _profile(self) -> dict:
        entry = lambda value: {"href": "", "value": value, "timestamp": 0}  # noqa: E731
        media = {}
        if self.profile_photo is not None:
            media["Profile Photo"] = {"uri": PROFILE_PHOTO_MEMBER, "creation_timestamp": CAFE_TS, "title": ""}
        return {"profile_user": [{
            "media_map_data": media,
            "string_map_data": {
                "Username": entry(OWNER_USERNAME),
                "Name": entry(mojibake(self.owner)),
                "Bio": entry(mojibake(OWNER_BIO)),
                "Website": entry(OWNER_WEBSITE),
            },
        }]}

    def _media_path(self, kind: str, spec: InstagramPostSpec, k: int) -> str:
        return f"media/{kind}/{spec.timestamp}/photo_{k + 1}.jpg"

    def _record(self, kind: str, spec: InstagramPostSpec) -> dict:
        """One photo: caption and date on the photo; several: on the post."""
        caption = mojibake(spec.text + self.tag)
        media = [
            {"uri": self._media_path(kind, spec, k), "creation_timestamp": spec.timestamp, "title": ""}
            for k in range(len(spec.photos))
        ]
        if len(media) == 1:
            media[0]["title"] = caption
            return {"media": media}
        return {"media": media, "title": caption, "creation_timestamp": spec.timestamp}

    def _posts(self) -> list[dict]:
        out = [self._record("posts", spec) for spec in self.posts]
        if self.bad_record:
            out.append({"media": [], "title": "A record with no timestamp"})
        return out

    def _thread(self) -> dict:
        return {
            "participants": [{"name": "Friend One"}, {"name": mojibake(self.owner)}],
            "messages": [
                {"sender_name": "Friend One", "timestamp_ms": 1_650_000_000_000,
                 "content": mojibake("Ahoj! Přijdeš?")},
            ],
            "title": "Friend One", "is_still_participant": True,
            "thread_path": "inbox/friendone_17840000000000001",
        }

    def members(self) -> dict[str, bytes]:
        members = {
            PROFILE_MEMBER: _dumps(self._profile()),
            POSTS_MEMBER: _dumps(self._posts()),
            ARCHIVED_MEMBER: _dumps({"ig_archived_post_media": [self._record("archived_posts", self.archived)]}),
            "your_instagram_activity/likes/liked_posts.json": _dumps({"likes_media_likes": [
                {"title": "friend.one", "string_list_data": [
                    {"href": "https://www.instagram.com/p/FIXTURE1/", "value": "👍", "timestamp": 1_603_000_000}]},
            ]}),
            "connections/followers_and_following/followers_1.json": _dumps([
                {"title": "", "media_list_data": [], "string_list_data": [
                    {"href": "https://www.instagram.com/friend.one", "value": "friend.one", "timestamp": 1_500_000_000}]},
            ]),
            "your_instagram_activity/messages/inbox/friendone_17840000000000001/message_1.json": _dumps(self._thread()),
        }
        for kind, specs in (("posts", self.posts), ("archived_posts", [self.archived])):
            for spec in specs:
                for k, photo in enumerate(spec.photos):
                    members[self._media_path(kind, spec, k)] = photo
        if self.profile_photo is not None:
            members[PROFILE_PHOTO_MEMBER] = self.profile_photo
        for name in NEVER_IMPORTED_MEMBERS:
            members[name] = _dumps({"entries": [{"title": NEVER_IMPORTED_TEXT, "creation_timestamp": LAKE_TS}]})
        return members

    # ── the zip ─────────────────────────────────────────

    def build(self) -> bytes:
        """The archive as bytes: JSON Deflated, everything else Stored, paths
        sorted, mtimes fixed -- the same spec always yields the same bytes."""
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
