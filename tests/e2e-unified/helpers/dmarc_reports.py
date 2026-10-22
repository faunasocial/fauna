"""DMARC aggregate reports mailed into a mail venue the way large receivers do.

Shared by the receive-pipeline tests (`test_mail_dmarc_reports.py`) and the
admin page's witness (`test_admin_mail_reports.py`): one report shape, one
attachment shape, one SMTP transaction (`dmarc-reporting.md` § Aggregate-report
receive (RUA)). Every fixture address is on a reserved `.test` domain.
"""

from __future__ import annotations

import base64
import gzip
import time

from helpers.mail_wire import _connect_smtp_starttls


def report_xml(domain: str, report_id: str, *, org_name: str = "reporter.test") -> str:
    """An RFC 7489 §A.1 aggregate report about `domain`: 15 messages, 12 from
    192.0.2.10 passing DKIM and SPF, 3 from 203.0.113.9 failing both and
    rejected. `org_name` names the reporting peer — the submitter is
    `<org_name> <dmarc-noreply@reporter.test>`."""
    return (
        '<?xml version="1.0" encoding="UTF-8"?>'
        '<feedback xmlns="urn:ietf:params:xml:ns:dmarc-2.0"><report_metadata>'
        f"<org_name>{org_name}</org_name><email>dmarc-noreply@reporter.test</email>"
        f"<report_id>{report_id}</report_id>"
        "<date_range><begin>1700000000</begin><end>1700086399</end></date_range>"
        f"</report_metadata><policy_published><domain>{domain}</domain>"
        "<adkim>s</adkim><aspf>s</aspf><p>reject</p><sp>reject</sp><pct>100</pct>"
        "</policy_published>"
        "<record><row><source_ip>192.0.2.10</source_ip><count>12</count>"
        "<policy_evaluated><disposition>none</disposition><dkim>pass</dkim>"
        "<spf>pass</spf></policy_evaluated></row>"
        f"<identifiers><header_from>{domain}</header_from></identifiers></record>"
        "<record><row><source_ip>203.0.113.9</source_ip><count>3</count>"
        "<policy_evaluated><disposition>reject</disposition><dkim>fail</dkim>"
        "<spf>fail</spf></policy_evaluated></row>"
        f"<identifiers><header_from>{domain}</header_from></identifiers></record>"
        "</feedback>"
    )


def report_message(rcpt: str, xml: str, tag: str) -> bytes:
    """A report mailed the way large receivers send one: a gzip attachment."""
    b64 = base64.encodebytes(gzip.compress(xml.encode())).decode().replace("\n", "\r\n")
    lines = [
        "From: DMARC reporter <dmarc-noreply@reporter.test>",
        f"To: {rcpt}",
        f"Subject: Report domain: {tag}",
        f"Message-ID: <{tag}@reporter.test>",
        "Date: Sat, 23 May 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        'Content-Type: multipart/mixed; boundary="rpt"',
        "",
        "--rpt",
        "Content-Type: text/plain; charset=utf-8",
        "",
        "This is an aggregate report.",
        "--rpt",
        f'Content-Type: application/gzip; name="{tag}.xml.gz"',
        "Content-Transfer-Encoding: base64",
        f'Content-Disposition: attachment; filename="{tag}.xml.gz"',
        "",
        b64.rstrip("\r\n"),
        "--rpt--",
        "",
    ]
    return "\r\n".join(lines).encode()


def mail_report(handle, rcpt: str, message: bytes) -> None:
    """One SMTP transaction into `handle`'s MX; every report outcome is a 250
    on the wire, and the 250 on `.` is the barrier — the hand-off to the nest
    completes inside DATA, before the reply."""
    deadline = time.monotonic() + 30.0
    with _connect_smtp_starttls(handle.mx_port, handle.domain, deadline) as conn:
        conn.cmd("MAIL FROM:<dmarc-noreply@reporter.test>", "250", deadline)
        conn.cmd(f"RCPT TO:<{rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(message)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)
