"""The report-abuse nest outcomes, observed from outside — full-stack (tier_3) API E2E.

Witnesses the ``[nest]`` outcomes 12–21 of ``docs/features/report-abuse.md``
over real ``fauna-nest`` binaries and the real WS-RPC wire
(``docs/goal/behavior/moderation.md`` § User-initiated reporting). The app
journeys — the sheet, the queue, the ledger — are
``tests/e2e-unified/tests/test_abuse_reporting.py``'s; this module is the
nest's half, driven with no app at all:

* **12** — the reported author is told nothing (§ What the reporter is told).
* **13** — a report changes nothing for anyone else: no label, no score, no
  report-sharing row (§ Anti-abuse posture, bound 4).
* **14** — the per-reporter hourly limit, and one open report per subject: a
  second report on a subject already open is folded into the first (§ Anti-abuse
  posture).
* **15** — a supervised account reports with no guardian step.
* **16** — a report about a foreign author reaches its home nest naming the
  origin nest and no reporter (§ Routing).
* **17** — a home nest that is down when the report is filed gets it once it is
  back: the outbox retry (§ Routing).
* **18** — a nest refuses a forwarded report about a subject it does not host.
* **19** — the outcome the home admin records comes back to the reporter on their
  own nest (§ What the reporter is told).
* **20** — withdrawing deletes the note and the excerpt on both nests (§ Where it
  lands).
* **21** — deleting the reporter's account withdraws the open report and keeps
  the resolved one whole (§ Where it lands, the deletion ruling).

**Nests.** The single-nest outcomes run on a dedicated module nest, never the
session ``nest_instance``: outcome 14 spends a reporter's whole hourly budget,
outcome 13 asserts on table rows, and outcome 21 fires
``POST /api/v1/test/pending_actions/run_due``, which makes every queued pending
action on its nest due. The two-nest outcomes use the handled TLS pair the
federation triad needs — ``report_origin_nest`` (the reporter's nest) and
``cross_nest_foreign`` (the author's home) — and outcome 17 starts a home nest of
its own, because it stops that nest mid-test. A foreign post gets its origin
stamp on the reporter's nest the way a real one does: a discovery feed seeded
with the home nest indexes it. Reads ``peer_url``, so the two-nest tests are a
nest-dials-nest journey, standalone only (``testing.md`` § Default app and nest
mode, ruling (2)).

**Outcome 18's door.** Neither nest will forward a report about a subject the
receiver does not host — the origin resolves the home from the post's own
stamp — so the refusal is driven at the door that makes it: the harness opens
the federation channel *as* ``report_origin_nest`` (its deployment key, the
``FederationChannelClient`` handshake) and calls
``fauna.federation.abuse_report.deliver`` on the home nest, the shape
``test_family_nest_witnesses.py`` uses for the cross-nest family doors. The same
call naming an account the home nest does host is accepted, so "refused" cannot
be read as "this call never works".

Some observations read the running nest's SQLite file read-only (the
``test_report_sharing.py`` pattern): what a report did *not* write, and what a
withdrawal or a deletion left of a row, are not on any reply.

Process safety: no ``pkill``/``killall``. Every nest comes from a fixture's
dedicated-nest seam; outcome 17's stop and restart signal only that fixture's own
process handle (``common.nest.stop_nest`` / ``start_nest_in_place``).
"""

from __future__ import annotations

import os
import sqlite3
import time
import uuid

import pytest
import requests

from actions.api_actor import ApiActor
from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from clients.ws_rpc_federation_client import FederationChannelClient
from common.auth import create_actor_and_register
from helpers.admin_wire import admin_client
from helpers.family_wire import actor_ws, admit_adult_directly, admit_ward
from tests.api import ws_api

pytestmark = pytest.mark.tier_3

RECEIVED = "abuse_report.received"
RESOLVED = "abuse_report.resolved"
#: `fauna_core::scoring::reports::ABUSE_REPORTS_PER_HOUR`.
REPORTS_PER_HOUR = 20
#: `rpc_errors::rate_limited_ns("moderation")`.
RATE_LIMITED = "fauna.moderation.rate_limited"
#: `abuse_report_federation.rs::not_hosted`.
NOT_HOSTED = "fauna.federation.not_found"

# A discovery poll fires when its feed is created; the budget covers the first
# dial to a TLS peer plus the index write.
_DISCOVERY_BUDGET_S = 90.0
# The forward's one inline try waits 4 s and the withdraw and outcome legs are
# nudged straight out of the outbox; a generous ceiling over all three.
_FEDERATION_BUDGET_S = 90.0
# Outcome 17: the first retry is due 60 s after the failed submit and the
# forwarder drains every 30 s (`abuse_report_federation.rs`, FIRST_RETRY_DELAY_SECS
# and DRAIN_INTERVAL), so a restarted home nest has the report within ~90 s of the
# submit. The budget runs from the restart, with room for a loaded box.
_RETRY_BUDGET_S = 180.0


def _poll(check, budget_s, tag):
    """Deadline-poll ``check`` until it returns something truthy; return it."""
    deadline = time.monotonic() + budget_s
    while True:
        got = check()
        if got:
            return got
        if time.monotonic() >= deadline:
            raise AssertionError(f"{tag}: not reached within {budget_s}s")
        time.sleep(0.5)


# ── identities and calls ────────────────────────────────────────────────────


def _actor(nest) -> tuple[dict, ApiActor]:
    """A fresh ordinary account on ``nest`` and its posting handle."""
    who = create_actor_and_register(
        nest["port"], base_url=nest["url"], admin_signing_key=nest["admin"]["signing_key"]
    )
    return who, ApiActor(nest["url"], who["token"], who["actor_id_hex"], bytes(who["signing_key"]))


def _call(nest, who: dict, kind: str, payload: dict) -> dict:
    with actor_ws(nest, who) as ws:
        return ws.call(kind, payload)


def _admin_call(nest, kind: str, payload: dict) -> dict:
    with admin_client(nest) as admin:
        return admin.call(kind, payload)


def _report(nest, reporter: dict, subject: dict, *, reason="harassment", note=None,
            excerpt=None, subject_actor=None) -> dict:
    payload = {"subject": subject, "reason": reason}
    if note is not None:
        payload["note"] = note
    if excerpt is not None:
        payload["excerpt"] = excerpt
    if subject_actor is not None:
        payload["subject_actor"] = subject_actor
    return _call(nest, reporter, "fauna.moderation.abuse_report.submit", payload)


def _post(cid: str) -> dict:
    return {"kind": "post", "cid": cid}


def _mine(nest, reporter: dict) -> list[dict]:
    return _call(nest, reporter, "fauna.moderation.abuse_report.mine", {})["reports"]


def _queue(nest) -> list[dict]:
    return _admin_call(nest, "fauna.moderation.abuse_report.queue", {})["reports"]


def _queued_about(nest, cid: str) -> list[dict]:
    return [r for r in _queue(nest) if r["subject"].get("cid") == cid]


def _notif_types(nest, who: dict) -> list[str]:
    reply = _call(nest, who, "fauna.notifications.list", {"limit": 200})
    return [row.get("notif_type") for row in reply["notifications"]]


def _admin_notif_count(nest, notif_type: str) -> int:
    reply = _admin_call(nest, "fauna.notifications.list", {"limit": 200})
    return sum(1 for row in reply["notifications"] if row.get("notif_type") == notif_type)


def _db_rows(nest, sql: str, params: tuple = ()) -> list[tuple]:
    """Read-only rows from the running nest's SQLite file."""
    conn = sqlite3.connect(f"file:{nest['db_path']}?mode=ro", uri=True)
    try:
        return conn.execute(sql, params).fetchall()
    finally:
        conn.close()


def _report_row(nest, *, report_id=None, origin_ref=None) -> tuple | None:
    """``(status, outcome, note, excerpt, reporter_actor)`` of one report row —
    by its own id, or (a forwarded copy) by the origin's ``report_ref``."""
    column, key = ("id", report_id) if report_id is not None else ("origin_report_ref", origin_ref)
    rows = _db_rows(
        nest,
        f"SELECT status, outcome, note, excerpt, reporter_actor FROM abuse_reports WHERE {column} = ?",
        (key,),
    )
    return rows[0] if rows else None


def _token() -> str:
    return uuid.uuid4().hex[:10]


# ── nests ───────────────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def report_nest(request, nest_mode, tmp_path_factory):
    """A nest exclusive to this module's single-nest outcomes (module docstring)."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "abuse-report")
    yield nest
    cleanup()


@pytest.fixture
def stoppable_home_nest(request, nest_mode, tmp_path_factory):
    """A handled TLS home nest of outcome 17's own — the shape of
    ``cross_nest_foreign``, function-scoped so the test may stop it."""
    from conftest import _start_handled_tls_nest

    nest, cleanup = _start_handled_tls_nest(
        request, nest_mode, tmp_path_factory, "abuse-report-home")
    yield nest
    cleanup()


def _foreign_post_on_origin(origin, home, text: str) -> tuple[dict, str]:
    """An author on ``home`` posts ``text``; a discovery feed on ``origin``
    seeded with ``home`` indexes it, which stamps the post with the nest it came
    from. Returns the author and the post id once ``origin`` holds it."""
    author, author_api = _actor(home)
    post_id = author_api.post_to_feed("", text)
    follower, _ = _actor(origin)
    term = text.split()[-1]
    ws_api.create_feed(
        origin["port"], follower, f"discovery {term}",
        rules=[{"BodyContains": {"terms": [term]}}],
        scope="discovery", contributor_seeds=[home["peer_url"]],
    )
    # A discovery stub carries no body, so the discovery feed's own body rule
    # never lists it; an empty-rules feed (every post the nest holds) does.
    everything = ws_api.create_feed(origin["port"], follower, "General")
    _poll(
        lambda: any(p["post_id"] == post_id for p in ws_api.feed_posts(origin["port"], follower, everything)),
        _DISCOVERY_BUDGET_S,
        "the discovery poll indexes the foreign post on the reporter's nest",
    )
    return author, post_id


# ── 12: the author is told nothing ──────────────────────────────────────────


@pytest.mark.feature("report-abuse")
def test_the_reported_author_is_told_nothing(report_nest):
    """Outcome 12. A report and its resolution leave the author's
    notifications and reads exactly as they were, while the admin's doorbell
    and the reporter's outcome prove both notifications really were written."""
    nest = report_nest
    author, author_api = _actor(nest)
    reporter, _ = _actor(nest)
    post_id = author_api.post_to_feed("", f"reported author {_token()}")
    before = _notif_types(nest, author)
    doorbells = _admin_notif_count(nest, RECEIVED)

    report_id = _report(nest, reporter, _post(post_id), note="a note the author never sees")["report_id"]
    assert _admin_notif_count(nest, RECEIVED) == doorbells + 1, "the admins' doorbell rings"
    _admin_call(nest, "fauna.moderation.abuse_report.resolve",
                {"report_id": report_id, "outcome": "acted"})
    _poll(lambda: RESOLVED in _notif_types(nest, reporter), 30.0,
          "the reporter is told the outcome")

    after = _notif_types(nest, author)
    assert after == before, (
        f"a report must write nothing to its author's notifications: before {before!r}, after {after!r}"
    )
    assert _mine(nest, author) == [], "the author's own report ledger holds nothing about it"
    with pytest.raises(RpcCallError):
        _call(nest, author, "fauna.moderation.abuse_report.queue", {})
    assert _call(nest, author, "fauna.posts.get", {"post_id": post_id}).get("legal_takedown") is None, (
        "the author's own read of the post carries no trace of the report"
    )


# ── 13: nothing changes for anyone else ─────────────────────────────────────


def _moderation_trace(nest, post_id: str, author_hex: str) -> dict:
    """Everything a report could have written into the shared moderation
    planes about this post or its author."""
    cid = bytes.fromhex(post_id)
    actor = bytes.fromhex(author_hex)
    return {
        "labels": sorted(_db_rows(
            nest, "SELECT category, confidence FROM content_labels WHERE content_id = ?", (post_id,))),
        "report_scores": _db_rows(
            nest, "SELECT factor, score FROM content_scores "
                  "WHERE (content_id = ? OR actor_id = ?) AND factor LIKE 'report%'", (cid, actor)),
        "content_reports": _db_rows(nest, "SELECT factor, content_kind FROM content_reports"),
    }


@pytest.mark.feature("report-abuse")
def test_a_report_changes_nothing_for_anyone_but_the_reporter(report_nest):
    """Outcome 13. After a report the post is still served to a third account
    and still in its feed, carries no new label, gains no report score, and no
    report-sharing row exists — on the post or its author."""
    nest = report_nest
    author, author_api = _actor(nest)
    reporter, _ = _actor(nest)
    reader, _ = _actor(nest)
    text = f"reported but untouched {_token()}"
    post_id = author_api.post_to_feed("", text)
    # The ingest is done once a reader can fetch the post.
    assert _call(nest, reader, "fauna.posts.get", {"post_id": post_id}).get("body")
    before = _moderation_trace(nest, post_id, author["actor_id_hex"])

    _report(nest, reporter, _post(post_id), reason="spam", note="spam, surely")

    assert _call(nest, reader, "fauna.posts.get", {"post_id": post_id}).get("body"), (
        "a reported post is still served to everyone else"
    )
    feed = ws_api.local_feed_posts(nest["port"], reader, limit=200)
    assert any(p["post_id"] == post_id for p in feed), "and still in a third account's feed"
    after = _moderation_trace(nest, post_id, author["actor_id_hex"])
    assert after == before, f"a report must write no label, score or shared signal: {before!r} → {after!r}"
    assert after["report_scores"] == [] and after["content_reports"] == []


# ── 14: the hourly limit and one open report per subject ────────────────────


@pytest.mark.feature("report-abuse")
def test_a_reporter_is_rate_limited_and_holds_one_open_report_per_subject(report_nest):
    """Outcome 14. A second report on a subject the reporter already has open
    is folded into the first — same id, one ledger row, no second doorbell —
    and once the hour's budget is spent the next new report is refused, while
    the fold still answers (the dedupe runs before the rate check)."""
    nest = report_nest
    reporter, _ = _actor(nest)
    _, author_api = _actor(nest)
    post_id = author_api.post_to_feed("", f"reported twice {_token()}")

    first = _report(nest, reporter, _post(post_id))["report_id"]
    doorbells = _admin_notif_count(nest, RECEIVED)
    second = _report(nest, reporter, _post(post_id), reason="spam")["report_id"]
    assert second == first, "a second open report on the same subject is the first one"
    assert len(_mine(nest, reporter)) == 1, "the reporter's ledger holds one report"
    assert _admin_notif_count(nest, RECEIVED) == doorbells, "the fold rings no second doorbell"

    for _ in range(REPORTS_PER_HOUR - 1):
        _report(nest, reporter, {"kind": "actor", "actor_id": os.urandom(32).hex()}, reason="spam")
    assert len(_mine(nest, reporter)) == REPORTS_PER_HOUR

    with pytest.raises(RpcCallError) as refused:
        _report(nest, reporter, {"kind": "actor", "actor_id": os.urandom(32).hex()}, reason="spam")
    assert refused.value.code == RATE_LIMITED, f"the {REPORTS_PER_HOUR + 1}st report: {refused.value!r}"
    assert len(_mine(nest, reporter)) == REPORTS_PER_HOUR, "the refused report was not recorded"
    assert _report(nest, reporter, _post(post_id))["report_id"] == first, (
        "a retry of an open report is still answered at the cap"
    )
    other, _ = _actor(nest)
    assert _report(nest, other, _post(post_id))["report_id"] != first, (
        "the limit and the fold are per reporter: another account files its own report"
    )


# ── 15: a supervised account reports like anyone else ───────────────────────


@pytest.mark.feature("report-abuse")
def test_a_supervised_accounts_report_needs_no_guardian(report_nest):
    """Outcome 15. A ward's report lands on the admins' queue at once, and
    nothing waits in the guardian's approval queue."""
    nest = report_nest
    guardian = admit_adult_directly(nest, f"abuse-guardian-{os.urandom(3).hex()}")
    ward = admit_ward(nest, guardian, f"abuse-ward-{os.urandom(3).hex()}")
    _, author_api = _actor(nest)
    post_id = author_api.post_to_feed("", f"reported by a ward {_token()}")

    with actor_ws(nest, guardian) as ws:
        approvals_before = ws.call("fauna.family.approvals.list", {}).get("approvals") or []
    report_id = _report(nest, ward, _post(post_id), note="a ward's note")["report_id"]

    assert [r["report_id"] for r in _queued_about(nest, post_id)] == [report_id], (
        "the ward's report is on the admins' queue with no approval step"
    )
    mine = _mine(nest, ward)
    assert [(r["report_id"], r["status"]) for r in mine] == [(report_id, "open")]
    with actor_ws(nest, guardian) as ws:
        approvals_after = ws.call("fauna.family.approvals.list", {}).get("approvals") or []
    assert approvals_after == approvals_before, "nothing waits on the guardian"


# ── 16, 19: forwarded without identity; the outcome comes back ──────────────


@pytest.mark.feature("report-abuse")
def test_a_forwarded_report_names_no_reporter_and_its_outcome_comes_back(
    report_origin_nest, cross_nest_foreign
):
    """Outcomes 16 and 19. A report on a foreign author's post is routed to both
    nests; the home nest's queue row names the origin nest and no reporter, and
    its row stores none; the home admin's outcome reaches the reporter as a
    notification on their own nest and reads resolved in their ledger."""
    origin, home = report_origin_nest, cross_nest_foreign
    note = f"forwarded note {_token()}"
    author, post_id = _foreign_post_on_origin(origin, home, f"forwarded target {_token()}")
    reporter, _ = _actor(origin)

    reply = _report(origin, reporter, _post(post_id), note=note)
    assert reply["routed_to"] == [origin["authority"], home["authority"]], (
        f"the report is routed to the reporter's nest and the author's home: {reply!r}"
    )
    report_id = reply["report_id"]

    row = _poll(lambda: next(iter(_queued_about(home, post_id)), None), _FEDERATION_BUDGET_S,
                "the forwarded report reaches the home nest's queue")
    assert row["origin_nest"] == origin["authority"], f"the copy names its origin nest: {row!r}"
    assert row.get("reporter_handle") is None, f"the copy names no reporter: {row!r}"
    assert row["note"] == note and row["subject_actor"] == author["actor_id_hex"]
    for trace in (reporter["handle"], reporter["actor_id_hex"]):
        assert trace not in repr(row), f"no trace of the reporter ({trace!r}) on the home nest: {row!r}"
    stored = _report_row(home, origin_ref=report_id)
    assert stored is not None and stored[4] is None, "the home nest stores no reporter for the copy"

    _admin_call(home, "fauna.moderation.abuse_report.resolve",
                {"report_id": row["report_id"], "outcome": "acted"})
    _poll(lambda: RESOLVED in _notif_types(origin, reporter), _FEDERATION_BUDGET_S,
          "the home admin's outcome reaches the reporter on their own nest")
    mine = next(r for r in _mine(origin, reporter) if r["report_id"] == report_id)
    assert (mine["status"], mine["outcome"]) == ("resolved", "acted"), f"the ledger reads it: {mine!r}"


# ── 17: the home nest down at submit, up later ──────────────────────────────


@pytest.mark.feature("report-abuse")
def test_a_report_reaches_a_home_nest_that_was_down_once_it_is_back(
    report_origin_nest, stoppable_home_nest
):
    """Outcome 17. With the author's home nest stopped, the report is filed
    and routed to the reporter's nest alone; once the home nest is started again
    on the same data and port, the outbox's retry delivers it and the reporter's
    ledger names the home nest."""
    from common.nest import start_nest_in_place, stop_nest

    origin, home = report_origin_nest, stoppable_home_nest
    _, post_id = _foreign_post_on_origin(origin, home, f"retried target {_token()}")
    reporter, _ = _actor(origin)

    stop_nest(home)
    reply = _report(origin, reporter, _post(post_id), note="filed while the home nest was down")
    assert reply["routed_to"] == [origin["authority"]], (
        f"an unreachable home nest is not named as reached: {reply!r}"
    )
    report_id = reply["report_id"]

    start_nest_in_place(home)
    _poll(lambda: _queued_about(home, post_id), _RETRY_BUDGET_S,
          "the retried forward reaches the restarted home nest")
    _poll(
        lambda: any(r["report_id"] == report_id and home["authority"] in r["routed_to"]
                    for r in _mine(origin, reporter)),
        _FEDERATION_BUDGET_S,
        "the reporter's ledger names the home nest once it was reached",
    )


# ── 18: a nest takes only reports about what it hosts ───────────────────────


@pytest.mark.feature("report-abuse")
def test_a_nest_refuses_a_forwarded_report_about_a_subject_it_does_not_host(
    report_origin_nest, cross_nest_foreign
):
    """Outcome 18, at the federation door (module docstring). A delivery about
    an account of the origin nest — one the home nest does not host — is refused
    and queues nothing; the identical delivery about an account the home nest
    hosts is accepted."""
    origin, home = report_origin_nest, cross_nest_foreign
    stranger, _ = _actor(origin)
    hosted, _ = _actor(home)

    def deliver(fed, ref: str, actor_hex: str):
        return fed.call("fauna.federation.abuse_report.deliver", {
            "report_ref": ref,
            "subject": {"kind": "actor", "actor_id": actor_hex},
            "reason": "spam",
            "origin_nest_id": origin["authority"],
            "origin_nest_url": origin["url"],
        })

    refused_ref, accepted_ref = uuid.uuid4().hex, uuid.uuid4().hex
    with FederationChannelClient(initiator=origin, target=home) as fed:
        with pytest.raises(RpcCallError) as refused:
            deliver(fed, refused_ref, stranger["actor_id_hex"])
        deliver(fed, accepted_ref, hosted["actor_id_hex"])
    assert refused.value.code == NOT_HOSTED, f"a subject not hosted here: {refused.value!r}"
    assert _report_row(home, origin_ref=refused_ref) is None, "the refused delivery queued nothing"
    accepted = _report_row(home, origin_ref=accepted_ref)
    assert accepted is not None and accepted[0] == "open", "a hosted subject's delivery is queued"
    queued = [r for r in _queue(home) if r["subject"].get("actor_id") == hosted["actor_id_hex"]]
    assert len(queued) == 1, f"the accepted copy waits in the home admin's queue: {queued!r}"
    # Tidy the shared home nest's queue (the outcome leg names a ref the origin
    # never filed, which it ignores).
    _admin_call(home, "fauna.moderation.abuse_report.resolve",
                {"report_id": queued[0]["report_id"], "outcome": "dismissed"})


# ── 20: withdrawal deletes the words everywhere ─────────────────────────────


@pytest.mark.feature("report-abuse")
def test_withdrawing_a_report_deletes_its_note_and_excerpt_on_both_nests(
    report_origin_nest, cross_nest_foreign
):
    """Outcome 20. A forwarded report carrying a note and an excerpt is in both
    admins' queues with both; after the reporter withdraws it, neither queue
    lists it and neither nest's row keeps the note or the excerpt."""
    origin, home = report_origin_nest, cross_nest_foreign
    token = _token()
    note, excerpt = f"withdrawn note {token}", f"withdrawn excerpt {token}"
    _, post_id = _foreign_post_on_origin(origin, home, f"withdrawn target {token}")
    reporter, _ = _actor(origin)

    report_id = _report(origin, reporter, _post(post_id), note=note, excerpt=excerpt)["report_id"]
    for nest, tag in ((origin, "the reporter's nest"), (home, "the home nest")):
        row = _poll(lambda nest=nest: next(iter(_queued_about(nest, post_id)), None),
                    _FEDERATION_BUDGET_S, f"the report waits in {tag}'s queue")
        assert (row["note"], row["excerpt"]) == (note, excerpt), f"{tag}'s queue row carries both: {row!r}"

    _call(origin, reporter, "fauna.moderation.abuse_report.withdraw", {"report_id": report_id})

    assert _queued_about(origin, post_id) == [], "the withdrawn report leaves the reporter's nest's queue"
    _poll(lambda: _queued_about(home, post_id) == [], _FEDERATION_BUDGET_S,
          "the withdrawal follows the forward to the home nest's queue")
    for nest, row in ((origin, _report_row(origin, report_id=report_id)),
                      (home, _report_row(home, origin_ref=report_id))):
        assert row is not None and row[0] == "withdrawn", f"{nest['url']}: the row reads withdrawn: {row!r}"
        assert (row[2], row[3]) == (None, None), f"{nest['url']}: the note and excerpt are gone: {row!r}"


# ── 21: deleting the account withdraws what is open ─────────────────────────


def _run_due(nest) -> None:
    """Make every queued pending action due and run the production executor
    once (``pending_actions_test_hook.rs``) — the 14-day cool-off of an account
    deletion, crossed without waiting."""
    resp = requests.post(f"{nest['url']}/api/v1/test/pending_actions/run_due", json={}, timeout=60)
    assert resp.status_code == 200, f"run_due returned {resp.status_code}: {resp.text}"


@pytest.mark.feature("report-abuse")
def test_deleting_the_reporters_account_withdraws_open_reports_and_keeps_resolved_ones(report_nest):
    """Outcome 21. The reporter has one open report and one the admin resolved;
    after their account deletion runs, the open one reads withdrawn with its
    note gone and has left the queue, and the resolved one is whole — status,
    outcome and note."""
    nest = report_nest
    reporter, _ = _actor(nest)
    _, author_api = _actor(nest)
    open_post = author_api.post_to_feed("", f"open at deletion {_token()}")
    resolved_post = author_api.post_to_feed("", f"resolved before deletion {_token()}")
    open_id = _report(nest, reporter, _post(open_post), note="the open note")["report_id"]
    resolved_id = _report(nest, reporter, _post(resolved_post), note="the resolved note")["report_id"]
    _admin_call(nest, "fauna.moderation.abuse_report.resolve",
                {"report_id": resolved_id, "outcome": "acted"})

    _call(nest, reporter, "fauna.account.delete", {})
    assert _report_row(nest, report_id=open_id)[0] == "open", "a scheduled deletion withdraws nothing yet"
    _run_due(nest)

    withdrawn = _report_row(nest, report_id=open_id)
    assert withdrawn[0] == "withdrawn" and withdrawn[2] is None, (
        f"the open report reads withdrawn and its note is gone: {withdrawn!r}"
    )
    assert _queued_about(nest, open_post) == [], "and it has left the admins' queue"
    kept = _report_row(nest, report_id=resolved_id)
    assert kept[:3] == ("resolved", "acted", "the resolved note"), (
        f"the resolved report stays the record of what was decided: {kept!r}"
    )


# ── admin-nest: the doorbell's daily dedupe, and resolving is a record ──────


def _utc_day() -> int:
    return int(time.time()) // 86_400


@pytest.mark.feature("admin-nest")
def test_many_reports_of_one_thing_in_a_day_ring_the_admin_once(report_nest):
    """`docs/features/admin-nest.md` outcome 14 (`moderation.md` § Where it
    lands — the doorbell is deduped per subject and UTC day). Three accounts
    each file their own report on one post: three open reports reach the queue,
    one doorbell rings."""
    nest = report_nest
    _, author_api = _actor(nest)
    post_id = author_api.post_to_feed("", f"piled on {_token()}")
    doorbells = _admin_notif_count(nest, RECEIVED)

    first_day = _utc_day()
    filed = {_report(nest, _actor(nest)[0], _post(post_id), reason=reason)["report_id"]
             for reason in ("spam", "harassment", "hate")}
    days = _utc_day() - first_day + 1

    assert len(filed) == 3 and len(_queued_about(nest, post_id)) == 3, (
        "each account's report is its own open report on the queue"
    )
    rang = _admin_notif_count(nest, RECEIVED) - doorbells
    assert rang >= 1, "the admins' doorbell rings for the first report"
    assert rang <= days, f"a pile-on within one UTC day rings the admin once, rang {rang}"


@pytest.mark.feature("admin-nest")
def test_resolving_a_report_changes_nothing_about_the_content_or_its_author(report_nest):
    """`docs/features/admin-nest.md` outcome 15 (`moderation.md` § Where it
    lands — *resolve as acted* and *dismiss* are records, not actions). After
    one report on a post is resolved as acted and another dismissed, the post is
    still served to a third account, nothing new is written to the moderation
    planes, and its author still posts."""
    nest = report_nest
    author, author_api = _actor(nest)
    reader, _ = _actor(nest)
    post_id = author_api.post_to_feed("", f"resolved but untouched {_token()}")
    assert _call(nest, reader, "fauna.posts.get", {"post_id": post_id}).get("body")
    before = _moderation_trace(nest, post_id, author["actor_id_hex"])

    acted = _report(nest, _actor(nest)[0], _post(post_id))["report_id"]
    dismissed = _report(nest, _actor(nest)[0], _post(post_id), reason="spam")["report_id"]
    _admin_call(nest, "fauna.moderation.abuse_report.resolve", {"report_id": acted, "outcome": "acted"})
    _admin_call(nest, "fauna.moderation.abuse_report.resolve",
                {"report_id": dismissed, "outcome": "dismissed"})
    assert _report_row(nest, report_id=acted)[:2] == ("resolved", "acted")
    assert _report_row(nest, report_id=dismissed)[:2] == ("resolved", "dismissed")

    got = _call(nest, reader, "fauna.posts.get", {"post_id": post_id})
    assert got.get("body") and got.get("legal_takedown") is None, (
        f"a resolved report leaves the post served to everyone: {got!r}"
    )
    assert _moderation_trace(nest, post_id, author["actor_id_hex"]) == before, (
        "resolving writes no label, score or shared signal"
    )
    assert author_api.post_to_feed("", f"still posting {_token()}"), (
        "the reported author's account is untouched: they still post"
    )
