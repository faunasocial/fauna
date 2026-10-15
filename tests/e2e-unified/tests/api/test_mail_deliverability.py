"""tier_3 e2e for the deliverability diagnostic + blocklist self-check
(``fauna.bridges.run_deliverability_diagnostics`` +
``fauna.bridges.blocklist_self_check_run``;
``docs/goal/behavior/mail-deliverability.md`` § Symptom diagnostics +
§ Blocklist self-check).

Proves the **RPC plumbing + Admin auth + structured reply shape** against the
real nest binary over the socket: both kinds are Admin-class (admin-pane-only —
NOT MTA-class), a non-admin User is denied by the allowlist, the diagnostic
returns a structured checklist, and the blocklist sweep returns the default
DNSBL set with the per-DNSBL force-refresh rate-limit enforced.

The check *verdict* logic (pass/warn/fail interpretation of SPF/DKIM/DMARC/
MTA-STS/TLSRPT/PTR/STARTTLS) is covered in-process by the nest unit tests
(``mail_deliverability::tests`` — scripted resolver + fake prober). Here the
test domain has no published records, so the verdicts are fail/warn; the test
asserts the row *shape* + valid status tokens, not specific passes. The e2e nest
build installs the Null STARTTLS prober (no real gmail connect), so the run is
deterministic + fast.
"""

import secrets

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import create_actor_and_register
from helpers.mail_wire import outbound_clock_now
from helpers.recipient_seal_key import provision_recipient_seal_key

pytestmark = pytest.mark.tier_3

DOMAIN = "deliverability-e2e.test"
VALID_STATUS = {"pass", "warn", "fail"}
# The self-check's own default set (`mail.outbound.blocklist_self_check_servers`),
# deliberately broader than the inbound `mail.inbound.dnsbl_servers` default of
# `zen.spamhaus.org` alone. Owner: `docs/goal/behavior/mail-deliverability.md`
# § Blocklist self-check; the Rust constant is
# `fauna_mail::deliverability::DEFAULT_BLOCKLIST_SELF_CHECK_SERVERS`.
#
# ⚠ This set used to carry
# a fourth entry, `sbl.sorbs.net`, and it was the TEST that was stale. SORBS was
# dropped from the default on 2026-07-08 — the service was decommissioned in 2024,
# so its dark zones NXDOMAIN and a query against it yields a permanently-green
# "not listed", which is worse than no check at all. The drop is ratified in the
# goal doc AND carries the same dated rationale on the Rust constant; only this
# mirror was missed, and nothing runs these suites on a merge path
# (`merge-gate-check.md` § Accepted gaps item (6)), so it sat red.
DEFAULT_DNSBLS = {
    "zen.spamhaus.org",
    "b.barracudacentral.org",
    "bl.spamcop.net",
}


def _admin_client(nest_instance):
    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _user_client(nest_instance, user):
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=user["actor_id_bytes"],
        signing_key=bytes(user["signing_key"]),
    )


def _ensure_domain(admin):
    """Idempotently add the test domain so a primary mail domain exists for
    the diagnostic to run its full check set against."""
    admin.call(
        "fauna.bridges.add_local_domain",
        {
            "domain": DOMAIN,
            "mta_sts_cert_mode": "expand_primary",
        },
    )


@pytest.mark.feature("admin-mail-policy")
def test_admin_runs_diagnostics_over_wire(nest_instance):
    """The diagnostic returns a structured checklist with the standard
    (domain-independent) check rows + valid status tokens + a real ran_at."""
    admin = _admin_client(nest_instance)
    with admin:
        _ensure_domain(admin)
        reply = admin.call("fauna.bridges.run_deliverability_diagnostics", {})

    checks = reply.get("checks")
    assert isinstance(checks, list) and checks, f"expected a non-empty checklist: {reply!r}"
    assert reply.get("ran_at", 0) > 0, f"ran_at must be set: {reply!r}"
    for row in checks:
        assert set(row) >= {"name", "status", "detail"}, f"bad row shape: {row!r}"
        assert row["status"] in VALID_STATUS, f"invalid status token: {row!r}"

    names = {row["name"] for row in checks}
    # Domain-independent rows the orchestrator always emits when a primary
    # mail domain is configured (DKIM rows are selector-dependent, so not
    # asserted here).
    for expected in (
        "SPF record present",
        "DMARC record present",
        "MTA-STS record present",
        "TLSRPT record present",
        "Reverse-DNS for outbound IP",
        "Outbound TLS to gmail.com",
    ):
        assert expected in names, f"missing check {expected!r}; got {sorted(names)}"


@pytest.mark.feature("admin-mail-policy")
def test_non_admin_denied_on_diagnostics(nest_instance):
    """A User-class actor is rejected by the allowlist (admin-pane-only)."""
    user = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    with _user_client(nest_instance, user) as client:
        with pytest.raises(RpcCallError) as excinfo:
            client.call("fauna.bridges.run_deliverability_diagnostics", {})
    assert excinfo.value.code == "fauna.bridges.permission_denied", (
        f"User on an Admin diagnostic kind must be denied; got {excinfo.value.code!r}"
    )


@pytest.mark.feature("admin-mail-policy")
def test_admin_runs_blocklist_self_check_and_rate_limit(nest_instance):
    """The blocklist sweep returns the default DNSBL set; an immediate second
    force-refresh is rate-limited (1/min/DNSBL)."""
    admin = _admin_client(nest_instance)
    with admin:
        first = admin.call("fauna.bridges.blocklist_self_check_run", {})
        assert first.get("checked_at", 0) > 0, f"checked_at must be set: {first!r}"
        servers = {row["server"] for row in first["results"]}
        assert servers == DEFAULT_DNSBLS, f"expected the default DNSBL set; got {servers}"
        for row in first["results"]:
            assert set(row) >= {"server", "listed", "reason", "error"}, f"bad row: {row!r}"

        # Immediate re-run → every server is force-refresh rate-limited.
        second = admin.call("fauna.bridges.blocklist_self_check_run", {})
        assert all("rate-limited" in row["error"] for row in second["results"]), (
            f"second force-refresh must be rate-limited per DNSBL: {second!r}"
        )


@pytest.mark.feature("admin-mail-policy")
def test_non_admin_denied_on_blocklist_self_check(nest_instance):
    """A User-class actor is rejected by the allowlist (admin-pane-only)."""
    user = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    with _user_client(nest_instance, user) as client:
        with pytest.raises(RpcCallError) as excinfo:
            client.call("fauna.bridges.blocklist_self_check_run", {})
    assert excinfo.value.code == "fauna.bridges.permission_denied", (
        f"User on an Admin blocklist kind must be denied; got {excinfo.value.code!r}"
    )


# ── Fresh-IP warm-up (mail-deliverability.md § Fresh-IP warm-up) ──────────────

WARMUP_FIELDS = {
    "current_day",
    "today_used",
    "today_max",
    "ramp_end_date",
    "lifetime_total",
    "first_outbound_at",
    "last_reset_at",
}


@pytest.mark.feature("admin-mail-policy", "mail-server")
def test_admin_reads_warmup_status(nest_instance):
    """A fresh deployment (no outbound yet) reports day 1, nothing used, the
    day-1 cap of 50, and 'never' (0) for both timestamps."""
    admin = _admin_client(nest_instance)
    with admin:
        reply = admin.call("fauna.bridges.outbound_warmup_status", {})
    assert set(reply) >= WARMUP_FIELDS, f"bad status shape: {reply!r}"
    assert reply["current_day"] == 1, f"fresh deployment is day 1: {reply!r}"
    assert reply["today_used"] == 0
    assert reply["today_max"] == 50, f"day-1 cap is 50: {reply!r}"
    assert reply["first_outbound_at"] == 0, "no outbound yet → 0 (never)"
    assert reply["last_reset_at"] == 0, "no reset yet → 0 (never)"


@pytest.mark.feature("admin-mail-health")
def test_admin_resets_warmup_round_trip(nest_instance):
    """Reset stamps first_outbound_at + last_reset_at to now, restarts at day 1,
    and the follow-up status read agrees."""
    admin = _admin_client(nest_instance)
    with admin:
        reset = admin.call("fauna.bridges.outbound_warmup_reset", {})
        assert reset["current_day"] == 1, f"reset restarts at day 1: {reset!r}"
        assert reset["today_used"] == 0
        assert reset["today_max"] == 50
        assert reset["first_outbound_at"] > 0, f"reset stamps first_outbound_at: {reset!r}"
        assert reset["last_reset_at"] > 0, f"reset stamps last_reset_at: {reset!r}"
        # A subsequent status read sees the same reset state.
        status = admin.call("fauna.bridges.outbound_warmup_status", {})
        assert status["last_reset_at"] == reset["last_reset_at"]
        assert status["first_outbound_at"] == reset["first_outbound_at"]


@pytest.mark.feature("admin-mail-policy")
def test_non_admin_denied_on_warmup_status(nest_instance):
    """A User-class actor is rejected by the allowlist (admin-only reset/read)."""
    user = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    with _user_client(nest_instance, user) as client:
        with pytest.raises(RpcCallError) as excinfo:
            client.call("fauna.bridges.outbound_warmup_status", {})
    assert excinfo.value.code == "fauna.bridges.permission_denied", (
        f"User on the Admin warmup-status kind must be denied; got {excinfo.value.code!r}"
    )


@pytest.mark.feature("admin-mail-policy")
def test_non_admin_denied_on_warmup_reset(nest_instance):
    """A User-class actor cannot reset the deployment-wide warm-up."""
    user = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    with _user_client(nest_instance, user) as client:
        with pytest.raises(RpcCallError) as excinfo:
            client.call("fauna.bridges.outbound_warmup_reset", {})
    assert excinfo.value.code == "fauna.bridges.permission_denied", (
        f"User on the Admin warmup-reset kind must be denied; got {excinfo.value.code!r}"
    )


# ── Deliverability history reads (mail-deliverability.md § Operator-visible audit)

@pytest.mark.feature("admin-mail-policy")
def test_admin_reads_blocklist_self_check_history(nest_instance):
    """After a blocklist self-check is recorded, the history read returns it with
    the same structured per-DNSBL verdicts a live run carries (newest first)."""
    admin = _admin_client(nest_instance)
    with admin:
        # Record one sweep, then read the history back.
        run = admin.call("fauna.bridges.blocklist_self_check_run", {})
        history = admin.call(
            "fauna.bridges.list_blocklist_self_check_history", {"window_days": 30}
        )
        rows = history.get("rows")
        assert isinstance(rows, list) and rows, f"expected a non-empty history: {history!r}"
        latest = rows[0]
        assert latest["checked_at"] == run["checked_at"], (
            f"newest-first row must be the run just recorded: {latest!r} vs {run!r}"
        )
        # The row reuses the live-run BlocklistServerResult shape, not a raw string.
        servers = {r["server"] for r in latest["results"]}
        assert servers == DEFAULT_DNSBLS, f"history row carries the DNSBL set: {latest!r}"
        for r in latest["results"]:
            assert set(r) >= {"server", "listed", "reason", "error"}, f"bad row: {r!r}"


@pytest.mark.feature("admin-mail-policy", "admin-mail-health")
def test_nest_runs_its_daily_blocklist_self_check_unasked(nest_instance):
    """The nest's own daily pass — the one its timer runs at ~03:00 UTC, driven
    here once through the `test-hooks` endpoint that calls the same function —
    records a blocklist check of the default DNSBL set that no admin asked for,
    is not held back by the admin's one-a-minute refresh limit, and records a
    diagnostics run made by the nest itself, not by an admin."""
    from conftest import _bridge_admin_post

    def _limited(row):
        return any("rate-limited" in (r.get("error") or "") for r in row["results"])

    admin = _admin_client(nest_instance)
    with admin:
        _ensure_domain(admin)
        # Two admin force-refreshes just before leave the per-DNSBL limit hot
        # (the second is always rate-limited). The daily pass is the cadence,
        # not a force-refresh, so that limit must not touch it.
        admin.call("fauna.bridges.blocklist_self_check_run", {})
        limited = admin.call("fauna.bridges.blocklist_self_check_run", {})
    assert _limited(limited), f"precondition: the second admin refresh is rate-limited: {limited!r}"
    ran = _bridge_admin_post(nest_instance["url"], nest_instance["admin"]["token"],
                             "/api/v1/test/mail/blocklist-self-check/run", {}, timeout=120.0)
    assert ran == {"ran": True}, ran
    with admin:
        rows = admin.call("fauna.bridges.list_blocklist_self_check_history",
                          {"window_days": 30})["rows"]
        diagnostics = admin.call("fauna.bridges.list_deliverability_diagnostic_runs",
                                 {"limit": 1})["rows"]

    daily = [row for row in rows if row["checked_at"] >= limited["checked_at"] and not _limited(row)]
    assert daily, (
        f"the nest's own pass is recorded and, not being an admin force-refresh, is never "
        f"rate-limited: {rows[:3]!r}")
    assert {r["server"] for r in daily[0]["results"]} == DEFAULT_DNSBLS, daily[0]
    assert diagnostics and bytes(diagnostics[0]["ran_by_actor_id"]) == bytes(32), (
        f"the same daily pass records a diagnostics run made by the nest, not an admin: {diagnostics!r}")


@pytest.mark.feature("admin-mail-policy")
def test_admin_reads_diagnostic_run_history(nest_instance):
    """After a diagnostic run is recorded, the history read returns it with the
    structured checklist + the admin actor id that ran it (newest first)."""
    admin = _admin_client(nest_instance)
    admin_actor = bytes(nest_instance["admin"]["signing_key"].verify_key)
    with admin:
        _ensure_domain(admin)
        run = admin.call("fauna.bridges.run_deliverability_diagnostics", {})
        history = admin.call(
            "fauna.bridges.list_deliverability_diagnostic_runs", {"limit": 10}
        )
        rows = history.get("rows")
        assert isinstance(rows, list) and rows, f"expected a non-empty audit: {history!r}"
        latest = rows[0]
        assert latest["ran_at"] == run["ran_at"], (
            f"newest-first row must be the run just recorded: {latest!r} vs {run!r}"
        )
        assert bytes(latest["ran_by_actor_id"]) == admin_actor, (
            f"audit row records the admin actor id: {latest!r}"
        )
        # `checks` reuses the live-run DiagnosticCheckResult shape.
        assert latest["checks"], f"audit row carries the checklist: {latest!r}"
        for row in latest["checks"]:
            assert set(row) >= {"name", "status", "detail"}, f"bad check row: {row!r}"
            assert row["status"] in VALID_STATUS, f"invalid status token: {row!r}"


@pytest.mark.feature("admin-mail-policy")
def test_non_admin_denied_on_blocklist_self_check_history(nest_instance):
    """A User-class actor cannot read the blocklist history (operator-only)."""
    user = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    with _user_client(nest_instance, user) as client:
        with pytest.raises(RpcCallError) as excinfo:
            client.call("fauna.bridges.list_blocklist_self_check_history", {"window_days": 30})
    assert excinfo.value.code == "fauna.bridges.permission_denied", (
        f"User on the Admin blocklist-history kind must be denied; got {excinfo.value.code!r}"
    )


@pytest.mark.feature("admin-mail-policy")
def test_non_admin_denied_on_diagnostic_run_history(nest_instance):
    """A User-class actor cannot read the diagnostic audit (operator-only)."""
    user = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    with _user_client(nest_instance, user) as client:
        with pytest.raises(RpcCallError) as excinfo:
            client.call("fauna.bridges.list_deliverability_diagnostic_runs", {"limit": 10})
    assert excinfo.value.code == "fauna.bridges.permission_denied", (
        f"User on the Admin diagnostic-history kind must be denied; got {excinfo.value.code!r}"
    )


# ── the warm-up's hold on the sender's Sent read ─────────────────────
#
# `mail-deliverability.md` § Enforcement at submission time, the deferred-state
# bullet: a message the warm-up holds back says, where its sender reads their
# sent mail, that it is held, when it will be delivered and how many held
# messages are ahead of it. The list fan-out is a warm-up-counted door, so a
# list wider than the day-1 ramp (50) is held in part however much of today's
# budget earlier tests on this session's nest spent. Nothing here delivers:
# held or not, the rows only sit in the queue.

# Wider than the day-1 cap of 50, so at least five of its rows are held.
_HELD_LIST_MEMBERS = 55


def _list_send(client, domain, *, members, subject):
    local_part = "warmup-" + secrets.token_hex(4)
    list_id = client.call(
        "fauna.bridges.create_account_list",
        {"local_part": local_part, "local_domain": domain},
    )["list_id"]
    for _ in range(members):
        client.call(
            "fauna.bridges.add_list_member",
            {"list_id": list_id, "recipient_address": f"m-{secrets.token_hex(4)}@external.test"},
        )
    message = (
        f"From: {local_part}@{domain}\r\nTo: {local_part}@{domain}\r\nSubject: {subject}\r\n"
        f"Message-ID: <{secrets.token_hex(8)}@{domain}>\r\n"
        "Date: Fri, 09 Oct 2026 12:00:00 +0000\r\n\r\nA warm-up-held list message.\r\n"
    ).encode()
    client.call("fauna.bridges.send_list_message", {"list_id": list_id, "message": message})


@pytest.mark.feature("mail-server")
def test_warmup_held_list_send_reads_back_held_on_sent(nest_instance):
    """Two list sends past the day's ramp read back, through the owner's own
    `fauna.email.sent.fetch`, as held by the warm-up until the next 00:00 UTC,
    the later one behind the earlier one in the held queue."""
    user = create_actor_and_register(
        nest_instance["port"], admin_signing_key=nest_instance["admin"]["signing_key"]
    )
    try:
        with _admin_client(nest_instance) as admin:
            _ensure_domain(admin)
        with _user_client(nest_instance, user) as client:
            # The Sent copy is sealed to the sender, so the sender needs a key.
            provision_recipient_seal_key(client, user["actor_id_bytes"])
            # The hold is stamped on the nest's outbound clock, which an earlier
            # test on this session's nest may have moved off the real one.
            before, _ = outbound_clock_now(nest_instance["url"])
            _list_send(client, DOMAIN, members=_HELD_LIST_MEMBERS, subject="first")
            _list_send(client, DOMAIN, members=_HELD_LIST_MEMBERS, subject="second")
            after, _ = outbound_clock_now(nest_instance["url"])
            sent = client.call("fauna.email.sent.fetch", {"after_uid": 0, "limit": 50})
    finally:
        # Hand later tests on this session's nest a fresh day's budget; the held
        # rows stay held until their release (a reset never releases them).
        with _admin_client(nest_instance) as admin:
            admin.call("fauna.bridges.outbound_warmup_reset", {})

    messages = sent["messages"]
    assert len(messages) == 2, f"each list send files one Sent copy: {messages!r}"
    holds = [m.get("warmup_hold") for m in messages]
    assert all(holds), (
        f"both sends are wider than the day's ramp, so the warm-up holds both: {holds!r}"
    )
    # The next 00:00 UTC after the submission — either side of a midnight the
    # test straddled.
    releases = {(before // 86_400 + 1) * 86_400, (after // 86_400 + 1) * 86_400}
    for hold in holds:
        assert hold["release_at"] in releases, f"held until the next 00:00 UTC: {hold!r}"
    assert holds[1]["messages_ahead"] > holds[0]["messages_ahead"], (
        f"the later send waits behind the earlier one: {holds!r}"
    )
