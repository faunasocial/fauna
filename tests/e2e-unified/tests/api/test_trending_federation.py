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
  · time-decay driving withdraw — ``scoring.rs::velocity_decays_by_half_each_half_life``
    + ``db/trends.rs::sweep_re_decays_and_withdraws``. (The withdraw *mechanism*
    IS exercised here, via the real unlike wire zeroing local velocity.)
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
