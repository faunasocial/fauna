"""The admin Mail reports page shows the DMARC aggregate reports peers mail in.

`docs/goal/behavior/dmarc-reporting.md` § Admin pane summary surface: the
`admin-mail-reports` page reads `fauna.bridges.list_dmarc_aggregate_reports` for
a window and a domain filter, folds the window totals in shared Rust
(`fauna_mail::dmarc_report::WindowSummary`, through the `DmarcReportsMachine`),
lists the top sources and senders and one row per report, and opens a report's
XML (`get_dmarc_aggregate_report`, gunzipped in shared Rust) into its raw view.
An empty window paints the empty state.

tier_3: a real app driver against a real `fauna-nest` binary and the real Go
mail bridge (`mail_bridge_mta`). The report reaches the nest the way a peer's
does — SMTP into the MX, routed at RCPT TO to the DMARC processor — and the
page's numbers are checked against the nest's own answer to the same list read,
folded here independently, never against the UI alone. Latency-independent
throughout (convention 14): every wait polls for an outcome.

tui is the lead app (the first implementation); the other six follow in their
batched trickle-down and join this module's marks as they land.
"""

from __future__ import annotations

import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers import budgets
from helpers.dmarc_reports import mail_report, report_message, report_xml
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier_3, pytest.mark.tui, pytest.mark.web, pytest.mark.linux]

_T = S.admin.mail_reports_page


def _admin_ws(nest_instance) -> WsRpcAdminClient:
    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _list(nest_instance, window_days: int, our_domain: str | None = None) -> list[dict]:
    body: dict = {"window_days": window_days}
    if our_domain is not None:
        body["our_domain"] = our_domain
    with _admin_ws(nest_instance) as ws:
        return ws.call("fauna.bridges.list_dmarc_aggregate_reports", body)["reports"]


def _share(part: int, whole: int) -> str:
    """The whole-number share, rounded half up — `share_percent`'s contract."""
    return str((part * 100 + whole // 2) // whole) if whole else "0"


def _expected_totals(reports: list[dict]) -> tuple[str, str, str]:
    """The messages / alignment / dispositions lines the page owes `reports`."""
    total = {k: 0 for k in (
        "message_count", "dmarc_pass", "dkim_pass", "spf_pass",
        "disposition_none", "disposition_quarantine", "disposition_reject",
    )}
    for r in reports:
        for k in total:
            total[k] += r["summary"][k]
    n = total["message_count"]
    return (
        _T.messages(
            messages=str(n),
            reports=str(len(reports)),
            peers=str(len({r["submitter"] for r in reports})),
        ),
        _T.alignment(
            dmarc=str(total["dmarc_pass"]), dmarc_share=_share(total["dmarc_pass"], n),
            dkim=str(total["dkim_pass"]), dkim_share=_share(total["dkim_pass"], n),
            spf=str(total["spf_pass"]), spf_share=_share(total["spf_pass"], n),
        ),
        _T.dispositions(
            none=str(total["disposition_none"]),
            quarantine=str(total["disposition_quarantine"]),
            reject=str(total["disposition_reject"]),
        ),
    )


def _page_state(admin_app) -> str:
    """Diagnosis for a timed-out wait (convention 6)."""
    page = admin_app.admin
    shown = (
        page.mail_reports_messages_text() if page.mail_reports_answered() else "<unanswered>"
    )
    return f"messages={shown!r}; error={admin_app.error_text()!r}"


@pytest.mark.feature("dmarc-reporting")
def test_mail_reports_page_shows_a_mailed_in_report(admin_app, nest_instance, mail_bridge_mta):
    """A report mailed to `dmarc-report@` appears on the page: the window
    totals the nest's own list implies, its sources and sender, its row, and
    its XML in the raw view; the domain filter narrows to an empty window and
    back, and a wider window still holds the report."""
    handle = mail_bridge_mta
    domain, other = handle.domain, handle.second_domain
    assert other, "the session mail venue claims a second local domain"
    assert _list(nest_instance, 7, other) == [], (
        f"precondition: nothing has reported on {other}, so its window is empty"
    )

    stamp = int(time.time() * 1000)
    org = f"reporter-{stamp}.test"
    report_id = f"ui-{stamp}"
    xml = report_xml(domain, report_id, org_name=org)
    rcpt = f"dmarc-report@{domain}"
    mail_report(handle, rcpt, report_message(rcpt, xml, report_id))
    # The 250 on `.` is the barrier: the report is stored before the reply.
    window = _list(nest_instance, 7)
    assert any(r["report_id"] == report_id for r in window), (
        f"the nest stored the mailed report; listed {[r['report_id'] for r in window]}"
    )

    page = admin_app.admin
    page.open_mail_reports()
    wait_until(
        lambda: page.mail_reports_answered() and page.mail_report_index(org) is not None,
        budgets.UI_SETTLE_S,
        diagnose=lambda: _page_state(admin_app),
    )
    assert page.mail_reports_window() == "7", "the page opens on the 7-day window"
    assert not admin_app.error_text(), f"no error on a clean read: {admin_app.error_text()!r}"

    # The totals are the fold of exactly what the nest lists for the window.
    messages, alignment, dispositions = _expected_totals(_list(nest_instance, 7))
    assert page.mail_reports_messages_text() == messages
    assert page.mail_reports_alignment_text() == alignment
    assert page.mail_reports_dispositions_text() == dispositions
    assert not page.mail_reports_empty_shown(), "a window with reports has no empty state"

    sources = dict(page.mail_reports_sources())
    assert {"192.0.2.10", "203.0.113.9"} <= sources.keys(), f"top sources: {sources}"
    assert domain in dict(page.mail_reports_senders()), "the report's header-From domain"

    row = page.mail_report_row(page.mail_report_index(org))
    assert row["domain"] == domain
    assert row["messages"] == _T.row_messages(count="15")
    assert row["pass"] == _T.report_pass(passed="12", count="15")

    page.open_mail_report(page.mail_report_index(org))
    raw = wait_until(page.mail_report_raw, budgets.UI_SETTLE_S,
                     diagnose=lambda: _page_state(admin_app))
    assert raw == xml, "the raw view is the archived report, gunzipped"

    # Narrow to a domain nobody reported on: an answered, empty window.
    page.select_mail_reports_domain(other)
    wait_until(page.mail_reports_empty_shown, budgets.UI_SETTLE_S,
               diagnose=lambda: _page_state(admin_app))
    assert page.mail_reports_messages_text() == _expected_totals([])[0]
    assert page.mail_report_index(org) is None
    assert page.mail_report_raw() is None, "a filter change closes the opened report"

    # Back to every domain on a wider window: the report is still in it.
    page.select_mail_reports_domain("")
    page.select_mail_reports_window(30)
    wait_until(
        lambda: page.mail_reports_window() == "30" and page.mail_report_index(org) is not None,
        budgets.UI_SETTLE_S,
        diagnose=lambda: _page_state(admin_app),
    )
    assert page.mail_reports_messages_text() == _expected_totals(_list(nest_instance, 30))[0]
