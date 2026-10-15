"""tier_3 e2e witnesses for the nest half of the mail health readout
(``fauna.bridges.mail_health``; ``docs/goal/behavior/mail-deliverability.md``
§ The mail health readout on ``admin-mail`` and § Blocklist self-check).

Every assertion is read from OUTSIDE the nest over the Admin WS-RPC — the health
read, the self-check run's reply, the warm-up status — with no app driver: these
are the ``[nest]`` outcomes of ``docs/features/admin-mail-health.md``. The app
half (the status line, the rows, the link) is ``tests/test_admin_mail_health.py``.

Neither DNS nor the minute is real. ``POST /api/v1/test/dns/record`` (the
``test-hooks`` DNS seam) makes the outbound address ``mail.<primary>`` resolve
and a DNSBL list it, or makes a list not answer; ``POST
/api/v1/test/outbound/clock`` moves the mail clock past the once-a-minute
force-refresh limit. Both hooks are present in the standard e2e nest build.

Shared-nest hygiene: a test that seeds a listing ends by recording a real
not-listed answer for every list and removing its seeds, so the session nest's
blocklist state reads clean for the app tests that follow.
"""

import base64
import json
import time
import urllib.request
import uuid

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import create_actor_and_register
from helpers.mail_health_seeds import (
    LISTS,
    OUTBOUND_IP,
    PAST_THE_LIMIT,
    PRIMARY,
    admin_client,
    advance_mail_clock,
    answer_listed,
    answer_not_listed,
    post,
    query_name,
    seed_dns_record,
    seeded_outbound as _seeded_outbound,
)
from helpers.mail_wire import (
    _connect_smtp_starttls,
    _poke_outbound_bridge,
    _wait_for_tagged,
    submit_one_message,
)

pytestmark = pytest.mark.tier_3

#: The list this module lists the address on, and the reason it gives.
LISTING = "zen.spamhaus.org"
REASON = "Listed for spam pattern X"

# The DNS seam, the mail clock and the standing-result hygiene are shared with
# the app-half journeys (`tests/test_admin_mail_health.py`).
_admin = admin_client
_post = post
_seed_dns_record = seed_dns_record
_advance_mail_clock = advance_mail_clock
_query_name = query_name
_answer_not_listed = answer_not_listed
_answer_listed = answer_listed


def _blocklist_row(health):
    return health["checks"][1]


@pytest.fixture
def seeded_outbound(nest_instance):
    """The shared seeding (`helpers/mail_health_seeds.seeded_outbound`)."""
    with _seeded_outbound(nest_instance) as nest:
        yield nest


def _listed_then(nest, admin):
    """Record a listing on `LISTING` (with its reason) and return the read."""
    _answer_listed(nest, LISTING, REASON)
    _advance_mail_clock(nest, PAST_THE_LIMIT)
    run = admin.call("fauna.bridges.blocklist_self_check_run", {})
    row = next(r for r in run["results"] if r["server"] == LISTING)
    assert row["listed"] and not row["error"], f"the seeded listing must answer: {run!r}"
    health = admin.call("fauna.bridges.mail_health", {})
    assert LISTING in _blocklist_row(health)["detail"], health
    return health


@pytest.mark.feature("admin-mail-health")
def test_an_unanswered_check_leaves_the_listing_standing(seeded_outbound):
    """A list that does not answer (timeout / SERVFAIL) keeps the earlier
    result standing: the address still reads as listed, with the list's reason,
    and the removal link stays (mail-deliverability.md § Blocklist self-check
    → *Admin-pane rendering*: "the prior result is preserved until the next
    successful query")."""
    nest = seeded_outbound
    with _admin(nest) as admin:
        listed = _listed_then(nest, admin)
        assert _blocklist_row(listed)["state"] == "fail", listed
        assert listed["delist_url"], f"a listing offers the removal link: {listed!r}"

        # The list stops answering; the next real check gets no answer from it.
        _seed_dns_record(nest, _query_name(LISTING), "A", transient=True)
        _advance_mail_clock(nest, PAST_THE_LIMIT)
        run = admin.call("fauna.bridges.blocklist_self_check_run", {})
        row = next(r for r in run["results"] if r["server"] == LISTING)
        assert row["error"] and not row["listed"], f"the list must not answer: {run!r}"

        after = admin.call("fauna.bridges.mail_health", {})
        blocklist = _blocklist_row(after)
        assert blocklist["state"] == "fail", (
            f"an unanswered check must not clear the listing: {after!r}"
        )
        assert LISTING in blocklist["detail"] and REASON in blocklist["detail"], after
        assert after["delist_url"] == listed["delist_url"], after
        if after["state"] not in ("off", "bridge_down"):
            assert after["state"] == "blocklisted", after

        # The list answers again, not listed: now — and only now — it clears.
        _answer_not_listed(nest, LISTING)
        _advance_mail_clock(nest, PAST_THE_LIMIT)
        admin.call("fauna.bridges.blocklist_self_check_run", {})
        cleared = admin.call("fauna.bridges.mail_health", {})
        assert _blocklist_row(cleared)["state"] == "pass", cleared
        assert cleared["delist_url"] is None, cleared


@pytest.mark.feature("admin-mail-health")
def test_a_second_check_within_the_minute_asks_no_list_and_keeps_the_listing(
    seeded_outbound,
):
    """Check again twice within a minute: the second press asks no list (every
    list is reported rate-limited, and the history gains no row), and the
    listing the first check found stands (§ Force-refresh)."""
    nest = seeded_outbound
    with _admin(nest) as admin:
        listed = _listed_then(nest, admin)
        before = admin.call(
            "fauna.bridges.list_blocklist_self_check_history", {"window_days": 0}
        )["rows"]

        # Even were the list to answer differently now, it is not asked.
        _answer_not_listed(nest, LISTING)
        second = admin.call("fauna.bridges.blocklist_self_check_run", {})
        assert all("rate-limited" in r["error"] for r in second["results"]), second
        after_rows = admin.call(
            "fauna.bridges.list_blocklist_self_check_history", {"window_days": 0}
        )["rows"]
        assert len(after_rows) == len(before), (
            "a rate-limited press asks no list, so it records no check: "
            f"{len(before)} → {len(after_rows)}"
        )

        after = admin.call("fauna.bridges.mail_health", {})
        assert _blocklist_row(after) == _blocklist_row(listed), after
        assert after["state"] == listed["state"], after
        assert after["delist_url"] == listed["delist_url"], after


@pytest.mark.feature("admin-mail-health")
def test_a_member_cannot_read_mail_health(nest_instance):
    """Only an admin reads mail health: a member's read is refused."""
    user = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    client = WsRpcAdminClient(
        nest_instance["url"],
        actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    )
    with client:
        with pytest.raises(RpcCallError) as excinfo:
            client.call("fauna.bridges.mail_health", {})
    assert excinfo.value.code == "fauna.bridges.permission_denied", excinfo.value.code


# ── Reading mail health tells only when, never whose (outcome 22) ─────────────

#: Every field the read carries — the categorical state, the seven rows, the two
#: heartbeat stamps and the removal link (`MailHealthReply`).
REPLY_FIELDS = {
    "state",
    "checks",
    "last_outbound_delivered_at",
    "last_inbound_accepted_at",
    "delist_url",
}
CHECK_FIELDS = {"label_key", "state", "detail"}


@pytest.mark.feature("admin-mail-health")
def test_mail_health_names_no_person_domain_or_message(mail_bridge_mta, nest_instance):
    """With mail flowing on the nest (a recipient, a sender, local domains),
    the read carries only the state, the rows, two bare times and the link:
    no actor, no mail domain, no address and no message identity."""
    handle = mail_bridge_mta
    with _admin(nest_instance) as admin:
        health = admin.call("fauna.bridges.mail_health", {})
    assert set(health) == REPLY_FIELDS, f"unexpected fields: {sorted(health)}"
    for check in health["checks"]:
        assert set(check) == CHECK_FIELDS, f"unexpected row fields: {check!r}"
    for stamp in ("last_outbound_delivered_at", "last_inbound_accepted_at"):
        assert health[stamp] is None or isinstance(health[stamp], int), health
    text = json.dumps(health)
    for needle in (
        handle.domain,
        handle.second_domain,
        handle.recipient_local_part,
        handle.submission_sender_local,
        "@",
        "Message-ID",
    ):
        assert needle not in text, f"the read names {needle!r}: {text}"
    for actor in (handle.recipient_actor, handle.submission_sender_actor):
        actor_id = actor["actor_id_bytes"]
        for form in (actor_id.hex(), base64.b64encode(actor_id).decode()):
            assert form not in text, f"the read names an actor: {text}"


# ── The heartbeats move on real mail (outcomes 20, 21) ────────────────────────


def _health(nest):
    with _admin(nest) as admin:
        return admin.call("fauna.bridges.mail_health", {})


def _wait_for_stamp(nest, field, at_least):
    deadline = time.monotonic() + 30.0
    while time.monotonic() < deadline:
        health = _health(nest)
        if (health[field] or 0) >= at_least:
            return health
        time.sleep(0.25)
    pytest.fail(f"{field} never reached {at_least}: {health!r}")


_submit_one = submit_one_message


@pytest.mark.feature("admin-mail-health")
def test_last_delivered_moves_when_mail_leaves(mail_bridge_mta, nest_instance):
    """Last delivered moves when the nest hands a message to an outside
    server: submit one message to an outside recipient, see the outside MX
    receive it, and the stamp is now at or after the submission."""
    handle = mail_bridge_mta
    token = f"heartbeat-out-{uuid.uuid4().hex[:12]}"
    started = int(time.time())
    _submit_one(handle, f"{token}@external.test", token)
    _poke_outbound_bridge(handle.nest_url)
    assert _wait_for_tagged(handle.stub_mx, token, timeout=30.0) is not None, (
        "the outside MX never received the message"
    )
    _wait_for_stamp(nest_instance, "last_outbound_delivered_at", started)


@pytest.mark.feature("admin-mail-health")
def test_last_received_moves_when_mail_arrives(mail_bridge_mta, nest_instance):
    """Last received moves when the nest accepts a message from outside: an
    outside server delivers to a local recipient over the MX port, and the
    stamp is now at or after the delivery."""
    handle = mail_bridge_mta
    token = f"heartbeat-in-{uuid.uuid4().hex[:12]}"
    started = int(time.time())
    rcpt = f"{handle.recipient_local_part}@{handle.domain}"
    raw = (
        "From: Outside <someone@external.test>\r\n"
        f"To: {rcpt}\r\n"
        f"Subject: heartbeat {token}\r\n"
        f"Message-ID: <{token}@external.test>\r\n"
        "\r\n"
        "An inbound heartbeat probe.\r\n"
    ).encode()
    deadline = time.monotonic() + 30.0
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd("MAIL FROM:<someone@external.test>", "250", deadline)
        conn.cmd(f"RCPT TO:<{rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)
    _wait_for_stamp(nest_instance, "last_inbound_accepted_at", started)


# ── Mail the warm-up holds back is never "delayed" (outcome 12) ───────────────

#: The day-one allowance (§ The ramp); one message to one more outside
#: recipient than this is held back whole until the next day.
DAY_ONE_ALLOWANCE = 50
#: The outbound policy's default delayed-delivery warning age.
WARNING_AGE = 4 * 3_600


def _outbound_queue(nest):
    req = urllib.request.Request(f"{nest['url']}/api/v1/test/outbound/queue", method="GET")
    with urllib.request.urlopen(req, timeout=5.0) as resp:
        assert resp.status == 200, f"outbound queue hook returned {resp.status}"
        return json.loads(resp.read().decode())["rows"]


@pytest.mark.feature("admin-mail-health")
def test_mail_held_by_the_warmup_never_reads_as_delayed(mail_bridge_mta, nest_instance):
    """A message over the day's warm-up allowance waits for tomorrow — pending,
    never attempted — and however long it waits today, mail health never calls
    outgoing mail delayed: the queue row passes and the state is not
    `queue_stalled` (a held message is not a failing one)."""
    handle = mail_bridge_mta
    nest = nest_instance
    token = f"held-{uuid.uuid4().hex[:12]}"
    # An hour into a whole UTC day ahead of every clock the nest has seen, so
    # the held message's release at the next midnight is 23 hours away.
    with _admin(nest) as admin:
        _set_mail_clock(nest, (admin_now(admin) // DAY + 2) * DAY + 3_600)
        admin.call("fauna.bridges.outbound_warmup_reset", {})
    rcpts = [f"{token}-{n}@external.test" for n in range(DAY_ONE_ALLOWANCE + 1)]
    _submit_one(handle, rcpts[0], token, extra_rcpts=rcpts[1:])

    deadline = time.monotonic() + 30.0
    while True:
        held = [r for r in _outbound_queue(nest) if r["recipient"] in rcpts]
        if held:
            break
        assert time.monotonic() < deadline, f"the message was never queued: {held!r}"
        time.sleep(0.25)
    assert all(r["status"] == "pending" and r["attempt_count"] == 0 for r in held), held

    _advance_mail_clock(nest, WARNING_AGE + 3_600)
    assert all(
        r["status"] == "pending" and r["attempt_count"] == 0
        for r in _outbound_queue(nest)
        if r["recipient"] in rcpts
    ), "the held message must still be waiting for tomorrow"
    health = _health(nest)
    assert health["state"] != "queue_stalled", health
    assert health["checks"][2]["state"] == "pass", health


def admin_now(admin):
    """The nest's mail clock, as the warm-up read reports it (the newest of
    its stamps, or the real clock when it has none)."""
    status = admin.call("fauna.bridges.outbound_warmup_status", {})
    stamps = [status.get("first_outbound_at") or 0, status.get("last_reset_at") or 0]
    return max([int(time.time()), *stamps])


# ── Restarting the warm-up keeps the lifetime count (outcome 28) ──────────────


@pytest.mark.feature("admin-mail-health")
def test_restarting_the_warmup_keeps_the_lifetime_count(mail_bridge_mta, nest_instance):
    """After mail has gone out, restarting the warm-up puts the ramp back on
    day 1 and keeps the count of every message the nest has ever sent."""
    handle = mail_bridge_mta
    token = f"lifetime-{uuid.uuid4().hex[:12]}"
    with _admin(nest_instance) as admin:
        before = admin.call("fauna.bridges.outbound_warmup_status", {})["lifetime_total"]
    _submit_one(handle, f"{token}@external.test", token)
    deadline = time.monotonic() + 30.0
    while True:
        with _admin(nest_instance) as admin:
            sent = admin.call("fauna.bridges.outbound_warmup_status", {})
        if sent["lifetime_total"] > before:
            break
        assert time.monotonic() < deadline, f"the send was never counted: {sent!r}"
        time.sleep(0.25)
    with _admin(nest_instance) as admin:
        reset = admin.call("fauna.bridges.outbound_warmup_reset", {})
        status = admin.call("fauna.bridges.outbound_warmup_status", {})
    assert reset["current_day"] == 1 and reset["today_used"] == 0, reset
    assert reset["lifetime_total"] == sent["lifetime_total"], (
        f"a restart must keep the lifetime count: {sent!r} → {reset!r}"
    )
    assert status["lifetime_total"] == sent["lifetime_total"], status


# ── The checks only report (outcome 31) ───────────────────────────────────────


def _settings_snapshot(admin):
    """Every mail setting and record the checks could conceivably touch."""
    return {
        "mail_config": admin.call("fauna.bridges.get_mail_config", {}),
        "domains": admin.call("fauna.bridges.list_local_domains", {}),
        "dkim": admin.call("fauna.bridges.list_dkim_selectors", {"domain": None}),
        "dns": admin.call("fauna.dns.list_records", {"domain": None}),
        "warmup": admin.call("fauna.bridges.outbound_warmup_status", {}),
    }


@pytest.mark.feature("admin-mail-health")
def test_running_the_checks_changes_no_record_or_setting(seeded_outbound):
    """Check again — the blocklist check and the records check — finds a
    listing and failing records, and changes nothing: every mail setting, every
    domain and DKIM row, every DNS record the nest offers and the warm-up are
    exactly as they were (§ No automated remediation)."""
    nest = seeded_outbound
    with _admin(nest) as admin:
        before = _settings_snapshot(admin)
        _answer_listed(nest, LISTING, REASON)
        _advance_mail_clock(nest, PAST_THE_LIMIT)
        run = admin.call("fauna.bridges.blocklist_self_check_run", {})
        assert any(r["listed"] for r in run["results"]), run
        diag = admin.call("fauna.bridges.run_deliverability_diagnostics", {})
        assert any(c["status"] == "fail" for c in diag["checks"]), diag
        after = _settings_snapshot(admin)
    for key in before:
        assert after[key] == before[key], (
            f"the checks changed {key}: {before[key]!r} → {after[key]!r}"
        )


# ── The ramp: its curve, and nothing but a restart moves it back ──────────────
# (mail-server outcomes 38 and 39; `mail-deliverability.md` § The ramp)

DAY = 86_400
#: § The ramp's exact anchors.
RAMP_ANCHORS = {1: 50, 2: 100, 3: 200, 4: 400, 5: 750, 6: 1500, 7: 3000, 14: 10_000}


@pytest.fixture(scope="module")
def ramp_nest(request, nest_mode, tmp_path_factory):
    """A dedicated nest these tests move through days and restart — never the
    session-shared nest, whose mail clock and process other tests share."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "nest-warmup-ramp"
    )
    try:
        yield nest
    finally:
        cleanup()


def _set_mail_clock(nest, epoch):
    _post(nest, "/api/v1/test/outbound/clock", {"set": epoch})


def _warmup_on_day(nest, start, day):
    _set_mail_clock(nest, start + (day - 1) * DAY)
    with _admin(nest) as admin:
        return admin.call("fauna.bridges.outbound_warmup_status", {})


def _ramp_start():
    """An hour into a whole UTC day, well ahead of the real clock."""
    return (int(time.time()) // DAY + 2) * DAY + 3_600


@pytest.mark.feature("mail-server")
def test_the_allowance_grows_along_the_ramp_and_lifts_after_thirty_days(ramp_nest):
    """Day by day the daily allowance follows § The ramp: exact at its
    anchors, rising between them, and lifted altogether from day 30."""
    nest = ramp_nest
    start = _ramp_start()
    _set_mail_clock(nest, start)
    with _admin(nest) as admin:
        admin.call("fauna.bridges.outbound_warmup_reset", {})
    previous = 0
    for day in range(1, 30):
        status = _warmup_on_day(nest, start, day)
        assert status["current_day"] == day, status
        allowance = status["today_max"]
        assert allowance is not None, f"day {day} is still capped: {status!r}"
        if day in RAMP_ANCHORS:
            assert allowance == RAMP_ANCHORS[day], f"day {day}: {allowance}"
        if 8 <= day <= 13:
            assert 3_000 < allowance < 10_000, f"day {day}: {allowance}"
        if 15 <= day <= 29:
            assert 10_000 < allowance <= 50_000, f"day {day}: {allowance}"
        assert allowance > previous, f"day {day} did not grow: {previous} → {allowance}"
        previous = allowance
    for day in (30, 31, 90):
        status = _warmup_on_day(nest, start, day)
        assert status["today_max"] is None, f"day {day} must be uncapped: {status!r}"


@pytest.mark.feature("mail-server")
def test_a_nest_restart_leaves_the_ramp_where_it_was(ramp_nest):
    """Only an admin's restart moves the ramp back: reading it never does, and
    stopping and starting the nest leaves it on the same day."""
    from common.nest import restart_nest

    nest = ramp_nest
    start = _ramp_start()
    _set_mail_clock(nest, start)
    with _admin(nest) as admin:
        admin.call("fauna.bridges.outbound_warmup_reset", {})
    on_day_six = _warmup_on_day(nest, start, 6)
    assert on_day_six["current_day"] == 6, on_day_six
    assert _warmup_on_day(nest, start, 6) == on_day_six, "a read moved the ramp"

    restart_nest(nest, graceful=True)
    # The mail clock is the test's to set, not the nest's to keep: the same
    # moment again, read from the restarted process.
    after = _warmup_on_day(nest, start, 6)
    assert after == on_day_six, f"a restart moved the ramp: {on_day_six!r} → {after!r}"


# ── Several problems at once: the worst is named (outcome 16) ─────────────────

#: Worst first (`mail-deliverability.md` § The mail health readout, the states
#: table).
ORDER = (
    "off",
    "bridge_down",
    "blocklisted",
    "queue_stalled",
    "records_failing",
    "warming_up",
    "delivering",
)


def _holding(health, mail_enabled):
    """The states whose condition the read's own rows show holding."""
    rows = health["checks"]
    held = []
    if not mail_enabled:
        held.append("off")
    if rows[0]["state"] == "fail":
        held.append("bridge_down")
    if rows[1]["state"] == "fail":
        held.append("blocklisted")
    if rows[2]["state"] == "fail":
        held.append("queue_stalled")
    if rows[3]["state"] == "fail":
        held.append("records_failing")
    if rows[4]["state"] == "info" and "day" in rows[4]["detail"]:
        held.append("warming_up")
    return held


def _assert_worst_named(health, mail_enabled, at_least):
    held = _holding(health, mail_enabled)
    assert len(held) >= at_least, f"expected several problems at once: {held} in {health!r}"
    assert health["state"] == min(held, key=ORDER.index), (
        f"with {held} holding the read must name the worst: {health!r}"
    )


@pytest.mark.feature("admin-mail-health")
def test_the_worst_of_several_problems_is_named(mail_bridge_mta, seeded_outbound):
    """With the mail helpers connected, the address listed, the records failing
    and the ramp under way all at once, mail health says blocklisted — and
    whichever problems hold, the state is the worst of them in the stated order."""
    nest = seeded_outbound
    with _admin(nest) as admin:
        admin.call("fauna.bridges.run_deliverability_diagnostics", {})
        records = admin.call("fauna.bridges.mail_health", {})
        _assert_worst_named(records, mail_enabled=True, at_least=1)
        listed = _listed_then(nest, admin)
    _assert_worst_named(listed, mail_enabled=True, at_least=2)
    assert "blocklisted" in _holding(listed, True), listed
    assert "records_failing" in _holding(listed, True), listed


@pytest.mark.feature("admin-mail-health")
def test_off_and_not_connected_outrank_a_listing(ramp_nest):
    """On a nest with no mail helper, a listed address is outranked: while mail
    is off the read says off, and once mail is on, not connected — never
    blocklisted while either holds."""
    nest = ramp_nest
    domain = "health-order.test"
    with _admin(nest) as admin:
        admin.call(
            "fauna.bridges.add_local_domain",
            {"domain": domain, "mta_sts_cert_mode": "expand_primary"},
        )
        primary = next(
            d["domain_name"]
            for d in admin.call("fauna.bridges.list_local_domains", {})["active"]
            if d.get("is_primary")
        )
        _seed_dns_record(nest, f"mail.{primary}", "A", [OUTBOUND_IP])
        _answer_listed(nest, LISTING, REASON)
        _advance_mail_clock(nest, PAST_THE_LIMIT)
        admin.call("fauna.bridges.blocklist_self_check_run", {})
        admin.call("fauna.bridges.set_mail_enabled", {"enabled": False})
        off = admin.call("fauna.bridges.mail_health", {})
        admin.call("fauna.bridges.set_mail_enabled", {"enabled": True})
        on = admin.call("fauna.bridges.mail_health", {})
    assert off["state"] == "off", off
    _assert_worst_named(on, mail_enabled=True, at_least=2)
    assert on["state"] == "bridge_down", on
    assert "blocklisted" in _holding(on, True), on


# ── An idle nest is not a problem (outcome 19) ────────────────────────────────


@pytest.mark.feature("admin-mail-health")
def test_a_nest_that_never_sent_or_received_is_not_a_problem(dedicated_mail_nest):
    """A fresh mail nest with its helpers connected has sent and received
    nothing — both times read never — and long silence changes nothing: two
    months on the state is the same, and it is not a problem state."""
    nest = dedicated_mail_nest.nest
    problems = {"bridge_down", "blocklisted", "queue_stalled", "records_failing"}
    first = _health(nest)
    assert first["last_outbound_delivered_at"] is None, first
    assert first["last_inbound_accepted_at"] is None, first
    assert first["state"] not in problems, first

    _advance_mail_clock(nest, 60 * DAY)
    later = _health(nest)
    assert later["last_outbound_delivered_at"] is None, later
    assert later["last_inbound_accepted_at"] is None, later
    assert later["state"] == first["state"], (
        f"silence alone moved the state: {first['state']} → {later['state']}"
    )
