"""tier_3 end-to-end proof: Layer-B engagement-signal sharing (engagement-cues.md
§ Layer B nest legs).

The distributed-moderation Phase-4 nest legs over REAL ``fauna-nest`` binaries +
real WS-RPC: an opted-in user's derived cue verdict on a PUBLIC post
(``fauna.moderation.signal_contribute``) contributes to a per-content-hash count
that becomes a transparent tier-3 ``signal:watch-complete`` / ``signal:skip``
scores-bus factor ONLY at >= k=3 distinct local contributors — below k nothing is
readable on any surface (wire or bus). It rides the SAME ``content_reports``
table, k-gate, count curve, and export view as ``report:spam`` (no new tables,
no new federation kinds), so this is the report-sharing narrative re-run for a
``signal:*`` factor, PLUS the two signal-specific behaviours:

* **verdict flip** — a contributor's verdict is last-wins per item, so flipping
  ``watch-complete`` → ``skip`` withdraws the old factor's row and inserts the
  new (the whole aggregate migrates factor when every contributor flips).
* **public posts only** — a NEW verdict about a non-public post is rejected at
  the handler (its ``signal:*`` aggregate would leak readership); a ``withdraw``
  is exempt.

A post's content-addressed id IS its report-hash (report-sharing.md § Content
identity), so the report row keys directly on the post id and the bus row lands
on that same ``content_id`` — no mail/SMTP/floor plumbing.

This is a **dedicated API-contract test whose purpose is the wire contract**
(E2E rule 8 carve-out (a); the sibling ``test_report_sharing.py`` drives the
report path over ``ws_api`` identically). The client that WILL call
``signal_contribute`` from a derived cue is the personalization
Phase-4 client leg (gated on this track); the ``CueEngine`` signal producer it
consumes already exists (``engagement-cues.md`` § Implementation status ✅
Phase-3, ``libs/fauna-feed/src/cues.rs``).
"""

import sqlite3
import time

import pytest

from clients._ws_rpc_core import RpcCallError
from common.auth import create_actor_and_register
from tests.api import ws_api
from tests.api.bare import sign_and_encode_post

pytestmark = pytest.mark.tier_3

WC = "signal:watch-complete"
SKIP = "signal:skip"


# ---------------------------------------------------------------------------
# Fixture: a DEDICATED nest (never the session-shared ``nest_instance`` — a
# signal contribution writes per-actor ``content_reports`` / ``content_scores``
# rows that would poison later tests AND corrupt the k-gate math; the
# ``test_report_sharing.py`` precedent). Plaintext storage so
# ``fauna.posts.create`` is not gated ``not_ready``.
# ---------------------------------------------------------------------------
@pytest.fixture()
def signal_nest(request, nest_mode, tmp_path_factory):
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "signal_sharing")
    yield nest
    cleanup()


def _bus_score(db_path: str, post_id_hex: str, factor: str):
    """The bus-row score (per-mille) for a post under ``factor``, or ``None``.

    Read-only against the running nest's SQLite file (a post aggregate's
    ``actor_id`` column is NULL, ``reports.rs`` post attach).
    """
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        row = conn.execute(
            "SELECT score FROM content_scores WHERE content_id = ? AND factor = ?",
            (bytes.fromhex(post_id_hex), factor),
        ).fetchone()
        return None if row is None else row[0]
    finally:
        conn.close()


def _published_entry(port: int, actor: dict, post_id_hex: str, factor: str):
    """The ``signal_share.status.published`` entry for ``(post, factor)``, or
    ``None``. ``published`` is the >=k-gated export view (identical for every
    caller); below k a hash is absent from it entirely."""
    status = ws_api.signal_share_status(port, actor)
    for entry in status.get("published", []):
        if entry["content_hash"] == post_id_hex and entry["factor"] == factor:
            return entry
    return None


def _wait_bus(db_path: str, post_id_hex: str, factor: str, want, timeout=3.0):
    """Poll the bus row until it equals ``want`` (guards content_meta settling —
    the aggregate write is synchronous within the handler)."""
    deadline = time.time() + timeout
    score = _bus_score(db_path, post_id_hex, factor)
    while score != want and time.time() < deadline:
        time.sleep(0.1)
        score = _bus_score(db_path, post_id_hex, factor)
    return score


@pytest.mark.feature("learn-from-my-activity")
def test_signal_sharing_k_gate_transparency_flip_public_gate_and_opt_out(signal_nest):
    """The full Layer-B journey over the real nest binary + real WS-RPC:
    opt-in gating → k-gate at exactly 3 → transparency identity → non-opted
    ignored → verdict flip migrates the factor → public-posts-only rejection →
    opt-out withdrawal."""
    port = signal_nest["port"]
    db_path = signal_nest["db_path"]
    admin_sk = signal_nest["admin"]["signing_key"]

    author = create_actor_and_register(port, admin_signing_key=admin_sk)
    c1 = create_actor_and_register(port, admin_signing_key=admin_sk)
    c2 = create_actor_and_register(port, admin_signing_key=admin_sk)
    c3 = create_actor_and_register(port, admin_signing_key=admin_sk)
    outsider = create_actor_and_register(port, admin_signing_key=admin_sk)

    # One public post — the item every contributor judges. Its content-addressed
    # id IS the report-hash, so all verdicts about it aggregate.
    now_us = int(time.time() * 1_000_000)
    body = "A very good cat video that everyone watches to the end."
    post_bytes = sign_and_encode_post(author["signing_key"], now_us, body, tags=[])
    post_id = ws_api.create_post(port, author, post_bytes)
    time.sleep(0.3)  # let content_meta settle so the bus row can attach

    # ── Opt-in is default-off — a verdict WITHOUT opting in captures nothing.
    reply = ws_api.signal_contribute(port, c1, post_id, "watch-complete")
    assert reply["status"] == "recorded"
    assert _published_entry(port, c1, post_id, WC) is None, (
        "a non-opted-in contributor's verdict must capture nothing (opt-in default off)"
    )
    assert _bus_score(db_path, post_id, WC) is None, "no bus row without opt-in"
    assert ws_api.signal_share_status(port, c1)["share"] is False

    # ── k-gate ramp: opt in + contribute, below k nothing is readable anywhere.
    for i, c in enumerate((c1, c2), start=1):
        assert ws_api.signal_share_set(port, c, True)["share"] is True
        ws_api.signal_contribute(port, c, post_id, "watch-complete")
        assert _published_entry(port, c, post_id, WC) is None, (
            f"below k (={i} contributor(s)) nothing is published anywhere"
        )
        assert _bus_score(db_path, post_id, WC) is None, f"below k (={i}) no bus row"

    # The third distinct opted-in contributor crosses k=3.
    assert ws_api.signal_share_set(port, c3, True)["share"] is True
    ws_api.signal_contribute(port, c3, post_id, "watch-complete")

    # ── Transparency identity — status.published (the export view a peer would
    # receive) now carries exactly (hash, signal:watch-complete, 3).
    entry = _published_entry(port, c3, post_id, WC)
    assert entry is not None, "at k=3 the aggregate becomes readable"
    assert entry["count"] == 3, "the published count is the local contributor count"
    # Same list for every caller (whole-nest export; caller-scoped only for `share`).
    assert _published_entry(port, author, post_id, WC) == entry
    # The tier-3 bus factor landed at 200‰ (k=3; same curve as report:spam).
    assert _wait_bus(db_path, post_id, WC, 200) == 200, "k=3 → 200‰ bus factor"

    # ── A non-opted-in outsider's verdict does NOT count — stays at 3, never 4.
    ws_api.signal_contribute(port, outsider, post_id, "watch-complete")
    assert _published_entry(port, outsider, post_id, WC)["count"] == 3, (
        "a non-opted-in verdict must not raise the count"
    )

    # ── Verdict flip: every contributor flips watch-complete → skip. Each flip
    # withdraws the old factor's row and inserts the new, so the WHOLE aggregate
    # migrates: watch-complete withdrawn, skip now at k=3.
    for c in (c1, c2, c3):
        ws_api.signal_contribute(port, c, post_id, "skip")
    assert _wait_bus(db_path, post_id, WC, None) is None, (
        "flip withdraws watch-complete → below k → withdrawn from the bus"
    )
    assert _published_entry(port, c1, post_id, WC) is None, (
        "watch-complete gone from the export view after the flip"
    )
    assert _wait_bus(db_path, post_id, SKIP, 200) == 200, "skip reaches k=3 after the flip"
    skip_entry = _published_entry(port, c1, post_id, SKIP)
    assert skip_entry is not None and skip_entry["count"] == 3

    # ── Public posts only: a NEW verdict about an unseen/non-public post is
    # rejected at the handler (content_is_public → false → invalid_params). An
    # aggregate on restricted content would leak readership. (The gated-vs-unseen
    # distinction is proven at the db layer: signals.rs::content_is_public_gate.)
    unseen_post = "ab" * 32
    with pytest.raises(RpcCallError) as excinfo:
        ws_api.signal_contribute(port, c1, unseen_post, "watch-complete")
    assert excinfo.value.code == "fauna.moderation.invalid_params", (
        "a verdict about a non-public post is rejected"
    )
    # A withdraw is exempt from the public gate (a retraction reveals nothing and
    # must always succeed) — it is accepted even for the unseen post (no-op).
    assert ws_api.signal_contribute(port, c1, unseen_post, "withdraw")["status"] == "recorded"

    # ── Opt-out withdrawal: one contributor opts out of SIGNAL sharing → their
    # signal rows are deleted + the aggregate recomputes below k → withdrawn from
    # the export view AND the bus. (Independent of report_share by construction.)
    assert ws_api.signal_share_set(port, c3, False)["share"] is False
    assert _published_entry(port, c3, post_id, SKIP) is None, (
        "opt-out drops the aggregate below k → withdrawn from published"
    )
    assert _wait_bus(db_path, post_id, SKIP, None) is None, (
        "opt-out withdraws the signal:skip bus row"
    )
