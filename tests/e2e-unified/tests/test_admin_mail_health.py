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
import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers import budgets
from helpers.waiting import wait_until

pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.tui,
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
    admin_app.admin.recheck_mail_health()

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
    admin_app.admin.navigate_dashboard()
    state = _nest(nest_instance, "fauna.bridges.mail_health")["state"]
    assert _wait(
        lambda: admin_app.admin.dashboard_card_value("Mail") == STATE_LABELS[state]
    ), (
        f"Mail card reads {admin_app.admin.dashboard_card_value('Mail')!r}, "
        f"expected {STATE_LABELS[state]!r}"
    )
