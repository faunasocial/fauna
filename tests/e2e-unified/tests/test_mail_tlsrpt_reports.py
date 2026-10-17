"""TLSRPT reports mailed to the nest are processed, not delivered.

`docs/goal/behavior/smtp-server.md` § TLSRPT inbound report listener: every
domain the nest serves publishes `_smtp._tls` with `rua=mailto:tlsrpt@<primary>`,
so a sending MTA that saw TLS trouble delivering to us mails it an RFC 8460
JSON report a day. The MTA routes `tlsrpt@` at RCPT TO to the nest's processor
(`resolve_recipient` → `Processor { tlsrpt_inbound }`), which validates and
stores the report; the admin lists it over the wire. A malformed report, or one
about a domain the nest does not serve, is accepted on the wire (250) and
dropped. None of them ever reaches a mailbox.

The DMARC aggregate leg's twin (`test_mail_dmarc_reports.py`): real SMTP into
the real MTA beside a real nest, on a mail venue of this module's own, so its
inbound heartbeat (`fauna.bridges.mail_health` → `last_inbound_accepted_at`,
stamped by every mailbox ingest) staying unset is the observation that no copy
reached the admin's inbox.

Test taxonomy: `tier_3` — every binary real, real wire end to end; no app
driver participates.
"""

import base64
import gzip
import json
import time

import pytest

from helpers.mail_wire import _connect_smtp_starttls, _open_mail_gate_and_wait_for_the_mta

pytestmark = pytest.mark.tier_3


def _admin_ws(nest):
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    admin = nest["admin"]
    return WsRpcAdminClient(
        nest["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


@pytest.fixture(scope="module")
def tlsrpt_venue(request, nest_mode, tmp_path_factory):
    """A claimed mail nest of this module's own, its mail gate open."""
    from conftest import _start_mail_venue

    handle, cleanup = _start_mail_venue(
        request, nest_mode, tmp_path_factory, "tlsrpt-report-venue"
    )
    try:
        _open_mail_gate_and_wait_for_the_mta(handle)
        yield handle
    finally:
        cleanup()


def _report_json(domain: str, report_id: str) -> str:
    """An RFC 8460 §4.4 report: one STS policy about `domain` with two kinds
    of failure."""
    return json.dumps(
        {
            "organization-name": "Reporter",
            "date-range": {
                "start-datetime": "2026-05-22T00:00:00Z",
                "end-datetime": "2026-05-22T23:59:59Z",
            },
            "contact-info": "tlsrpt-noreply@reporter.test",
            "report-id": report_id,
            "policies": [
                {
                    "policy": {
                        "policy-type": "sts",
                        "policy-string": ["version: STSv1", "mode: enforce"],
                        "policy-domain": domain,
                        "mx-host": [f"mx.{domain}"],
                    },
                    "summary": {
                        "total-successful-session-count": 40,
                        "total-failure-session-count": 5,
                    },
                    "failure-details": [
                        {
                            "result-type": "certificate-expired",
                            "sending-mta-ip": "192.0.2.10",
                            "receiving-mx-hostname": f"mx.{domain}",
                            "failed-session-count": 4,
                        },
                        {
                            "result-type": "starttls-not-supported",
                            "sending-mta-ip": "203.0.113.9",
                            "receiving-mx-hostname": f"mx.{domain}",
                            "failed-session-count": 1,
                        },
                    ],
                }
            ],
        }
    )


def _report_message(rcpt: str, body: str, tag: str) -> bytes:
    """A report mailed per RFC 8460 §5.3: multipart/report with an
    `application/tlsrpt+gzip` part."""
    b64 = base64.encodebytes(gzip.compress(body.encode())).decode().replace("\n", "\r\n")
    lines = [
        "From: TLS reporter <tlsrpt-noreply@reporter.test>",
        f"To: {rcpt}",
        f"Subject: Report Domain: {tag} Submitter: reporter.test Report-ID: <{tag}>",
        f"Message-ID: <{tag}@reporter.test>",
        "Date: Sat, 23 May 2026 12:00:00 +0000",
        "TLS-Report-Submitter: reporter.test",
        "MIME-Version: 1.0",
        'Content-Type: multipart/report; report-type="tlsrpt"; boundary="rpt"',
        "",
        "--rpt",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "This is an aggregate TLS report.",
        "--rpt",
        "Content-Type: application/tlsrpt+gzip",
        "Content-Transfer-Encoding: base64",
        f'Content-Disposition: attachment; filename="{tag}.json.gz"',
        "",
        b64.rstrip("\r\n"),
        "--rpt--",
        "",
    ]
    return "\r\n".join(lines).encode()


def _mail(handle, rcpt: str, message: bytes) -> None:
    """One SMTP transaction; every report outcome is a 250 on the wire."""
    deadline = time.monotonic() + 30.0
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd("MAIL FROM:<tlsrpt-noreply@reporter.test>", "250", deadline)
        conn.cmd(f"RCPT TO:<{rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(message)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


@pytest.mark.feature("tlsrpt-reporting")
def test_tlsrpt_report_is_stored_and_listed_never_delivered(tlsrpt_venue):
    """A valid report is stored and listed to the admin; a malformed one and
    one about a foreign domain are accepted on the wire but not stored; no copy
    of any of them reaches a mailbox."""
    handle = tlsrpt_venue
    domain = handle.domain
    rcpt = f"tlsrpt@{domain}"
    stamp = int(time.time() * 1000)
    good_id = f"good-{stamp}"

    _mail(handle, rcpt, _report_message(rcpt, _report_json(domain, good_id), good_id))
    # Required field missing (`report-id`): malformed → 250, dropped.
    _mail(handle, rcpt, _report_message(rcpt, _report_json(domain, ""), f"bad-{stamp}"))
    # Not JSON at all → 250, dropped.
    _mail(handle, rcpt, _report_message(rcpt, "<feedback/>", f"xml-{stamp}"))
    # About a domain this nest does not serve → 250, dropped.
    foreign_id = f"foreign-{stamp}"
    _mail(
        handle,
        rcpt,
        _report_message(rcpt, _report_json("not-ours.test", foreign_id), foreign_id),
    )

    # The 250 on `.` is the barrier: the hand-off to the nest completes inside
    # DATA, before the reply.
    with _admin_ws(handle.nest) as ws:
        listed = ws.call("fauna.bridges.list_tlsrpt_inbound_reports", {"window_days": 1})
        reports = listed["reports"]
        ids = sorted(r["report_id"] for r in reports)
        assert ids == [good_id], (
            f"only the valid report about our own domain is stored; listed {ids}"
        )
        entry = reports[0]
        assert entry["our_domain"] == domain.lower()
        assert entry["submitter"] == "Reporter <tlsrpt-noreply@reporter.test>"
        summary = entry["summary"]
        assert summary["policy_count"] == 1, summary
        assert summary["successful_sessions"] == 40, summary
        assert summary["failed_sessions"] == 5, summary
        assert summary["top_result_types"][0] == {
            "value": "certificate-expired",
            "count": 4,
        }, summary

        got = ws.call(
            "fauna.bridges.get_tlsrpt_inbound_report",
            {
                "our_domain": entry["our_domain"],
                "submitter": entry["submitter"],
                "report_id": entry["report_id"],
            },
        )
        assert gzip.decompress(bytes(got["payload_json_gz"])).decode() == _report_json(
            domain, good_id
        ), "the archived JSON is the report as received"

        health = ws.call("fauna.bridges.mail_health", {})
        assert health.get("last_inbound_accepted_at") is None, (
            "no report may reach a mailbox: this venue's inbound heartbeat moved, "
            f"so something was ingested into one ({health.get('last_inbound_accepted_at')})"
        )
