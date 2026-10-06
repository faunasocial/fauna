"""Unit tests for the AP harness's nest-internal state readers (tier_1).

`helpers/ap_nest.py`'s `nest.db` readers are how the real-Mastodon interop suite
asserts the *inbound* half of federation (F5-F9): an ingested reply and the
synthetic upvote/repost a Like/Announce mints are our own rows, invisible to
Mastodon and — deliberately — to the nest's own read APIs too (bridge-ingested
posts are not FTS-indexed, and the AP mint path never bumps engagement counts).

That makes these readers load-bearing *and* silent when wrong: a stale column
name or an off-shape reaction key would surface only as "the flow never
happened" 90 seconds into a Rails-backed run, which is indistinguishable from a
real interop break. So pin them here, against a synthetic DB carrying the same
DDL as the nest (`fauna-bridge-activitypub/src/db.rs`, `db/schema.rs`).
"""

import sqlite3
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from helpers import ap_nest  # noqa: E402

pytestmark = pytest.mark.tier_1

# Mirrors the nest's own DDL for the two tables the readers touch.
_DDL = """
CREATE TABLE ap_post_map (
    fauna_post_id    TEXT NOT NULL,
    ap_url           TEXT NOT NULL,
    actor_id         TEXT NOT NULL,
    created_at       INTEGER NOT NULL,
    tombstoned       INTEGER NOT NULL DEFAULT 0,
    remote_actor_uri TEXT,
    PRIMARY KEY (fauna_post_id, ap_url)
);
CREATE TABLE content (
    id          BLOB PRIMARY KEY,
    schema      TEXT NOT NULL,
    author      BLOB NOT NULL,
    created_at  INTEGER NOT NULL,
    payload     BLOB NOT NULL,
    expires_at  INTEGER,
    source      TEXT NOT NULL DEFAULT 'fauna',
    blob_hash   BLOB
);
CREATE TABLE ap_delivery_queue (
    id               INTEGER PRIMARY KEY,
    activity_json    TEXT NOT NULL,
    target_inbox     TEXT NOT NULL,
    created_at       INTEGER NOT NULL,
    attempts         INTEGER NOT NULL DEFAULT 0,
    next_retry_at    INTEGER NOT NULL,
    status           TEXT NOT NULL DEFAULT 'pending'
);
"""

NOTE_URL = "https://nest.test/ap/users/bob/notes/ab12"
ACTOR = "https://mastodon.test/users/alice"
LOCAL_POST = "aa" * 32
LIKE_POST = "bb" * 32
ANNOUNCE_POST = "cc" * 32


@pytest.fixture
def nest(tmp_path):
    """A synthetic `nest.db` with one pushed note, one live Like, one undone boost."""
    db_path = tmp_path / "nest.db"
    conn = sqlite3.connect(db_path)
    conn.executescript(_DDL)
    conn.executemany(
        "INSERT INTO ap_post_map VALUES (?,?,?,?,?,?)",
        [
            # The Create-push's own row for a local note (no remote owner).
            (LOCAL_POST, NOTE_URL, "localactor", 1, 0, None),
            # A live synthetic upvote, and a boost already retracted by an Undo.
            (LIKE_POST, f"like:{ACTOR}:{NOTE_URL}", "synth", 2, 0, ACTOR),
            (ANNOUNCE_POST, f"announce:{ACTOR}:{NOTE_URL}", "synth", 3, 1, ACTOR),
        ],
    )
    # Only the live reaction still has a content projection — the retraction
    # withdrew the boost's.
    conn.execute(
        "INSERT INTO content VALUES (?,?,?,?,?,?,?,?)",
        (bytes.fromhex(LIKE_POST), "post", b"x", 2, b"", None, "activitypub", None),
    )
    conn.commit()
    conn.close()
    return {"db_path": str(db_path)}


def test_reaction_map_key_matches_the_nest_key_shape():
    """`{verb}:{actor}:{object}` — not the activity id, which is what makes a
    replayed reaction idempotent and gives Undo a stable lookup."""
    assert ap_nest.reaction_map_key("like", ACTOR, NOTE_URL) == f"like:{ACTOR}:{NOTE_URL}"
    assert (
        ap_nest.reaction_map_key("announce", ACTOR, NOTE_URL)
        == f"announce:{ACTOR}:{NOTE_URL}"
    )


def test_reaction_map_key_rejects_an_unknown_verb():
    with pytest.raises(AssertionError):
        ap_nest.reaction_map_key("boost", ACTOR, NOTE_URL)


def test_local_note_url_resolves_the_pushed_note(nest):
    assert ap_nest.local_note_url(nest, LOCAL_POST) == NOTE_URL


def test_local_note_url_is_none_for_an_unmapped_post(nest):
    assert ap_nest.local_note_url(nest, "dd" * 32) is None


def test_ap_post_map_row_reads_every_column(nest):
    row = ap_nest.ap_post_map_row(nest, f"like:{ACTOR}:{NOTE_URL}")
    assert row == {
        "fauna_post_id": LIKE_POST,
        "actor_id": "synth",
        "created_at": 2,
        "tombstoned": False,
        "remote_actor_uri": ACTOR,
    }


def test_ap_post_map_row_is_none_when_absent(nest):
    assert ap_nest.ap_post_map_row(nest, "like:nobody:nothing") is None


def test_reaction_rows_select_by_verb_and_object(nest):
    likes = ap_nest.reaction_rows_for_object(nest, "like", NOTE_URL)
    assert [r["fauna_post_id"] for r in likes] == [LIKE_POST]
    boosts = ap_nest.reaction_rows_for_object(nest, "announce", NOTE_URL)
    assert [r["fauna_post_id"] for r in boosts] == [ANNOUNCE_POST]


def test_reaction_rows_report_the_tombstone_state(nest):
    """The Undo half: a retracted reaction stays readable, marked tombstoned."""
    assert ap_nest.reaction_rows_for_object(nest, "like", NOTE_URL)[0]["tombstoned"] is False
    assert (
        ap_nest.reaction_rows_for_object(nest, "announce", NOTE_URL)[0]["tombstoned"]
        is True
    )


def test_reaction_rows_do_not_match_the_pushed_note_row(nest):
    """The local note's own map row is keyed on the bare URL — it must never be
    mistaken for a reaction against itself."""
    for verb in ("like", "announce"):
        rows = ap_nest.reaction_rows_for_object(nest, verb, NOTE_URL)
        assert all(r["fauna_post_id"] != LOCAL_POST for r in rows)


def test_reaction_rows_do_not_match_a_different_object(nest):
    assert ap_nest.reaction_rows_for_object(nest, "like", "https://nest.test/other") == []


def test_reaction_rows_do_not_match_a_url_that_merely_contains_the_object(nest):
    """Suffix-matching is anchored on the `:` separator, so a longer object URL
    ending in ours does not collide."""
    assert ap_nest.reaction_rows_for_object(nest, "like", "notes/ab12") == []


def test_content_row_exists_tracks_the_projection(nest):
    assert ap_nest.content_row_exists(nest, LIKE_POST) is True
    # The retracted boost's projection was withdrawn with its map-row tombstone.
    assert ap_nest.content_row_exists(nest, ANNOUNCE_POST) is False



def test_delivery_jobs_for_reads_one_inbox_in_every_status_oldest_first(nest):
    """The full record of what was queued for one inbox: delivered (`done`),
    dead (`failed`) and still-pending jobs alike, and no other inbox's."""
    inbox = "https://mastodon.test/users/alice/inbox"
    conn = sqlite3.connect(nest["db_path"])
    conn.executemany(
        "INSERT INTO ap_delivery_queue "
        "(activity_json, target_inbox, created_at, next_retry_at, status) "
        "VALUES (?,?,?,?,?)",
        [
            ('{"type":"Accept"}', inbox, 1, 1, "done"),
            ('{"type":"Create"}', "https://other.test/inbox", 2, 2, "done"),
            ('{"type":"Create"}', inbox, 3, 3, "failed"),
            ('{"type":"Delete"}', inbox, 4, 4, "pending"),
        ],
    )
    conn.commit()
    conn.close()
    assert ap_nest.delivery_jobs_for(nest, inbox) == [
        '{"type":"Accept"}',
        '{"type":"Create"}',
        '{"type":"Delete"}',
    ]
    assert ap_nest.delivery_jobs_for(nest, "https://nobody.test/inbox") == []
