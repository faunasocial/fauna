"""tier_3 end-to-end proof: distributed report sharing (report-sharing.md).

The whole feature over REAL ``fauna-nest`` binaries + real WS-RPC (Slice 5):
an opt-in user's explicit spam flag on a post contributes to a per-content-hash
count that becomes a transparent tier-3 ``report:spam`` scores-bus factor ONLY
at >= k=3 distinct local reporters; below k nothing is readable on any
surface (wire or bus); an opt-out withdraws; a non-opted-in flag captures
nothing.

Post path (a post's content-addressed id IS its report-hash —
``report-sharing.md`` § Content identity), so no mail/SMTP/floor plumbing: the
report row keys directly on the post id and the bus row lands on that same
``content_id``. This is a **dedicated API-contract test whose purpose is the
wire contract** (E2E rule 8 carve-out (a); prior art
``test_content_moderation.py`` drives ``fauna.moderation.*`` over ``ws_api``).

Complementary proofs kept where they already are (honestly noted, not
re-proven over real binaries here):

* **Two-nest federation non-scaling / no-laundering** —
  ``conformance_federation_channel.rs::reports_exchange_is_non_scaling_and_export_never_launders``
  already drives ``fauna.federation.reports.{exchange,export}`` over the REAL
  federation wire between two nests (a two-real-*binary* federation harness does
  not exist and is disproportionate; sanctioned by an internal design decision).
* **Mail ``\\Junk`` k-gate** —
  ``bridge_imap_handlers.rs::a_spam_lesson_captures_the_report_and_a_ham_lesson_withdraws_it``
  proves the identical capture through the real ``put_spam_model`` lesson write;
  ``capture_report`` is shared, so the post path here proves the same aggregate
  logic. Over real SMTP it needs 3 untrained SMTP-delivered recipients —
  disproportionate.
"""

import sqlite3
import time

import pytest

from common.auth import create_actor_and_register
from tests.api import ws_api
from tests.api.bare import sign_and_encode_post

pytestmark = pytest.mark.tier_3


# ---------------------------------------------------------------------------
# Fixture: a DEDICATED nest (never the session-shared ``nest_instance`` — a
# report flag writes per-actor ``content_reports`` / ``content_scores`` rows on
# the nest it hits, which would poison later tests AND corrupt the k-gate math;
# ``e2e_status_and_tips.md`` N+48). Plaintext storage so ``fauna.posts.create``
# is not gated ``not_ready`` (mirrors ``two_nodes`` / the ``logged_in_app`` UI).
# ---------------------------------------------------------------------------
@pytest.fixture()
def report_nest(request, nest_mode, tmp_path_factory):
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "report_sharing")
    yield nest
    cleanup()


def _bus_report_score(db_path: str, post_id_hex: str):
    """The ``report:spam`` bus-row score (per-mille) for a post, or ``None``.

    Mirrors the nest-side ``reports.rs::test_report_row_score`` exactly
    (``content_id`` + ``factor``; a post aggregate's ``actor_id`` column is NULL,
    ``reports.rs:238``). Read-only against the running nest's SQLite file.
    """
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    try:
        row = conn.execute(
            "SELECT score FROM content_scores "
            "WHERE content_id = ? AND factor = 'report:spam'",
            (bytes.fromhex(post_id_hex),),
        ).fetchone()
        return None if row is None else row[0]
    finally:
        conn.close()


def _published_entry(port: int, actor: dict, post_id_hex: str):
    """``report_share.status.published`` entry for ``post_id_hex``, or ``None``.

    ``published`` is the >=k-gated export view (identical for every caller);
    below k a hash is absent from it entirely.
    """
    status = ws_api.report_share_status(port, actor)
    for entry in status.get("published", []):
        if entry["content_hash"] == post_id_hex:
            return entry
    return None


@pytest.mark.feature("spam")
def test_report_sharing_k_gate_opt_in_transparency_and_opt_out(report_nest):
    """The full journey: opt-in gating -> k-gate at exactly 3 -> transparency
    identity -> non-opted flag ignored -> opt-out withdrawal, all over the real
    nest binary + real WS-RPC."""
    port = report_nest["port"]
    db_path = report_nest["db_path"]
    admin_sk = report_nest["admin"]["signing_key"]

    author = create_actor_and_register(port, admin_signing_key=admin_sk)
    r1 = create_actor_and_register(port, admin_signing_key=admin_sk)
    r2 = create_actor_and_register(port, admin_signing_key=admin_sk)
    r3 = create_actor_and_register(port, admin_signing_key=admin_sk)
    outsider = create_actor_and_register(port, admin_signing_key=admin_sk)

    # A single public post — the ONE content every reporter flags. Its
    # content-addressed id IS the report-hash, so all three reports aggregate.
    now_us = int(time.time() * 1_000_000)
    body = "Congratulations! You have won. Click http://spam.example.test to claim."
    post_bytes = sign_and_encode_post(author["signing_key"], now_us, body, tags=[])
    post_id = ws_api.create_post(port, author, post_bytes)
    # Let the post settle into content_meta so the aggregate can attach its bus
    # row (the report-count k-gate itself does not depend on this; the bus row
    # does — reports.rs:230-239).
    time.sleep(0.3)

    # ── Leg 2: opt-in is default-off — a flag WITHOUT opting in captures nothing.
    reply = ws_api.moderation_train(port, r1, post_id, "spam")
    assert reply["status"] == "trained"
    assert _published_entry(port, r1, post_id) is None, (
        "a non-opted-in reporter's flag must capture nothing (opt-in default off)"
    )
    assert _bus_report_score(db_path, post_id) is None, "no bus row without opt-in"
    # The caller's own opt-in state is off.
    assert ws_api.report_share_status(port, r1)["share"] is False

    # ── Leg 1: k-gate ramp. Opt in + flag, reporter by reporter. Below k the
    # aggregate is readable on NO surface (wire export nor bus).
    for i, reporter in enumerate((r1, r2), start=1):
        assert ws_api.report_share_set(port, reporter, True)["share"] is True
        ws_api.moderation_train(port, reporter, post_id, "spam")
        assert _published_entry(port, reporter, post_id) is None, (
            f"below k (={i} reporter(s)) nothing is published anywhere"
        )
        assert _bus_report_score(db_path, post_id) is None, (
            f"below k (={i}) no report:spam bus row"
        )

    # The third distinct opted-in reporter crosses k=3.
    assert ws_api.report_share_set(port, r3, True)["share"] is True
    ws_api.moderation_train(port, r3, post_id, "spam")

    # ── Leg 4: transparency identity — status.published (the export view a peer
    # would receive) now carries exactly (hash, report:spam, 3).
    entry = _published_entry(port, r3, post_id)
    assert entry is not None, "at k=3 the aggregate becomes readable"
    assert entry["factor"] == "report:spam"
    assert entry["count"] == 3, "the published count is the local reporter count"
    # Same list for every caller (whole-nest export; caller-scoped only for `share`).
    assert _published_entry(port, author, post_id) == entry

    # ── The real tier-3 bus factor landed at 200permille (k=3 -> 200; curve
    # report-sharing.md:81). Poll briefly — the write is synchronous within the
    # train handler, this only guards content_meta settling.
    deadline = time.time() + 3.0
    score = _bus_report_score(db_path, post_id)
    while score is None and time.time() < deadline:
        time.sleep(0.1)
        score = _bus_report_score(db_path, post_id)
    assert score == 200, f"k=3 report:spam bus factor is 200permille (got {score})"

    # ── Leg 2 (again): a non-opted-in outsider's flag does NOT count — the
    # aggregate stays at 3, never 4.
    ws_api.moderation_train(port, outsider, post_id, "spam")
    assert _published_entry(port, outsider, post_id)["count"] == 3, (
        "a non-opted-in flag must not raise the count"
    )

    # ── Leg 3: opt-out withdrawal. One reporter opts out -> their rows are
    # deleted + the aggregate recomputes below k -> withdrawn from the export
    # view AND the bus.
    assert ws_api.report_share_set(port, r3, False)["share"] is False
    assert _published_entry(port, r3, post_id) is None, (
        "opt-out drops the aggregate below k -> withdrawn from published"
    )
    assert _bus_report_score(db_path, post_id) is None, (
        "opt-out withdraws the report:spam bus row"
    )
