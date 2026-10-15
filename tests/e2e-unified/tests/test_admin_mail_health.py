"""The mail health readout (mail-deliverability.md § Admin-pane Deliverability
surface → The mail health readout; admin.md § 6 Mail).

A read-only section at the top of `admin-mail`, directly under the mail-enable
toggle: the categorical status line (the shared label of the nest's
`fauna.bridges.mail_health` `state`) plus the two heartbeat facts, seven indexed
check rows in a fixed order, a de-listing link shown only while the outbound IP
is listed, a recheck button and a confirm-gated warm-up reset. The same state
renders as one more `admin-stat-card` ("Mail") on `admin-dashboard`.

tier_3: a real app driver against a real `fauna-nest` binary. Every assertion
reads its ground truth from the nest over the Admin WS-RPC — the health read
itself, the self-check history, the warm-up status — never from UI
introspection alone. Latency-independent throughout (convention 14): each
wait polls for an outcome, none for a duration.

tui is the lead app (the first implementation); the other six follow in their
batched trickle-down and join this module's marks as they land.
"""
import json
import re
import urllib.request
import uuid

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers import budgets
from helpers.app_surface import app_name, skip_unbuilt
from helpers.mail_wire import _poke_outbound_bridge, submit_one_message
from helpers.mail_health_seeds import (
    PAST_THE_LIMIT,
    advance_mail_clock,
    answer_listed,
    answer_nothing,
    answer_not_listed,
    passing_records,
    post,
    run_self_check,
    seeded_outbound,
)
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
    pytest.mark.web,
    pytest.mark.linux,
    pytest.mark.windows,
]

#: The categorical line's English labels (`admin.mail_page.health_state_*`),
#: keyed by the wire token the nest's shared fold returns.
STATE_LABELS = {
    "off": "Mail: off",
    "bridge_down": "Mail: mail service not connected",
    "blocklisted": "Mail: server address is blocklisted",
    "queue_stalled": "Mail: outgoing mail is delayed",
    "records_failing": "Mail: DNS records need attention",
    "warming_up": "Mail: warming up",
    "delivering": "Mail: delivering",
}

#: The seven rows' labels, in the fixed render order the goal doc ratifies.
CHECK_LABELS = [
    "Mail service connection",
    "Blocklist check",
    "Outgoing queue",
    "DNS and authentication records",
    "Sending warm-up",
    "Last delivered",
    "Last received",
]

#: The row verdict labels (`admin.mail_page.health_check_*`).
CHECK_STATE_LABELS = {"OK", "Warning", "Problem", "Info"}

#: The diagnostics need a primary mail domain to run their full check set —
#: the same idempotent domain `tests/api/test_mail_deliverability.py` adds, so
#: this module grows the session nest by nothing new.
DIAGNOSTICS_DOMAIN = "deliverability-e2e.test"

RESET_LABEL = "Restart warm-up"

#: The apps whose `admin-dashboard` does not yet render the shared card set
#: (`fauna_client_admin::admin_dashboard_cards`, whose fold already carries the
#: Mail card — admin.md § 1 Dashboard). tui, web, linux, windows and android
#: render that fold; these two follow in their card-set lift rows, not in this
#: readout's trickle-down.
DASHBOARD_CARD_PENDING = {"macos", "ios"}


def _skip_if_dashboard_card_unbuilt(admin_app) -> None:
    if app_name(admin_app.driver) in DASHBOARD_CARD_PENDING:
        skip_unbuilt(
            admin_app.driver,
            surface="the admin-dashboard Mail stat card",
            detail="the app has not lifted the shared dashboard card set yet",
            tracked="the per-app lift of the shared admin dashboard card set",
        )


def _admin_client(nest_instance):
    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _nest(nest_instance, kind, params=None):
    with _admin_client(nest_instance) as client:
        return client.call(kind, params or {})


def _wait(predicate, budget_s: float = budgets.RPC_ROUNDTRIP_S) -> bool:
    """The shared deadline poll, returning a bool so each caller's own assert
    carries its self-diagnosing message (convention 6)."""
    try:
        return bool(wait_until(predicate, budget_s))
    except AssertionError:
        return bool(predicate())


def _open_readout(admin_app):
    admin_app.admin.navigate_mail()
    assert _wait(admin_app.admin.mail_health_present), (
        "admin-mail-health-section missing — the readout never rendered. "
        f"error: {admin_app.error_text()!r}"
    )


@pytest.fixture
def mail_enabled_restored(nest_instance):
    """Put the deployment-wide mail-enable back the way this test found it —
    the session nest is shared by every later module."""
    before = _nest(nest_instance, "fauna.bridges.get_mail_config")["mail_enabled"]
    yield before
    _nest(nest_instance, "fauna.bridges.set_mail_enabled", {"enabled": before})


@pytest.mark.feature("admin-mail-health")
def test_readout_renders_the_nest_state_and_seven_rows(admin_app, nest_instance):
    """The section renders under the enable toggle: its status line opens with
    the shared label of the state the nest's fold decided, and the seven rows
    render in the ratified order, each with a verdict label."""
    _open_readout(admin_app)

    state = _nest(nest_instance, "fauna.bridges.mail_health")["state"]
    expected = STATE_LABELS[state]
    assert _wait(lambda: admin_app.admin.mail_health_status().startswith(expected)), (
        f"status line {admin_app.admin.mail_health_status()!r} does not open with "
        f"the label of the nest's state {state!r} ({expected!r})"
    )
    # The heartbeat facts follow the state label on the same line.
    status = admin_app.admin.mail_health_status().lower()
    assert "last delivered" in status and "last received" in status, status

    assert _wait(lambda: admin_app.admin.mail_health_check_count() == 7), (
        f"expected 7 health rows, got {admin_app.admin.mail_health_check_count()}"
    )
    rows = [admin_app.admin.mail_health_check(i) for i in range(7)]
    assert [r["label"] for r in rows] == CHECK_LABELS, rows
    for row in rows:
        assert row["state"] in CHECK_STATE_LABELS, f"unlabelled verdict: {row!r}"
    # Rows 5–6 are the heartbeats: the nest leaves their detail empty on purpose
    # and the app paints the stamp there ("Never" on a box that sent nothing).
    for row in rows[5:]:
        assert row["detail"].strip(), f"heartbeat row painted no fact: {row!r}"


@pytest.mark.feature("admin-mail-health")
def test_readout_reads_off_when_mail_is_disabled(
    admin_app, nest_instance, mail_enabled_restored
):
    """With mail disabled the readout is still present and reads the neutral
    `off` label; enabling mail from the same page re-reads it away from `off`."""
    _nest(nest_instance, "fauna.bridges.set_mail_enabled", {"enabled": False})
    _open_readout(admin_app)
    assert _wait(
        lambda: admin_app.admin.mail_health_status().startswith(STATE_LABELS["off"])
    ), f"disabled mail must read off; got {admin_app.admin.mail_health_status()!r}"

    admin_app.admin.toggle_mail_enabled()
    assert _wait(
        lambda: _nest(nest_instance, "fauna.bridges.get_mail_config")["mail_enabled"]
    ), "the enable toggle did not reach the nest"
    assert _wait(
        lambda: not admin_app.admin.mail_health_status().startswith(STATE_LABELS["off"])
    ), (
        "enabling mail must re-read the readout away from off; still "
        f"{admin_app.admin.mail_health_status()!r} (nest: "
        f"{_nest(nest_instance, 'fauna.bridges.mail_health')['state']!r})"
    )


@pytest.mark.feature("admin-mail-health")
def test_recheck_runs_a_self_check_and_rereads(admin_app, nest_instance):
    """The recheck button runs a fresh blocklist self-check and diagnostics run
    — the nest's own history grows — and the readout re-reads: the blocklist
    and records rows no longer say they never ran."""
    with _admin_client(nest_instance) as client:
        client.call(
            "fauna.bridges.add_local_domain",
            {
                "domain": DIAGNOSTICS_DOMAIN,
                "mta_sts_cert_mode": "expand_primary",
            },
        )

    def history_len():
        return len(
            _nest(
                nest_instance,
                "fauna.bridges.list_blocklist_self_check_history",
                {"window_days": 30},
            ).get("rows", [])
        )

    before = history_len()
    _open_readout(admin_app)
    # Each list is force-refreshed at most once a minute of mail clock, and the
    # session nest outlives this test: an earlier parametrization's press (or
    # any earlier self-check) inside that minute would leave this one with no
    # new row to find.
    _press_check_again(admin_app, nest_instance)

    assert _wait(lambda: history_len() > before), (
        f"no new self-check row after recheck (still {before}); "
        f"error: {admin_app.error_text()!r}"
    )
    assert _wait(
        lambda: admin_app.admin.mail_health_check(1)["detail"] != "not checked yet"
        and admin_app.admin.mail_health_check(3)["detail"] != "not run yet"
    ), (
        "the readout did not re-read after recheck: "
        f"{admin_app.admin.mail_health_check(1)!r} / "
        f"{admin_app.admin.mail_health_check(3)!r}"
    )
    assert not admin_app.error_text(), admin_app.error_text()


@pytest.mark.feature("admin-mail-health")
def test_delist_link_absent_while_not_listed(admin_app, nest_instance):
    """The de-listing link shows only while the latest self-check lists the
    outbound IP; the test nest is listed nowhere, so the link is absent."""
    _open_readout(admin_app)
    health = _nest(nest_instance, "fauna.bridges.mail_health")
    assert health.get("delist_url") is None, f"the test nest is listed: {health!r}"
    assert _wait(lambda: admin_app.admin.mail_health_check_count() == 7)
    assert not admin_app.admin.mail_health_delist_present(), (
        "admin-mail-health-delist-link rendered with no listing"
    )


@pytest.mark.feature("admin-mail-health")
def test_warmup_reset_needs_the_confirm(admin_app, nest_instance):
    """The warm-up reset is a two-click confirm: the first press only relabels
    the button (the nest's ramp is untouched); the second restarts the ramp,
    which the nest's own warm-up status records."""
    before = _nest(nest_instance, "fauna.bridges.outbound_warmup_status")["last_reset_at"]
    _open_readout(admin_app)
    assert _wait(lambda: admin_app.admin.mail_warmup_reset_text() == RESET_LABEL), (
        f"unarmed label {admin_app.admin.mail_warmup_reset_text()!r}"
    )

    admin_app.admin.arm_mail_warmup_reset()
    assert _wait(lambda: admin_app.admin.mail_warmup_reset_text() != RESET_LABEL), (
        "the first press did not arm the confirm"
    )
    assert (
        _nest(nest_instance, "fauna.bridges.outbound_warmup_status")["last_reset_at"]
        == before
    ), "the first press reset the ramp without a confirm"

    # The reset stamps the mail clock, which stands still once any test has set
    # it: tick it, or a reset an earlier parametrization made in this same
    # second would leave nothing newer to find.
    advance_mail_clock(nest_instance, 1)
    admin_app.admin.confirm_mail_warmup_reset()
    assert _wait(
        lambda: _nest(nest_instance, "fauna.bridges.outbound_warmup_status")[
            "last_reset_at"
        ]
        > before
    ), f"the confirm did not reset the ramp; error: {admin_app.error_text()!r}"
    # Confirming disarms: the button reads its resting label again.
    assert _wait(lambda: admin_app.admin.mail_warmup_reset_text() == RESET_LABEL)


@pytest.mark.feature("admin-mail-health")
def test_dashboard_mail_card_shows_the_state(admin_app, nest_instance):
    """The dashboard carries one more stat card, "Mail", whose value is the
    same shared label of the nest's state — a broken state is visible on the
    admin shell's landing page."""
    _skip_if_dashboard_card_unbuilt(admin_app)
    admin_app.admin.navigate_dashboard()
    state = _nest(nest_instance, "fauna.bridges.mail_health")["state"]
    assert _wait(
        lambda: admin_app.admin.dashboard_card_value("Mail") == STATE_LABELS[state]
    ), (
        f"Mail card reads {admin_app.admin.dashboard_card_value('Mail')!r}, "
        f"expected {STATE_LABELS[state]!r}"
    )


@pytest.mark.feature("admin-mail-health")
def test_mail_on_with_no_helper_connected_reads_not_connected(
    admin_app, nest_instance, mail_enabled_restored, session_mail_bridges_down
):
    """With mail on and no mail helper holding a live connection — the session
    bridges, if any were spawned, are stopped for this test — the nest folds
    `bridge_down`, and both the Mail page's line and the
    dashboard's Mail card say the mail service is not connected. Pins the
    not-connected state itself, where the readout's other witnesses assert
    whatever state the nest happened to decide."""
    _nest(nest_instance, "fauna.bridges.set_mail_enabled", {"enabled": True})
    assert _wait(
        lambda: _nest(nest_instance, "fauna.bridges.mail_health")["state"]
        == "bridge_down"
    ), (
        "precondition: mail on with no connected helper must fold bridge_down; "
        f"the nest says {_nest(nest_instance, 'fauna.bridges.mail_health')!r}"
    )
    expected = STATE_LABELS["bridge_down"]

    _open_readout(admin_app)
    assert _wait(lambda: admin_app.admin.mail_health_status().startswith(expected)), (
        f"Mail page line {admin_app.admin.mail_health_status()!r} does not say "
        f"{expected!r}"
    )

    _skip_if_dashboard_card_unbuilt(admin_app)
    admin_app.admin.navigate_dashboard()
    assert _wait(lambda: admin_app.admin.dashboard_card_value("Mail") == expected), (
        f"dashboard Mail card reads {admin_app.admin.dashboard_card_value('Mail')!r}, "
        f"expected {expected!r}"
    )


# ---------------------------------------------------------------------------
# The nest put into each state, read back on the Mail page
#
# Neither DNS nor the minute is real: the nest's DNS seam answers for the
# blocklists and the records, and its mail clock moves past the once-a-minute
# force-refresh limit (`helpers/mail_health_seeds.py`). Mail is on and its
# helper connected (`mail_bridge_mta`), so the states below "not connected" are
# reachable. Check again is the app's own press, never an RPC standing in for it
# (convention 8); only the DNS and the clock are seeded from outside.
# ---------------------------------------------------------------------------

LISTING = "zen.spamhaus.org"
SECOND_LISTING = "b.barracudacentral.org"
REASON = "Listed for spam pattern X"
SECOND_REASON = "Reported by receivers"
SPAMHAUS_REMOVAL = "https://check.spamhaus.org/"
BARRACUDA_REMOVAL = "https://www.barracudacentral.org/rbl/removal-request"
DAY = 86_400
#: The outbound policy's delay-warning age (4 h), and past its five-day give-up.
WARNING_AGE = 4 * 3_600
GIVE_UP_AGE = 6 * DAY
#: The shared relative-time wording for a heartbeat that has happened.
HEARTBEAT_AGO = re.compile(r"^(just now|\d+[mhd] ago)$")


@pytest.fixture
def outbound(mail_bridge_mta, nest_instance):
    """Mail on, its helper connected, the outbound address resolving and every
    blocklist answering "not listed"; the standing result is clean again on the
    way out."""
    with seeded_outbound(nest_instance) as nest:
        yield nest


def _press_check_again(admin_app, nest) -> None:
    """Move the mail clock past the per-list force-refresh limit, then press
    Check again."""
    advance_mail_clock(nest, PAST_THE_LIMIT)
    admin_app.admin.recheck_mail_health()


def _blocklist_row(admin_app) -> dict:
    return admin_app.admin.mail_health_check(1)


@pytest.mark.feature("admin-mail-health")
def test_a_listed_address_reads_blocklisted_naming_the_list_its_reason_and_the_removal_page(
    admin_app, nest_instance, outbound
):
    """The latest blocklist check lists the address: the line says it is
    blocklisted, the blocklist row names the list and the reason it gave, and
    the removal link leads to that list's removal page — and to another's once
    that is the list holding the address."""
    nest = outbound
    answer_listed(nest, LISTING, REASON)
    run_self_check(nest)
    health = _nest(nest_instance, "fauna.bridges.mail_health")
    assert health["state"] == "blocklisted", f"precondition: {health!r}"
    assert health["delist_url"] == SPAMHAUS_REMOVAL, health

    _open_readout(admin_app)
    expected = STATE_LABELS["blocklisted"]
    assert _wait(lambda: admin_app.admin.mail_health_status().startswith(expected)), (
        f"status line {admin_app.admin.mail_health_status()!r} does not say {expected!r}"
    )
    assert _wait(
        lambda: LISTING in _blocklist_row(admin_app)["detail"]
        and REASON in _blocklist_row(admin_app)["detail"]
    ), f"the blocklist row must name the list and its reason: {_blocklist_row(admin_app)!r}"
    assert _blocklist_row(admin_app)["state"] == "Problem", _blocklist_row(admin_app)
    assert _wait(admin_app.admin.mail_health_delist_present), (
        "the removal link is offered while the address is listed"
    )
    assert admin_app.admin.mail_health_delist_target() == SPAMHAUS_REMOVAL

    # The address moves to another list: the link follows it.
    answer_not_listed(nest, LISTING)
    answer_listed(nest, SECOND_LISTING, SECOND_REASON)
    _press_check_again(admin_app, nest)
    assert _wait(
        lambda: admin_app.admin.mail_health_delist_target() == BARRACUDA_REMOVAL
    ), (
        "the link must lead to the removal page of the list that now holds the "
        f"address: {admin_app.admin.mail_health_delist_target()!r}"
    )


@pytest.mark.feature("admin-mail-health")
def test_an_address_on_several_lists_names_them_all_and_links_the_first(
    admin_app, nest_instance, outbound
):
    nest = outbound
    answer_listed(nest, LISTING, REASON)
    answer_listed(nest, SECOND_LISTING, SECOND_REASON)
    run_self_check(nest)
    assert _nest(nest_instance, "fauna.bridges.mail_health")["state"] == "blocklisted"

    _open_readout(admin_app)
    assert _wait(
        lambda: all(
            needle in _blocklist_row(admin_app)["detail"]
            for needle in (LISTING, REASON, SECOND_LISTING, SECOND_REASON)
        )
    ), f"the blocklist row must name every list holding the address: {_blocklist_row(admin_app)!r}"
    assert _wait(admin_app.admin.mail_health_delist_present)
    link = admin_app.admin.mail_health_delist_target()
    assert link == SPAMHAUS_REMOVAL, (
        f"the link leads to the first listing that has a removal page: {link!r}"
    )


@pytest.mark.feature("admin-mail-health")
def test_a_blocklist_that_did_not_answer_is_a_warning_distinct_from_listed_and_clear(
    admin_app, nest_instance, outbound
):
    nest = outbound
    _open_readout(admin_app)
    seen = {}

    def reads(label):
        return lambda: _blocklist_row(admin_app)["state"] == label

    _press_check_again(admin_app, nest)
    assert _wait(reads("OK")), f"every list answering clear reads OK: {_blocklist_row(admin_app)!r}"
    seen["clear"] = _blocklist_row(admin_app)["state"]

    answer_nothing(nest, LISTING)
    _press_check_again(admin_app, nest)
    assert _wait(reads("Warning")), (
        f"a list that did not answer is a warning: {_blocklist_row(admin_app)!r}"
    )
    seen["unanswered"] = _blocklist_row(admin_app)["state"]
    assert "did not answer" in _blocklist_row(admin_app)["detail"], _blocklist_row(admin_app)
    assert not admin_app.admin.mail_health_status().startswith(STATE_LABELS["blocklisted"]), (
        "an unanswered list is not a listing"
    )

    answer_listed(nest, LISTING, REASON)
    _press_check_again(admin_app, nest)
    assert _wait(reads("Problem")), f"a listing is a problem: {_blocklist_row(admin_app)!r}"
    seen["listed"] = _blocklist_row(admin_app)["state"]
    assert len(set(seen.values())) == 3, f"the three outcomes must read differently: {seen!r}"


@pytest.mark.feature("admin-mail-health")
def test_failing_records_read_dns_records_need_attention(admin_app, nest_instance, outbound):
    """With the address clear and the sender-policy, signing, reporting-policy
    and reverse-lookup records unpublished, Check again runs the records check
    and the line says the DNS records need attention."""
    nest = outbound
    _open_readout(admin_app)
    _press_check_again(admin_app, nest)
    expected = STATE_LABELS["records_failing"]
    assert _wait(lambda: admin_app.admin.mail_health_status().startswith(expected)), (
        f"status line {admin_app.admin.mail_health_status()!r} does not say {expected!r}; "
        f"the nest says {_nest(nest_instance, 'fauna.bridges.mail_health')!r}"
    )
    row = admin_app.admin.mail_health_check(3)
    assert row["state"] == "Problem" and row["detail"].startswith("failing:"), row
    nest_row = _nest(nest_instance, "fauna.bridges.mail_health")["checks"][3]
    assert row["detail"] == nest_row["detail"], (row, nest_row)


@pytest.mark.feature("admin-mail-health")
def test_a_healthy_server_inside_its_ramp_reads_warming_up(admin_app, nest_instance, outbound):
    nest = outbound
    _nest(nest_instance, "fauna.bridges.outbound_warmup_reset")
    assert _nest(nest_instance, "fauna.bridges.outbound_warmup_status")["today_max"], (
        "precondition: a freshly restarted ramp is capped"
    )
    with passing_records(nest):
        _open_readout(admin_app)
        _press_check_again(admin_app, nest)
        expected = STATE_LABELS["warming_up"]
        assert _wait(lambda: admin_app.admin.mail_health_status().startswith(expected)), (
            f"status line {admin_app.admin.mail_health_status()!r} does not say {expected!r}; "
            f"the nest says {_nest(nest_instance, 'fauna.bridges.mail_health')!r}"
        )
        assert admin_app.admin.mail_health_check(3)["state"] == "OK", (
            "nothing worse is holding, so the records row passes: "
            f"{admin_app.admin.mail_health_check(3)!r}"
        )


@pytest.mark.feature("admin-mail-health")
def test_a_healthy_server_past_its_ramp_reads_delivering(admin_app, nest_instance, outbound):
    """Mail on, helper connected, address clear, records published and the
    thirty-day ramp over: the line says delivering. The mail clock only moves
    forward (the nest's limits read it as a monotone), and the ramp is put back
    on its first day afterwards."""
    nest = outbound
    _nest(nest_instance, "fauna.bridges.outbound_warmup_reset")
    started = _nest(nest_instance, "fauna.bridges.outbound_warmup_status")["last_reset_at"]
    post(nest, "/api/v1/test/outbound/clock", {"set": started + 31 * DAY})
    try:
        with passing_records(nest):
            assert _nest(nest_instance, "fauna.bridges.outbound_warmup_status")["today_max"] is None, (
                "precondition: the ramp is over thirty-one days on"
            )
            _open_readout(admin_app)
            _press_check_again(admin_app, nest)
            expected = STATE_LABELS["delivering"]
            assert _wait(lambda: admin_app.admin.mail_health_status().startswith(expected)), (
                f"status line {admin_app.admin.mail_health_status()!r} does not say "
                f"{expected!r}; the nest says "
                f"{_nest(nest_instance, 'fauna.bridges.mail_health')!r}"
            )
    finally:
        _nest(nest_instance, "fauna.bridges.outbound_warmup_reset")


@pytest.mark.feature("admin-mail-health")
def test_the_heartbeats_say_how_long_ago_or_never(admin_app, nest_instance):
    """Last delivered and last received each read as a relative time once the
    thing has happened, and as "Never" while the nest has no stamp for it; the
    status line carries the same two facts."""
    health = _nest(nest_instance, "fauna.bridges.mail_health")
    _open_readout(admin_app)
    assert _wait(lambda: admin_app.admin.mail_health_check_count() == 7)
    facts = []
    for index, field in (
        (5, "last_outbound_delivered_at"),
        (6, "last_inbound_accepted_at"),
    ):
        def detail(index=index):
            return admin_app.admin.mail_health_check(index)["detail"].strip()

        if health[field] is None:
            assert _wait(lambda: detail() == "Never"), (
                f"no stamp, so the row must read Never: {detail()!r}"
            )
        else:
            assert _wait(lambda: HEARTBEAT_AGO.match(detail())), (
                f"a stamp must read as a time ago: {detail()!r}"
            )
        facts.append(detail())
    status = admin_app.admin.mail_health_status()
    assert f"last delivered: {facts[0]} · last received: {facts[1]}" in status, (status, facts)


@pytest.mark.feature("admin-mail-health")
def test_a_state_newer_than_the_app_reads_needs_attention(admin_app, nest_instance):
    """The nest answers with a state token this app has never heard of (a newer
    nest's): the Mail page and the dashboard's Mail card say mail needs
    attention, and nothing fails. The nest is stood in for by its `test-hooks`
    state override, and put back afterwards."""
    expected = "Mail: needs attention"
    post(nest_instance, "/api/v1/test/mail_health/state", {"state": "state_from_a_newer_nest"})
    try:
        _open_readout(admin_app)
        assert _wait(lambda: admin_app.admin.mail_health_status().startswith(expected)), (
            f"status line {admin_app.admin.mail_health_status()!r} does not say {expected!r}; "
            f"error: {admin_app.error_text()!r}"
        )
        assert _wait(lambda: admin_app.admin.mail_health_check_count() == 7), (
            "the seven rows still render beside an unknown state"
        )
        assert not admin_app.error_text(), admin_app.error_text()

        _skip_if_dashboard_card_unbuilt(admin_app)
        admin_app.admin.navigate_dashboard()
        assert _wait(lambda: admin_app.admin.dashboard_card_value("Mail") == expected), (
            f"dashboard Mail card reads {admin_app.admin.dashboard_card_value('Mail')!r}, "
            f"expected {expected!r}"
        )
    finally:
        post(nest_instance, "/api/v1/test/mail_health/state", {"state": None})


def _queue_rows(nest, recipient: str) -> list[dict]:
    """The outbound queue's rows for one recipient (the nest's `test-hooks`
    queue read)."""
    with urllib.request.urlopen(
        f"{nest['url']}/api/v1/test/outbound/queue", timeout=5.0
    ) as resp:
        return [r for r in json.loads(resp.read().decode())["rows"] if r["recipient"] == recipient]


@pytest.mark.feature("admin-mail-health")
def test_a_failed_message_older_than_the_warning_age_reads_outgoing_mail_delayed(
    admin_app, nest_instance, outbound, mail_bridge_mta
):
    """A message the outside server turned away once and that has waited past
    the delay-warning age: the line says outgoing mail is delayed and the queue
    row says how many. The message is let go afterwards (the mail clock past the
    five-day ceiling), so the shared nest's queue holds nothing failing."""
    nest = outbound
    token = f"stalled-{uuid.uuid4().hex[:10]}"
    recipient = f"tempfail-{token}@external.test"
    submit_one_message(mail_bridge_mta, recipient, token)
    try:
        _poke_outbound_bridge(nest["url"])
        assert _wait(
            lambda: any(
                r["status"] == "pending" and r["attempt_count"] >= 1
                for r in _queue_rows(nest, recipient)
            ),
            30.0,
        ), f"the outside server never turned the message away: {_queue_rows(nest, recipient)!r}"
        advance_mail_clock(nest, WARNING_AGE + 3_600)
        health = _nest(nest_instance, "fauna.bridges.mail_health")
        assert health["state"] == "queue_stalled", f"precondition: {health!r}"

        _open_readout(admin_app)
        expected = STATE_LABELS["queue_stalled"]
        assert _wait(lambda: admin_app.admin.mail_health_status().startswith(expected)), (
            f"status line {admin_app.admin.mail_health_status()!r} does not say {expected!r}"
        )
        row = admin_app.admin.mail_health_check(2)
        assert row["state"] == "Problem" and "delayed" in row["detail"], row
    finally:
        # Past the five-day ceiling the next failed attempt is the last one: the
        # message bounces and leaves the queue.
        advance_mail_clock(nest, GIVE_UP_AGE)
        deadline_ok = _wait(
            lambda: (
                _poke_outbound_bridge(nest["url"]),
                all(r["status"] != "pending" for r in _queue_rows(nest, recipient)),
            )[1],
            60.0,
        )
        # The mail clock only moved forward. Restarting the warm-up stamps the
        # clock where `tests/api/test_mail_health.py::admin_now` looks for its
        # high-water mark, so a later module that sets the clock from there never
        # sets it back behind the checks already recorded.
        _nest(nest_instance, "fauna.bridges.outbound_warmup_reset")
        assert deadline_ok, (
            "the failing message was not let go: " f"{_queue_rows(nest, recipient)!r}"
        )
