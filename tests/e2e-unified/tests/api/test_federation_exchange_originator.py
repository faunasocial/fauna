"""tier_3 two-binary proof: the federation exchange originator plane.

Phase-1 flow assertion of the 2026-07-12 distributed-moderation plan
(``federation.md`` § the originator-gap note): k=3 local reports on nest A ->
with **no manual federation RPC anywhere in this test**, nest B's
``peer_content_reports`` gains the entry and the affected post's
``report:spam`` bus row on B reflects the flat non-scaling peer bucket
(exactly 100 per-mille — peer-only corroboration). The exchange is driven
solely by nest A's own spawned exchange-originator worker reacting to a
peering event (a user seeding a discovery-feed contributor pointing at B —
the client-reachable ``fauna.feed.create`` ``contributor_seeds`` path).

Two REAL ``fauna-nest`` binaries; the only wire calls this test makes are
ordinary client actions on each nest's own WS-RPC surface (register, post,
opt-in, flag, create feed) — E2E rule 8: the mutations under test are user
actions; the sqlite reads are read-only observation of the outcome.

The cycle's in-process proof-shape over the real federation wire is
``conformance_federation_channel.rs::exchange_originator_cycle_pushes_pulls_records_and_throttles``
(push + pull + partner memory + throttle); this test is its two-binary,
client-driven counterpart.
"""

import sqlite3
import time

import pytest

from common.auth import (
    create_actor_and_register,
    register_same_actor_on,
)
from tests.api import ws_api
from helpers.waiting import wait_until
from tests.api.bare import sign_and_encode_post

pytestmark = pytest.mark.tier_3


@pytest.fixture()
def two_report_nests(request, nest_mode, tmp_path_factory):
    """Two DEDICATED nests (report flags poison shared aggregates; see
    ``test_report_sharing.py``'s fixture note)."""
    from conftest import _start_dedicated_nest

    nest_a, cleanup_a = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "exchange-originator-a")
    try:
        nest_b, cleanup_b = _start_dedicated_nest(
            request, nest_mode, tmp_path_factory, "exchange-originator-b")
        try:
            yield nest_a, nest_b
        finally:
            cleanup_b()
    finally:
        cleanup_a()


def _query_one(db_path: str, sql: str, params: tuple):
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        return conn.execute(sql, params).fetchone()
    finally:
        conn.close()


def test_exchange_originator_moves_k3_aggregate_to_peer_unprompted(two_report_nests):
    nest_a, nest_b = two_report_nests
    port_a, port_b = nest_a["port"], nest_b["port"]
    nest_a_id = bytes.fromhex(ws_api.nest_info(port_a)["nest_id"])
    # The authority nest A's originator worker will DIAL — `peer_url`, not
    # `url`: this is the harness handing one nest the address of another, which
    # is exactly the act exclusion class (8) is derived from (`testing.md`
    # § Default app and nest mode, ruling (2)). Reading the key is what marks
    # this test class (8) in docker, where the address is RFC1918 and
    # `validate_peer_url` refuses it.
    b_url = nest_b["peer_url"]

    # The same author identity on both nests -> the identical signed post
    # bytes -> the identical content-addressed post id (a post's id IS its
    # report-hash), so B's import can attach the bus row to a resident item.
    author = create_actor_and_register(port_a, admin_signing_key=nest_a["admin"]["signing_key"])
    author_b = register_same_actor_on(
        port_b, author, admin_signing_key=nest_b["admin"]["signing_key"]
    )
    now_us = int(time.time() * 1_000_000)
    body ="Limited offer!!! Click http://campaign.example.test to claim your prize."
    post_bytes = sign_and_encode_post(author["signing_key"], now_us, body, tags=[])
    post_id = ws_api.create_post(port_a, author, post_bytes)
    post_id_b = ws_api.create_post(port_b, author_b, post_bytes)
    assert post_id == post_id_b, "content-addressed: same bytes, same id on both nests"

    # k=3 opted-in local reporters flag the post on A (the ordinary client
    # mark-as-spam path).
    for _ in range(3):
        reporter = create_actor_and_register(
            port_a, admin_signing_key=nest_a["admin"]["signing_key"]
        )
        assert ws_api.report_share_set(port_a, reporter, True)["share"] is True
        ws_api.moderation_train(port_a, reporter, post_id, "spam")

    # A's own aggregate is live (k=3 -> 200 per-mille locally).
    entry = next(
        e
        for e in ws_api.report_share_status(port_a, author)["published"]
        if e["content_hash"] == post_id
    )
    assert entry["count"] == 3

    # The peering event: a user on A seeds a discovery feed contributor at B.
    # From here on the test issues NO further mutation — the originator
    # worker on A must move the aggregate on its own.
    ws_api.create_feed(
        port_a,
        author,
        "cross-nest discovery",
        rules=[{"BodyContains": {"terms": ["offer"]}}],
        scope="discovery",
        contributor_seeds=[b_url],
    )

    # B gains the peer aggregate + the flat-bucket bus row, unprompted.
    # Budget: transition debounce (10 s) + one possible min-peer-interval
    # backoff (60 s) + slack.
    deadline = time.time() + 120.0
    peer_row = None
    while peer_row is None and time.time() < deadline:
        peer_row = _query_one(
            nest_b["db_path"],
            "SELECT claimed_count, peer_nest_id FROM peer_content_reports "
            "WHERE content_hash = ? AND factor = 'report:spam'",
            (bytes.fromhex(post_id),),
        )
        if peer_row is None:
            time.sleep(1.0)
    assert peer_row is not None, (
        "nest B never received the aggregate — the exchange originator did not fire"
    )
    claimed_count, peer_nest_id = peer_row
    assert claimed_count == 3, "the exporter's LOCAL k-gate-passed count crossed"
    assert bytes(peer_nest_id) == nest_a_id, "keyed on A's channel-verified nest id"

    # The affected post's bus row on B = the flat peer bucket, exactly 100
    # per-mille (peer-only; B has no local reporters — corroboration, never
    # consensus). The recompute is synchronous with the import, so no
    # further polling is needed once the peer row exists.
    score_row = _query_one(
        nest_b["db_path"],
        "SELECT score FROM content_scores WHERE content_id = ? AND factor = 'report:spam'",
        (bytes.fromhex(post_id),),
    )
    assert score_row is not None, "the peer bucket must land on B's resident post"
    assert score_row[0] == 100, f"flat non-scaling peer bucket (got {score_row[0]})"

    # A remembered B as a prior exchange partner (the third peer source —
    # partner memory survives contributor churn and restarts).
    partner = _query_one(
        nest_a["db_path"],
        "SELECT nest_url FROM exchange_peers WHERE nest_url = ?",
        (b_url,),
    )
    assert partner is not None, "successful exchange records the partner"


@pytest.mark.feature("learn-from-my-activity")
def test_a_shared_verdict_enough_people_agree_on_reaches_the_peer_nest_as_a_count(
    two_report_nests,
):
    """`learn-from-my-activity` outcome 11: once enough people on your nest share
    the same verdict on a public post, that anonymous count also reaches other
    nests (`engagement-cues.md` § Layer B — signal aggregates ride the
    report-sharing federation export unchanged).

    The report journey above, re-run for an activity verdict: three opted-in
    people on nest A each find the same public post worth watching to the end,
    and with no federation call from the test, nest B holds the count — keyed
    on A, carrying no one's identity. One fewer than enough is published
    nowhere, so it is checked before the third verdict.
    """
    nest_a, nest_b = two_report_nests
    port_a, port_b = nest_a["port"], nest_b["port"]
    admin_a = nest_a["admin"]["signing_key"]
    nest_a_id = bytes.fromhex(ws_api.nest_info(port_a)["nest_id"])
    b_url = nest_b["peer_url"]
    factor = "signal:watch-complete"

    author = create_actor_and_register(port_a, admin_signing_key=admin_a)
    author_b = register_same_actor_on(
        port_b, author, admin_signing_key=nest_b["admin"]["signing_key"]
    )
    now_us = int(time.time() * 1_000_000)
    post_bytes =sign_and_encode_post(
        author["signing_key"], now_us, "A cat video everyone watches to the end.", tags=[]
    )
    post_id = ws_api.create_post(port_a, author, post_bytes)
    assert ws_api.create_post(port_b, author_b, post_bytes) == post_id

    def published(port, actor):
        return [
            e for e in ws_api.signal_share_status(port, actor)["published"]
            if e["content_hash"] == post_id and e["factor"] == factor
        ]

    people = [create_actor_and_register(port_a, admin_signing_key=admin_a) for _ in range(3)]
    for i, person in enumerate(people):
        assert ws_api.signal_share_set(port_a, person, True)["share"] is True
        if i == len(people) - 1:
            assert published(port_a, author) == [], (
                "two verdicts are not enough — nothing may be published yet"
            )
        ws_api.signal_contribute(port_a, person, post_id, "watch-complete")
    assert [e["count"] for e in published(port_a, author)] == [3]

    # The peering event (a user on A seeds a discovery feed at B); from here the
    # test makes no mutation — A's exchange originator moves the count itself.
    ws_api.create_feed(
        port_a,
        author,
        "cross-nest discovery",
        rules=[{"BodyContains": {"terms": ["cat"]}}],
        scope="discovery",
        contributor_seeds=[b_url],
    )

    # Budget: transition debounce (10 s) + one possible min-peer-interval
    # backoff (60 s) + slack — the report journey's.
    peer_row = wait_until(
        lambda: _query_one(
            nest_b["db_path"],
            "SELECT claimed_count, peer_nest_id FROM peer_content_reports "
            "WHERE content_hash = ? AND factor = ?",
            (bytes.fromhex(post_id), factor),
        ),
        120.0,
        interval=1.0,
        diagnose=lambda: (
            "nest B never received the shared verdict's count — signal aggregates "
            "must ride the report exchange"
        ),
    )
    claimed_count, peer_nest_id = peer_row
    assert claimed_count == 3, "the count A's own people reached crossed, nothing more"
    assert bytes(peer_nest_id) == nest_a_id, "keyed on A's channel-verified nest id"
    # Anonymous: what B holds about the verdict is the count from A, never who.
    local_rows = _query_one(
        nest_b["db_path"],
        "SELECT COUNT(*) FROM content_reports WHERE content_hash = ? AND factor = ?",
        (bytes.fromhex(post_id), factor),
    )
    assert local_rows == (0,), (
        "nest B holds per-person verdict rows for A's people — the count must arrive "
        "anonymous"
    )
