"""tier_3 nest witnesses that the personalization planes rest SEALED: the nest
never learns a trained topic's name, what it learned or which posts were marked
(``topic-factors.md`` § At rest — seal + home), nor what an app collected about
the user's engagement (``engagement-cues.md`` § At rest — the sealed cue
rollup).

Both outcomes are client-kept, so they are witnessed app-less through the e2e
harness library (``feature-catalog.md`` § The two surfaces): the writes run the
apps' own shared-Rust funnels — ``create_trained_topic`` over a hosted account
store, ``FeedManager::train_post``, the manager's cue hydrate → observe → flush
path — over the real wire, and the nest is read from outside, through its own
SQLite file and the owner's fetch. The owner-side opens prove the opaque blob is
the user's own data, not noise the nest was handed instead.

The same planes are the input a published model must NOT carry: the last
witness publishes a topic through the apps' own rebuild and publish
(``topic-factors.md`` § Publishing a trained factor) and pins that the artifact
the nest serves is the one its explicit-public-only twin publishes.
"""

import secrets
import sqlite3
import time
from pathlib import Path

import cbor2
import pytest

import fauna_ffi
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register
from helpers.blob_upload import SEALED_MIME, upload_blob
from tests.api import ws_api
from tests.api.bare import sign_and_encode_post

pytestmark = pytest.mark.tier_3

#: The kind-chunk form byte every sealed personalization blob opens with
#: (``topic-factors.md`` § At rest: ``[0x02][nonce][ct+tag]``).
SEALED_FORM = 0x02
CUES_FACTOR = "cues:v1"


def _model_row(db_path: str, actor_id: bytes, factor: str) -> list[bytes]:
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        rows = conn.execute(
            "SELECT sealed_blob FROM personalization_models WHERE actor_id = ? AND factor = ?",
            (actor_id, factor),
        ).fetchall()
    finally:
        conn.close()
    return [bytes(r[0]) for r in rows]


def _nest_files(db_path: str) -> dict[str, bytes]:
    """The nest's whole database as bytes on disk — the main file and its WAL,
    where a fresh write rests until a checkpoint."""
    files = {}
    for suffix in ("", "-wal"):
        path = Path(db_path + suffix)
        if path.exists():
            files[path.name] = path.read_bytes()
    return files


def _assert_opaque(blob: bytes, forbidden: dict[str, bytes], what: str) -> None:
    assert blob and blob[0] == SEALED_FORM, (
        f"{what}: expected the sealed kind-chunk form byte {SEALED_FORM:#04x}, "
        f"got {blob[:1].hex() or 'an empty blob'}"
    )
    leaked = [label for label, needle in forbidden.items() if needle in blob]
    assert not leaked, f"{what} rests on the nest with plaintext inside it: {leaked}"


def _two_posts(port: int, author: dict, tag: str) -> list[tuple[str, str]]:
    posts = []
    for i, word in enumerate(("marmalade", "tectonics")):
        text = f"{word}{tag} {word}{tag} distinctive post {i}"
        now_us = int(time.time() * 1_000_000) + i
        post_id = ws_api.create_post(
            port, author, sign_and_encode_post(author["signing_key"], now_us, text, tags=[])
        )
        posts.append((post_id, f"{word}{tag}"))
    return posts


@pytest.mark.feature("trained-topics")
def test_a_trained_topic_rests_on_the_nest_with_no_name_no_marks_and_no_text(nest_instance):
    """`trained-topics` outcome 15: your nest never learns your topics' names,
    what they contain, or which posts you marked.

    A topic is minted and two public posts marked through the apps' own
    funnels. The nest's model row is one sealed blob holding neither post id,
    neither post's words, nor the name; the name appears nowhere in the nest's
    database at all (its only nest-side home is the sealed registry row, which
    the creating export proved reached the nest by reading it back through a
    fresh seat). The owner opens the fetched blob to exactly the marks made."""
    port, url, db_path = nest_instance["port"], nest_instance["url"], nest_instance["db_path"]
    owner = create_actor_and_register(port, admin_signing_key=nest_instance["admin"]["signing_key"])
    seed = bytes(owner["signing_key"])
    tag = secrets.token_hex(4)
    name = f"quietly-private-topic-{secrets.token_hex(6)}"
    (more_id, more_word), (less_id, less_word) = _two_posts(port, owner, tag)

    factor = fauna_ffi.harness_topic_create(url, seed, name)
    assert factor.startswith("topic:"), f"not a topic composition key: {factor!r}"
    try:
        assert fauna_ffi.harness_topic_train(url, seed, factor, more_id, "more-like-this") == "trained"
        assert fauna_ffi.harness_topic_train(url, seed, factor, less_id, "less-like-this") == "trained"

        rows = _model_row(db_path, owner["actor_id_bytes"], factor)
        assert len(rows) == 1, f"expected one model row for {factor}, found {len(rows)}"
        blob = rows[0]
        _assert_opaque(
            blob,
            {
                "the topic name": name.encode(),
                "the marked post's id (hex)": more_id.encode(),
                "the marked post's id (raw)": bytes.fromhex(more_id),
                "the other post's id (hex)": less_id.encode(),
                "the other post's id (raw)": bytes.fromhex(less_id),
                "the marked post's words": more_word.encode(),
                "the other post's words": less_word.encode(),
                "a verb": b"MoreLikeThis",
            },
            f"the {factor} model row",
        )
        fetched = ws_api.personalization_model_fetch(port, owner, factor)
        assert bytes(fetched["sealed_blob"]) == blob, (
            "the owner's fetch must return the stored blob byte for byte"
        )

        holders = [f for f, raw in _nest_files(db_path).items() if name.encode() in raw]
        assert not holders, f"the topic's name rests in plaintext in the nest's {holders}"

        opened = fauna_ffi.personalization_open_topic_model(seed, bytes(fetched["sealed_blob"]))
        assert opened["markers"] == {more_id: "more-like-this", less_id: "less-like-this"}, (
            f"the sealed model does not hold the owner's marks: {opened!r}"
        )
        assert opened["example_count"] == 2, opened
    finally:
        ws_api.personalization_model_delete(port, owner, factor)


@pytest.mark.feature("learn-from-my-activity")
def test_the_engagement_rollup_rests_on_the_nest_only_sealed(nest_instance):
    """`learn-from-my-activity` outcome 10: your nest keeps what your app
    collected about your activity only sealed, in a form its owner cannot read.

    Two verdicts — one item watched to completion, one skipped — are folded
    and flushed through the feed manager's own cue path. The nest's `cues:v1`
    row is one sealed blob holding neither content id nor either verdict word,
    no verdict word rests anywhere in the nest's database, and the owner opens
    the fetched blob to exactly those two verdicts."""
    port, url, db_path = nest_instance["port"], nest_instance["url"], nest_instance["db_path"]
    owner = create_actor_and_register(port, admin_signing_key=nest_instance["admin"]["signing_key"])
    seed = bytes(owner["signing_key"])
    (watched, _), (skipped, _) = _two_posts(port, owner, secrets.token_hex(4))

    recorded = fauna_ffi.harness_cues_record_and_flush(
        url, seed, [(watched, "watch-complete"), (skipped, "skip")]
    )
    assert recorded == 2
    try:
        rows = _model_row(db_path, owner["actor_id_bytes"], CUES_FACTOR)
        assert len(rows) == 1, f"expected one {CUES_FACTOR} row, found {len(rows)}"
        blob = rows[0]
        _assert_opaque(
            blob,
            {
                "the watched item's id": watched.encode(),
                "the skipped item's id": skipped.encode(),
                "the watched item's id (raw)": bytes.fromhex(watched),
                "the skipped item's id (raw)": bytes.fromhex(skipped),
                "the verdict watch-complete": b"watch-complete",
                "the verdict skip": b'"skip"',
            },
            f"the {CUES_FACTOR} row",
        )
        fetched = ws_api.personalization_model_fetch(port, owner, CUES_FACTOR)
        assert bytes(fetched["sealed_blob"]) == blob, (
            "the owner's fetch must return the stored blob byte for byte"
        )

        holders = [f for f, raw in _nest_files(db_path).items() if b"watch-complete" in raw]
        assert not holders, f"an engagement verdict rests in plaintext in the nest's {holders}"

        opened = fauna_ffi.personalization_open_cue_rollup(seed, bytes(fetched["sealed_blob"]))
        assert opened == {watched: "watch-complete", skipped: "skip"}, (
            f"the sealed rollup does not hold the owner's verdicts: {opened!r}"
        )
    finally:
        ws_api.personalization_model_delete(port, owner, CUES_FACTOR)


def _post(port: int, author: dict, text: str, i: int) -> str:
    now_us = int(time.time() * 1_000_000) + i
    return ws_api.create_post(
        port, author, sign_and_encode_post(author["signing_key"], now_us, text, tags=[])
    )


def _gated_posts(nest_instance, author: dict, preview: str, sealed_body: str) -> list[str]:
    """Two posts restricted to a fresh tier, each with its full body sealed into
    the blob store — the shape every app's gated compose submits."""
    port = nest_instance["port"]
    secret = bytes(author["signing_key"])
    period_key = secrets.token_bytes(32)
    tier = f"gold{secrets.token_hex(3)}"
    posts = []
    with _ws(nest_instance, author) as ws:
        ws.call(
            "fauna.subscriptions.tiers.create",
            {
                "name": tier,
                "rank": 2,
                "description": None,
                "price_hint": "5 EUR / month",
                "payment_url": "https://pay.example/gold",
                "auto_approve": False,
                "encrypted_upload": fauna_ffi.build_tier_birth_upload(secret, tier, period_key),
            },
        )
    for i in range(2):
        body, sealed = fauna_ffi.build_gated_post(
            secret, f"{preview} {i}", f"{preview} {i}\n\n{sealed_body}", tier, 2,
            secrets.token_bytes(32), period_key,
        )
        upload_blob(
            port, author["token"], sealed,
            audience_class="PeriodRestrictedPost", mime=SEALED_MIME,
        )
        with _ws(nest_instance, author) as ws:
            posts.append(ws.call("fauna.posts.create", {"body": body})["post_id"])
    return posts


def _ws(nest_instance, actor):
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


def _labeler_row(db_path: str, labeler_id: bytes) -> tuple[bytes, bytes]:
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        row = conn.execute(
            "SELECT metadata_blob, wasm_bytes FROM labelers WHERE labeler_id = ?",
            (labeler_id,),
        ).fetchone()
    finally:
        conn.close()
    assert row, f"no labelers row for {labeler_id.hex()}"
    return bytes(row[0]), bytes(row[1])


@pytest.mark.feature("trained-topics")
def test_a_published_model_is_built_only_from_marked_public_posts(nest_instance):
    """`trained-topics` outcome 24: a published model is built only from your
    marked public posts, never from restricted posts or from what you lingered
    on (`topic-factors.md` § Publishing a trained factor — the rebuild).

    Topic F1 learns from activity, is marked *more like this* on four public
    posts and two restricted ones, and is taught two lingered posts by
    engagement; its twin F2 is marked on the four public posts only. Both are
    published through the apps' own rebuild and publish under one public name,
    and the artifacts the nest serves are BYTE-IDENTICAL — the structural pin
    that nothing past the explicit public examples reached F1's. No restricted
    or lingered word is among its n-grams, and none rests in F1's labeler row;
    the sealed full body of the restricted posts and the topics' private names
    rest nowhere in the nest's database at all. (The lingered posts are public
    and the restricted posts' previews are plaintext by design, so those words
    legitimately sit in the posts table.)"""
    port, url, db_path = nest_instance["port"], nest_instance["url"], nest_instance["db_path"]
    owner = create_actor_and_register(port, admin_signing_key=nest_instance["admin"]["signing_key"])
    seed = bytes(owner["signing_key"])
    tag = secrets.token_hex(4)
    shared = f"zephyrine{tag} quillwork{tag}"
    gated_preview, gated_sealed, lingered_word = (
        f"gatedpreview{tag}", f"gatedsealed{tag}", f"lingered{tag}",
    )
    names = {f"f1-private-{secrets.token_hex(6)}", f"f2-private-{secrets.token_hex(6)}"}
    f1_name, f2_name = sorted(names)
    public_name = f"public model {tag}"

    public = [_post(port, owner, f"{shared} public note {i}", i) for i in range(4)]
    restricted = _gated_posts(
        nest_instance, owner, f"{shared} {gated_preview}", f"{gated_sealed} {gated_sealed}"
    )
    lingered = [_post(port, owner, f"{shared} {lingered_word} {i}", 10 + i) for i in range(2)]

    f1 = fauna_ffi.harness_topic_create(url, seed, f1_name)
    f2 = fauna_ffi.harness_topic_create(url, seed, f2_name)
    try:
        assert fauna_ffi.harness_topic_set_engagement(url, seed, f1, True) is True
        for post in public + restricted:
            assert fauna_ffi.harness_topic_train(url, seed, f1, post, "more-like-this") == "trained"
        for post in public:
            assert fauna_ffi.harness_topic_train(url, seed, f2, post, "more-like-this") == "trained"
        for n, post in enumerate(lingered, start=1):
            assert fauna_ffi.harness_topic_train_engagement(
                url, seed, f1, post, "watch-complete") == n

        # The control: F1 really holds what the artifact must ignore.
        f1_blob = _model_row(db_path, owner["actor_id_bytes"], f1)
        assert len(f1_blob) == 1, f"expected one model row for {f1}"
        opened = fauna_ffi.personalization_open_topic_model(seed, f1_blob[0])
        assert set(opened["markers"]) == set(public + restricted), opened
        assert (opened["example_count"], opened["engagement_count"]) == (6, 2), opened

        p1 = fauna_ffi.harness_topic_publish_model(url, seed, f1, public_name)
        p2 = fauna_ffi.harness_topic_publish_model(url, seed, f2, public_name)
        assert (p1["marked_examples"], p1["included_examples"]) == (6, 4), (
            f"the rebuild must walk F1's six marks and keep only the four public ones: {p1!r}"
        )
        assert (p1["more_docs"], p1["less_docs"]) == (4, 0), p1
        assert p1["labeler_id"] != p2["labeler_id"], "each topic publishes as its own labeler"

        l1, l2 = bytes.fromhex(p1["labeler_id"]), bytes.fromhex(p2["labeler_id"])
        with _ws(nest_instance, owner) as ws:
            a1 = ws.call("fauna.labelers.inspect", {"labeler_id": l1})
            a2 = ws.call("fauna.labelers.inspect", {"labeler_id": l2})
        assert a1["artifact_kind"] == a2["artifact_kind"] == "text-model", (a1, a2)
        artifact = bytes(a1["wasm_bytes"])
        assert artifact == bytes(a2["wasm_bytes"]), (
            "F1's artifact differs from its explicit-public-only twin's: something past "
            f"the public marks reached it.\nF1: {cbor2.loads(artifact)!r}\n"
            f"F2: {cbor2.loads(bytes(a2['wasm_bytes']))!r}"
        )

        decoded = cbor2.loads(artifact)
        ngrams = [n["ngram"] for n in decoded["ngrams"]]
        assert decoded["more_docs"] == 4 and decoded["less_docs"] == 0, decoded
        assert f"zephyrine{tag}" in ngrams, f"the shared public vocabulary is missing: {ngrams!r}"
        forbidden = {
            "the restricted posts' preview word": gated_preview,
            "the restricted posts' sealed word": gated_sealed,
            "the lingered posts' word": lingered_word,
        }
        leaked = [label for label, w in forbidden.items() if any(w in n for n in ngrams)]
        assert not leaked, f"the published n-grams carry {leaked}: {ngrams!r}"

        needles = {label: w.encode() for label, w in forbidden.items()}
        needles |= {f"post id {p}": p.encode() for p in restricted + lingered}
        needles |= {f"raw post id {p}": bytes.fromhex(p) for p in restricted + lingered}
        needles |= {"F1's private name": f1_name.encode()}
        metadata, stored = _labeler_row(db_path, l1)
        for what, raw in (("metadata", metadata), ("artifact", stored)):
            hits = [label for label, needle in needles.items() if needle in raw]
            assert not hits, f"F1's labeler row {what} carries {hits}"
        assert stored == artifact, "the nest serves the artifact it stores"

        _assert_opaque(
            _model_row(db_path, owner["actor_id_bytes"], f1)[0], needles, f"the {f1} model row"
        )

        never = {"the sealed full body": gated_sealed.encode()}
        never |= {f"the private name {n}": n.encode() for n in names}
        holders = {
            label: f for f, raw in _nest_files(db_path).items()
            for label, needle in never.items() if needle in raw
        }
        assert not holders, f"plaintext that must never rest on the nest does: {holders}"
    finally:
        for factor in (f1, f2):
            ws_api.personalization_model_delete(port, owner, factor)
