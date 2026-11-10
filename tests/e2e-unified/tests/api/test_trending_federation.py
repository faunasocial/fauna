"""tier_3 two-binary proof: the distributed Trending v1 cross-nest cycle.

Phase-2 flow assertion of the 2026-07-12 distributed-moderation plan
(``trending.md`` § Import-triggered fetch + § The read model). Two REAL
``fauna-nest`` binaries; the only wire calls this test makes are ordinary
**client** actions on each nest's own WS-RPC surface (register, post, like,
create feed, trending read) — E2E rule 8: the mutations under test are user
actions, and the sqlite reads are read-only observation of the outcome.

End-to-end flow (no manual federation RPC anywhere):

  A (origin)                           B (fetcher)
  ─ author posts a PUBLIC post P
  ─ 3 DISTINCT users LIKE P  ─────────▶ A's `content_scores` trending row = 130
    (real `fauna.posts.interact`)       (local velocity v≈3 → round(3000/23))
                                        B seeds A as a discovery contributor
                                        (`fauna.feed.create` scope=discovery) —
                                        the ONLY peering event; from here B's own
                                        exchange-originator worker must, unprompted:
                                          · pull A's k-gate-passed trend entry
                                          · FETCH P over `fauna.federation.post.get`
                                          · verify the author sig + CID↔id binding
                                          · store P (gated_tier NULL → public)
                                          · recompute → B trending row = 100‰
                                            (single-peer ramp; B has 0 local velocity)
  ─ 3 users UNLIKE P  ────────────────▶ A's trending row is DELETEd (withdraw-at-zero)

What this two-binary test proves that nothing else does: the FULL cross-PROCESS
cycle over the real WS-RPC + federation HTTP wire, driven only by client actions,
AND the ``fauna.feed.trending.posts`` read kind surfacing the fetched peer-only
post. The in-process channel conformance test
``conformance_federation_channel.rs::trends_originator_fetches_and_surfaces_peer_only_post``
calls ``exchange_with_peer`` directly, writes engagement rows straight to the DB,
and reads ``get_content_scores`` directly — it never crosses a process boundary,
never drives the client like/feed-create paths, and never exercises the read kind.

Deliberately NOT re-proven here (already covered — this test does not duplicate,
mirroring ``test_federation_exchange_originator.py``'s note):
  · the peer ramp magnitude / bound / presence-only shape — unit-tested in
    ``fauna-core scoring.rs::{peer_ramp_is_bounded_presence_only, score_composes_and_caps}``;
  · a hostile peer's inflated ``score_pm``/``engager_count`` buying nothing —
    ``conformance_federation_channel.rs::trends_exchange_is_presence_only_and_export_never_launders``
    (H pushes ``score_pm: 60_000, engager_count: 90_000`` over the real channel wire);
  · the decay curve's exact shape — ``scoring.rs::velocity_decays_by_half_each_half_life``
    + ``db/trends.rs::sweep_re_decays_and_withdraws``. (Withdraw-at-zero IS
    exercised here, via the real unlike wire zeroing local velocity, and the
    wall-clock sweep's fade-out by
    ``test_a_post_with_no_new_engagement_fades_out_of_trending`` below.)

Note: this cycle test's fetch observables are also produced by B's discovery
poller's index stub for the same id (its feed rule matches the post), so the
fetch leg's own witness is ``test_a_peer_cannot_substitute_content_for_a_trending_post``,
whose rules match nothing and leave the fetch the only way in.
"""

import sqlite3
import time

import pytest

import fauna_ffi
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common import create_actor_and_register
from helpers import budgets, waiting
from helpers.blob_upload import SEALED_MIME, upload_blob
from tests.api import ws_api
from tests.api.bare import sign_and_encode_post

pytestmark = pytest.mark.tier_3


@pytest.fixture()
def two_trend_nests(request, nest_mode, tmp_path_factory):
    """Two DEDICATED nests — the whole point is a real cross-process exchange."""
    from conftest import _start_dedicated_nest

    nest_a, cleanup_a = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "trending-federation-a")
    try:
        nest_b, cleanup_b = _start_dedicated_nest(
            request, nest_mode, tmp_path_factory, "trending-federation-b")
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


def _trend_score(db_path: str, post_id_hex: str):
    """The post's local/composed ``trending`` factor value (per-mille), or ``None``
    when no row exists (never scored, or withdrawn)."""
    row = _query_one(
        db_path,
        "SELECT score FROM content_scores WHERE content_id = ? AND factor = 'trending'",
        (bytes.fromhex(post_id_hex),),
    )
    return None if row is None else row[0]


def _await_trend_score(db_path: str, post_id_hex: str, what: str) -> int:
    """Block until the post has a ``trending`` row on this nest; return its per-mille.

    A deadline poll on a named budget (convention 14, mechanism 1) through the
    shared primitive: a green run returns the instant the row lands and pays
    nothing, and only a genuine failure spends the budget. The predicate is the
    raw ROW rather than the score, so a legitimate 0 could never read as "not
    there yet".
    """
    row = waiting.wait_until(
        lambda: _query_one(
            db_path,
            "SELECT score FROM content_scores WHERE content_id = ? AND factor = 'trending'",
            (bytes.fromhex(post_id_hex),),
        ),
        budgets.FEDERATION_EXCHANGE_CYCLE_S,
        interval=1.0,
        diagnose=lambda: what,
    )
    return row[0]


@pytest.mark.feature("trending")
def test_trending_cycle_fetches_peer_only_post_and_surfaces_it(two_trend_nests):
    nest_a, nest_b = two_trend_nests
    port_a, port_b = nest_a["port"], nest_b["port"]
    # The authority nest B's discovery worker will DIAL — `peer_url`, not `url`.
    # This is the enrolling half of class (8) and the one that fails SILENTLY:
    # `fauna.feed.contributors.grant` validates nothing and upserts the row, and
    # the refusal happens minutes later inside the discovery poll loop, absorbed
    # as a failure count and a row DELETE (`testing.md` § Default app and nest
    # mode, ruling (2)). Reading the key is what classifies the test out of
    # docker at COLLECTION instead of letting it time out with no diagnosis.
    a_url = nest_a["peer_url"]
    nest_a_id = bytes.fromhex(ws_api.nest_info(port_a)["nest_id"])

    # ── A: one PUBLIC post + 3 DISTINCT local likers (the real client like wire) ──
    author = create_actor_and_register(
        port_a, admin_signing_key=nest_a["admin"]["signing_key"]
    )
    now_us = int(time.time() * 1_000_000)
    body = "Breaking: the trendingbeacon launch is live!"
    post_bytes = sign_and_encode_post(author["signing_key"], now_us, body, tags=[])
    post_id = ws_api.create_post(port_a, author, post_bytes)

    likers = []
    for _ in range(3):
        liker = create_actor_and_register(
            port_a, admin_signing_key=nest_a["admin"]["signing_key"]
        )
        ws_api.interact(port_a, liker, post_id, action="like")
        likers.append(liker)

    # A's local trending row is live: 3 fresh likes → v≈3 → round(1000·3/(3+20))
    # = 130. The recompute is synchronous inside the like handler, so the value
    # is settled once the third like returns.
    assert _trend_score(nest_a["db_path"], post_id) == 130, (
        "A: 3 fresh local likes → local trend per-mille"
    )

    # B has never seen the post: no body → no trending row (no blind peer row).
    assert _trend_score(nest_b["db_path"], post_id) is None, (
        "B has not seen the post before the exchange"
    )

    # ── the peering event: a user on B seeds A as a discovery contributor ──
    # From here the test issues NO further trend-plane mutation — B's own
    # exchange-originator worker must pull A's trend, FETCH P over the real
    # `fauna.federation.post.get` wire, verify + ingest it, and recompute.
    b_user = create_actor_and_register(
        port_b, admin_signing_key=nest_b["admin"]["signing_key"]
    )
    ws_api.create_feed(
        port_b,
        b_user,
        "cross-nest trending discovery",
        rules=[{"BodyContains": {"terms": ["trendingbeacon"]}}],
        scope="discovery",
        contributor_seeds=[a_url],
    )

    # B gains the FETCHED post + the single-peer ramp, unprompted — within the
    # exchange-cycle budget (debounce + a possible min-peer-interval backoff +
    # the round trip), never at a fixed moment.
    b_score = _await_trend_score(
        nest_b["db_path"], post_id, "B never scored the peer-only post"
    )
    assert b_score == 100, (
        "B: peer-only post fetched + ingested → single-peer ramp "
        f"(100‰, no local velocity); got {b_score}"
    )

    # The peer presence bit is keyed on A's channel-verified nest id and carries
    # A's k-gate-passed engager count (the export's LOCAL count crossed — never a
    # laundered magnitude; presence-only ramp ignores the claimed value).
    peer_row = _query_one(
        nest_b["db_path"],
        "SELECT engager_count, peer_nest_id FROM peer_content_trends WHERE content_id = ?",
        (bytes.fromhex(post_id),),
    )
    assert peer_row is not None, "B imported A's trend presence bit"
    engager_count, peer_nest_id = peer_row
    assert engager_count == 3, "the exporter's local k-gate-passed engager count crossed"
    assert bytes(peer_nest_id) == nest_a_id, "keyed on A's channel-verified nest id"

    # The READ KIND (nothing else in the suite exercises `fauna.feed.trending.posts`):
    # B's Trending virtual read surfaces the peer-only post it fetched, ranked first
    # (it is B's only trending public post).
    trending = ws_api.trending_feed_posts(port_b, b_user)
    assert trending, "B's Trending read returned the fetched peer-only post"
    assert trending[0]["post_id"] == post_id, (
        "the fetched peer-only post ranks first in B's Trending read"
    )

    # B recorded A as an exchange partner (partner memory survives contributor
    # churn + restarts — the prior-partner peer source for later cycles).
    partner = _query_one(
        nest_b["db_path"],
        "SELECT nest_url FROM exchange_peers WHERE nest_url = ?",
        (a_url,),
    )
    assert partner is not None, "a successful exchange records the partner"

    # ── withdraw-at-zero through the real unlike wire ──
    # All 3 likers unlike → A's local velocity falls to 0 and, with no peers of its
    # own for P, the row is DELETEd. Each unlike's recompute is synchronous, so the
    # row is gone once the last unlike returns.
    for liker in likers:
        ws_api.interact(port_a, liker, post_id, action="unlike")
    assert _trend_score(nest_a["db_path"], post_id) is None, (
        "A: local engagement removed → trending row withdrawn at zero"
    )


def _peer_trend_row(db_path: str, post_id_hex: str):
    """The post's imported peer-presence row, or ``None`` when no peer ever
    told this nest the post was trending."""
    return _query_one(
        db_path,
        "SELECT engager_count FROM peer_content_trends WHERE content_id = ?",
        (bytes.fromhex(post_id_hex),),
    )


@pytest.mark.feature("trending")
def test_export_withholds_below_k_and_restricted_posts(two_trend_nests):
    """The k-gate and the public-only rule, observed at the far end of the wire.

    `trending.md` § Federation exchange ("each backed by >= `TREND_MIN_ENGAGERS`
    distinct local engagers -- the k-gate ... every export path routes through")
    and § The factor ("a restricted post never gets a `trending` row and never
    appears in any export"). What this nest *tells other nests* is not readable
    from A's own tables -- A's local rows are deliberately NOT k-gated, so a
    row's presence says nothing about whether it crossed -- so the observation
    point is B's `peer_content_trends`, which is exactly what A disclosed.

    Three posts on A, differing only in what the gate is supposed to look at:

    * **control** -- public, 3 distinct likers (k is met) -> crosses;
    * **below-k** -- public, 2 distinct likers -> has a live local trending row
      on A and still must NOT cross;
    * **restricted** -- tier-gated, 3 distinct likers -> never even gets a local
      trending row, so there is nothing to cross.

    Latency-independent (convention 14): the control's arrival on B is the
    settling event, and it is the SAME export pull that would have carried the
    other two -- `import_trend_entries` lands every accepted entry of one batch
    before the cycle moves on, and A's export is recomputed from current state
    on every pull, so an entry absent when the control landed is an entry the
    gate withheld, not one still in flight.
    """
    nest_a, nest_b = two_trend_nests
    port_a, port_b = nest_a["port"], nest_b["port"]
    a_url = nest_a["peer_url"]
    admin_sk_a = nest_a["admin"]["signing_key"]

    author = create_actor_and_register(port_a, admin_signing_key=admin_sk_a)
    now_us = int(time.time() * 1_000_000)

    # -- the three posts --
    control_id = ws_api.create_post(
        port_a,
        author,
        sign_and_encode_post(
            author["signing_key"], now_us, "Public and popular: kgatebeacon control"
        ),
    )
    below_k_id = ws_api.create_post(
        port_a,
        author,
        sign_and_encode_post(
            author["signing_key"], now_us + 1_000_000, "Public but quiet: kgatebeacon below-k"
        ),
    )

    # The restricted one: a tier-gated post, built + sealed by the same shared
    # client helper a creator's composer uses, with its sealed body in the blob
    # store (`monetization.md` section Pillar 2 -- the shape `test_web_paywall` pins).
    with WsRpcAdminClient(
        nest_a["url"],
        actor_id=author["actor_id_bytes"],
        signing_key=bytes(author["signing_key"]),
    ) as ws:
        ws.call(
            "fauna.subscriptions.tiers.create",
            {
                "name": "gold",
                "rank": 2,
                "description": None,
                "price_hint": "5 EUR / month",
                "payment_url": "https://pay.example/gold",
                "auto_approve": False,
                # The required birth KeyBlob (empty roster).
                "encrypted_upload": fauna_ffi.build_tier_birth_upload(bytes(author["signing_key"]), "gold", bytes(range(32))),
            },
        )
    gated_bytes, sealed_blob = fauna_ffi.build_gated_post(
        bytes(author["signing_key"]),
        "Restricted: kgatebeacon paywalled",
        "Restricted: kgatebeacon paywalled\n\nThe part you pay for.",
        "gold",
        2,
        b"\x11" * 32,
        bytes(range(32)),
    )
    upload_blob(
        port_a,
        author["token"],
        sealed_blob,
        audience_class="PeriodRestrictedPost",
        mime=SEALED_MIME,
    )
    gated_id = ws_api.create_post(port_a, author, gated_bytes)

    # -- the engagement: 3 distinct likers each for control + restricted, 2 for
    # below-k. Nothing about the LIKE path is gated, so the restricted post
    # really does accumulate the same explicit-engagement events as the control
    # -- which is what makes its missing trend row a statement about the
    # public-only rule rather than about missing engagement.
    def _like(post_id: str, n: int) -> None:
        for _ in range(n):
            liker = create_actor_and_register(port_a, admin_signing_key=admin_sk_a)
            ws_api.interact(port_a, liker, post_id, action="like")

    _like(control_id, 3)
    _like(below_k_id, 2)
    _like(gated_id, 3)

    # -- A's own rows: the k-gate is an EXPORT gate, not a scoring one --
    # round(1000*3/(3+20)) = 130 and round(1000*2/(2+20)) = 91; both recomputes
    # are synchronous inside the like handler, so the values are settled here.
    assert _trend_score(nest_a["db_path"], control_id) == 130, "A: the control is trending"
    assert _trend_score(nest_a["db_path"], below_k_id) == 91, (
        "A: a below-k post still scores locally -- local rows are deliberately "
        "NOT k-gated (the counters are already public per-post on this nest)"
    )
    assert _trend_score(nest_a["db_path"], gated_id) is None, (
        "A: a restricted post never gets a trending row, however much engagement it has"
    )

    # -- the peering event: a user on B seeds A as a discovery contributor --
    b_user = create_actor_and_register(
        port_b, admin_signing_key=nest_b["admin"]["signing_key"]
    )
    ws_api.create_feed(
        port_b,
        b_user,
        "cross-nest k-gate discovery",
        rules=[{"BodyContains": {"terms": ["kgatebeacon"]}}],
        scope="discovery",
        contributor_seeds=[a_url],
    )

    # Settling event: A's k-gate-passed entry reaches B, on the same
    # exchange-cycle budget the test above uses.
    control_row = waiting.wait_until(
        lambda: _peer_trend_row(nest_b["db_path"], control_id),
        budgets.FEDERATION_EXCHANGE_CYCLE_S,
        interval=1.0,
        diagnose=lambda: "B never imported A's k-gate-passed trend entry",
    )
    assert control_row[0] == 3, "the exporter's local engager count crossed"

    # -- what the gate withheld, in the same batch --
    assert _peer_trend_row(nest_b["db_path"], below_k_id) is None, (
        "a post with only 2 distinct local engagers must never cross the wire -- "
        "below k, a small nest's engagement pattern is not the network's business"
    )
    assert _peer_trend_row(nest_b["db_path"], gated_id) is None, (
        "a restricted post never appears in any export"
    )
    # ...and B never learned the withheld posts at all: no presence bit means no
    # import-triggered fetch, so neither body was pulled across either.
    assert _trend_score(nest_b["db_path"], below_k_id) is None, "B: no below-k trend row"
    assert _trend_score(nest_b["db_path"], gated_id) is None, "B: no restricted trend row"


@pytest.mark.feature("room-restricted-posts")
def test_a_room_post_never_trends_nor_crosses_the_exchange(two_trend_nests):
    """A room post never appears in the public feed or in Trending
    (`room-restricted-posts` outcome 13; `restricted-posts.md` § Encryption at
    rest — a room post is restricted, and `trending.md` § The factor: "a
    restricted post never gets a `trending` row and never appears in any
    export").

    The room-addressed twin of the tier-gated arm above: a community room is
    founded on A over the wire (``helpers/community_room.py``) and its owner
    posts to it; a public control post by the same owner gets the same three
    distinct likers. The public read of Trending on A — the built-in feed that
    reads public posts only — lists the control and not the room post, A gives
    the room post no trending row, and when B's discovery feed pulls A's export
    the control's arrival settles the batch that would have carried the room
    post too (the same anchor as above, convention 14)."""
    from helpers.community_room import found_room, room_post

    nest_a, nest_b = two_trend_nests
    port_a, port_b = nest_a["port"], nest_b["port"]
    admin_sk_a = nest_a["admin"]["signing_key"]

    room = found_room(nest_a)
    author = room.owner
    control_id = ws_api.create_post(
        port_a,
        author,
        sign_and_encode_post(
            author["signing_key"], int(time.time() * 1_000_000), "Public: roombeacon control"
        ),
    )
    room_id, _, _ = room_post(
        room, "Room: roombeacon teaser", "Room: roombeacon teaser\n\nFor the square only."
    )

    for post_id in (control_id, room_id):
        for _ in range(3):
            liker = create_actor_and_register(port_a, admin_signing_key=admin_sk_a)
            ws_api.interact(port_a, liker, post_id, action="like")

    assert _trend_score(nest_a["db_path"], control_id) == 130, "A: the control is trending"
    assert _trend_score(nest_a["db_path"], room_id) is None, (
        "A: a room post never gets a trending row, however much engagement it has"
    )
    reader = create_actor_and_register(port_a, admin_signing_key=admin_sk_a)
    trending = [p.get("post_id") for p in ws_api.trending_feed_posts(port_a, reader, limit=200)]
    assert control_id in trending, f"A's Trending read lists the control: {trending!r}"
    assert room_id not in trending, "A's Trending read never lists a room post"

    b_user = create_actor_and_register(port_b, admin_signing_key=nest_b["admin"]["signing_key"])
    ws_api.create_feed(
        port_b,
        b_user,
        "cross-nest room-post discovery",
        rules=[{"BodyContains": {"terms": ["roombeacon"]}}],
        scope="discovery",
        contributor_seeds=[nest_a["peer_url"]],
    )
    waiting.wait_until(
        lambda: _peer_trend_row(nest_b["db_path"], control_id),
        budgets.FEDERATION_EXCHANGE_CYCLE_S,
        interval=1.0,
        diagnose=lambda: "B never imported A's trending control entry",
    )
    assert _peer_trend_row(nest_b["db_path"], room_id) is None, (
        "a room post never appears in any export"
    )
    assert _trend_score(nest_b["db_path"], room_id) is None, "B: no room-post trend row"


# ── the two mechanisms only a test hook brings inside a test's lifetime ──
#
# `bins/fauna-nest/src/trending_test_hook.rs` (built into every `test-hooks`
# nest): a one-shot run of the production trend sweep at an advanced clock, and
# a switch making a nest's `post.get` answer serve bytes that no longer hash to
# the requested id. Both stand in for a clock or a peer, never for a user: every
# user act below still goes over the real client wire.


def _hook(nest: dict, path: str, body: dict | None = None) -> dict:
    """Call a `/api/v1/test/trending/*` hook on `nest` (POST with a body, else GET)."""
    import json
    import urllib.request

    req = urllib.request.Request(
        f"{nest['url']}/api/v1/test/trending/{path}",
        data=None if body is None else json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
        method="GET" if body is None else "POST",
    )
    with urllib.request.urlopen(req, timeout=30.0) as resp:
        assert resp.status == 200, f"trending hook {path} returned {resp.status}"
        return json.loads(resp.read())


def _trending_score_in_read(port: int, reader: dict, post_id: str):
    """The post's composed score in the Trending read, or ``None`` when the read
    does not list it."""
    for item in ws_api.trending_feed_posts(port, reader, limit=200):
        if item.get("post_id") == post_id:
            return item.get("score")
    return None


@pytest.fixture()
def one_trend_nest(request, nest_mode, tmp_path_factory):
    """A DEDICATED nest: the sweep hook re-scores every live trending row on the
    nest it runs on, which must never reach another test's rows."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "trending-decay")
    try:
        yield nest
    finally:
        cleanup()


@pytest.mark.feature("trending")
def test_a_post_with_no_new_engagement_fades_out_of_trending(one_trend_nest):
    """`trending` outcome 7 (`trending.md` § Local velocity): the factor is a
    decayed velocity, re-decayed by a ~15-minute wall-clock sweep that withdraws
    a row fallen to 0‰ — so a post nobody engages with again sinks and leaves.

    The sweep under test is the production `CacheDb::sweep_trend_scores`, run by
    the hook at the real clock plus an advance (the half-life is hours). Three
    readings of the same post, no new act between them: at the present the row
    keeps its score (the control: the sweep alone withdraws nothing live); one
    half-life on, it has decayed but still trends; a week on, the row is gone
    and the Trending read scores the post 0. Each sweep returns after its
    writes commit, so every reading is settled state (convention 14)."""
    nest = one_trend_nest
    port = nest["port"]
    admin_sk = nest["admin"]["signing_key"]

    author = create_actor_and_register(port, admin_signing_key=admin_sk)
    post_id = ws_api.create_post(
        port,
        author,
        sign_and_encode_post(
            author["signing_key"],
            int(time.time() * 1_000_000),
            "Hot for an afternoon: fadebeacon",
            tags=[],
        ),
    )
    for _ in range(3):
        liker = create_actor_and_register(port, admin_signing_key=admin_sk)
        ws_api.interact(port, liker, post_id, action="like")
    assert _trend_score(nest["db_path"], post_id) == 130, "3 fresh likes → 130‰"

    reader = create_actor_and_register(port, admin_signing_key=admin_sk)
    fresh = _trending_score_in_read(port, reader, post_id)
    assert fresh is not None and fresh > 0, f"the post trends in the read: {fresh!r}"

    # Control: a sweep at the present re-scores the live row to what it was.
    assert _hook(nest, "sweep", {"advance_s": 0})["rescored"] >= 1
    assert _trend_score(nest["db_path"], post_id) == 130, (
        "a sweep at the present leaves a freshly-engaged post's score alone"
    )

    # One half-life (6 h) on: decayed, still trending.
    _hook(nest, "sweep", {"advance_s": 6 * 3600})
    faded = _trend_score(nest["db_path"], post_id)
    assert faded is not None and 0 < faded < 130, (
        f"after a half-life with no new engagement the score has decayed: {faded!r}"
    )

    # A week on: withdrawn at zero, and the Trending read no longer lifts it.
    _hook(nest, "sweep", {"advance_s": 7 * 24 * 3600})
    assert _trend_score(nest["db_path"], post_id) is None, (
        "a post with no engagement for a week is withdrawn from trending (row DELETEd)"
    )
    stale = _trending_score_in_read(port, reader, post_id)
    assert not stale, (
        f"the Trending read scores the faded post 0 (or drops it), was {fresh}: {stale!r}"
    )


@pytest.mark.feature("trending")
def test_a_peer_cannot_substitute_content_for_a_trending_post(two_trend_nests):
    """`trending` outcome 8 (`trending.md` § Import-triggered public-post fetch):
    a post Trending brings from a peer is fetched over `post.get` and accepted
    only if its bytes hash to the id the peer exported (plus the author's
    signature), so the peer cannot hand over other content under it.

    A stands in for a substituting peer through the hook (its `post.get` serves
    the stored bytes with the last byte flipped); nothing on A is written. B's
    discovery feed seeds A with rules that match NONE of A's posts, so B's
    discovery poller never writes an index stub for the id — the stub is a
    `content_meta` row, which the fetch reads as "already seen" and skips. The
    fetch leg is therefore the only way the post can reach B.

    Latency-independent (convention 14): A's serve counters are the barriers.
    The tampered serve proves B fetched; B then holds no `content_meta` and no
    trending row. Once A answers honestly and B is nudged again, an HONEST serve
    proves the first was refused — a B that had stored the substitute would
    hold the id and never ask again — and B ends with the genuine post trending
    on the single-peer ramp."""
    nest_a, nest_b = two_trend_nests
    port_a, port_b = nest_a["port"], nest_b["port"]
    admin_sk_a = nest_a["admin"]["signing_key"]

    author = create_actor_and_register(port_a, admin_signing_key=admin_sk_a)
    post_id = ws_api.create_post(
        port_a,
        author,
        sign_and_encode_post(
            author["signing_key"],
            int(time.time() * 1_000_000),
            "The genuine words: substitutebeacon",
            tags=[],
        ),
    )
    for _ in range(3):
        liker = create_actor_and_register(port_a, admin_signing_key=admin_sk_a)
        ws_api.interact(port_a, liker, post_id, action="like")
    assert _trend_score(nest_a["db_path"], post_id) == 130, "A: the post is trending"

    assert _hook(nest_a, "tamper-post-get", {"on": True})["on"] is True

    b_user = create_actor_and_register(
        port_b, admin_signing_key=nest_b["admin"]["signing_key"]
    )

    def _peer_with_a(name: str) -> None:
        # A peering event (`fauna.feed.create` scope=discovery with a seed)
        # nudges B's exchange originator; the rules match nothing A holds.
        ws_api.create_feed(
            port_b,
            b_user,
            name,
            rules=[{"BodyContains": {"terms": ["nothingonamatchesthis"]}}],
            scope="discovery",
            contributor_seeds=[nest_a["peer_url"]],
        )

    _peer_with_a("substitution probe")

    waiting.wait_until(
        lambda: _hook(nest_a, "tamper-post-get")["tampered_serves"] >= 1,
        budgets.FEDERATION_EXCHANGE_CYCLE_S,
        interval=1.0,
        diagnose=lambda: f"B never fetched the post from A: {_hook(nest_a, 'tamper-post-get')!r}",
    )
    assert _peer_trend_row(nest_b["db_path"], post_id) is not None, (
        "B imported A's presence bit (the pull lands before the fetch)"
    )
    content_meta = _query_one(
        nest_b["db_path"],
        "SELECT 1 FROM content_meta WHERE content_id = ?",
        (bytes.fromhex(post_id),),
    )
    assert content_meta is None, "B refused the substituted bytes: no content_meta row"
    assert _trend_score(nest_b["db_path"], post_id) is None, (
        "B: no trending row for a post it has not seen"
    )

    # A answers honestly from here; a second peering event nudges B again (its
    # per-peer throttle reschedules the cycle, never drops it).
    assert _hook(nest_a, "tamper-post-get", {"on": False})["on"] is False
    _peer_with_a("substitution probe, honest")

    b_score = _await_trend_score(
        nest_b["db_path"], post_id, "B never fetched the honest post"
    )
    assert b_score == 100, f"B: the genuine post trends on the single-peer ramp; got {b_score}"
    served = _hook(nest_a, "tamper-post-get")
    assert served["honest_serves"] >= 1, (
        f"B asked A again after the substitute — it had refused it: {served!r}"
    )
    trending = ws_api.trending_feed_posts(port_b, b_user)
    assert trending and trending[0]["post_id"] == post_id, (
        "B's Trending read surfaces the genuine post"
    )
