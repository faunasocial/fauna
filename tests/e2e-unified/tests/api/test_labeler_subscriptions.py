"""tier_3 nest-tier witnesses for community-labeler subscriptions
(``content-moderation-and-ranking.md`` § Tier-3 community models & background
re-processing).

Real ``fauna-nest`` binary + real WS-RPC; no app. Publishing is the publisher's
act no app offers in v1, so it is fixture setup (convention 8 carve-out (b),
``helpers/labeler_publish.py``); the subscription and the posts are the wire
acts under test. What the nest keeps is read from its own SQLite file, the
outside observer the scoring bus has.
"""

import secrets
import sqlite3
import time

import cbor2
import pytest

from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register, register_same_actor_on
from helpers.labeler_publish import FIXTURES, new_publisher, publish_labeler_version
from tests.api import ws_api
from tests.api.bare import sign_and_encode_post

pytestmark = pytest.mark.tier_3


def _ws(nest_instance, actor):
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=actor["actor_id_bytes"],
        signing_key=bytes(actor["signing_key"]),
    )


def _rows_for(db_path: str, sql: str, factor: str) -> int:
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        return conn.execute(sql, (factor,)).fetchone()[0]
    finally:
        conn.close()


@pytest.mark.feature("community-labelers")
def test_a_text_model_subscription_leaves_the_nest_nothing_to_score_with(
    nest_instance, run_seal_helper,
):
    """`community-labelers` outcome 15: a text-model labeler you subscribe to
    scores posts only on your own device; your nest never scores your posts
    with it (§ Tier-3 → *Artifact kinds*, `text-model`: subscribe records the
    subscription row ONLY — no `content_scores` rows, no `model_versions`
    entry, no drain, no grant).

    The subscriber subscribes, then a public post that is all vocabulary
    arrives. Both acts are synchronous in their handlers — the place a List's
    rows are written — so the absence of any row for the factor afterwards is
    the nest's decision, not a race. A subscription that tries to hand the nest
    a grant for the model is refused outright.
    """
    port = nest_instance["port"]
    db_path = nest_instance["db_path"]
    admin_sk = nest_instance["admin"]["signing_key"]

    artifact = run_seal_helper(
        "encode-text-model-artifact",
        {
            "version": 1,
            "name": "kittens",
            # Clears TEXT_MODEL_PUBLISH_MIN_DOCS, the nest's structural privacy floor.
            "more_docs": 3,
            "less_docs": 0,
            "ngrams": [{"ngram": "kittens", "more": 3, "less": 0}],
        },
    )
    labeler_id, pub_seed = new_publisher(nest_instance)
    publish_labeler_version(
        run_seal_helper, nest_instance["url"], labeler_id, pub_seed, artifact,
        version=1, content_kind="post", artifact_kind="text-model",
    )
    factor = "labeler:" + labeler_id.hex()

    subscriber = create_actor_and_register(port, admin_signing_key=admin_sk)
    author = create_actor_and_register(port, admin_signing_key=admin_sk)

    # A subscription naming a grant is refused: the model is client-evaluated,
    # so there is nothing on the nest a grant could license.
    with _ws(nest_instance, subscriber) as ws:
        with pytest.raises(RpcCallError) as excinfo:
            ws.call(
                "fauna.labelers.subscribe",
                {"labeler_id": labeler_id, "grant_id": secrets.token_bytes(16)},
            )
    assert excinfo.value.code == "fauna.protocol.malformed", (
        f"a text-model subscription carrying a grant must be refused, got {excinfo.value.code}"
    )

    with _ws(nest_instance, subscriber) as ws:
        reply = ws.call("fauna.labelers.subscribe", {"labeler_id": labeler_id})
    assert reply.get("ok") is True and reply["factor"] == factor, f"subscribe: {reply!r}"

    now_us = int(time.time() * 1_000_000)
    ws_api.create_post(
        port, author,
        sign_and_encode_post(author["signing_key"], now_us, "kittens kittens kittens", tags=[]),
    )

    scored = _rows_for(db_path, "SELECT COUNT(*) FROM content_scores WHERE factor = ?", factor)
    assert scored == 0, (
        f"the nest wrote {scored} score row(s) for a text-model labeler — a text model "
        f"scores only on the subscriber's device"
    )
    registered = _rows_for(
        db_path, "SELECT COUNT(*) FROM model_versions WHERE model_kind = ?", factor
    )
    assert registered == 0, (
        "a text-model subscription must register no model version — that would owe "
        "the nest drain work nothing can ever do"
    )

    with _ws(nest_instance, subscriber) as ws:
        assert ws.call("fauna.labelers.unsubscribe", {"labeler_id": labeler_id}).get("ok") is True


def test_a_post_kind_wasm_subscription_leaves_the_nest_nothing_to_score_with(
    nest_instance, run_seal_helper,
):
    """The `wasm` twin of the text-model witness above: a labeler over public
    posts runs at the subscriber's own app, in the feed manager's sealed-scorer
    seam, so its subscription leaves the nest a `labeler_subscriptions` row ONLY
    (§ Tier-3 → *Where it runs*, position (1) and invariant (f)) — no
    `content_scores` rows, no `model_versions` entry (so no drain), no grant;
    and a republish has no registration to raise. `inspect` tells the app the
    module's content kind beside its artifact kind (invariant (c)), which is
    what lets a feed compose a `post` module and leave a `mail` one inert.
    """
    port = nest_instance["port"]
    db_path = nest_instance["db_path"]
    admin_sk = nest_instance["admin"]["signing_key"]

    module = (FIXTURES / "cat_labeler.wat").read_bytes()
    labeler_id, pub_seed = new_publisher(nest_instance)
    publish_labeler_version(
        run_seal_helper, nest_instance["url"], labeler_id, pub_seed, module,
        version=1, content_kind="post", artifact_kind="wasm",
    )
    factor = "labeler:" + labeler_id.hex()

    subscriber = create_actor_and_register(port, admin_signing_key=admin_sk)
    author = create_actor_and_register(port, admin_signing_key=admin_sk)

    with _ws(nest_instance, subscriber) as ws:
        inspected = ws.call("fauna.labelers.inspect", {"labeler_id": labeler_id})
    assert (inspected.get("artifact_kind"), inspected.get("content_kind")) == ("wasm", "post"), (
        f"inspect must say the module's content kind beside its artifact kind: "
        f"{inspected.get('artifact_kind')!r}, {inspected.get('content_kind')!r}"
    )

    with _ws(nest_instance, subscriber) as ws:
        with pytest.raises(RpcCallError) as excinfo:
            ws.call(
                "fauna.labelers.subscribe",
                {"labeler_id": labeler_id, "grant_id": secrets.token_bytes(16)},
            )
    assert excinfo.value.code == "fauna.protocol.malformed", (
        f"a post-kind wasm subscription carrying a grant must be refused, got {excinfo.value.code}"
    )

    with _ws(nest_instance, subscriber) as ws:
        reply = ws.call("fauna.labelers.subscribe", {"labeler_id": labeler_id})
    assert reply.get("ok") is True and reply["factor"] == factor, f"subscribe: {reply!r}"

    now_us = int(time.time() * 1_000_000)
    ws_api.create_post(
        port, author,
        sign_and_encode_post(author["signing_key"], now_us, "my cat is asleep", tags=[]),
    )
    # The publisher releases v2: with no registration, nothing is raised.
    publish_labeler_version(
        run_seal_helper, nest_instance["url"], labeler_id, pub_seed, module,
        version=2, content_kind="post", artifact_kind="wasm",
    )

    scored = _rows_for(db_path, "SELECT COUNT(*) FROM content_scores WHERE factor = ?", factor)
    assert scored == 0, (
        f"the nest wrote {scored} score row(s) for a post-kind wasm labeler — it runs "
        f"only in the subscriber's app"
    )
    registered = _rows_for(
        db_path, "SELECT COUNT(*) FROM model_versions WHERE model_kind = ?", factor
    )
    assert registered == 0, (
        "a post-kind wasm subscription must register no model version — that would owe "
        "the nest drain work no holder serves"
    )

    with _ws(nest_instance, subscriber) as ws:
        assert ws.call("fauna.labelers.unsubscribe", {"labeler_id": labeler_id}).get("ok") is True


# ── Curated lists and published topics ──────────────────────────────────


def _signed_post(author, text: str) -> bytes:
    return sign_and_encode_post(
        author["signing_key"], int(time.time() * 1_000_000), text, tags=[]
    )


def _post(port: int, author, text: str) -> bytes:
    """Post ``text`` publicly as ``author``; returns the 32-byte content id."""
    return bytes.fromhex(ws_api.create_post(port, author, _signed_post(author, text)))


@pytest.fixture()
def scratch_nest(request, nest_mode, tmp_path_factory):
    """A second, throwaway nest — where a test learns a post's id before the post
    reaches the nest under test (ids are content-addressed: the same signed
    bytes get the same id on every nest)."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "post-id-scratch")
    yield nest
    cleanup()


def _list_artifact(scores: dict[bytes, int], name: str | None = None) -> bytes:
    """A canonical `LabelerListArtifact`: entries strictly ascending by id (the
    publish gate refuses any other order), an optional publisher-chosen name."""
    artifact = {
        "entries": [{"content_id": cid, "score": scores[cid]} for cid in sorted(scores)]
    }
    if name is not None:
        artifact["name"] = name
    return cbor2.dumps(artifact, canonical=True)


def _list_score(db_path: str, content_id: bytes, factor: str):
    """``(score, scorer_version)`` of one post's row for ``factor``, or None."""
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        row = conn.execute(
            "SELECT score, scorer_version FROM content_scores "
            "WHERE content_id = ? AND factor = ?",
            (content_id, factor),
        ).fetchone()
        return (row[0], row[1]) if row else None
    finally:
        conn.close()


def _inspect(nest_instance, actor, labeler_id: bytes) -> dict:
    with _ws(nest_instance, actor) as ws:
        return ws.call("fauna.labelers.inspect", {"labeler_id": labeler_id})


def _subscribed(nest_instance, actor, labeler_id: bytes) -> bool:
    with _ws(nest_instance, actor) as ws:
        rows = ws.call("fauna.labelers.list", {})["labelers"]
    return any(bytes(r["labeler_id"]) == labeler_id and r["subscribed"] for r in rows)


@pytest.mark.feature("community-labelers")
def test_a_curated_list_scores_the_posts_it_names_including_later_ones_until_unsubscribe(
    nest_instance, scratch_nest, run_seal_helper,
):
    """`community-labelers` outcome 17: a curated list you subscribe to scores
    the posts it names, including ones that arrive later, and stops when you
    unsubscribe (§ Tier-3 → a List's rows are materialized nest-side only for
    ids the nest holds, joined on a post's arrival, withdrawn at the last
    unsubscribe).

    The list names two posts: one already on the nest, and one that does not
    exist on it yet — its id learned by posting the same signed bytes to a
    scratch nest first (content-addressed, so it is the id this nest will key
    it under). A post the list does not name is the control.
    """
    port = nest_instance["port"]
    db_path = nest_instance["db_path"]
    admin_sk = nest_instance["admin"]["signing_key"]
    author = create_actor_and_register(port, admin_signing_key=admin_sk)
    subscriber = create_actor_and_register(port, admin_signing_key=admin_sk)

    early_id = _post(port, author, "an orange cat on a windowsill")
    other_id = _post(port, author, "the bus was late again")
    late_wire = _signed_post(author, "a small orange cat in a box")
    late_id = bytes.fromhex(ws_api.create_post(
        scratch_nest["port"],
        register_same_actor_on(
            scratch_nest["port"], author, admin_signing_key=scratch_nest["admin"]["signing_key"]
        ),
        late_wire,
    ))

    labeler_id, pub_seed = new_publisher(nest_instance)
    publish_labeler_version(
        run_seal_helper, nest_instance["url"], labeler_id, pub_seed,
        _list_artifact({early_id: 900, late_id: 400}),
        version=1, content_kind="post", artifact_kind="list",
    )
    factor = "labeler:" + labeler_id.hex()
    assert _list_score(db_path, early_id, factor) is None, (
        "publishing alone must score nothing — a list scores for its subscribers"
    )

    with _ws(nest_instance, subscriber) as ws:
        assert ws.call("fauna.labelers.subscribe", {"labeler_id": labeler_id})["factor"] == factor
    assert _list_score(db_path, early_id, factor) == (900, 1), (
        "subscribing did not score the post the list names"
    )
    assert _list_score(db_path, other_id, factor) is None, (
        "the list scored a post it does not name"
    )
    assert _list_score(db_path, late_id, factor) is None, (
        "a score row for a post the nest does not hold yet"
    )

    # ── The named post arrives after the subscription: it is scored on arrival.
    assert ws_api.create_post(port, author, late_wire) == late_id.hex(), (
        "the later post's id differs between the two nests — the list named the wrong post"
    )
    assert _list_score(db_path, late_id, factor) == (400, 1), (
        "a post the list names that arrived after the subscription was not scored"
    )

    # ── Unsubscribing (the last subscriber) stops it.
    with _ws(nest_instance, subscriber) as ws:
        assert ws.call("fauna.labelers.unsubscribe", {"labeler_id": labeler_id})["ok"] is True
    remaining = {
        cid.hex()[:12]: _list_score(db_path, cid, factor) for cid in (early_id, late_id)
    }
    assert set(remaining.values()) == {None}, (
        f"the list still scores posts after unsubscribe: {remaining}"
    )


@pytest.mark.feature("trained-topics")
def test_a_published_topic_is_a_snapshot_and_publishing_again_is_a_new_version(
    nest_instance, run_seal_helper,
):
    """`trained-topics` outcome 22: a published topic is a snapshot — training
    it further changes nothing for subscribers, and publishing again releases a
    new version (`topic-factors.md` § Publishing a trained factor: a
    fork-at-publish snapshot; re-publish is a new version).

    The topic's owner publishes it as a list, keeps training it (the trained
    model reaches the nest only as the owner's sealed model blob, under the
    topic's own `topic:` key), and the subscriber's view does not move: the
    published bytes and the score are unchanged. Publishing again as version 2
    is what moves them — same labeler, same subscription, the new scores.
    """
    port = nest_instance["port"]
    db_path = nest_instance["db_path"]
    admin_sk = nest_instance["admin"]["signing_key"]
    author = create_actor_and_register(port, admin_signing_key=admin_sk)
    owner = create_actor_and_register(port, admin_signing_key=admin_sk)
    subscriber = create_actor_and_register(port, admin_signing_key=admin_sk)

    post_id = _post(port, author, "a small orange cat")

    labeler_id, pub_seed = new_publisher(nest_instance)
    factor = "labeler:" + labeler_id.hex()
    v1 = _list_artifact({post_id: 900}, name="small orange cats")
    publish_labeler_version(
        run_seal_helper, nest_instance["url"], labeler_id, pub_seed, v1,
        version=1, content_kind="post", artifact_kind="list",
    )
    with _ws(nest_instance, subscriber) as ws:
        ws.call("fauna.labelers.subscribe", {"labeler_id": labeler_id})
    assert _list_score(db_path, post_id, factor) == (900, 1)

    # ── The owner trains the topic further: a new sealed model, nothing else.
    topic = "topic:" + secrets.token_hex(16)
    with _ws(nest_instance, owner) as ws:
        for samples in (3, 4):
            ws.call(
                "fauna.personalization.model.put",
                {"factor": topic, "sealed_blob": secrets.token_bytes(256), "sample_count": samples},
            )
    published = _inspect(nest_instance, subscriber, labeler_id)
    assert bytes(published["wasm_bytes"]) == v1, (
        "further training changed the published topic — it must be a snapshot"
    )
    assert _list_score(db_path, post_id, factor) == (900, 1), (
        "further training changed what the subscriber's list scores"
    )

    # ── Publishing again is a new version of the same labeler.
    v2 = _list_artifact({post_id: 300}, name="small orange cats")
    publish_labeler_version(
        run_seal_helper, nest_instance["url"], labeler_id, pub_seed, v2,
        version=2, content_kind="post", artifact_kind="list",
    )
    assert bytes(_inspect(nest_instance, subscriber, labeler_id)["wasm_bytes"]) == v2
    assert _list_score(db_path, post_id, factor) == (300, 2), (
        "the re-published version did not reach the subscriber"
    )
    assert _subscribed(nest_instance, subscriber, labeler_id), (
        "a new version must not cost the subscriber the subscription"
    )

    with _ws(nest_instance, subscriber) as ws:
        ws.call("fauna.labelers.unsubscribe", {"labeler_id": labeler_id})


@pytest.mark.feature("trained-topics")
def test_publishing_a_listed_topic_again_as_a_model_keeps_the_labeler_and_its_subscribers(
    nest_instance, run_seal_helper,
):
    """`trained-topics` outcome 23: publishing a list-published topic again as a
    model upgrades the same labeler, and its subscribers keep their
    subscription (`topic-factors.md` § Publishing a trained factor → *Identity
    and versioning*: `artifact_kind` is per-version, List → Model is an
    ordinary version bump; the frame withdraws the List's rows on the kind
    change).

    Pinned at tier_1 by the registry tests; this is the wire: the same
    publisher key publishes version 2 as a text model, the publish echoes the
    same labeler id, the subscriber is still subscribed and now fetches the
    model, and the list's nest-side rows are gone — a model scores only on the
    subscriber's device.
    """
    port = nest_instance["port"]
    db_path = nest_instance["db_path"]
    admin_sk = nest_instance["admin"]["signing_key"]
    author = create_actor_and_register(port, admin_signing_key=admin_sk)
    subscriber = create_actor_and_register(port, admin_signing_key=admin_sk)

    post_id = _post(port, author, "a small orange cat")

    labeler_id, pub_seed = new_publisher(nest_instance)
    factor = "labeler:" + labeler_id.hex()
    publish_labeler_version(
        run_seal_helper, nest_instance["url"], labeler_id, pub_seed,
        _list_artifact({post_id: 900}, name="small orange cats"),
        version=1, content_kind="post", artifact_kind="list",
    )
    with _ws(nest_instance, subscriber) as ws:
        ws.call("fauna.labelers.subscribe", {"labeler_id": labeler_id})
    assert _list_score(db_path, post_id, factor) == (900, 1)

    model = run_seal_helper(
        "encode-text-model-artifact",
        {
            "version": 1,
            "name": "small orange cats",
            # Clears TEXT_MODEL_PUBLISH_MIN_DOCS, the nest's structural privacy floor.
            "more_docs": 3,
            "less_docs": 0,
            "ngrams": [{"ngram": "orange", "more": 3, "less": 0}],
        },
    )
    # `publish_labeler_version` asserts the publish echoes this same labeler id.
    publish_labeler_version(
        run_seal_helper, nest_instance["url"], labeler_id, pub_seed, model,
        version=2, content_kind="post", artifact_kind="text-model",
    )

    assert _subscribed(nest_instance, subscriber, labeler_id), (
        "republishing the topic as a model cost the subscriber the subscription"
    )
    fetched = _inspect(nest_instance, subscriber, labeler_id)
    assert fetched["artifact_kind"] == "text-model" and bytes(fetched["wasm_bytes"]) == model, (
        f"the subscriber does not fetch the model: {fetched.get('artifact_kind')!r}"
    )
    assert _list_score(db_path, post_id, factor) is None, (
        "the list's nest-side rows outlived the upgrade to a model, which scores only "
        "on the subscriber's device"
    )

    with _ws(nest_instance, subscriber) as ws:
        ws.call("fauna.labelers.unsubscribe", {"labeler_id": labeler_id})
